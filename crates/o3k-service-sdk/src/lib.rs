//! Safe Rust-first helpers for implementing an external O3K controller.
//! Business handlers receive validated domain requests; transport/session and
//! replay plumbing remains in this crate.

use o3k_kernel::{ControllerFailure, FailureCategory, OperationContext};
use sha2::{Digest, Sha256};
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
        Ok(ServerTlsConfig::new()
            .identity(Identity::from_pem(
                read(certificate, "server certificate")?,
                read(key, "server key")?,
            ))
            .client_ca_root(Certificate::from_pem(read(ca, "client CA")?)))
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
}
