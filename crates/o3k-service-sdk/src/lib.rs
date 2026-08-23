//! Safe Rust-first helpers for implementing an external O3K controller.
//! Business handlers receive validated domain requests; transport/session and
//! replay plumbing remains in this crate.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use o3k_kernel::{ControllerFailure, FailureCategory, OperationContext};
use prost::Message;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod tls {
    use super::SdkError;
    use std::{fs, path::Path};
    use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

    fn read(path: impl AsRef<Path>, label: &'static str) -> Result<Vec<u8>, SdkError> {
        fs::read(path).map_err(|error| SdkError::Invalid(format!("cannot read {label}: {error}")))
    }

    pub fn client(
        ca: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
        server_name: &str,
    ) -> Result<ClientTlsConfig, SdkError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if server_name.trim().is_empty() {
            return Err(SdkError::Invalid("TLS server name is required".into()));
        }
        Ok(ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(read(ca, "CA")?))
            .identity(Identity::from_pem(
                read(certificate, "client certificate")?,
                read(key, "client key")?,
            ))
            .domain_name(server_name))
    }

    pub fn server(
        ca: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
    ) -> Result<ServerTlsConfig, SdkError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(ServerTlsConfig::new()
            .identity(Identity::from_pem(
                read(certificate, "server certificate")?,
                read(key, "server key")?,
            ))
            .client_ca_root(Certificate::from_pem(read(ca, "client CA")?)))
    }
}

