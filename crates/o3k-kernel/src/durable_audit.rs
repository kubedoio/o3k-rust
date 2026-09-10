//! Durable audit repository contract.
//!
//! This port is deliberately separate from [`AuditSink`].  A sink is useful
//! for diagnostics and tests, but cannot express persistence or failure.  A
//! production caller must use this port when audit durability is mandatory.

use async_trait::async_trait;

use crate::{audit::AuditEvent, error::KernelError, scope::OwnershipScope};

/// Maximum number of audit records returned by one repository operation.
pub const MAX_AUDIT_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditQuery {
    /// The effective scope, already derived from AuthContext by the caller.
    pub scope: OwnershipScope,
    pub after_event_id: Option<String>,
    pub event_id: Option<String>,
    pub limit: usize,
    pub service: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub operation_id: Option<String>,
    pub principal_id: Option<String>,
    pub request_id: Option<String>,
    pub audit_id: Option<String>,
    pub from_timestamp: Option<String>,
    pub until_timestamp: Option<String>,
}

impl AuditQuery {
    pub fn validate(&self) -> Result<(), KernelError> {
        if !(1..=MAX_AUDIT_PAGE_SIZE).contains(&self.limit) {
            return Err(KernelError::InvalidIdentifier("audit page limit".into()));
        }
        for value in [
            self.service.as_deref(),
            self.action.as_deref(),
            self.outcome.as_deref(),
            self.resource_type.as_deref(),
            self.resource_id.as_deref(),
            self.operation_id.as_deref(),
            self.principal_id.as_deref(),
            self.request_id.as_deref(),
            self.audit_id.as_deref(),
            self.from_timestamp.as_deref(),
            self.until_timestamp.as_deref(),
            self.after_event_id.as_deref(),
            self.event_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > 256 || value.bytes().any(|b| b == 0) {
                return Err(KernelError::InvalidIdentifier("audit query value".into()));
            }
        }
        Ok(())
    }
}

/// Bounded repository result. The continuation key is private to the store;
/// public APIs must integrity-protect it before exposing a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableAuditPage {
    pub events: Vec<AuditEvent>,
    pub has_more: bool,
    pub continuation_key: Option<String>,
}

#[async_trait]
pub trait DurableAuditRepository: Send + Sync {
    /// Persist one canonical event. Implementations must not acknowledge this
    /// call before the event satisfies their documented durability class.
    async fn append(&self, event: &AuditEvent) -> Result<(), KernelError>;

    /// Query by effective scope using a bounded database operation.
    async fn page(&self, query: &AuditQuery) -> Result<DurableAuditPage, KernelError>;

    /// Privileged retention operation; normal tenant callers must not reach it.
    async fn prune_before(&self, cutoff: &str) -> Result<u64, KernelError>;
}
