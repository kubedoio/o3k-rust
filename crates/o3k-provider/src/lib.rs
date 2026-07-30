use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub provider_name: String,
    pub provider_version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateInstanceRequest {
    pub operation_id: Uuid,
    pub o3k_server_id: Uuid,
    pub name: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub image_id: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteInstanceRequest {
    pub operation_id: Uuid,
    pub provider_instance_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub provider_instance_id: String,
    pub o3k_server_id: Uuid,
    pub state: InstanceState,
    pub observed_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    Creating,
    Running,
    Stopped,
    Deleting,
    Deleted,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub provider_operation_id: Uuid,
    pub o3k_operation_id: Uuid,
    pub state: OperationState,
    pub error_category: Option<ErrorCategory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Accepted,
    Running,
    Succeeded,
    Retryable,
    UnknownOutcome,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    InvalidRequest,
    NotFound,
    Conflict,
    Capacity,
    Retryable,
    UnknownOutcome,
    Terminal,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("provider rejected the request")]
    InvalidRequest,
    #[error("provider resource was not found")]
    NotFound,
    #[error("provider operation conflicts with existing state")]
    Conflict,
    #[error("provider capacity is unavailable")]
    Capacity,
    #[error("provider returned a retryable failure")]
    Retryable,
    #[error("provider outcome is unknown; observe operation {operation_id}")]
    UnknownOutcome { operation_id: Uuid },
    #[error("provider returned a terminal failure")]
    Terminal,
    #[error("provider state is unavailable")]
    StaleState,
    #[error("provider fake storage is unavailable")]
    Storage,
}

impl ProviderError {
    #[must_use]
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::InvalidRequest => ErrorCategory::InvalidRequest,
            Self::NotFound => ErrorCategory::NotFound,
            Self::Conflict => ErrorCategory::Conflict,
            Self::Capacity => ErrorCategory::Capacity,
            Self::Retryable => ErrorCategory::Retryable,
            Self::UnknownOutcome { .. } => ErrorCategory::UnknownOutcome,
            Self::Terminal | Self::StaleState | Self::Storage => ErrorCategory::Terminal,
        }
    }
}

#[async_trait]
pub trait ComputeProvider: Send + Sync {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError>;
    async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Operation, ProviderError>;
    async fn get_instance(&self, provider_instance_id: &str) -> Result<Instance, ProviderError>;
    async fn delete_instance(
        &self,
        request: DeleteInstanceRequest,
    ) -> Result<Operation, ProviderError>;
    async fn get_operation(&self, provider_operation_id: Uuid) -> Result<Operation, ProviderError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureInjection {
    None,
    Transient,
    Terminal,
    Timeout,
    StaleState,
    PartialCompletion,
}

#[derive(Clone)]
pub struct FakeComputeProvider {
    inner: Arc<Mutex<FakeState>>,
}

struct FakeState {
    failure: FailureInjection,
    capabilities: Capabilities,
    instances: HashMap<String, Instance>,
    operations: HashMap<Uuid, Operation>,
    idempotency: HashMap<String, (Uuid, String)>,
}

impl Default for FakeComputeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeComputeProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                failure: FailureInjection::None,
                capabilities: Capabilities {
                    provider_name: "o3k-fake".to_owned(),
                    provider_version: "0.1".to_owned(),
                    capabilities: vec![
                        "compute.instance.create".to_owned(),
                        "compute.instance.get".to_owned(),
                        "compute.instance.delete".to_owned(),
                    ],
                },
                instances: HashMap::new(),
                operations: HashMap::new(),
                idempotency: HashMap::new(),
            })),
        }
    }

    pub fn set_failure(&self, failure: FailureInjection) -> Result<(), ProviderError> {
        self.inner
            .lock()
            .map(|mut state| state.failure = failure)
            .map_err(|_| ProviderError::Storage)
    }

    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.inner
            .lock()
            .map(|state| state.instances.len())
            .unwrap_or_default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, FakeState>, ProviderError> {
        self.inner.lock().map_err(|_| ProviderError::Storage)
    }

    fn operation(
        state: &mut FakeState,
        request: Uuid,
        state_value: OperationState,
        category: Option<ErrorCategory>,
    ) -> Operation {
        let operation = Operation {
            provider_operation_id: Uuid::now_v7(),
            o3k_operation_id: request,
            state: state_value,
            error_category: category,
        };
        state
            .operations
            .insert(operation.provider_operation_id, operation.clone());
        operation
    }

    fn create_fingerprint(request: &CreateInstanceRequest) -> String {
        format!(
            "{}:{}:{}:{}:{:?}",
            request.o3k_server_id,
            request.name,
            request.vcpus,
            request.memory_mib,
            request.image_id
        )
    }

    fn delete_fingerprint(request: &DeleteInstanceRequest) -> String {
        request.provider_instance_id.clone()
    }
}

