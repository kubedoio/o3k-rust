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
        verify_wire_delegation(&delegation.credential, &self.delegation_keys, now)
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
        self.validate_delegation(input.delegation.as_ref(), now)?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
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
        self.validate_delegation(input.delegation.as_ref(), now)?;
        Ok(tonic::Response::new(self.handler.observe(input).await?))
    }
    async fn delete(
        &self,
        request: tonic::Request<proto::DeleteRequest>,
    ) -> Result<tonic::Response<proto::DeleteResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        self.validate_delegation(input.delegation.as_ref(), now)?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
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
