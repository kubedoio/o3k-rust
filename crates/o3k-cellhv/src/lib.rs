use std::{fmt, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use o3k_provider::{
    Capabilities, ComputeProvider, CreateInstanceRequest, DeleteInstanceRequest, ErrorCategory,
    Instance, InstanceAction, InstanceState, Operation, OperationState, ProviderError,
};
use o3k_provider_contract::proto::{self, compute_provider_client::ComputeProviderClient};
use thiserror::Error;
use tokio::sync::Mutex;
use tonic::{
    Request,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct CellHvConfig {
    pub endpoint: String,
    pub expected_version: String,
    pub ca_certificate: Option<PathBuf>,
    pub client_certificate: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

impl fmt::Debug for CellHvConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CellHvConfig")
            .field("endpoint", &self.endpoint)
            .field("expected_version", &self.expected_version)
            .field("ca_certificate", &self.ca_certificate)
            .field("client_certificate", &self.client_certificate)
            .field("client_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CellHvError {
    #[error("CellHV configuration is invalid")]
    InvalidConfiguration,
    #[error("CellHV TLS material is unavailable")]
    TlsMaterial,
    #[error("CellHV transport is unavailable")]
    Transport(#[source] tonic::transport::Error),
    #[error("CellHV capability or operation response is incompatible")]
    Incompatible,
}

#[derive(Clone)]
pub struct CellHvProvider {
    client: Arc<Mutex<ComputeProviderClient<Channel>>>,
    expected_version: String,
}

impl CellHvProvider {
    pub async fn connect(config: &CellHvConfig) -> Result<Self, CellHvError> {
        if config.endpoint.trim().is_empty() || config.expected_version.trim().is_empty() {
            return Err(CellHvError::InvalidConfiguration);
        }
        let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
            .map_err(|_| CellHvError::InvalidConfiguration)?;
        if config.endpoint.starts_with("https://") {
            let (Some(ca), Some(cert), Some(key)) = (
                &config.ca_certificate,
                &config.client_certificate,
                &config.client_key,
            ) else {
                return Err(CellHvError::TlsMaterial);
            };
            let ca = fs::read(ca).map_err(|_| CellHvError::TlsMaterial)?;
            let cert = fs::read(cert).map_err(|_| CellHvError::TlsMaterial)?;
            let key = fs::read(key).map_err(|_| CellHvError::TlsMaterial)?;
            endpoint = endpoint
                .tls_config(
                    ClientTlsConfig::new()
                        .ca_certificate(Certificate::from_pem(ca))
                        .identity(Identity::from_pem(cert, key)),
                )
                .map_err(|_| CellHvError::InvalidConfiguration)?;
        }
        let client = ComputeProviderClient::connect(endpoint)
            .await
            .map_err(CellHvError::Transport)?;
        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            expected_version: config.expected_version.clone(),
        })
    }

    async fn operation(&self, response: proto::Operation) -> Result<Operation, ProviderError> {
        let provider_operation_id = Uuid::parse_str(&response.provider_operation_id)
            .map_err(|_| ProviderError::Terminal)?;
        let o3k_operation_id =
            Uuid::parse_str(&response.o3k_operation_id).map_err(|_| ProviderError::Terminal)?;
        let state = match proto::OperationState::try_from(response.state)
            .map_err(|_| ProviderError::Terminal)?
        {
            proto::OperationState::Accepted => OperationState::Accepted,
            proto::OperationState::Running => OperationState::Running,
            proto::OperationState::Succeeded => OperationState::Succeeded,
            proto::OperationState::Retryable => OperationState::Retryable,
            proto::OperationState::UnknownOutcome => OperationState::UnknownOutcome,
            proto::OperationState::Failed => OperationState::Failed,
            proto::OperationState::Unspecified => return Err(ProviderError::Terminal),
        };
        let category = proto::ErrorCategory::try_from(response.error_category)
            .ok()
            .and_then(|category| match category {
                proto::ErrorCategory::InvalidRequest => Some(ErrorCategory::InvalidRequest),
                proto::ErrorCategory::NotFound => Some(ErrorCategory::NotFound),
                proto::ErrorCategory::Conflict => Some(ErrorCategory::Conflict),
                proto::ErrorCategory::Capacity => Some(ErrorCategory::Capacity),
                proto::ErrorCategory::Retryable => Some(ErrorCategory::Retryable),
                proto::ErrorCategory::UnknownOutcome => Some(ErrorCategory::UnknownOutcome),
                proto::ErrorCategory::Terminal => Some(ErrorCategory::Terminal),
                proto::ErrorCategory::Unspecified => None,
            });
        Ok(Operation {
            provider_operation_id,
            o3k_operation_id,
            state,
            error_category: category,
            provider_resource_id: (!response.provider_resource_id.is_empty())
                .then_some(response.provider_resource_id),
        })
    }

    fn transport_error(status: tonic::Status) -> ProviderError {
        match status.code() {
            tonic::Code::InvalidArgument => ProviderError::InvalidRequest,
            tonic::Code::NotFound => ProviderError::NotFound,
            tonic::Code::AlreadyExists | tonic::Code::FailedPrecondition => ProviderError::Conflict,
            tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::ResourceExhausted => ProviderError::Retryable,
            _ => ProviderError::Terminal,
        }
    }

    async fn action(
        &self,
        provider_instance_id: &str,
        action: InstanceAction,
        operation_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Operation, ProviderError> {
        let request = proto::ActionInstanceRequest {
            operation_id: operation_id.to_string(),
            provider_instance_id: provider_instance_id.to_owned(),
            idempotency_key: idempotency_key.to_owned(),
        };
        let response = match action {
            InstanceAction::Start => {
                self.client
                    .lock()
                    .await
                    .start_instance(Request::new(request))
                    .await
            }
            InstanceAction::Stop => {
                self.client
                    .lock()
                    .await
                    .stop_instance(Request::new(request))
                    .await
            }
            InstanceAction::Reboot => {
                self.client
                    .lock()
                    .await
                    .reboot_instance(Request::new(request))
                    .await
            }
        }
        .map_err(Self::transport_error)?;
        self.operation(response.into_inner()).await
    }
}

#[async_trait]
impl ComputeProvider for CellHvProvider {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
        let response = self
            .client
            .lock()
            .await
            .get_capabilities(Request::new(proto::GetCapabilitiesRequest {}))
            .await
            .map_err(Self::transport_error)?
            .into_inner();
        validate_capabilities(
            &Capabilities {
                provider_name: response.provider_name.clone(),
                provider_version: response.provider_version.clone(),
                capabilities: response.capabilities.clone(),
            },
            &self.expected_version,
        )?;
        Ok(Capabilities {
            provider_name: response.provider_name,
            provider_version: response.provider_version,
            capabilities: response.capabilities,
        })
    }

    async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        let response = self
            .client
            .lock()
            .await
            .create_instance(Request::new(proto::CreateInstanceRequest {
                operation_id: request.operation_id.to_string(),
                o3k_server_id: request.o3k_server_id.to_string(),
                name: request.name,
                vcpus: request.vcpus,
                memory_mib: request.memory_mib,
                image_id: request.image_id.unwrap_or_default(),
                idempotency_key: request.idempotency_key,
            }))
            .await
            .map_err(Self::transport_error)?;
        self.operation(response.into_inner()).await
    }

    async fn get_instance(&self, provider_instance_id: &str) -> Result<Instance, ProviderError> {
        let response = self
            .client
            .lock()
            .await
            .get_instance(Request::new(proto::GetInstanceRequest {
                provider_instance_id: provider_instance_id.to_owned(),
            }))
            .await
            .map_err(Self::transport_error)?
            .into_inner();
        let state = match proto::InstanceState::try_from(response.state)
            .map_err(|_| ProviderError::Terminal)?
        {
            proto::InstanceState::Creating => InstanceState::Creating,
            proto::InstanceState::Running => InstanceState::Running,
            proto::InstanceState::Stopped => InstanceState::Stopped,
            proto::InstanceState::Deleting => InstanceState::Deleting,
            proto::InstanceState::Deleted => InstanceState::Deleted,
            proto::InstanceState::Error => InstanceState::Error,
            proto::InstanceState::Unspecified => return Err(ProviderError::Terminal),
        };
        Ok(Instance {
            provider_instance_id: response.provider_instance_id,
            o3k_server_id: Uuid::parse_str(&response.o3k_server_id)
                .map_err(|_| ProviderError::Terminal)?,
            state,
            observed_message: (!response.observed_message.is_empty())
                .then_some(response.observed_message),
        })
    }

    async fn delete_instance(
        &self,
        request: DeleteInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        let response = self
            .client
            .lock()
            .await
            .delete_instance(Request::new(proto::DeleteInstanceRequest {
                operation_id: request.operation_id.to_string(),
                provider_instance_id: request.provider_instance_id,
                idempotency_key: request.idempotency_key,
            }))
            .await
            .map_err(Self::transport_error)?;
        self.operation(response.into_inner()).await
    }

    async fn action_instance(
        &self,
        provider_instance_id: &str,
        action: InstanceAction,
        operation_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Operation, ProviderError> {
        self.action(provider_instance_id, action, operation_id, idempotency_key)
            .await
    }

    async fn get_operation(&self, provider_operation_id: Uuid) -> Result<Operation, ProviderError> {
        let response = self
            .client
            .lock()
            .await
            .get_operation(Request::new(proto::GetOperationRequest {
                provider_operation_id: provider_operation_id.to_string(),
            }))
            .await
            .map_err(Self::transport_error)?;
        self.operation(response.into_inner()).await
    }
}

pub fn validate_capabilities(
    capabilities: &Capabilities,
    expected_version: &str,
) -> Result<(), ProviderError> {
    if capabilities.provider_version != expected_version
        || !capabilities
            .capabilities
            .iter()
            .any(|capability| capability == "compute.instance.create")
        || !capabilities
            .capabilities
            .iter()
            .any(|capability| capability == "compute.instance.delete")
    {
        return Err(ProviderError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_redacts_client_key_path() {
        let config = CellHvConfig {
            endpoint: "https://cellhv.invalid".to_owned(),
            expected_version: "v1".to_owned(),
            ca_certificate: None,
            client_certificate: None,
            client_key: Some(PathBuf::from("private-key.pem")),
        };
        assert!(!format!("{config:?}").contains("private-key.pem"));
    }

    #[tokio::test]
    async fn https_connection_requires_mtls_material() {
        let config = CellHvConfig {
            endpoint: "https://cellhv.invalid".to_owned(),
            expected_version: "v1".to_owned(),
            ca_certificate: None,
            client_certificate: None,
            client_key: None,
        };
        assert!(matches!(
            CellHvProvider::connect(&config).await,
            Err(CellHvError::TlsMaterial)
        ));
    }

    #[test]
    fn capability_mismatch_is_rejected_before_mutation() {
        let capabilities = Capabilities {
            provider_name: "cellhv".to_owned(),
            provider_version: "v0".to_owned(),
            capabilities: vec!["compute.instance.create".to_owned()],
        };
        assert!(matches!(
            validate_capabilities(&capabilities, "v1"),
            Err(ProviderError::InvalidRequest)
        ));
    }
}
