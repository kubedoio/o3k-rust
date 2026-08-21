//! Service-neutral Cloud Kernel Operation model.
//!
//! ADR-0173 / SPEC-0030 require a service-neutral operation contract that
//! preserves the proven semantics of the existing Compute reconciler
//! (durability, idempotency, unknown-outcome, observation-before-retry,
//! fencing, compensation) without being tied to Compute-specific types.
//!
//! This module provides the canonical Operation type. It is an architectural
//! contract type, not a storage implementation — the store layer maps this
//! to its own persisted representation.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::ActionId;
use crate::resource::{ResourceId, ResourceType};
use crate::scope::OwnershipScope;

/// Operation lifecycle state.
///
/// These states map to the accepted durable store semantics
/// (o3k-store OperationState) but are defined here as the canonical
/// service-neutral vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Operation has been durably recorded but not yet started.
    Pending,
    /// Operation is in progress.
    Running,
    /// Operation completed successfully.
    Succeeded,
    /// Operation failed but may be retried.
    Retryable,
    /// A side effect may or may not have occurred (timeout / stale state).
    UnknownOutcome,
    /// Operation terminally failed.
    Failed,
}

/// Service-neutral operation record.
///
/// This is the canonical public Operation type consumed by the native API
/// and by cross-service workflow/audit paths. It is intentionally separated
/// from the store-level `OperationRecord` which may contain provider-internal
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Stable operation identifier.
    pub id: Uuid,
    /// Namespace of the service that owns this operation
    /// (e.g. `"compute"`, `"volume"`, `"database"`).
    pub service: String,
    /// Canonical action being performed (e.g. `compute:CreateServer`).
    pub action: ActionId,
    /// The authenticated actor or service-principal context identifier.
    pub actor: String,
    /// Ownership/security scope for the target resource.
    pub owner_scope: OwnershipScope,
    /// Type of the target resource.
    pub resource_type: ResourceType,
    /// ID of the target resource, if already allocated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<ResourceId>,
    /// Current operation state.
    pub state: OperationState,
    /// Zero-based attempt counter (0 = first attempt).
    pub attempt: u32,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 start timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// RFC3339 completion timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Error category, if the operation failed or has unknown outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Correlation ID linking this operation to its parent request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Operation {
    /// Creates a new pending `Operation`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        service: impl Into<String>,
        action: ActionId,
        actor: impl Into<String>,
        owner_scope: OwnershipScope,
        resource_type: ResourceType,
        resource_id: Option<ResourceId>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            id,
            service: service.into(),
            action,
            actor: actor.into(),
            owner_scope,
            resource_type,
            resource_id,
            state: OperationState::Pending,
            attempt: 0,
            created_at: crate::audit::now_rfc3339(),
            started_at: None,
            finished_at: None,
            error: None,
            request_id,
        }
    }
}
