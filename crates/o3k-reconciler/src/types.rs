//! Reconciler-domain types: journal events, lifecycle actions,
//! canonical mutation context, reconciler errors.
//!
//! ## Boundary
//!
//! These types belong to the reconciliation domain. Compute-domain
//! types (flavors, keypairs, server inputs) live in `o3k-compute::types`.

use std::sync::{Arc, Mutex};

use o3k_domain::ServerState;
use o3k_provider::{AgentObservation, AgentOperationState, ProviderError};
use o3k_store::{
    AgentCommandRecord, CanonicalOperationRecord, IdempotencyReservationRequest, OperationRecord,
    StoreError,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
// ─── Event types ──────────────────────────────────────────

pub enum JournalEventKind {
    IntentPersisted,
    ProviderStarted,
    RetryScheduled,
    UnknownObserved,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    Start,
    Stop,
    Reboot,
    Delete,
}

#[derive(Debug, Clone)]
pub struct CanonicalMutationContext {
    pub action: o3k_kernel::ActionId,
    pub actor: String,
    pub owner_scope: o3k_kernel::OwnershipScope,
    pub request_id: Option<String>,
    pub idempotency_key: String,
    pub semantic_request: serde_json::Value,
    pub created_at: String,
}

impl CanonicalMutationContext {
    pub fn new(
        action: o3k_kernel::ActionId,
        actor: String,
        owner_scope: o3k_kernel::OwnershipScope,
        request_id: Option<String>,
        idempotency_key: String,
        semantic_request: serde_json::Value,
    ) -> Result<Self, ReconcileError> {
        if owner_scope.kind() != o3k_kernel::ScopeKind::Project
            || actor.trim().is_empty()
            || idempotency_key.is_empty()
            || idempotency_key.len() > IdempotencyReservationRequest::MAX_KEY_LENGTH
        {
            return Err(ReconcileError::InvalidIntent);
        }
        Ok(Self {
            action,
            actor,
            owner_scope,
            request_id,
            idempotency_key,
            semantic_request,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    pub(crate) fn records(
        &self,
        operation: &OperationRecord,
        resource_type: o3k_kernel::ResourceType,
        target: Option<&str>,
    ) -> Result<(CanonicalOperationRecord, IdempotencyReservationRequest), ReconcileError> {
        if self.action.namespace() != resource_type.namespace() {
            return Err(ReconcileError::InvalidIntent);
        }
        let kernel = o3k_kernel::Operation {
            id: operation.id,
            service: self.action.namespace().to_owned(),
            action: self.action.clone(),
            actor: self.actor.clone(),
            owner_scope: self.owner_scope.clone(),
            resource_type: resource_type.clone(),
            resource_id: Some(
                o3k_kernel::ResourceId::new(operation.resource_id.to_string())
                    .map_err(|_| ReconcileError::InvalidIntent)?,
            ),
            state: operation.state.into(),
            attempt: 0,
            created_at: self.created_at.clone(),
            started_at: None,
            finished_at: None,
            error: None,
            request_id: self.request_id.clone(),
        };
        let canonical = CanonicalOperationRecord::from_kernel_operation(&kernel)?;
        let reservation = IdempotencyReservationRequest::from_semantics(
            self.owner_scope.id().as_str(),
            self.action.to_string(),
            self.idempotency_key.clone(),
            &resource_type.to_string(),
            target,
            &self.semantic_request,
            operation.id,
        )?;
        Ok((canonical, reservation))
    }
}

impl LifecycleAction {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::Start => "lifecycle:start",
            Self::Stop => "lifecycle:stop",
            Self::Reboot => "lifecycle:reboot",
            Self::Delete => "lifecycle:delete",
        }
    }

    pub(crate) fn parse(kind: &str) -> Option<Self> {
        match kind {
            "lifecycle:start" => Some(Self::Start),
            "lifecycle:stop" => Some(Self::Stop),
            "lifecycle:reboot" => Some(Self::Reboot),
            "lifecycle:delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEvent {
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub kind: JournalEventKind,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("durable store error")]
    Store(#[from] StoreError),
    #[error("provider error")]
    Provider(#[from] ProviderError),
    #[error("stored create intent is invalid")]
    InvalidIntent,
    #[error("retry budget exhausted")]
    RetryExhausted,
    #[error("agent operation evidence is stale")]
    StaleAgentEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEvidenceFence {
    pub(crate) agent_id: String,
    pub(crate) agent_epoch: String,
    pub(crate) sequence: u64,
    pub(crate) state: AgentOperationState,
    pub(crate) provider_resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservationEvidenceFence {
    pub(crate) agent_id: String,
    pub(crate) agent_epoch: String,
    pub(crate) sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceDisposition {
    New,
    Duplicate,
    Stale,
}

pub(crate) struct AgentEvidencePermit {
    pub(crate) disposition: EvidenceDisposition,
    pub(crate) _epoch_lease: Option<Box<dyn o3k_provider::AgentEpochLease>>,
}

// ─── Operation journal ─────────────────────────────────────

// ─── OperationJournal ──────────────────────────────────────