#[async_trait]
impl ComputeProvider for FakeComputeProvider {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
        Ok(self.lock()?.capabilities.clone())
    }

    async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        let mut state = self.lock()?;
        let fingerprint = Self::create_fingerprint(&request);
        if let Some((operation_id, original)) = state.idempotency.get(&request.idempotency_key) {
            if original != &fingerprint {
                return Err(ProviderError::Conflict);
            }
            return state
                .operations
                .get(operation_id)
                .cloned()
                .ok_or(ProviderError::Storage);
        }
        if request.name.trim().is_empty()
            || request.vcpus == 0
            || request.memory_mib == 0
            || request.idempotency_key.trim().is_empty()
        {
            return Err(ProviderError::InvalidRequest);
        }
        match state.failure {
            FailureInjection::Transient => return Err(ProviderError::Retryable),
            FailureInjection::Terminal => return Err(ProviderError::Terminal),
            FailureInjection::StaleState => return Err(ProviderError::StaleState),
            _ => {}
        }
        let provider_id = format!("fake-{}", Uuid::now_v7());
        let instance = Instance {
            provider_instance_id: provider_id.clone(),
            o3k_server_id: request.o3k_server_id,
            state: if state.failure == FailureInjection::PartialCompletion {
                InstanceState::Creating
            } else {
                InstanceState::Running
            },
            observed_message: None,
        };
        state.instances.insert(provider_id, instance);
        let operation_state = if state.failure == FailureInjection::Timeout {
            OperationState::UnknownOutcome
        } else {
            OperationState::Succeeded
        };
        let operation = Self::operation(
            &mut state,
            request.operation_id,
            operation_state,
            (operation_state == OperationState::UnknownOutcome)
                .then_some(ErrorCategory::UnknownOutcome),
        );
        state.idempotency.insert(
            request.idempotency_key,
            (operation.provider_operation_id, fingerprint),
        );
        if operation_state == OperationState::UnknownOutcome {
            return Err(ProviderError::UnknownOutcome {
                operation_id: operation.provider_operation_id,
            });
        }
        Ok(operation)
    }

    async fn get_instance(&self, provider_instance_id: &str) -> Result<Instance, ProviderError> {
        let state = self.lock()?;
        if state.failure == FailureInjection::StaleState {
            return Err(ProviderError::StaleState);
        }
        state
            .instances
            .get(provider_instance_id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }

    async fn delete_instance(
        &self,
        request: DeleteInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        let mut state = self.lock()?;
        let fingerprint = Self::delete_fingerprint(&request);
        if let Some((operation_id, original)) = state.idempotency.get(&request.idempotency_key) {
            if original != &fingerprint {
                return Err(ProviderError::Conflict);
            }
            return state
                .operations
                .get(operation_id)
                .cloned()
                .ok_or(ProviderError::Storage);
        }
        let absent = !state.instances.contains_key(&request.provider_instance_id);
        if absent {
            let operation = Self::operation(
                &mut state,
                request.operation_id,
                OperationState::Succeeded,
                None,
            );
            state.idempotency.insert(
                request.idempotency_key,
                (operation.provider_operation_id, fingerprint),
            );
            return Ok(operation);
        }
        match state.failure {
            FailureInjection::Transient => return Err(ProviderError::Retryable),
            FailureInjection::Terminal => return Err(ProviderError::Terminal),
            FailureInjection::StaleState => return Err(ProviderError::StaleState),
            _ => {}
        }
        state.instances.remove(&request.provider_instance_id);
        let operation_state = if state.failure == FailureInjection::Timeout {
            OperationState::UnknownOutcome
        } else {
            OperationState::Succeeded
        };
        let operation = Self::operation(
            &mut state,
            request.operation_id,
            operation_state,
            (operation_state == OperationState::UnknownOutcome)
                .then_some(ErrorCategory::UnknownOutcome),
        );
        state.idempotency.insert(
            request.idempotency_key,
            (operation.provider_operation_id, fingerprint),
        );
        if operation_state == OperationState::UnknownOutcome {
            return Err(ProviderError::UnknownOutcome {
                operation_id: operation.provider_operation_id,
            });
        }
        Ok(operation)
    }

    async fn get_operation(&self, provider_operation_id: Uuid) -> Result<Operation, ProviderError> {
        self.lock()?
            .operations
            .get(&provider_operation_id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
}

pub async fn run_compute_conformance(provider: &dyn ComputeProvider) -> Result<(), ProviderError> {
    let capabilities = provider.capabilities().await?;
    if !capabilities
        .capabilities
        .iter()
        .any(|value| value == "compute.instance.create")
    {
        return Err(ProviderError::InvalidRequest);
    }
    let request = CreateInstanceRequest {
        operation_id: Uuid::now_v7(),
        o3k_server_id: Uuid::now_v7(),
        name: "conformance".to_owned(),
        vcpus: 1,
        memory_mib: 128,
        image_id: None,
        idempotency_key: format!("conformance-{}", Uuid::now_v7()),
    };
    let operation = provider.create_instance(request.clone()).await?;
    let observed = provider
        .get_operation(operation.provider_operation_id)
        .await?;
    if observed.state != OperationState::Succeeded {
        return Err(ProviderError::InvalidRequest);
    }
    Ok(())
}

pub async fn run_conformance<P: ComputeProvider>(provider: &P) -> Result<(), ProviderError> {
    run_compute_conformance(provider).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(key: &str) -> CreateInstanceRequest {
        CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            name: "test".to_owned(),
            vcpus: 1,
            memory_mib: 128,
            image_id: None,
            idempotency_key: key.to_owned(),
        }
    }

    #[tokio::test]
    async fn duplicate_create_is_idempotent_and_delete_is_absent_safe() -> Result<(), ProviderError>
    {
        let provider = FakeComputeProvider::new();
        let create = request("same");
        let first = provider.create_instance(create.clone()).await?;
        let second = provider.create_instance(create).await?;
        assert_eq!(first, second);
        assert_eq!(provider.instance_count(), 1);
        let deleted = provider
            .delete_instance(DeleteInstanceRequest {
                operation_id: Uuid::now_v7(),
                provider_instance_id: "missing".to_owned(),
                idempotency_key: "delete-missing".to_owned(),
            })
            .await?;
        assert_eq!(deleted.state, OperationState::Succeeded);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_is_observable_after_unknown_result() -> Result<(), ProviderError> {
        let provider = FakeComputeProvider::new();
        provider.set_failure(FailureInjection::Timeout)?;
        let error = match provider.create_instance(request("timeout")).await {
            Ok(_) => return Err(ProviderError::InvalidRequest),
            Err(error) => error,
        };
        let ProviderError::UnknownOutcome { operation_id } = error else {
            return Err(ProviderError::InvalidRequest);
        };
        assert_eq!(
            provider.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn conformance_suite_runs_against_fake() -> Result<(), ProviderError> {
        run_conformance(&FakeComputeProvider::new()).await
    }
}