/// Generic service-to-service composition port. Implementations are supplied
/// by the O3K control plane; external services receive only bounded child
/// authority and never provider/store access.
pub mod composition {
    use o3k_controller_protocol::composition as wire;
    use o3k_kernel::{
        ActionId, OperationContext, OwnershipScope, RelationshipOwnership, ResourceId,
        ResourceReference, ResourceType,
    };
    use uuid::Uuid;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChildResourceRequest {
        pub parent: ResourceReference,
        pub parent_operation_id: Uuid,
        pub context: OperationContext,
        pub service_principal: String,
        pub delegation: Vec<u8>,
        pub child: Option<ResourceReference>,
        pub action: ActionId,
        pub resource_type: o3k_kernel::ResourceType,
        pub owner_scope: OwnershipScope,
        pub slot: String,
        pub idempotency_key: String,
        pub desired_spec: serde_json::Value,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChildResourceReceipt {
        pub resource: ResourceReference,
        pub operation_id: Uuid,
        pub owner_scope: OwnershipScope,
        pub ownership: RelationshipOwnership,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum CompositionError {
        #[error("child request is not authorized")]
        Unauthorized,
        #[error("child operation outcome is unknown")]
        UnknownOutcome,
        #[error("child operation failed: {0}")]
        Failed(String),
    }

    fn scope_to_wire(scope: &OwnershipScope) -> wire::Scope {
        wire::Scope {
            id: scope.id().as_str().to_owned(),
            kind: match scope.kind() {
                o3k_kernel::ScopeKind::Project => "project",
                o3k_kernel::ScopeKind::Domain => "domain",
                o3k_kernel::ScopeKind::System => "system",
            }
            .to_owned(),
        }
    }

    fn resource_to_wire(reference: &ResourceReference) -> wire::ResourceRef {
        wire::ResourceRef {
            namespace: reference.resource_type.namespace().to_owned(),
            r#type: reference.resource_type.name().to_owned(),
            id: reference.resource_id.as_str().to_owned(),
            generation: reference.generation,
        }
    }

    /// Converts the validated domain request into the language-neutral wire
    /// request. Identity/session/delegation fields are populated from the
    /// canonical operation context; no caller-supplied strings are inferred.
    pub fn child_request_to_wire(
        request: &ChildResourceRequest,
        lifecycle: &str,
    ) -> Result<wire::ChildRequest, CompositionError> {
        if request.context.operation_id != request.parent_operation_id {
            return Err(CompositionError::Failed(
                "parent operation does not match request context".into(),
            ));
        }
        if request.context.owner_scope != request.owner_scope {
            return Err(CompositionError::Failed(
                "owner scope does not match request context".into(),
            ));
        }
        let parent = wire::ParentContext {
            request_id: request.context.request_id.to_string(),
            operation_id: request.context.operation_id.to_string(),
            service_id: request.context.service_id.clone(),
            service_principal: request.service_principal.clone(),
            parent_action: request.context.action.to_string(),
            parent: Some(resource_to_wire(&request.parent)),
            parent_generation: request.parent.generation,
            owner_scope: Some(scope_to_wire(&request.context.owner_scope)),
            session_id: request.context.session_id.to_string(),
            session_generation: request.context.session_generation,
            slot: request.slot.clone(),
            replay_identity: request.context.replay_identity.clone(),
            audit_correlation: request.context.audit_correlation.clone(),
            delegation: request.delegation.clone(),
        };
        Ok(wire::ChildRequest {
            parent: Some(parent),
            lifecycle: lifecycle.to_owned(),
            resource_type: request.resource_type.to_string(),
            desired_spec: serde_json::to_vec(&request.desired_spec)
                .map_err(|_| CompositionError::Failed("invalid child spec".into()))?,
            requested_action: request.action.to_string(),
        })
    }

    /// Strictly validates a wire resource reference before it can enter the
    /// canonical domain model.
    pub fn resource_from_wire(
        value: wire::ResourceRef,
    ) -> Result<ResourceReference, CompositionError> {
        let resource_type = ResourceType::new(&value.namespace, &value.r#type)
            .map_err(|_| CompositionError::Failed("invalid resource type".into()))?;
        let resource_id = ResourceId::new(&value.id)
            .map_err(|_| CompositionError::Failed("invalid resource id".into()))?;
        Ok(ResourceReference {
            resource_type,
            resource_id,
            generation: value.generation,
        })
    }

    fn parse_uuid(value: &str, field: &str) -> Result<Uuid, CompositionError> {
        Uuid::parse_str(value).map_err(|_| CompositionError::Failed(format!("invalid {field}")))
    }

    fn response_to_receipt(
        request: &ChildResourceRequest,
        response: wire::ChildResponse,
    ) -> Result<ChildResourceReceipt, CompositionError> {
        let expected_request = request.context.request_id.to_string();
        if response.request_id != expected_request
            || response.service_id != request.context.service_id
            || response.session_id != request.context.session_id.to_string()
            || response.session_generation != request.context.session_generation
            || response.parent_operation_id != request.parent_operation_id.to_string()
            || response.slot != request.slot
        {
            return Err(CompositionError::Failed(
                "composition response correlation mismatch".into(),
            ));
        }
        let resource = response
            .resource
            .ok_or_else(|| CompositionError::Failed("composition response omitted resource".into()))
            .and_then(resource_from_wire)?;
        if resource.resource_type != request.resource_type {
            return Err(CompositionError::Failed(
                "composition response resource mismatch".into(),
            ));
        }
        let operation_id = parse_uuid(&response.operation_id, "child operation id")?;
        let owner_scope = request.owner_scope.clone();
        let ownership = match response.ownership.as_str() {
            "exclusive" => RelationshipOwnership::Exclusive,
            "referenced" => RelationshipOwnership::Referenced,
            _ => {
                return Err(CompositionError::Failed(
                    "invalid relationship ownership".into(),
                ));
            }
        };
        Ok(ChildResourceReceipt {
            resource,
            operation_id,
            owner_scope,
            ownership,
        })
    }

    #[tonic::async_trait]
    pub trait CompositionHandler: Send + Sync + 'static {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError>;
        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
    }

    /// Real generic controller-to-O3K composition client. It contains only
    /// the versioned composition transport and typed conversions; the O3K
    /// server remains responsible for authorization, descriptor resolution,
    /// delegation verification, and durable relationship writes.
    pub struct GrpcCompositionClient {
        client: tokio::sync::Mutex<
            wire::composition_service_client::CompositionServiceClient<tonic::transport::Channel>,
        >,
    }

    impl GrpcCompositionClient {
        pub async fn connect(
            endpoint: &str,
            tls: tonic::transport::ClientTlsConfig,
        ) -> Result<Self, CompositionError> {
            let channel = tonic::transport::Endpoint::from_shared(endpoint.to_owned())
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .tls_config(tls)
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .connect()
                .await
                .map_err(|error| CompositionError::Failed(error.to_string()))?;
            Ok(Self {
                client: tokio::sync::Mutex::new(
                    wire::composition_service_client::CompositionServiceClient::new(channel),
                ),
            })
        }
    }

    #[tonic::async_trait]
    pub trait ServiceCompositionClient: Send + Sync {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError>;
        async fn delete_child(&self, request: ChildResourceRequest)
        -> Result<(), CompositionError>;
    }

    #[tonic::async_trait]
    impl ServiceCompositionClient for GrpcCompositionClient {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError> {
            let wire_request = child_request_to_wire(&request, "create")?;
            let response = self
                .client
                .lock()
                .await
                .create_child(wire_request)
                .await
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .into_inner();
            response_to_receipt(&request, response)
        }

        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError> {
            let child = request
                .child
                .clone()
                .ok_or_else(|| CompositionError::Failed("missing child reference".into()))?;
            let mut client = self.client.lock().await;
            let response = client
                .observe_child(wire::ObserveRequest {
                    parent: Some(
                        child_request_to_wire(&request, "observe")?
                            .parent
                            .ok_or_else(|| CompositionError::Failed("missing parent".into()))?,
                    ),
                    resource: Some(resource_to_wire(&child)),
                    child_operation_id: request.parent_operation_id.to_string(),
                })
                .await
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .into_inner();
            if response.request_id != request.context.request_id.to_string()
                || response.session_id != request.context.session_id.to_string()
                || response.session_generation != request.context.session_generation
                || response.parent_operation_id != request.parent_operation_id.to_string()
                || response.slot != request.slot
            {
                return Err(CompositionError::Failed(
                    "composition observation correlation mismatch".into(),
                ));
            }
            serde_json::from_slice(&response.observed_status)
                .map_err(|_| CompositionError::Failed("invalid child observation".into()))
        }

        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<(), CompositionError> {
            let wire_request = child_request_to_wire(&request, "delete")?;
            let response = self
                .client
                .lock()
                .await
                .delete_child(wire::DeleteRequest {
                    child: Some(wire_request),
                })
                .await
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .into_inner();
            let _ = response_to_receipt(&request, response)?;
            Ok(())
        }
    }
}

use o3k_controller_protocol::proto::{
    self,
    controller_service_server::{ControllerService, ControllerServiceServer},
};
use tonic::transport::{Channel, Endpoint};

#[tonic::async_trait]
pub trait ControllerHandler: Send + Sync + 'static {
    async fn health(
        &self,
        request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, tonic::Status>;
    async fn capabilities(
        &self,
        request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, tonic::Status>;
    async fn reconcile(
        &self,
        request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, tonic::Status>;
    async fn observe(
        &self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, tonic::Status>;
    async fn delete(
        &self,
        request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, tonic::Status>;
}

pub struct ServiceControllerServer<H> {
    handler: Arc<H>,
    service_id: String,
    namespace: String,
    manifest_digest: String,
    generation: u64,
    fence: SessionFence,
    replay: ReplayLedger,
    delegation_keys: Arc<HashMap<String, VerifyingKey>>,
}

#[derive(Clone)]
pub struct ExternalControllerClient {
    client: proto::controller_service_client::ControllerServiceClient<Channel>,
}

impl ExternalControllerClient {
    pub async fn connect(
        endpoint: &str,
        tls: tonic::transport::ClientTlsConfig,
    ) -> Result<Self, SdkError> {
        let channel = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|error| SdkError::Invalid(error.to_string()))?
            .tls_config(tls)
            .map_err(|error| SdkError::Invalid(error.to_string()))?
            .connect()
            .await
            .map_err(|error| SdkError::Invalid(error.to_string()))?;
        Ok(Self {
            client: proto::controller_service_client::ControllerServiceClient::new(channel),
        })
    }
    pub async fn register(
        &mut self,
        request: proto::RegisterRequest,
    ) -> Result<proto::RegisterResponse, SdkError> {
        self.client
            .register(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn health(
        &mut self,
        request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, SdkError> {
        self.client
            .health(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn capabilities(
        &mut self,
        request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, SdkError> {
        self.client
            .capabilities(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn reconcile(
        &mut self,
        request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, SdkError> {
        self.client
            .reconcile(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn observe(
        &mut self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, SdkError> {
        self.client
            .observe(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn delete(
        &mut self,
        request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, SdkError> {
        self.client
            .delete(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
}

impl<H: ControllerHandler> ServiceControllerServer<H> {
    pub fn new(
        handler: H,
        service_id: impl Into<String>,
        namespace: impl Into<String>,
        manifest_digest: impl Into<String>,
        generation: u64,
    ) -> Self {
        Self {
            handler: Arc::new(handler),
            service_id: service_id.into(),
            namespace: namespace.into(),
            manifest_digest: manifest_digest.into(),
            generation,
            fence: SessionFence::default(),
            replay: ReplayLedger::default(),
            delegation_keys: Arc::new(HashMap::new()),
        }
    }
    pub fn into_service(self) -> ControllerServiceServer<Self> {
        ControllerServiceServer::new(self)
    }
    pub fn with_delegation_keys(mut self, keys: HashMap<String, VerifyingKey>) -> Self {
        self.delegation_keys = Arc::new(keys);
        self
    }
    #[allow(clippy::result_large_err)]
    fn validate_delegation(
        &self,
        delegation: Option<&proto::Delegation>,
        context: &proto::Context,
        resource: Option<&proto::ResourceRef>,
        now: u64,
    ) -> Result<(), tonic::Status> {
        let Some(delegation) = delegation else {
            return Ok(());
        };
        if self.delegation_keys.is_empty() {
            return Err(tonic::Status::permission_denied(
                "delegation verification keys are not configured",
            ));
        }
        let claims = verify_wire_delegation(&delegation.credential, &self.delegation_keys, now)
            .map_err(|_| tonic::Status::permission_denied("invalid delegation"))?;
        bind_delegation(&claims, context, resource, &self.service_id)
            .map(|_| ())
            .map_err(|_| tonic::Status::permission_denied("invalid delegation"))
    }
    async fn check_context(&self, context: Option<&proto::Context>) -> Result<u64, tonic::Status> {
        let context =
            context.ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        let session = Uuid::parse_str(&context.session_id)
            .map_err(|_| tonic::Status::unauthenticated("invalid session"))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| tonic::Status::internal("clock error"))?
            .as_millis() as u64;
        if context.deadline_unix_ms <= now
            || context.deadline_unix_ms - now > o3k_kernel::controller::MAX_DEADLINE_MS
        {
            return Err(tonic::Status::deadline_exceeded(
                "invalid controller deadline",
            ));
        }
        self.fence
            .validate(&self.service_id, session, context.session_generation)
            .await
            .map_err(|_| tonic::Status::failed_precondition("stale session"))?;
        Ok(now)
    }
}

#[tonic::async_trait]
impl<H: ControllerHandler> ControllerService for ServiceControllerServer<H> {
    async fn register(
        &self,
        request: tonic::Request<proto::RegisterRequest>,
    ) -> Result<tonic::Response<proto::RegisterResponse>, tonic::Status> {
        let input = request.into_inner();
        if input.service_id != self.service_id
            || input.namespace != self.namespace
            || input.manifest_digest != self.manifest_digest
            || input.manifest_generation != self.generation
        {
            return Err(tonic::Status::permission_denied(
                "manifest identity mismatch",
            ));
        }
        let negotiated = input
            .supported_versions
            .into_iter()
            .find(|v| v.major == 1 && v.minor == 0)
            .ok_or_else(|| tonic::Status::failed_precondition("no compatible protocol version"))?;
        let session_id = Uuid::new_v4();
        self.fence
            .establish(SessionBinding {
                service_id: self.service_id.clone(),
                session_id,
                generation: 1,
            })
            .await;
        Ok(tonic::Response::new(proto::RegisterResponse {
            negotiated_version: Some(negotiated),
            session_id: session_id.to_string(),
            session_generation: 1,
        }))
    }
    async fn health(
        &self,
        request: tonic::Request<proto::HealthRequest>,
    ) -> Result<tonic::Response<proto::HealthResponse>, tonic::Status> {
        let input = request.into_inner();
        self.check_context(input.context.as_ref()).await?;
        Ok(tonic::Response::new(self.handler.health(input).await?))
    }
    async fn capabilities(
        &self,
        request: tonic::Request<proto::CapabilitiesRequest>,
    ) -> Result<tonic::Response<proto::CapabilitiesResponse>, tonic::Status> {
        let input = request.into_inner();
        self.check_context(input.context.as_ref()).await?;
        Ok(tonic::Response::new(
            self.handler.capabilities(input).await?,
        ))
    }
    async fn reconcile(
        &self,
        request: tonic::Request<proto::ReconcileRequest>,
    ) -> Result<tonic::Response<proto::ReconcileResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref().and_then(|s| s.resource.as_ref()),
            now,
        )?;
        self.replay
            .reserve(&context.replay_identity, &input.encode_to_vec())
            .await
            .map_err(|_| tonic::Status::aborted("conflicting replay identity"))?;
        Ok(tonic::Response::new(self.handler.reconcile(input).await?))
    }
    async fn observe(
        &self,
        request: tonic::Request<proto::ObserveRequest>,
    ) -> Result<tonic::Response<proto::ObserveResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref(),
            now,
        )?;
        Ok(tonic::Response::new(self.handler.observe(input).await?))
    }
    async fn delete(
        &self,
        request: tonic::Request<proto::DeleteRequest>,
    ) -> Result<tonic::Response<proto::DeleteResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref(),
            now,
        )?;
        self.replay
            .reserve(&context.replay_identity, &input.encode_to_vec())
            .await
            .map_err(|_| tonic::Status::aborted("conflicting replay identity"))?;
        Ok(tonic::Response::new(self.handler.delete(input).await?))
    }
}

pub const MAX_REPLAY_ENTRIES: usize = 4096;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("controller is not registered")]
    NotRegistered,
    #[error("stale controller session")]
    StaleSession,
    #[error("conflicting replay identity")]
    ReplayConflict,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("delegation signature is invalid")]
    InvalidDelegation,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DelegationClaims {
    pub version: u8,
    pub credential_id: Uuid,
    pub issuer: String,
    pub key_id: String,
    pub original_actor: String,
    pub owner_scope: String,
    pub calling_service: String,
    pub recipient_service: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub request_id: Uuid,
    pub operation_id: Uuid,
    pub session_id: Uuid,
    pub session_generation: u64,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedDelegation {
    pub claims: DelegationClaims,
    pub signature: Vec<u8>,
}

pub fn verify_wire_delegation(
    bytes: &[u8],
    keys: &HashMap<String, VerifyingKey>,
    now_unix_ms: u64,
) -> Result<DelegationClaims, SdkError> {
    if bytes.is_empty() || bytes.len() > o3k_controller_protocol::MAX_DELEGATION_BYTES {
        return Err(SdkError::InvalidDelegation);
    }
    let credential: SignedDelegation =
        serde_json::from_slice(bytes).map_err(|_| SdkError::InvalidDelegation)?;
    Ok(credential.verify(keys, now_unix_ms)?.clone())
}

/// Bind an already cryptographically verified credential to the exact wire
/// request. Signature validity alone never grants authority for a different
/// action, scope, operation, resource, or controller session.
pub fn bind_delegation(
    claims: &DelegationClaims,
    context: &proto::Context,
    resource: Option<&proto::ResourceRef>,
    recipient_service: &str,
) -> Result<(), SdkError> {
    let request_id =
        Uuid::parse_str(&context.request_id).map_err(|_| SdkError::InvalidDelegation)?;
    let operation_id =
        Uuid::parse_str(&context.operation_id).map_err(|_| SdkError::InvalidDelegation)?;
    let session_id =
        Uuid::parse_str(&context.session_id).map_err(|_| SdkError::InvalidDelegation)?;
    if claims.request_id != request_id
        || claims.operation_id != operation_id
        || claims.session_id != session_id
        || claims.session_generation != context.session_generation
        || claims.calling_service != context.service_id
        || claims.recipient_service != recipient_service
        || claims.action != context.action
    {
        return Err(SdkError::InvalidDelegation);
    }
    let scope = context
        .owner_scope
        .as_ref()
        .ok_or(SdkError::InvalidDelegation)?;
    let kind = proto::scope::Kind::try_from(scope.kind).map_err(|_| SdkError::InvalidDelegation)?;
    let kind = match kind {
        proto::scope::Kind::Project => "project",
        proto::scope::Kind::Domain => "domain",
        proto::scope::Kind::System => "system",
        proto::scope::Kind::Unspecified => return Err(SdkError::InvalidDelegation),
    };
    if claims.owner_scope != format!("{kind}:{}", scope.id) {
        return Err(SdkError::InvalidDelegation);
    }
    if let Some(resource) = resource {
        let resource_name = format!("{}:{}", resource.namespace, resource.r#type);
        if claims.resource_type != resource_name
            || claims.resource_id != resource.id
            || claims.resource_id.is_empty()
            || resource.generation < 0
        {
            return Err(SdkError::InvalidDelegation);
        }
    }
    Ok(())
}

impl SignedDelegation {
    fn signing_bytes(claims: &DelegationClaims) -> Result<Vec<u8>, SdkError> {
        serde_json::to_vec(claims).map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub fn sign(claims: DelegationClaims, key: &SigningKey) -> Result<Self, SdkError> {
        let signature = key.sign(&Self::signing_bytes(&claims)?).to_bytes().to_vec();
        Ok(Self { claims, signature })
    }
    pub fn verify(
        &self,
        keys: &std::collections::HashMap<String, VerifyingKey>,
        now_unix_ms: u64,
    ) -> Result<&DelegationClaims, SdkError> {
        if self.claims.version != 1
            || self.signature.len() != 64
            || now_unix_ms < self.claims.issued_at_unix_ms
            || now_unix_ms >= self.claims.expires_at_unix_ms
        {
            return Err(SdkError::InvalidDelegation);
        }
        let key = keys
            .get(&self.claims.key_id)
            .ok_or(SdkError::InvalidDelegation)?;
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| SdkError::InvalidDelegation)?;
        key.verify(&Self::signing_bytes(&self.claims)?, &signature)
            .map_err(|_| SdkError::InvalidDelegation)?;
        Ok(&self.claims)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    pub service_id: String,
    pub session_id: Uuid,
    pub generation: u64,
}

#[derive(Clone, Default)]
pub struct SessionFence {
    current: Arc<Mutex<Option<SessionBinding>>>,
}

impl SessionFence {
    pub async fn establish(&self, binding: SessionBinding) {
        *self.current.lock().await = Some(binding);
    }
    pub async fn validate(
        &self,
        service_id: &str,
        session_id: Uuid,
        generation: u64,
    ) -> Result<(), SdkError> {
        match self.current.lock().await.as_ref() {
            Some(current)
                if current.service_id == service_id
                    && current.session_id == session_id
                    && current.generation == generation =>
            {
                Ok(())
            }
            _ => Err(SdkError::StaleSession),
        }
    }
}

#[derive(Clone, Default)]
pub struct ReplayLedger {
    entries: Arc<Mutex<HashMap<String, [u8; 32]>>>,
}

impl ReplayLedger {
    pub async fn reserve(&self, key: &str, canonical_request: &[u8]) -> Result<(), SdkError> {
        let digest: [u8; 32] = Sha256::digest(canonical_request).into();
        let mut entries = self.entries.lock().await;
        if let Some(previous) = entries.get(key) {
            return if previous == &digest {
                Ok(())
            } else {
                Err(SdkError::ReplayConflict)
            };
        }
        if entries.len() >= MAX_REPLAY_ENTRIES {
            return Err(SdkError::Invalid("replay ledger capacity exhausted".into()));
        }
        entries.insert(key.to_owned(), digest);
        Ok(())
    }
}

pub fn validate_context(
    context: &OperationContext,
    now_unix_ms: u64,
) -> Result<(), ControllerFailure> {
    if context.service_id.trim().is_empty() {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "service identity is required",
        ));
    }
    if context.replay_identity.trim().is_empty() {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "replay identity is required",
        ));
    }
    if context.deadline_unix_ms <= now_unix_ms {
        return Err(ControllerFailure::new(
            FailureCategory::DeadlineExceeded,
            "deadline has expired",
        ));
    }
    if context.deadline_unix_ms - now_unix_ms > o3k_kernel::controller::MAX_DEADLINE_MS {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "deadline exceeds maximum",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    #[tokio::test]
    async fn equivalent_replay_is_allowed_and_conflict_is_rejected() {
        let ledger = ReplayLedger::default();
        assert!(ledger.reserve("op-1", b"same").await.is_ok());
        assert!(ledger.reserve("op-1", b"same").await.is_ok());
        assert!(matches!(
            ledger.reserve("op-1", b"changed").await,
            Err(SdkError::ReplayConflict)
        ));
    }
    #[tokio::test]
    async fn replacement_fences_old_session() {
        let fence = SessionFence::default();
        let old = SessionBinding {
            service_id: "svc".into(),
            session_id: Uuid::new_v4(),
            generation: 1,
        };
        let new = SessionBinding {
            service_id: "svc".into(),
            session_id: Uuid::new_v4(),
            generation: 2,
        };
        fence.establish(old.clone()).await;
        assert!(fence.validate("svc", old.session_id, 1).await.is_ok());
        fence.establish(new.clone()).await;
        assert!(matches!(
            fence.validate("svc", old.session_id, 1).await,
            Err(SdkError::StaleSession)
        ));
        assert!(fence.validate("svc", new.session_id, 2).await.is_ok());
    }

    #[test]
    fn delegation_tampering_and_expiry_fail_closed() -> Result<(), SdkError> {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let claims = DelegationClaims {
            version: 1,
            credential_id: Uuid::new_v4(),
            issuer: "o3k".into(),
            key_id: "k1".into(),
            original_actor: "user".into(),
            owner_scope: "project:p1".into(),
            calling_service: "svc-a".into(),
            recipient_service: "svc-b".into(),
            action: "compute:ReadServer".into(),
            resource_type: "compute:server".into(),
            resource_id: "s1".into(),
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            session_generation: 1,
            issued_at_unix_ms: 10,
            expires_at_unix_ms: 20,
        };
        let mut signed = SignedDelegation::sign(claims, &signing)?;
        let mut keys = HashMap::new();
        keys.insert("k1".into(), signing.verifying_key());
        assert!(signed.verify(&keys, 15).is_ok());
        signed.claims.action = "compute:DeleteServer".into();
        assert!(matches!(
            signed.verify(&keys, 15),
            Err(SdkError::InvalidDelegation)
        ));
        let signed = SignedDelegation::sign(signed.claims, &signing)?;
        assert!(matches!(
            signed.verify(&keys, 20),
            Err(SdkError::InvalidDelegation)
        ));
        Ok(())
    }
}
