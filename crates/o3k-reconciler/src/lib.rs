use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use o3k_domain::ServerState;
use o3k_provider::{
    AgentCommandAccepted, AgentErrorCategory, AgentNodeRegistry, AgentObservation,
    AgentOperationState, AgentOperationUpdate, ComputeProvider, CreateInstanceRequest,
    OperationState as ProviderOperationState, ProviderError,
};
use o3k_store::{
    AgentCommandRecord, AgentCommandState, CanonicalAcceptanceOutcome, CanonicalOperationRecord,
    DurableStore, IdempotencyReservationRequest, ObservationUpdate, OperationRecord,
    OperationState, ProviderReference, ResourceRecord, StoreError, server_state_to_storage,
};
use thiserror::Error;
use uuid::Uuid;

pub mod storage_workflow;

/// Test-only fault pause (issue #87): sleeps the configured duration when the
/// named env var is set. Absent, empty, non-numeric, or zero values are no-ops;
/// production configuration never sets these variables.
fn test_fault_pause_ms(name: &str, env_var: &str) {
    let Some(ms) = test_fault_pause_ms_value(std::env::var(env_var).ok()) else {
        return;
    };
    tracing::info!(pause_ms = ms, "test-only fault pause {} enabled", name);
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Parse/guard half of `test_fault_pause_ms`; split out so the no-op
/// conditions can be unit-tested without sleeping.
fn test_fault_pause_ms_value(raw: Option<String>) -> Option<u64> {
    let raw = raw?;
    let Ok(ms) = raw.parse::<u64>() else {
        return None;
    };
    if ms == 0 {
        return None;
    }
    Some(ms)
}

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

    fn records(
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
    fn kind(self) -> &'static str {
        match self {
            Self::Start => "lifecycle:start",
            Self::Stop => "lifecycle:stop",
            Self::Reboot => "lifecycle:reboot",
            Self::Delete => "lifecycle:delete",
        }
    }

    fn parse(kind: &str) -> Option<Self> {
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
struct AgentEvidenceFence {
    agent_id: String,
    agent_epoch: String,
    sequence: u64,
    state: AgentOperationState,
    provider_resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservationEvidenceFence {
    agent_id: String,
    agent_epoch: String,
    sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceDisposition {
    New,
    Duplicate,
    Stale,
}

struct AgentEvidencePermit {
    disposition: EvidenceDisposition,
    _epoch_lease: Option<Box<dyn o3k_provider::AgentEpochLease>>,
}


// ─── Operation journal ─────────────────────────────────────
pub struct OperationJournal<S: ?Sized, P: ?Sized> {
    store: Arc<S>,
    provider: Arc<P>,
    max_attempts: u8,
    events: Arc<Mutex<Vec<JournalEvent>>>,
    agent_evidence: Arc<Mutex<HashMap<Uuid, AgentEvidenceFence>>>,
    observation_evidence: Arc<Mutex<HashMap<Uuid, ObservationEvidenceFence>>>,
    /// Optional node registry used to resolve the agent's *current* registered
    /// epoch. When present it is authoritative for evidence fencing: the
    /// fence rejects evidence minted under any other epoch (a dead/stale
    /// stream, including the pre-restart connection) and re-anchors the
    /// operation when the same agent legitimately re-registered under a fresh
    /// epoch. Without a registry the fence keeps the strict first-evidence
    /// anchor of the original behavior.
    agent_registry: Option<Arc<dyn AgentNodeRegistry>>,
}

impl<S: ?Sized, P: ?Sized> Clone for OperationJournal<S, P> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            provider: self.provider.clone(),
            max_attempts: self.max_attempts,
            events: self.events.clone(),
            agent_evidence: self.agent_evidence.clone(),
            observation_evidence: self.observation_evidence.clone(),
            agent_registry: self.agent_registry.clone(),
        }
    }
}

impl<S: ?Sized, P: ?Sized> OperationJournal<S, P>
where
    S: DurableStore + 'static,
    P: ComputeProvider + 'static,
{
    pub fn new(store: Arc<S>, provider: Arc<P>, max_attempts: u8) -> Self {
        Self {
            store,
            provider,
            max_attempts: max_attempts.max(1),
            events: Arc::new(Mutex::new(Vec::new())),
            agent_evidence: Arc::new(Mutex::new(HashMap::new())),
            observation_evidence: Arc::new(Mutex::new(HashMap::new())),
            agent_registry: None,
        }
    }

    /// Attaches the agent node registry so evidence fencing can distinguish
    /// the agent's current registered epoch from dead epochs of replaced
    /// connections (issue #87 crash-restart replay). Wired by the composition
    /// root; the registry is intentionally optional so direct fake-provider
    /// operation keeps the strict anchored fence.
    #[must_use]
    pub fn with_agent_registry(mut self, registry: Arc<dyn AgentNodeRegistry>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    async fn fence_agent_evidence(
        &self,
        operation_id: Uuid,
        agent_id: &str,
        agent_epoch: &str,
        sequence: u64,
        state: AgentOperationState,
        provider_resource_id: &str,
    ) -> Result<AgentEvidencePermit, ReconcileError> {
        if agent_id.trim().is_empty()
            || agent_epoch.trim().is_empty()
            || sequence == 0
            || !valid_agent_reference(agent_id)
            || !valid_agent_reference(agent_epoch)
        {
            return Err(ReconcileError::InvalidIntent);
        }
        // The registry is authoritative for the agent's current epoch: every
        // registration replaces the stored epoch (minted per connection), so
        // evidence minted under any other epoch is a dead/stale stream and
        // must not mutate current state. This is what lets a legitimate
        // post-restart replay (the same agent re-registered with a fresh
        // epoch) re-anchor the operation while still rejecting evidence from
        // replaced connections. Fail closed when the agent is not registered:
        // an unregistered agent has no current epoch at all.
        let epoch_lease = if let Some(registry) = &self.agent_registry {
            Some(
                registry
                    .lease_current_epoch(agent_id, agent_epoch)
                    .await
                    .ok_or(ReconcileError::StaleAgentEvidence)?,
            )
        } else {
            None
        };
        let mut evidence = self
            .agent_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let next = AgentEvidenceFence {
            agent_id: agent_id.to_owned(),
            agent_epoch: agent_epoch.to_owned(),
            sequence,
            state,
            provider_resource_id: provider_resource_id.to_owned(),
        };
        let disposition = match evidence.get(&operation_id) {
            None => {
                evidence.insert(operation_id, next);
                Ok(EvidenceDisposition::New)
            }
            // A different agent cannot claim the operation. An epoch change of
            // the SAME agent is legitimate only when the registry resolved it
            // as current above; without a registry the operation stays
            // anchored to the first evidence epoch.
            Some(previous) if previous.agent_id != agent_id => Err(ReconcileError::InvalidIntent),
            Some(previous)
                if previous.agent_epoch != agent_epoch && self.agent_registry.is_none() =>
            {
                Err(ReconcileError::InvalidIntent)
            }
            Some(previous) if previous.agent_epoch != agent_epoch => {
                evidence.insert(operation_id, next);
                Ok(EvidenceDisposition::New)
            }
            Some(previous) if sequence < previous.sequence => Ok(EvidenceDisposition::Stale),
            Some(previous) if sequence == previous.sequence => {
                if previous.state == state && previous.provider_resource_id == provider_resource_id
                {
                    Ok(EvidenceDisposition::Duplicate)
                } else {
                    Err(ReconcileError::InvalidIntent)
                }
            }
            Some(_) => {
                evidence.insert(operation_id, next);
                Ok(EvidenceDisposition::New)
            }
        }?;
        Ok(AgentEvidencePermit {
            disposition,
            _epoch_lease: epoch_lease,
        })
    }

    /// Observations are terminal evidence for a durable command, not a free
    /// standing state update.  Keep their sequence fence separate from command
    /// acceptance/update sequences: both may legitimately start at one.
    async fn fence_agent_observation(
        &self,
        operation_id: Uuid,
        agent_id: &str,
        agent_epoch: &str,
        sequence: u64,
    ) -> Result<AgentEvidencePermit, ReconcileError> {
        if agent_id.trim().is_empty()
            || agent_epoch.trim().is_empty()
            || sequence == 0
            || !valid_agent_reference(agent_id)
            || !valid_agent_reference(agent_epoch)
        {
            return Err(ReconcileError::InvalidIntent);
        }
        let epoch_lease = if let Some(registry) = &self.agent_registry {
            Some(
                registry
                    .lease_current_epoch(agent_id, agent_epoch)
                    .await
                    .ok_or(ReconcileError::StaleAgentEvidence)?,
            )
        } else {
            None
        };
        let mut evidence = self
            .observation_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let disposition = match evidence.get(&operation_id) {
            None => {
                evidence.insert(
                    operation_id,
                    ObservationEvidenceFence {
                        agent_id: agent_id.to_owned(),
                        agent_epoch: agent_epoch.to_owned(),
                        sequence,
                    },
                );
                Ok(EvidenceDisposition::New)
            }
            Some(previous) if previous.agent_id != agent_id => Err(ReconcileError::InvalidIntent),
            Some(previous) if previous.agent_epoch != agent_epoch => {
                // A fresh epoch is allowed only when the registry above says
                // it is the current connection for the same durable agent.
                if self.agent_registry.is_none() {
                    return Err(ReconcileError::InvalidIntent);
                }
                evidence.insert(
                    operation_id,
                    ObservationEvidenceFence {
                        agent_id: agent_id.to_owned(),
                        agent_epoch: agent_epoch.to_owned(),
                        sequence,
                    },
                );
                Ok(EvidenceDisposition::New)
            }
            Some(previous) if sequence < previous.sequence => Ok(EvidenceDisposition::Stale),
            Some(_) => Ok(EvidenceDisposition::Duplicate),
        }?;
        Ok(AgentEvidencePermit {
            disposition,
            _epoch_lease: epoch_lease,
        })
    }

    pub fn events(&self) -> Vec<JournalEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }

    pub async fn begin_create(
        &self,
        project_id: &str,
        request: &CreateInstanceRequest,
    ) -> Result<Uuid, ReconcileError> {
        let resource = ResourceRecord {
            id: request.o3k_server_id,
            kind: "compute_instance".to_owned(),
            project_id: project_id.to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: serde_json::to_string(request)
                .map_err(|_| ReconcileError::InvalidIntent)?,
            observed_state: server_state_to_storage(ServerState::Requested).to_owned(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: request.operation_id,
            resource_id: request.o3k_server_id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        self.store
            .insert_resource_and_operation(
                &resource,
                &operation,
                request.placement_allocation_id.as_deref(),
            )
            .await?;
        self.event(
            request.operation_id,
            request.o3k_server_id,
            JournalEventKind::IntentPersisted,
        );
        Ok(operation.id)
    }

    pub async fn begin_canonical_create(
        &self,
        project_id: &str,
        request: &CreateInstanceRequest,
        context: &CanonicalMutationContext,
    ) -> Result<CanonicalAcceptanceOutcome, ReconcileError> {
        let resource = ResourceRecord {
            id: request.o3k_server_id,
            kind: "compute_instance".to_owned(),
            project_id: project_id.to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: serde_json::to_string(request)
                .map_err(|_| ReconcileError::InvalidIntent)?,
            observed_state: server_state_to_storage(ServerState::Requested).to_owned(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: request.operation_id,
            resource_id: request.o3k_server_id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let resource_type = o3k_kernel::ResourceType::new("compute", "server")
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let (canonical, reservation) = context.records(&operation, resource_type, None)?;
        let outcome = self
            .store
            .create_or_replay_canonical_resource_operation(
                &resource,
                &operation,
                &canonical,
                &reservation,
                request.placement_allocation_id.as_deref(),
            )
            .await?;
        if matches!(outcome, CanonicalAcceptanceOutcome::Created { .. }) {
            self.event(
                request.operation_id,
                request.o3k_server_id,
                JournalEventKind::IntentPersisted,
            );
        }
        Ok(outcome)
    }

    pub async fn begin_lifecycle(
        &self,
        resource_id: Uuid,
        operation_id: Uuid,
        action: LifecycleAction,
    ) -> Result<Uuid, ReconcileError> {
        self.store
            .insert_operation(&OperationRecord {
                id: operation_id,
                resource_id,
                kind: action.kind().to_owned(),
                state: OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        self.event(operation_id, resource_id, JournalEventKind::IntentPersisted);
        Ok(operation_id)
    }

    pub async fn begin_canonical_lifecycle(
        &self,
        resource_id: Uuid,
        operation_id: Uuid,
        action: LifecycleAction,
        context: &CanonicalMutationContext,
    ) -> Result<CanonicalAcceptanceOutcome, ReconcileError> {
        let operation = OperationRecord {
            id: operation_id,
            resource_id,
            kind: action.kind().to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let resource_type = o3k_kernel::ResourceType::new("compute", "server")
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let target = resource_id.to_string();
        let (canonical, reservation) = context.records(&operation, resource_type, Some(&target))?;
        let outcome = self
            .store
            .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &reservation)
            .await?;
        if matches!(outcome, CanonicalAcceptanceOutcome::Created { .. }) {
            self.event(operation_id, resource_id, JournalEventKind::IntentPersisted);
        }
        Ok(outcome)
    }

    /// Applies an authenticated compute-agent update to the same durable records
    /// used by the provider reconciliation loop. The agent is responsible for
    /// redacting secrets and connection information before sending a failure
    /// reason; the control plane persists it only after bounding and
    /// sanitizing (bounded_agent_failure_message), so durable records and
    /// operator logs stay free of control characters and unbounded payloads.
    pub async fn apply_agent_update(
        &self,
        update: &AgentOperationUpdate,
    ) -> Result<OperationState, ReconcileError> {
        let operation = self.store.get_operation(update.operation_id).await?;
        if operation.resource_id != update.resource_id {
            return Err(ReconcileError::InvalidIntent);
        }
        let command = self
            .store
            .get_agent_command_by_operation(update.operation_id)
            .await
            .map_err(|_| ReconcileError::InvalidIntent)?;
        if command.agent_id != update.agent_id || command.resource_id != update.resource_id {
            return Err(ReconcileError::InvalidIntent);
        }
        let (command_state, durable_state) = match update.state {
            AgentOperationState::Accepted => (AgentCommandState::Accepted, OperationState::Running),
            AgentOperationState::Running => (AgentCommandState::Running, OperationState::Running),
            AgentOperationState::Succeeded => {
                (AgentCommandState::Succeeded, OperationState::Succeeded)
            }
            AgentOperationState::Failed => (AgentCommandState::Failed, OperationState::Failed),
            AgentOperationState::UnknownOutcome => (
                AgentCommandState::UnknownOutcome,
                OperationState::UnknownOutcome,
            ),
        };
        if operation.state == OperationState::UnknownOutcome
            && matches!(durable_state, OperationState::Running)
            || command.state == AgentCommandState::UnknownOutcome
                && matches!(
                    command_state,
                    AgentCommandState::Accepted | AgentCommandState::Running
                )
        {
            return Err(ReconcileError::InvalidIntent);
        }
        let error_category = if durable_state == OperationState::Failed {
            Some(agent_error_category(update.error_category)?)
        } else {
            None
        };
        let error_message = (durable_state == OperationState::Failed).then(|| {
            bounded_agent_failure_message(update.redacted_message.as_deref().unwrap_or(""))
        });
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) && operation.state != durable_state
        {
            return Err(ReconcileError::InvalidIntent);
        }
        self.validate_agent_provider_identity(&command, update)
            .await?;
        let evidence_permit = self
            .fence_agent_evidence(
                update.operation_id,
                &update.agent_id,
                &update.agent_epoch,
                update.operation_sequence,
                update.state,
                update.provider_resource_id.as_deref().unwrap_or(""),
            )
            .await?;
        if evidence_permit.disposition == EvidenceDisposition::Stale {
            return Ok(operation.state);
        }
        // Persist the agent-reported, contract-redacted failure reason so the
        // durable record carries an actionable cause; it is bounded and
        // sanitized before storage. Unknown-outcome and non-failure updates
        // keep no message.
        // The epoch-fenced journal is the authoritative durable command
        // projector (the provider adapter keeps only a current-epoch,
        // identity-checked backup for broadcast lag).
        // A terminal observation can win the event-consumer race and close
        // the operation before this update arrives, but this write must still
        // close the matching command row (ASR-015 E1 -> E2 crash recovery).
        self.store
            .update_agent_command(
                &command.command_id,
                command_state,
                command.accepted_sequence,
                update.operation_sequence,
                command.provider_operation_id.as_deref(),
                update.provider_resource_id.as_deref(),
            )
            .await?;
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        let provider_operation_id = operation.provider_operation_id.as_deref();
        self.store
            .update_operation(
                update.operation_id,
                durable_state,
                provider_operation_id,
                error_category,
                error_message.as_deref(),
            )
            .await?;

        if durable_state == OperationState::Failed && operation.kind == "create" {
            // A terminally failed create must not leave the resource in its
            // pre-creation state: clients polling the server would otherwise
            // wait forever. Projecting ERROR keeps the failure durable and
            // visible while observations remain the only success projection.
            let resource = self.store.get_resource(update.resource_id).await?;
            self.store
                .update_resource(
                    update.resource_id,
                    resource.generation,
                    &resource.desired_state,
                    server_state_to_storage(ServerState::Error),
                    resource.generation,
                    resource.provider_id.as_deref(),
                )
                .await?;
        }

        if durable_state == OperationState::Succeeded {
            let resource = self.store.get_resource(update.resource_id).await?;
            let provider_id = update
                .provider_resource_id
                .as_deref()
                .or(resource.provider_id.as_deref());
            if let Some(provider_resource_id) = provider_id {
                match self
                    .store
                    .get_provider_reference(update.resource_id, "compute-agent")
                    .await
                {
                    Ok(existing) if existing.provider_resource_id == provider_resource_id => {}
                    Ok(_) => return Err(StoreError::ProviderReferenceAlreadyExists.into()),
                    Err(StoreError::ProviderReferenceNotFound) => {
                        self.store
                            .attach_provider_reference(&ProviderReference {
                                resource_id: update.resource_id,
                                provider_name: "compute-agent".to_owned(),
                                provider_resource_id: provider_resource_id.to_owned(),
                            })
                            .await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            self.store
                .update_resource(
                    update.resource_id,
                    resource.generation,
                    &resource.desired_state,
                    &resource.observed_state,
                    resource.generation,
                    provider_id,
                )
                .await?;
        }
        if evidence_permit.disposition == EvidenceDisposition::New {
            self.event(
                update.operation_id,
                update.resource_id,
                match durable_state {
                    OperationState::Succeeded => JournalEventKind::Succeeded,
                    OperationState::Failed => JournalEventKind::Failed,
                    OperationState::UnknownOutcome => JournalEventKind::UnknownObserved,
                    _ => JournalEventKind::ProviderStarted,
                },
            );
        }
        Ok(durable_state)
    }

    async fn validate_agent_provider_identity(
        &self,
        command: &AgentCommandRecord,
        update: &AgentOperationUpdate,
    ) -> Result<(), ReconcileError> {
        let Some(incoming) = update.provider_resource_id.as_deref() else {
            return Ok(());
        };
        if command
            .provider_resource_id
            .as_deref()
            .is_some_and(|existing| existing != incoming)
        {
            return Err(ReconcileError::InvalidIntent);
        }
        let resource = self.store.get_resource(update.resource_id).await?;
        if resource
            .provider_id
            .as_deref()
            .is_some_and(|existing| existing != incoming)
        {
            return Err(ReconcileError::InvalidIntent);
        }
        for provider_name in ["compute-agent", "agent"] {
            match self
                .store
                .get_provider_reference(update.resource_id, provider_name)
                .await
            {
                Ok(reference) if reference.provider_resource_id != incoming => {
                    return Err(ReconcileError::InvalidIntent);
                }
                Ok(_) | Err(StoreError::ProviderReferenceNotFound) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Commits an authenticated command acceptance before the agent executes
    /// the command. Duplicate acceptances are idempotent because the durable
    /// operation is simply kept in `running` state.
    pub async fn apply_agent_acceptance(
        &self,
        accepted: &AgentCommandAccepted,
    ) -> Result<OperationState, ReconcileError> {
        let operation = self.store.get_operation(accepted.operation_id).await?;
        let command = self
            .store
            .get_agent_command(&accepted.command_id)
            .await
            .map_err(|_| ReconcileError::InvalidIntent)?;
        if command.operation_id != accepted.operation_id || command.agent_id != accepted.agent_id {
            return Err(ReconcileError::InvalidIntent);
        }
        match accepted.state {
            AgentOperationState::Accepted | AgentOperationState::Running => {}
            _ => return Err(ReconcileError::InvalidIntent),
        }
        if operation.state == OperationState::UnknownOutcome
            || command.state == AgentCommandState::UnknownOutcome
        {
            return Err(ReconcileError::InvalidIntent);
        }
        let evidence_permit = self
            .fence_agent_evidence(
                accepted.operation_id,
                &accepted.agent_id,
                &accepted.agent_epoch,
                accepted.operation_sequence,
                accepted.state,
                "",
            )
            .await?;
        if evidence_permit.disposition == EvidenceDisposition::Stale {
            return Ok(operation.state);
        }
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        self.store
            .update_agent_command(
                &command.command_id,
                AgentCommandState::Accepted,
                accepted.operation_sequence,
                accepted.operation_sequence,
                command.provider_operation_id.as_deref(),
                command.provider_resource_id.as_deref(),
            )
            .await?;
        self.store
            .update_operation(
                accepted.operation_id,
                OperationState::Running,
                operation.provider_operation_id.as_deref(),
                operation.error_category.as_deref(),
                operation.error_message.as_deref(),
            )
            .await?;
        if evidence_permit.disposition == EvidenceDisposition::New {
            self.event(
                accepted.operation_id,
                operation.resource_id,
                JournalEventKind::ProviderStarted,
            );
        }
        Ok(OperationState::Running)
    }

    /// Applies the provider state carried by an authenticated agent
    /// observation. Operation updates describe command progress; observations
    /// are the only live input that may change the durable resource state.
    /// Unspecified or non-successful observations are rejected so an incomplete
    /// message cannot make Nova project a healthy state.
    pub async fn apply_agent_observation(
        &self,
        observation: &AgentObservation,
    ) -> Result<(), ReconcileError> {
        let operation = self.store.get_operation(observation.operation_id).await?;
        if operation.resource_id != observation.resource_id {
            tracing::debug!(
                operation_id=%observation.operation_id,
                operation_resource_id=%operation.resource_id,
                observation_resource_id=%observation.resource_id,
                "apply_agent_observation: resource_id mismatch"
            );
            return Err(ReconcileError::InvalidIntent);
        }
        // The operation row alone is not an authorization grant: bind the
        // observation to the durable command that was assigned to this agent
        // and resource.  This prevents any authenticated agent from claiming
        // a successful observation for another agent's operation.
        let command = self
            .store
            .get_agent_command_by_operation(observation.operation_id)
            .await
            .map_err(|_| ReconcileError::InvalidIntent)?;
        if command.agent_id != observation.agent_id
            || command.resource_id != observation.resource_id
        {
            return Err(ReconcileError::InvalidIntent);
        }
        if observation.operation_state != AgentOperationState::Succeeded {
            tracing::debug!(
                operation_id=%observation.operation_id,
                operation_state=?observation.operation_state,
                "apply_agent_observation: operation_state is not Succeeded"
            );
            return Err(ReconcileError::InvalidIntent);
        }
        let evidence_permit = self
            .fence_agent_observation(
                observation.operation_id,
                &observation.agent_id,
                &observation.agent_epoch,
                observation.observation_sequence,
            )
            .await?;
        if evidence_permit.disposition != EvidenceDisposition::New {
            return Ok(());
        }
        let observed_state = server_state_to_storage(ServerState::from(observation.state));
        let resource = self.store.get_resource(observation.resource_id).await?;
        let provider_id = observation
            .provider_resource_id
            .as_deref()
            .or(resource.provider_id.as_deref());
        tracing::debug!(
            operation_id=%observation.operation_id,
            resource_id=%observation.resource_id,
            operation_state=?operation.state,
            resource_generation=resource.generation,
            provider_id=?provider_id,
            observed_state=%observed_state,
            agent_epoch=%observation.agent_epoch,
            observation_sequence=observation.observation_sequence,
            "apply_agent_observation: processing observation"
        );
        if let Some(provider_resource_id) = provider_id {
            match self
                .store
                .get_provider_reference(observation.resource_id, "compute-agent")
                .await
            {
                Ok(existing) if existing.provider_resource_id == provider_resource_id => {
                    tracing::debug!(resource_id=%observation.resource_id, provider_resource_id, "apply_agent_observation: provider reference already matches");
                }
                Ok(existing) => {
                    tracing::debug!(
                        resource_id=%observation.resource_id,
                        existing_provider_resource_id=%existing.provider_resource_id,
                        new_provider_resource_id=provider_resource_id,
                        "apply_agent_observation: provider reference conflict"
                    );
                    return Err(StoreError::ProviderReferenceAlreadyExists.into());
                }
                Err(StoreError::ProviderReferenceNotFound) => {
                    tracing::debug!(resource_id=%observation.resource_id, provider_resource_id, "apply_agent_observation: attaching new provider reference");
                    self.store
                        .attach_provider_reference(&ProviderReference {
                            resource_id: observation.resource_id,
                            provider_name: "compute-agent".to_owned(),
                            provider_resource_id: provider_resource_id.to_owned(),
                        })
                        .await?;
                }
                Err(error) => {
                    tracing::debug!(error=%error, resource_id=%observation.resource_id, "apply_agent_observation: get_provider_reference failed");
                    return Err(error.into());
                }
            }
        }
        let update = ObservationUpdate {
            expected_generation: resource.generation,
            desired_state: &resource.desired_state,
            observed_state,
            observed_generation: resource.generation,
            provider_id,
            agent_epoch: &observation.agent_epoch,
            observation_sequence: observation.observation_sequence,
        };
        let updated = self
            .store
            .update_resource_from_observation(observation.resource_id, &update)
            .await
            .map_err(|e| {
                tracing::debug!(
                    error=%e,
                    resource_id=%observation.resource_id,
                    expected_generation=resource.generation,
                    observed_state=%observed_state,
                    agent_epoch=%observation.agent_epoch,
                    observation_sequence=observation.observation_sequence,
                    "apply_agent_observation: update_resource_from_observation failed"
                );
                e
            })?;
        tracing::debug!(
            operation_id=%observation.operation_id,
            resource_id=%observation.resource_id,
            old_generation=resource.generation,
            new_generation=updated.generation,
            "apply_agent_observation: resource updated from observation"
        );
        // Observations for inspect commands carry the terminal operation state
        // but do not emit a separate OperationUpdate. Promote the operation to
        // Succeeded so that idempotent re-inspect returns the durable result.
        if !matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            self.store
                .update_operation(
                    observation.operation_id,
                    OperationState::Succeeded,
                    operation.provider_operation_id.as_deref(),
                    None,
                    None,
                )
                .await?;
            tracing::debug!(operation_id=%observation.operation_id, "apply_agent_observation: promoted operation to Succeeded");
        }
        if updated.generation == resource.generation {
            tracing::debug!(operation_id=%observation.operation_id, resource_id=%observation.resource_id, "apply_agent_observation: generation unchanged, observation was duplicate");
            return Ok(());
        }
        self.event(
            observation.operation_id,
            observation.resource_id,
            JournalEventKind::UnknownObserved,
        );
        Ok(())
    }

    pub async fn reconcile_lifecycle_once(
        &self,
        operation_id: Uuid,
    ) -> Result<OperationState, ReconcileError> {
        let operation = self.store.get_operation(operation_id).await?;
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        let action =
            LifecycleAction::parse(&operation.kind).ok_or(ReconcileError::InvalidIntent)?;
        let resource = self.store.get_resource(operation.resource_id).await?;
        if operation.state == OperationState::UnknownOutcome {
            if operation.provider_operation_id.is_none() {
                // Issue #609: the retry budget exhausted before any provider
                // operation identity existed (every dispatch was rejected as
                // retryable), so there is no provider operation to poll.
                // Presence decides; a reconnected agent's terminal update can
                // also still arrive through the event stream.
                return self
                    .observe_lifecycle_presence(operation, resource, action)
                    .await;
            }
            return self.observe_lifecycle(operation, resource, action).await;
        }
        let provider_id = resource
            .provider_id
            .clone()
            .ok_or(ReconcileError::InvalidIntent)?;
        self.store
            .update_operation(
                operation_id,
                OperationState::Running,
                operation.provider_operation_id.as_deref(),
                None,
                None,
            )
            .await?;
        self.event(operation_id, resource.id, JournalEventKind::ProviderStarted);
        let result = match action {
            LifecycleAction::Delete => {
                self.provider
                    .delete_instance(o3k_provider::DeleteInstanceRequest {
                        operation_id,
                        provider_instance_id: provider_id.clone(),
                        idempotency_key: format!("o3k-operation-{operation_id}"),
                    })
                    .await
            }
            LifecycleAction::Start | LifecycleAction::Stop | LifecycleAction::Reboot => {
                self.provider
                    .action_instance(
                        &provider_id,
                        match action {
                            LifecycleAction::Start => o3k_provider::InstanceAction::Start,
                            LifecycleAction::Stop => o3k_provider::InstanceAction::Stop,
                            LifecycleAction::Reboot => o3k_provider::InstanceAction::Reboot,
                            LifecycleAction::Delete => unreachable!(),
                        },
                        operation_id,
                        &format!("o3k-operation-{operation_id}"),
                    )
                    .await
            }
        };
        self.handle_lifecycle_result(operation, resource, action, provider_id, result)
            .await
    }

    async fn observe_lifecycle(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
        action: LifecycleAction,
    ) -> Result<OperationState, ReconcileError> {
        let Some(provider_operation_id) = operation.provider_operation_id.as_deref() else {
            return Err(ReconcileError::InvalidIntent);
        };
        let provider_id = resource
            .provider_id
            .clone()
            .ok_or(ReconcileError::InvalidIntent)?;
        let presence = self.provider.get_instance(&provider_id).await;
        let instance_present = presence.is_ok();
        if action == LifecycleAction::Delete && matches!(presence, Err(ProviderError::NotFound)) {
            return self
                .finish_lifecycle(
                    operation.id,
                    resource,
                    action,
                    provider_operation_id.to_owned(),
                    provider_id,
                )
                .await;
        }
        let provider_operation = self
            .provider
            .get_operation(
                Uuid::parse_str(provider_operation_id)
                    .map_err(|_| ReconcileError::InvalidIntent)?,
            )
            .await?;
        validate_lifecycle_provider_operation_owner(operation.id, action, &provider_operation)?;
        match provider_operation.state {
            ProviderOperationState::Succeeded => {
                self.finish_lifecycle(
                    operation.id,
                    resource,
                    action,
                    provider_operation_id.to_owned(),
                    provider_id,
                )
                .await
            }
            ProviderOperationState::UnknownOutcome => {
                self.event(operation.id, resource.id, JournalEventKind::UnknownObserved);
                if action != LifecycleAction::Delete {
                    let instance = self.provider.get_instance(&provider_id).await?;
                    let converged = match action {
                        LifecycleAction::Start | LifecycleAction::Reboot => {
                            instance.state == o3k_provider::InstanceState::Running
                        }
                        LifecycleAction::Stop => {
                            instance.state == o3k_provider::InstanceState::Stopped
                        }
                        LifecycleAction::Delete => false,
                    };
                    if converged {
                        return self
                            .finish_lifecycle(
                                operation.id,
                                resource,
                                action,
                                provider_operation_id.to_owned(),
                                provider_id,
                            )
                            .await;
                    }
                } else if instance_present {
                    // #575: the delete command's outcome is unknown and the
                    // instance is still present, so the delete goal is NOT
                    // converged. The recorded command cannot make progress
                    // again (the agent journal replays the recorded unknown
                    // outcome instead of re-executing), so reconciliation
                    // mints one deterministic fresh command identity.
                    return self.redrive_delete(operation, resource, provider_id).await;
                }
                Ok(OperationState::UnknownOutcome)
            }
            ProviderOperationState::Retryable => {
                self.retry_or_fail(operation.id, resource.id, ProviderError::Retryable)
                    .await
            }
            ProviderOperationState::Accepted | ProviderOperationState::Running
                if action == LifecycleAction::Delete && instance_present =>
            {
                // The old accepted command may have been lost after the
                // host mutation began (#575).  Reusing its provider command
                // identity can keep the operation permanently Accepted.
                // Mint one deterministic command identity tied to the
                // durable lifecycle operation, so retries are idempotent and
                // old-stream evidence cannot complete the new command.
                self.redrive_delete(operation, resource, provider_id).await
            }
            ProviderOperationState::Accepted | ProviderOperationState::Running => {
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::Running,
                        Some(provider_operation_id),
                        None,
                        None,
                    )
                    .await?;
                Ok(OperationState::Running)
            }
            ProviderOperationState::Failed => {
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::Failed,
                        Some(provider_operation_id),
                        Some("terminal"),
                        Some("provider operation failed"),
                    )
                    .await?;
                self.event(operation.id, resource.id, JournalEventKind::Failed);
                Ok(OperationState::Failed)
            }
        }
    }

    /// Issue #609: observes the instance at the execution boundary for a
    /// lifecycle operation whose retry budget exhausted before any provider
    /// operation identity existed. There is no provider operation to poll,
    /// so presence decides exactly like `observe_lifecycle`'s unknown-outcome
    /// arm: a delete is converged when the instance is provably absent, a
    /// still-present delete re-drives with the #575 fresh command identity,
    /// and a start/stop/reboot converges when the instance state matches.
    /// Everything else stays `UnknownOutcome` — a reconnected agent's
    /// terminal update still arrives through the event stream, and the
    /// lifecycle convergence sweep re-drives the operation each pass.
    async fn observe_lifecycle_presence(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
        action: LifecycleAction,
    ) -> Result<OperationState, ReconcileError> {
        let provider_id = resource
            .provider_id
            .clone()
            .ok_or(ReconcileError::InvalidIntent)?;
        let presence = self.provider.get_instance(&provider_id).await;
        if action == LifecycleAction::Delete {
            return match presence {
                Err(ProviderError::NotFound) => {
                    self.finish_lifecycle(
                        operation.id,
                        resource,
                        action,
                        operation.id.to_string(),
                        provider_id,
                    )
                    .await
                }
                Ok(_) => self.redrive_delete(operation, resource, provider_id).await,
                Err(_) => Ok(OperationState::UnknownOutcome),
            };
        }
        let converged = match presence {
            Ok(instance) => match action {
                LifecycleAction::Start | LifecycleAction::Reboot => {
                    instance.state == o3k_provider::InstanceState::Running
                }
                LifecycleAction::Stop => instance.state == o3k_provider::InstanceState::Stopped,
                LifecycleAction::Delete => false,
            },
            Err(_) => false,
        };
        if converged {
            return self
                .finish_lifecycle(
                    operation.id,
                    resource,
                    action,
                    operation.id.to_string(),
                    provider_id,
                )
                .await;
        }
        Ok(OperationState::UnknownOutcome)
    }

    /// #575 stale-accepted delete re-drive: mints ONE deterministic fresh
    /// command identity tied to the durable lifecycle operation and
    /// dispatches the delete again. The recorded command cannot make
    /// progress (its observation was rejected and the agent journal replays
    /// the recorded outcome instead of re-executing), and the instance is
    /// still present, so the delete goal is not converged. Re-drives are
    /// idempotent: repeated passes re-dispatch the SAME command identity,
    /// which the agent journal reuses without a second execution, and
    /// old-stream evidence cannot complete the fresh command. The dispatch
    /// result is handled by `handle_lifecycle_result`, which keeps a
    /// re-drive that is merely accepted in `UnknownOutcome` so the lifecycle
    /// sweep keeps observing the fresh provider operation to terminal.
    async fn redrive_delete(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
        provider_id: String,
    ) -> Result<OperationState, ReconcileError> {
        let redrive_operation_id = delete_redrive_operation_id(operation.id);
        // The fresh command identity must have a durable operation row
        // BEFORE the provider persists its command record: the agent-command
        // ledger references `operations(id)`, the evidence consumers resolve
        // by operation id, and the lifecycle sweep lists lifecycle rows.
        // Without the row the command insert fails the foreign key, the
        // dispatch returns Conflict, and the delete op terminalizes Failed
        // while the instance is still present (real-host finding, run
        // local-5752). Mirrors the presence-inspection pattern of persisting
        // the operation intent before dispatch.
        match self
            .store
            .insert_operation(&OperationRecord {
                id: redrive_operation_id,
                resource_id: operation.resource_id,
                kind: "lifecycle:delete".to_owned(),
                state: OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await
        {
            Ok(_) | Err(StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(error.into()),
        }
        let result = self
            .provider
            .delete_instance(o3k_provider::DeleteInstanceRequest {
                operation_id: redrive_operation_id,
                provider_instance_id: provider_id.clone(),
                idempotency_key: format!("o3k-operation-{}-redrive", operation.id),
            })
            .await;
        self.handle_lifecycle_result(
            operation,
            resource,
            LifecycleAction::Delete,
            provider_id,
            result,
        )
        .await
    }

    async fn handle_lifecycle_result(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
        action: LifecycleAction,
        provider_id: String,
        result: Result<o3k_provider::Operation, ProviderError>,
    ) -> Result<OperationState, ReconcileError> {
        match result {
            Ok(provider_operation) => {
                validate_lifecycle_provider_operation_owner(
                    operation.id,
                    action,
                    &provider_operation,
                )?;
                match provider_operation.state {
                    ProviderOperationState::Succeeded => {
                        self.finish_lifecycle(
                            operation.id,
                            resource,
                            action,
                            provider_operation.provider_operation_id.to_string(),
                            provider_id,
                        )
                        .await
                    }
                    ProviderOperationState::Accepted | ProviderOperationState::Running => {
                        let redrive = action == LifecycleAction::Delete
                            && provider_operation.o3k_operation_id
                                == delete_redrive_operation_id(operation.id);
                        if redrive {
                            // The fresh re-drive command is in flight on the
                            // agent; its terminal evidence cannot arrive
                            // through the operation event stream (no durable
                            // operation row carries the redrive identity, so
                            // every agent evidence consumer rejects it by
                            // design). The durable operation therefore stays
                            // `UnknownOutcome` with the redrive provider
                            // identity so the lifecycle sweep keeps polling
                            // the fresh provider operation until it reaches
                            // terminal (issue #575).
                            self.store
                                .update_operation(
                                    operation.id,
                                    OperationState::UnknownOutcome,
                                    Some(&provider_operation.provider_operation_id.to_string()),
                                    Some("unknown_outcome"),
                                    None,
                                )
                                .await?;
                            self.event(operation.id, resource.id, JournalEventKind::RetryScheduled);
                            return Ok(OperationState::UnknownOutcome);
                        }
                        self.store
                            .update_operation(
                                operation.id,
                                OperationState::Running,
                                Some(&provider_operation.provider_operation_id.to_string()),
                                None,
                                None,
                            )
                            .await?;
                        Ok(OperationState::Running)
                    }
                    ProviderOperationState::UnknownOutcome => {
                        let provider_operation_id =
                            provider_operation.provider_operation_id.to_string();
                        self.store
                            .update_operation(
                                operation.id,
                                OperationState::UnknownOutcome,
                                Some(&provider_operation_id),
                                Some("unknown_outcome"),
                                None,
                            )
                            .await?;
                        self.event(operation.id, resource.id, JournalEventKind::RetryScheduled);
                        Ok(OperationState::UnknownOutcome)
                    }
                    ProviderOperationState::Retryable => {
                        self.retry_or_fail(operation.id, resource.id, ProviderError::Retryable)
                            .await
                    }
                    ProviderOperationState::Failed => {
                        self.store
                            .update_operation(
                                operation.id,
                                OperationState::Failed,
                                Some(&provider_operation.provider_operation_id.to_string()),
                                Some("terminal"),
                                Some("provider operation failed"),
                            )
                            .await?;
                        self.event(operation.id, resource.id, JournalEventKind::Failed);
                        Ok(OperationState::Failed)
                    }
                }
            }
            Err(ProviderError::UnknownOutcome { operation_id }) => {
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::UnknownOutcome,
                        Some(&operation_id.to_string()),
                        Some("unknown_outcome"),
                        None,
                    )
                    .await?;
                self.event(operation.id, resource.id, JournalEventKind::RetryScheduled);
                Ok(OperationState::UnknownOutcome)
            }
            Err(error @ ProviderError::Retryable) | Err(error @ ProviderError::StaleState) => {
                self.retry_or_fail(operation.id, resource.id, error).await
            }
            Err(ProviderError::NotFound) if action == LifecycleAction::Delete => {
                self.finish_lifecycle(
                    operation.id,
                    resource,
                    action,
                    operation
                        .provider_operation_id
                        .unwrap_or_else(|| operation.id.to_string()),
                    provider_id,
                )
                .await
            }
            Err(error) => {
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::Failed,
                        None,
                        Some(provider_error_category_name(error.category())),
                        Some(&error.to_string()),
                    )
                    .await?;
                self.event(operation.id, resource.id, JournalEventKind::Failed);
                Ok(OperationState::Failed)
            }
        }
    }

    async fn finish_lifecycle(
        &self,
        operation_id: Uuid,
        resource: ResourceRecord,
        action: LifecycleAction,
        provider_operation_id: String,
        provider_id: String,
    ) -> Result<OperationState, ReconcileError> {
        let observed_state = if action == LifecycleAction::Delete {
            server_state_to_storage(ServerState::Deleted).to_owned()
        } else {
            match self.provider.get_instance(&provider_id).await {
                Ok(instance) => {
                    server_state_to_storage(ServerState::from(instance.state)).to_owned()
                }
                Err(ProviderError::NotFound) if action == LifecycleAction::Delete => {
                    server_state_to_storage(ServerState::Deleted).to_owned()
                }
                Err(error) => return Err(ReconcileError::Provider(error)),
            }
        };
        self.store
            .update_operation(
                operation_id,
                OperationState::Succeeded,
                Some(&provider_operation_id),
                None,
                None,
            )
            .await?;
        self.store
            .update_resource(
                resource.id,
                resource.generation,
                &resource.desired_state,
                &observed_state,
                resource.generation,
                Some(&provider_id),
            )
            .await?;
        self.event(operation_id, resource.id, JournalEventKind::Succeeded);
        Ok(OperationState::Succeeded)
    }

    pub async fn reconcile_once(
        &self,
        operation_id: Uuid,
    ) -> Result<OperationState, ReconcileError> {
        let operation = self.store.get_operation(operation_id).await?;
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        let resource = self.store.get_resource(operation.resource_id).await?;
        if operation.state == OperationState::UnknownOutcome {
            if operation.provider_operation_id.is_some() {
                // Issue #611 (ASR-021 agent-control-plane-network-interruption):
                // an ACCEPTED create whose provider operation never produced a
                // provider resource (Accepted/Running with no
                // provider_resource_id) provably never executed — e.g. the
                // agent reported an unknown outcome because the committed
                // artifacts were missing after the control-channel
                // interruption. The create must be re-driven (the transfer
                // loop re-offers the missing artifact) instead of being parked
                // Running forever. Presence is never projected from transport
                // loss; the provider operation state is the authority.
                if !self.accepted_create_never_executed(operation_id).await? {
                    return self.observe_unknown(operation, resource).await;
                }
            } else {
                // Issue #609: the retry budget exhausted before any provider
                // operation identity existed (every dispatch was rejected as
                // retryable), so there is no provider operation to poll.
                //
                // Issue #610 (ASR-021 agent-control-plane-network-interruption):
                // when the create's durable agent command row is still
                // `pending`, the create was provably never accepted and never
                // executed — the budget only exhausts on pre-acceptance
                // rejections, and the agent journal carries no entry. Presence
                // inspection would then terminalize the absent create as
                // failed; the interruption contract requires the create to
                // converge ACTIVE after the agent returns, so the create falls
                // through to the re-drive below. `create_instance` rebuilds
                // the command with the current epoch and a fresh deadline, and
                // the deterministic command identity keeps the agent journal
                // idempotent — a journal entry that already exists rejects the
                // rebuilt fingerprint instead of re-executing, and its
                // terminal observation (replayed on reconnect) converges the
                // operation. Transport loss is never projected as absence.
                let create_pending = self
                    .store
                    .get_agent_command_by_operation(operation_id)
                    .await
                    .is_ok_and(|command| command.state == o3k_store::AgentCommandState::Pending);
                if !create_pending {
                    return self.observe_create_presence(operation, resource).await;
                }
            }
        }
        let request: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
            .map_err(|_| ReconcileError::InvalidIntent)?;
        self.store
            .update_operation(
                operation_id,
                OperationState::Running,
                operation.provider_operation_id.as_deref(),
                None,
                None,
            )
            .await?;
        self.event(operation_id, resource.id, JournalEventKind::ProviderStarted);
        test_fault_pause_ms("before-dispatch", "O3K_TEST_FAULT_PAUSE_BEFORE_DISPATCH_MS");
        match self.provider.create_instance(request).await {
            Ok(provider_operation) => {
                validate_provider_operation_owner(operation_id, &provider_operation)?;
                let provider_operation_id = provider_operation.provider_operation_id.to_string();
                match provider_operation.state {
                    ProviderOperationState::Succeeded => {
                        self.finish_create(
                            operation_id,
                            resource,
                            provider_operation_id,
                            provider_operation.provider_resource_id,
                        )
                        .await
                    }
                    ProviderOperationState::Accepted | ProviderOperationState::Running => {
                        if let Some(provider_resource_id) = provider_operation.provider_resource_id
                        {
                            return self
                                .finish_create(
                                    operation_id,
                                    resource,
                                    provider_operation_id,
                                    Some(provider_resource_id),
                                )
                                .await;
                        }
                        self.store
                            .update_operation(
                                operation_id,
                                OperationState::Running,
                                Some(&provider_operation_id),
                                None,
                                None,
                            )
                            .await?;
                        Ok(OperationState::Running)
                    }
                    ProviderOperationState::UnknownOutcome => {
                        self.store
                            .update_operation(
                                operation_id,
                                OperationState::UnknownOutcome,
                                Some(&provider_operation_id),
                                Some("unknown_outcome"),
                                None,
                            )
                            .await?;
                        self.event(operation_id, resource.id, JournalEventKind::RetryScheduled);
                        Ok(OperationState::UnknownOutcome)
                    }
                    ProviderOperationState::Retryable => {
                        self.retry_or_fail(operation_id, resource.id, ProviderError::Retryable)
                            .await
                    }
                    ProviderOperationState::Failed => {
                        self.store
                            .update_operation(
                                operation_id,
                                OperationState::Failed,
                                Some(&provider_operation_id),
                                Some("terminal"),
                                Some("provider operation failed"),
                            )
                            .await?;
                        self.event(operation_id, resource.id, JournalEventKind::Failed);
                        Ok(OperationState::Failed)
                    }
                }
            }
            Err(ProviderError::UnknownOutcome {
                operation_id: provider_operation_id,
            }) => {
                self.store
                    .update_operation(
                        operation_id,
                        OperationState::UnknownOutcome,
                        Some(&provider_operation_id.to_string()),
                        Some("unknown_outcome"),
                        None,
                    )
                    .await?;
                self.event(operation_id, resource.id, JournalEventKind::RetryScheduled);
                Ok(OperationState::UnknownOutcome)
            }
            Err(error @ ProviderError::Retryable) | Err(error @ ProviderError::StaleState) => {
                self.retry_or_fail(operation_id, resource.id, error).await
            }
            Err(ProviderError::NotFound) => {
                // No agent was registered to receive the create (an empty
                // registry while a preserved agent is still in reconnect
                // backoff after a control-plane restart). `selected_agent`
                // fails before anything is dispatched, so the command was
                // provably never delivered and no provider side effect can
                // exist. This is therefore not a terminal failure: the
                // operation stays `Running` without a provider operation
                // identity — the exact residue shape the create-convergence
                // sweep re-drives — until an agent registers and the create
                // dispatches (issue #87). The empty-registry condition must
                // not consume the retry budget: the sweep ticks every few
                // seconds while reconnect backoff can be 8s/16s/32s, so any
                // small budget would exhaust before the agent returns.
                Ok(OperationState::Running)
            }
            Err(error) => {
                self.store
                    .update_operation(
                        operation_id,
                        OperationState::Failed,
                        None,
                        Some("terminal"),
                        Some(&error.to_string()),
                    )
                    .await?;
                self.event(operation_id, resource.id, JournalEventKind::Failed);
                Ok(OperationState::Failed)
            }
        }
    }

    /// Issue #611 (ASR-021 agent-control-plane-network-interruption): decides
    /// whether an unknown-outcome create's provider operation is still
    /// Accepted/Running WITHOUT a provider resource — the create provably
    /// never executed (no instance exists), e.g. the agent reported an unknown
    /// outcome because the committed artifacts were missing after the control
    /// channel was interrupted mid-transfer. Such a create must be re-driven
    /// (the provider's transfer loop re-offers the missing artifact) instead
    /// of being parked Running forever with no recovery path.
    async fn accepted_create_never_executed(
        &self,
        operation_id: Uuid,
    ) -> Result<bool, ReconcileError> {
        let Some(provider_operation_id) = self
            .store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
        else {
            return Ok(false);
        };
        let provider_operation = self
            .provider
            .get_operation(
                Uuid::parse_str(&provider_operation_id)
                    .map_err(|_| ReconcileError::InvalidIntent)?,
            )
            .await?;
        Ok(matches!(
            provider_operation.state,
            o3k_provider::OperationState::Accepted | o3k_provider::OperationState::Running
        ) && provider_operation.provider_resource_id.is_none())
    }

    async fn observe_unknown(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
    ) -> Result<OperationState, ReconcileError> {
        let Some(provider_id) = operation.provider_operation_id.as_deref() else {
            return Err(ReconcileError::InvalidIntent);
        };
        let provider_operation = self
            .provider
            .get_operation(Uuid::parse_str(provider_id).map_err(|_| ReconcileError::InvalidIntent)?)
            .await?;
        validate_provider_operation_owner(operation.id, &provider_operation)?;
        self.event(operation.id, resource.id, JournalEventKind::UnknownObserved);
        match provider_operation.state {
            ProviderOperationState::UnknownOutcome => {
                if let Some(resource_id) = provider_operation.provider_resource_id {
                    return self
                        .finish_create(
                            operation.id,
                            resource,
                            provider_id.to_owned(),
                            Some(resource_id),
                        )
                        .await;
                }
                // The provider operation carries no resource identity: the
                // create may or may not have taken effect. Observe instance
                // presence by the server's durable identity before deciding
                // anything (SPEC-0021 unknown-outcome rules).
                self.observe_create_presence(operation, resource).await
            }
            ProviderOperationState::Succeeded => {
                self.finish_create(
                    operation.id,
                    resource,
                    provider_id.to_owned(),
                    provider_operation.provider_resource_id,
                )
                .await
            }
            ProviderOperationState::Accepted | ProviderOperationState::Running => {
                if let Some(resource_id) = provider_operation.provider_resource_id {
                    return self
                        .finish_create(
                            operation.id,
                            resource,
                            provider_id.to_owned(),
                            Some(resource_id),
                        )
                        .await;
                }
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::Running,
                        Some(provider_id),
                        None,
                        None,
                    )
                    .await?;
                Ok(OperationState::Running)
            }
            ProviderOperationState::Retryable => {
                self.retry_or_fail(operation.id, resource.id, ProviderError::Retryable)
                    .await
            }
            ProviderOperationState::Failed => {
                self.store
                    .update_operation(
                        operation.id,
                        OperationState::Failed,
                        Some(provider_id),
                        Some("terminal"),
                        Some("provider operation failed"),
                    )
                    .await?;
                self.event(operation.id, resource.id, JournalEventKind::Failed);
                Ok(OperationState::Failed)
            }
        }
    }

    /// Observes instance presence at the execution boundary for a create
    /// whose provider operation is unknown and carries no provider resource
    /// identity (SPEC-0021 "observe the selected provider before any create
    /// retry"). The agent is addressed by the durable placement identity
    /// recorded in the create intent, the O3K server id is the durable
    /// resource identity, and the inspection operation is deterministic per
    /// create operation so repeated triggers reuse an in-flight or terminal
    /// inspection instead of dispatching duplicates.
    ///
    /// - instance present → the create converged to success;
    /// - instance provably absent → the create never took effect, converges
    ///   to a terminal failure with the resource projected to error;
    /// - the inspection itself is unknown (dispatch timeout, transport loss,
    ///   unreachable agent) → the create stays `UnknownOutcome` and is
    ///   re-observed on the next trigger; transport loss is never projected
    ///   as absence.
    ///
    /// The agent executor settles commands inline in its message loop, so an
    /// inspection dispatched after a timed-out create observes the create's
    /// settled state, and the agent contract classifies only a provably
    /// absent domain as a terminal Failed/NotFound inspection — every other
    /// inspection failure stays unknown.
    async fn observe_create_presence(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
    ) -> Result<OperationState, ReconcileError> {
        let request: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let Some(provider_id) = request.placement_provider_id.as_deref() else {
            // No execution agent is recorded, so presence cannot be observed
            // by durable identity; the unknown outcome is preserved.
            return Ok(OperationState::UnknownOutcome);
        };
        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{}", operation.id).as_bytes(),
        );
        let idempotency_key = format!("o3k-inspect-create-{}", operation.id);
        match self.store.get_operation(inspect_operation_id).await {
            Ok(inspect) => match inspect.state {
                OperationState::Succeeded => {
                    let Some(provider_resource_id) =
                        self.provider_resource_id_for(&resource).await?
                    else {
                        return Ok(OperationState::UnknownOutcome);
                    };
                    return self
                        .finish_create(
                            operation.id,
                            resource,
                            operation
                                .provider_operation_id
                                .unwrap_or_else(|| inspect_operation_id.to_string()),
                            Some(provider_resource_id),
                        )
                        .await;
                }
                OperationState::Failed
                    if inspect.error_category.as_deref() == Some("not_found") =>
                {
                    return self.converge_absent_create(operation, resource).await;
                }
                OperationState::UnknownOutcome | OperationState::Retryable => {}
                _ => {
                    // The inspection never reached a durable terminal state
                    // (Pending/Running: a crash between persist and dispatch,
                    // a lost agent acceptance, or a write race), or ended
                    // terminally for a reason other than absence (ambiguous).
                    // The durable agent command record is the authoritative
                    // terminal evidence when the agent's update overtook the
                    // reconciler's in-flight write.
                    if let Ok(command) = self
                        .store
                        .get_agent_command_by_operation(inspect_operation_id)
                        .await
                    {
                        match command.state {
                            o3k_store::AgentCommandState::Succeeded => {
                                let Some(provider_resource_id) =
                                    self.provider_resource_id_for(&resource).await?
                                else {
                                    return Ok(OperationState::UnknownOutcome);
                                };
                                let provider_operation_id = command
                                    .provider_operation_id
                                    .clone()
                                    .unwrap_or_else(|| inspect_operation_id.to_string());
                                self.store
                                    .update_operation(
                                        inspect_operation_id,
                                        OperationState::Succeeded,
                                        Some(&provider_operation_id),
                                        None,
                                        None,
                                    )
                                    .await?;
                                return self
                                    .finish_create(
                                        operation.id,
                                        resource,
                                        operation
                                            .provider_operation_id
                                            .unwrap_or_else(|| inspect_operation_id.to_string()),
                                        Some(provider_resource_id),
                                    )
                                    .await;
                            }
                            o3k_store::AgentCommandState::Failed => {
                                // The real agent classifies an absent domain
                                // as a terminal Failed inspection; every other
                                // inspection failure is reported as unknown,
                                // so a terminal failed command record proves
                                // absence.
                                let provider_operation_id = command
                                    .provider_operation_id
                                    .clone()
                                    .unwrap_or_else(|| inspect_operation_id.to_string());
                                self.store
                                    .update_operation(
                                        inspect_operation_id,
                                        OperationState::Failed,
                                        Some(&provider_operation_id),
                                        Some("not_found"),
                                        Some("presence inspection: instance is absent"),
                                    )
                                    .await?;
                                return self.converge_absent_create(operation, resource).await;
                            }
                            _ => {}
                        }
                    }
                    // Without terminal evidence, a Pending record (a crash
                    // between persist and dispatch, or a lost dispatch
                    // response) is re-observed by re-dispatching the
                    // read-only inspection (the provider dedups by the
                    // deterministic operation identity). A Running record is
                    // already accepted by the agent, whose journal guarantees
                    // delivery of the terminal update, so it is never
                    // re-dispatched; a terminal non-absence classification is
                    // ambiguous and is also never re-dispatched.
                    if !matches!(inspect.state, OperationState::Pending) {
                        return Ok(OperationState::UnknownOutcome);
                    }
                }
            },
            Err(_) => {
                // No inspection record yet: persist the intent before
                // dispatch so a terminal agent update can never arrive for an
                // unknown operation.
                self.store
                    .insert_operation(&OperationRecord {
                        id: inspect_operation_id,
                        resource_id: resource.id,
                        kind: "inspect".to_owned(),
                        state: OperationState::Pending,
                        provider_operation_id: None,
                        error_category: None,
                        error_message: None,
                    })
                    .await?;
            }
        }
        // If a provider reference was recorded meanwhile (lost-update window
        // where the agent completed the create), pass it so the provider
        // validates the identity instead of rejecting an empty id; an empty
        // id keeps the inspection keyed on the server's durable identity.
        let known_provider_resource_id = self.provider_resource_id_for(&resource).await?;
        // The inspection dispatch itself persists the durable command row
        // (with the REAL payload and the selected agent's identity) before
        // sending anything over the wire, so a terminal observation can never
        // arrive for an unbound operation after a control-plane crash. A
        // pre-inserted placeholder row would be reused by the provider's
        // `dispatch_recorded` instead, and its empty payload decodes into an
        // action-less command that the agent rejects — stranding every
        // unknown-outcome create in REQUESTED forever (issue #575 real-host
        // finding; introduced by 13d1d65).
        let result = self
            .provider
            .inspect_instance(
                provider_id,
                &resource.id.to_string(),
                known_provider_resource_id.as_deref().unwrap_or(""),
                inspect_operation_id,
                &idempotency_key,
            )
            .await;
        match result {
            Ok(inspect_operation) => {
                validate_provider_operation_owner(inspect_operation_id, &inspect_operation)?;
                match inspect_operation.state {
                    ProviderOperationState::Succeeded => {
                        let Some(provider_resource_id) = inspect_operation.provider_resource_id
                        else {
                            // A success without a resource identity cannot be
                            // converged; keep the operation unknown rather
                            // than inventing a provider identity.
                            self.store
                                .update_operation(
                                    inspect_operation_id,
                                    OperationState::Running,
                                    Some(&inspect_operation.provider_operation_id.to_string()),
                                    None,
                                    None,
                                )
                                .await?;
                            return Ok(OperationState::UnknownOutcome);
                        };
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::Succeeded,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                None,
                                None,
                            )
                            .await?;
                        self.finish_create(
                            operation.id,
                            resource,
                            operation.provider_operation_id.unwrap_or_else(|| {
                                inspect_operation.provider_operation_id.to_string()
                            }),
                            Some(provider_resource_id),
                        )
                        .await
                    }
                    ProviderOperationState::Failed
                        if inspect_operation.error_category
                            == Some(o3k_provider::ErrorCategory::NotFound) =>
                    {
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::Failed,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                Some("not_found"),
                                Some("presence inspection: instance is absent"),
                            )
                            .await?;
                        self.converge_absent_create(operation, resource).await
                    }
                    ProviderOperationState::Failed => {
                        // A terminal inspection failure other than absence is
                        // ambiguous (the instance may still exist); preserve
                        // the unknown outcome.
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::Failed,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                inspect_operation
                                    .error_category
                                    .map(provider_error_category_name),
                                Some("presence inspection failed"),
                            )
                            .await?;
                        Ok(OperationState::UnknownOutcome)
                    }
                    ProviderOperationState::Retryable => {
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::Retryable,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                Some("retryable"),
                                None,
                            )
                            .await?;
                        Ok(OperationState::UnknownOutcome)
                    }
                    ProviderOperationState::Accepted | ProviderOperationState::Running => {
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::Running,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                None,
                                None,
                            )
                            .await?;
                        Ok(OperationState::UnknownOutcome)
                    }
                    ProviderOperationState::UnknownOutcome => {
                        self.store
                            .update_operation(
                                inspect_operation_id,
                                OperationState::UnknownOutcome,
                                Some(&inspect_operation.provider_operation_id.to_string()),
                                Some("unknown_outcome"),
                                None,
                            )
                            .await?;
                        Ok(OperationState::UnknownOutcome)
                    }
                }
            }
            Err(error) => {
                // Transport loss, timeout, or an unreachable agent: the
                // inspection outcome is unknown. Never project absence from a
                // failed dispatch; the record stays re-observable.
                self.store
                    .update_operation(
                        inspect_operation_id,
                        OperationState::UnknownOutcome,
                        None,
                        Some("unknown_outcome"),
                        Some(&error.to_string()),
                    )
                    .await?;
                Ok(OperationState::UnknownOutcome)
            }
        }
    }

    /// Converges a create whose presence inspection provably found no
    /// instance: the create never took effect, so the operation is terminal
    /// Failed and the resource projects a visible error state (mirror of the
    /// agent-failed create projection), which is what makes clients polling
    /// the server stop waiting.
    async fn converge_absent_create(
        &self,
        operation: OperationRecord,
        resource: ResourceRecord,
    ) -> Result<OperationState, ReconcileError> {
        self.store
            .update_operation(
                operation.id,
                OperationState::Failed,
                operation.provider_operation_id.as_deref(),
                Some("not_found"),
                Some("presence inspection: create never took effect; instance is absent"),
            )
            .await?;
        self.store
            .update_resource(
                resource.id,
                resource.generation,
                &resource.desired_state,
                server_state_to_storage(ServerState::Error),
                resource.generation,
                resource.provider_id.as_deref(),
            )
            .await?;
        self.event(operation.id, resource.id, JournalEventKind::Failed);
        Ok(OperationState::Failed)
    }

    /// Resolves the provider resource identity recorded for the server by
    /// either execution-boundary reference name.
    async fn provider_resource_id_for(
        &self,
        resource: &ResourceRecord,
    ) -> Result<Option<String>, ReconcileError> {
        for name in ["compute", "compute-agent"] {
            match self.store.get_provider_reference(resource.id, name).await {
                Ok(reference) => return Ok(Some(reference.provider_resource_id)),
                Err(StoreError::ProviderReferenceNotFound) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    async fn finish_create(
        &self,
        operation_id: Uuid,
        resource: ResourceRecord,
        provider_operation_id: String,
        provider_resource_id: Option<String>,
    ) -> Result<OperationState, ReconcileError> {
        let provider_resource_id = provider_resource_id.ok_or(ReconcileError::InvalidIntent)?;
        let instance = self.provider.get_instance(&provider_resource_id).await?;
        let observed_state = server_state_to_storage(ServerState::from(instance.state));
        if instance.state == o3k_provider::InstanceState::Error {
            self.store
                .update_operation(
                    operation_id,
                    OperationState::Failed,
                    Some(&provider_operation_id),
                    Some("terminal"),
                    Some("provider instance entered an error state"),
                )
                .await?;
            self.store
                .update_resource(
                    resource.id,
                    resource.generation,
                    &resource.desired_state,
                    observed_state,
                    resource.generation,
                    Some(&provider_resource_id),
                )
                .await?;
            self.event(operation_id, resource.id, JournalEventKind::Failed);
            return Ok(OperationState::Failed);
        }
        if instance.state == o3k_provider::InstanceState::Running {
            return self
                .finish(
                    operation_id,
                    resource,
                    provider_operation_id,
                    Some(provider_resource_id),
                )
                .await;
        }
        if instance.state == o3k_provider::InstanceState::Creating {
            // The create is still in flight at the execution boundary; keep
            // the Running wait with the observed projection.
            self.store
                .update_operation(
                    operation_id,
                    OperationState::Running,
                    Some(&provider_operation_id),
                    None,
                    None,
                )
                .await?;
            self.store
                .update_resource(
                    resource.id,
                    resource.generation,
                    &resource.desired_state,
                    observed_state,
                    resource.generation,
                    Some(&provider_resource_id),
                )
                .await?;
            self.event(operation_id, resource.id, JournalEventKind::ProviderStarted);
            return Ok(OperationState::Running);
        }
        // A present instance in a settled non-Running state (the issue-87
        // crash-between-define-and-start adoption: the domain was defined but
        // never started, so the instance is observed Stopped) is a converged
        // create: presence observation treats any present instance as success,
        // so the adoption terminalizes Succeeded and the server is projected
        // with its observed state (SHUTOFF) — never left Running without a
        // transition path. `reconcile_once` short-circuits on Succeeded, so
        // there is exactly one terminal transition.
        self.store
            .update_operation(
                operation_id,
                OperationState::Succeeded,
                Some(&provider_operation_id),
                None,
                None,
            )
            .await?;
        self.store
            .update_resource(
                resource.id,
                resource.generation,
                &resource.desired_state,
                observed_state,
                resource.generation,
                Some(&provider_resource_id),
            )
            .await?;
        self.event(operation_id, resource.id, JournalEventKind::Succeeded);
        Ok(OperationState::Succeeded)
    }

    async fn finish(
        &self,
        operation_id: Uuid,
        resource: ResourceRecord,
        provider_operation_id: String,
        provider_resource_id: Option<String>,
    ) -> Result<OperationState, ReconcileError> {
        // A concurrent driver (an idempotent retry whose show path re-drives
        // a create while the synchronous pass is still dispatching) can reach
        // `finish` with the operation already converged: both dispatches
        // raced, the provider returned the same deterministic identity, and
        // the first driver already attached the reference and projected the
        // terminal state. Short-circuit on that first-writer outcome instead
        // of re-attaching the reference (unique violation) and clobbering the
        // resource generation (stale generation) — the exact
        // "duplicate reference attach / stale generation" race the create
        // convergence driver documents above.
        let current = self.store.get_operation(operation_id).await?;
        if current.state == OperationState::Succeeded
            && current.provider_operation_id.as_deref() == Some(provider_operation_id.as_str())
        {
            return Ok(OperationState::Succeeded);
        }
        if let Some(provider_resource_id) = provider_resource_id.as_deref() {
            let reference = ProviderReference {
                resource_id: resource.id,
                provider_name: "compute".to_owned(),
                provider_resource_id: provider_resource_id.to_owned(),
            };
            match self
                .store
                .get_provider_reference(resource.id, "compute")
                .await
            {
                Ok(existing) if existing.provider_resource_id == provider_resource_id => {}
                Ok(_) => return Err(StoreError::ProviderReferenceAlreadyExists.into()),
                Err(StoreError::ProviderReferenceNotFound) => {
                    match self.store.attach_provider_reference(&reference).await {
                        Ok(()) => {}
                        // A concurrent driver attached between the read above
                        // and this insert (the same read-then-attach window
                        // the observation path documents). Converge when the
                        // attached identity matches; a different identity is a
                        // genuine drift and stays an error.
                        Err(StoreError::ProviderReferenceAlreadyExists) => {
                            let existing = self
                                .store
                                .get_provider_reference(resource.id, "compute")
                                .await?;
                            if existing.provider_resource_id != provider_resource_id {
                                return Err(StoreError::ProviderReferenceAlreadyExists.into());
                            }
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.store
            .update_operation(
                operation_id,
                OperationState::Succeeded,
                Some(&provider_operation_id),
                None,
                None,
            )
            .await?;
        // The resource projection is a generation-guarded CAS, and a
        // concurrent driver's finish can land between this driver's re-read
        // and its update. Converge instead of erroring: re-read, short-circuit
        // on the already-projected terminal state, and retry the CAS on a
        // bounded number of stale snapshots (no sleeps; the re-read sees the
        // other driver's committed projection).
        let mut attempts = 0;
        loop {
            let fresh = self.store.get_resource(resource.id).await?;
            if fresh.observed_state == server_state_to_storage(ServerState::Active)
                && fresh.provider_id.as_deref() == provider_resource_id.as_deref()
            {
                return Ok(OperationState::Succeeded);
            }
            match self
                .store
                .update_resource(
                    fresh.id,
                    fresh.generation,
                    &fresh.desired_state,
                    server_state_to_storage(ServerState::Active),
                    fresh.generation,
                    provider_resource_id.as_deref(),
                )
                .await
            {
                Ok(_) => break,
                Err(StoreError::StaleGeneration)
                    if attempts < FINISH_RESOURCE_UPDATE_MAX_ATTEMPTS =>
                {
                    attempts += 1;
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.event(operation_id, resource.id, JournalEventKind::Succeeded);
        Ok(OperationState::Succeeded)
    }

    async fn retry_or_fail(
        &self,
        operation_id: Uuid,
        resource_id: Uuid,
        error: ProviderError,
    ) -> Result<OperationState, ReconcileError> {
        let attempts = self.store.increment_operation_retry(operation_id).await?;
        if attempts >= self.max_attempts {
            // Issue #609 (ASR-021 agent-control-plane-network-interruption):
            // a retryable provider outcome is by definition not a
            // definitively-known failure, so exhausting the retry budget must
            // never terminalize the operation. The budget only bounds
            // dispatching: the operation stays `UnknownOutcome` (keeping the
            // error for diagnosis) and the convergence sweeps keep observing
            // and re-driving it, so a transient interruption that resolves
            // still converges after recovery. Presence inspection resolves
            // genuinely-absent cases; a reconnected agent's terminal update
            // can also still arrive through the event stream.
            self.store
                .update_operation(
                    operation_id,
                    OperationState::UnknownOutcome,
                    None,
                    Some("retry_exhausted"),
                    Some(&error.to_string()),
                )
                .await?;
            self.event(operation_id, resource_id, JournalEventKind::UnknownObserved);
            Ok(OperationState::UnknownOutcome)
        } else {
            self.store
                .update_operation(
                    operation_id,
                    OperationState::Retryable,
                    None,
                    Some("retryable"),
                    None,
                )
                .await?;
            self.event(operation_id, resource_id, JournalEventKind::RetryScheduled);
            Ok(OperationState::Retryable)
        }
    }

    fn event(&self, operation_id: Uuid, resource_id: Uuid, kind: JournalEventKind) {
        if let Ok(mut events) = self.events.lock() {
            events.push(JournalEvent {
                operation_id,
                resource_id,
                kind,
            });
        }
    }
}


// ─── Internal helpers ─────────────────────────────────────
fn agent_error_category(
    category: Option<AgentErrorCategory>,
) -> Result<&'static str, ReconcileError> {
    // A terminal failure must carry a classified category: an unspecified one
    // means the update is not complete evidence and is rejected.
    let category = category.ok_or(ReconcileError::InvalidIntent)?;
    Ok(match category {
        AgentErrorCategory::InvalidRequest => "invalid_request",
        AgentErrorCategory::Unauthenticated => "unauthenticated",
        AgentErrorCategory::Unauthorized => "unauthorized",
        AgentErrorCategory::Conflict => "conflict",
        AgentErrorCategory::Capacity => "capacity",
        AgentErrorCategory::NotFound => "not_found",
        AgentErrorCategory::Retryable => "retryable",
        AgentErrorCategory::UnknownOutcome => "unknown_outcome",
        AgentErrorCategory::Terminal => "terminal",
    })
}

fn provider_error_category_name(category: o3k_provider::ErrorCategory) -> &'static str {
    match category {
        o3k_provider::ErrorCategory::InvalidRequest => "invalid_request",
        o3k_provider::ErrorCategory::NotFound => "not_found",
        o3k_provider::ErrorCategory::Conflict => "conflict",
        o3k_provider::ErrorCategory::Capacity => "capacity",
        o3k_provider::ErrorCategory::Retryable => "retryable",
        o3k_provider::ErrorCategory::UnknownOutcome => "unknown_outcome",
        o3k_provider::ErrorCategory::Terminal => "terminal",
    }
}

/// Maximum length of a persisted agent failure reason. The durable record
/// stays bounded even if an agent message grows unexpectedly.
const MAX_AGENT_FAILURE_MESSAGE_LEN: usize = 240;

/// Maximum stale-snapshot retries for the terminal resource projection in
/// [`OperationJournal::finish`]. A concurrent driver's finish can commit the
/// same projection between this driver's re-read and its generation-guarded
/// CAS; each retry re-reads the committed state and short-circuits on the
/// converged projection, so the bound is never exercised in a healthy run.
const FINISH_RESOURCE_UPDATE_MAX_ATTEMPTS: u32 = 3;

/// Bounds and sanitizes the agent-reported failure reason for durable storage
/// and operator logs. The agent contract already redacts secrets; the control
/// plane additionally neutralizes control characters, truncates to a bounded
/// length, and falls back to a generic reason when the agent supplied nothing
/// usable.
fn bounded_agent_failure_message(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "agent operation failed".to_owned();
    }
    let bounded: String = trimmed
        .chars()
        .take(MAX_AGENT_FAILURE_MESSAGE_LEN)
        .collect();
    if trimmed.chars().count() > MAX_AGENT_FAILURE_MESSAGE_LEN {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn validate_provider_operation_owner(
    operation_id: Uuid,
    provider_operation: &o3k_provider::Operation,
) -> Result<(), ReconcileError> {
    if provider_operation.o3k_operation_id != operation_id {
        return Err(ReconcileError::InvalidIntent);
    }
    Ok(())
}

/// Deterministic fresh command identity for a #575 delete re-drive, tied to
/// the durable lifecycle operation so repeated reconciliations re-dispatch
/// the SAME command and old-stream evidence cannot complete it.
fn delete_redrive_operation_id(operation_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:lifecycle-delete-redrive:{operation_id}").as_bytes(),
    )
}

fn validate_lifecycle_provider_operation_owner(
    operation_id: Uuid,
    action: LifecycleAction,
    provider_operation: &o3k_provider::Operation,
) -> Result<(), ReconcileError> {
    if provider_operation.o3k_operation_id == operation_id {
        return Ok(());
    }
    if action == LifecycleAction::Delete
        && provider_operation.o3k_operation_id == delete_redrive_operation_id(operation_id)
    {
        return Ok(());
    }
    Err(ReconcileError::InvalidIntent)
}

fn valid_agent_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}


// ─── Tests ───────────────────────────────────────────────-
#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider::{FailureInjection, FakeComputeProvider};
    use o3k_store::testkit::TestStore;
    use std::path::PathBuf;

    #[test]
    fn test_fault_pause_guard_accepts_only_positive_numeric_durations() {
        assert_eq!(test_fault_pause_ms_value(None), None);
        assert_eq!(test_fault_pause_ms_value(Some(String::new())), None);
        assert_eq!(test_fault_pause_ms_value(Some("0".to_owned())), None);
        assert_eq!(test_fault_pause_ms_value(Some("abc".to_owned())), None);
        assert_eq!(test_fault_pause_ms_value(Some("250".to_owned())), Some(250));
    }

    fn request() -> CreateInstanceRequest {
        CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project".to_owned(),
            name: "journal-test".to_owned(),
            vcpus: 1,
            memory_mib: 128,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: None,
            key_name: None,
            keypair_id: None,
            network_ids: Vec::new(),
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "journal-test-key".to_owned(),
        }
    }

    async fn journal(
        label: &str,
        max_attempts: u8,
    ) -> Result<
        (
            OperationJournal<TestStore, FakeComputeProvider>,
            Arc<TestStore>,
            Arc<FakeComputeProvider>,
        ),
        ReconcileError,
    > {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        Ok((
            OperationJournal::new(store.clone(), provider.clone(), max_attempts),
            store,
            provider,
        ))
    }

    async fn bind_observation_command(
        store: &TestStore,
        operation_id: Uuid,
        resource_id: Uuid,
        agent_id: &str,
        agent_epoch: &str,
    ) -> Result<(), ReconcileError> {
        let command_id = format!("observation-command-{operation_id}");
        bind_command(
            store,
            command_id.clone(),
            operation_id,
            resource_id,
            agent_id,
            agent_epoch,
        )
        .await?;
        store
            .update_agent_command(&command_id, AgentCommandState::Succeeded, 1, 1, None, None)
            .await?;
        Ok(())
    }

    async fn bind_command(
        store: &TestStore,
        command_id: String,
        operation_id: Uuid,
        resource_id: Uuid,
        agent_id: &str,
        agent_epoch: &str,
    ) -> Result<(), ReconcileError> {
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id,
                idempotency_key: format!("observation-{operation_id}"),
                operation_id,
                resource_id,
                agent_id: agent_id.to_owned(),
                agent_epoch: agent_epoch.to_owned(),
                payload_fingerprint_sha256: "f".repeat(64),
                payload: Vec::new(),
                state: AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: None,
                provider_resource_id: None,
            })
            .await?;
        Ok(())
    }

    /// Minimal in-memory node registry used to simulate agent registration and
    /// re-registration. A re-registration replaces the stored epoch, mirroring
    /// `NodeRegistry::register` in o3k-compute-agent.
    #[derive(Clone, Default)]
    struct TestAgentRegistry {
        nodes: Arc<tokio::sync::RwLock<HashMap<String, o3k_provider::AgentNodeSnapshot>>>,
    }

    struct TestAgentEpochLease {
        _nodes: tokio::sync::OwnedRwLockReadGuard<HashMap<String, o3k_provider::AgentNodeSnapshot>>,
    }

    impl o3k_provider::AgentEpochLease for TestAgentEpochLease {}

    #[async_trait::async_trait]
    impl o3k_provider::AgentNodeRegistry for TestAgentRegistry {
        async fn all(&self) -> Vec<o3k_provider::AgentNodeSnapshot> {
            self.nodes.read().await.values().cloned().collect()
        }

        async fn snapshot(&self, agent_id: &str) -> Option<o3k_provider::AgentNodeSnapshot> {
            self.nodes.read().await.get(agent_id).cloned()
        }

        async fn lease_current_epoch(
            &self,
            agent_id: &str,
            agent_epoch: &str,
        ) -> Option<Box<dyn o3k_provider::AgentEpochLease>> {
            let nodes = self.nodes.clone().read_owned().await;
            if nodes
                .get(agent_id)
                .is_some_and(|node| node.agent_epoch == agent_epoch)
            {
                Some(Box::new(TestAgentEpochLease { _nodes: nodes }))
            } else {
                None
            }
        }

        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<o3k_provider::AgentEvent> {
            let (_, receiver) = tokio::sync::broadcast::channel(1);
            receiver
        }
    }

    impl TestAgentRegistry {
        /// Registers (or re-registers) the agent, replacing the stored epoch.
        async fn register(&self, agent_id: &str, agent_epoch: &str) {
            self.nodes.write().await.insert(
                agent_id.to_owned(),
                o3k_provider::AgentNodeSnapshot {
                    agent_id: agent_id.to_owned(),
                    agent_epoch: agent_epoch.to_owned(),
                    availability: o3k_provider::AgentAvailability::Available,
                    administrative_state: o3k_provider::AgentAdministrativeState::Enabled,
                    capabilities: o3k_provider::AgentCapabilities {
                        agent_provider_name: "o3k-compute".to_owned(),
                        agent_provider_version: "test".to_owned(),
                        max_vcpus: 1,
                        max_memory_mib: 128,
                        max_disk_gb: 1,
                        lifecycle_actions: Vec::new(),
                        console_log: false,
                        flags: Vec::new(),
                    },
                },
            );
        }
    }

    struct ForeignOperationProvider {
        inner: FakeComputeProvider,
    }

    impl ForeignOperationProvider {
        fn new() -> Self {
            Self {
                inner: FakeComputeProvider::new(),
            }
        }

        fn foreign(mut operation: o3k_provider::Operation) -> o3k_provider::Operation {
            operation.o3k_operation_id = Uuid::now_v7();
            operation
        }
    }

    #[async_trait::async_trait]
    impl o3k_provider::ComputeProvider for ForeignOperationProvider {
        async fn capabilities(
            &self,
        ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.create_instance(request).await.map(Self::foreign)
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.delete_instance(request).await.map(Self::foreign)
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: o3k_provider::InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
                .map(Self::foreign)
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .get_operation(provider_operation_id)
                .await
                .map(Self::foreign)
        }
    }

    #[tokio::test]
    async fn intent_and_provider_success_are_durable() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("success", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn create_rejects_provider_operation_owned_by_another_request()
    -> Result<(), ReconcileError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-foreign-create-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(ForeignOperationProvider::new());
        let journal = OperationJournal::new(store.clone(), provider, 2);
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;

        assert!(matches!(
            journal.reconcile_once(operation_id).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_recovery_rejects_foreign_provider_operation() -> Result<(), ReconcileError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-foreign-unknown-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(ForeignOperationProvider::new());
        provider.inner.set_failure(FailureInjection::Timeout)?;
        let journal = OperationJournal::new(store.clone(), provider, 2);
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert!(matches!(
            journal.reconcile_once(operation_id).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_failed_create_update_marks_resource_error_and_replays_safely()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-create-failed", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            format!("command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let update = failed_update(
            &operation_id.to_string(),
            &request.o3k_server_id.to_string(),
            "agent-1",
            "epoch-1",
            "gateway preparation failed",
        )?;
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        // A replayed delivery of the same terminal update stays Failed and
        // keeps the ERROR projection without reviving the operation.
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Failed
        );
        Ok(())
    }

    #[test]
    fn agent_failure_reason_is_sanitized_bounded_and_fallback_safe() {
        // Redaction contract: control characters never reach durable storage
        // or operator logs, so a crafted payload cannot forge log lines.
        assert_eq!(
            bounded_agent_failure_message("gateway preparation failed:\n\tforeign interface\r\n"),
            "gateway preparation failed:  foreign interface"
        );
        // Truncation: oversized reasons are bounded with an explicit marker.
        let long = "x".repeat(MAX_AGENT_FAILURE_MESSAGE_LEN + 100);
        let bounded = bounded_agent_failure_message(&long);
        assert_eq!(bounded.len(), MAX_AGENT_FAILURE_MESSAGE_LEN + 3);
        assert!(bounded.ends_with("..."));
        // Fallback: an empty or whitespace-only reason stays actionable.
        assert_eq!(bounded_agent_failure_message(""), "agent operation failed");
        assert_eq!(
            bounded_agent_failure_message("  \n\t "),
            "agent operation failed"
        );
    }

    #[tokio::test]
    async fn agent_failed_update_persists_the_bounded_agent_reason() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-failed-reason", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            format!("command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let update = failed_update(
            &operation_id.to_string(),
            &request.o3k_server_id.to_string(),
            "agent-1",
            "epoch-1",
            "gateway preparation failed:\nexisting interface is foreign",
        )?;
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_operation(operation_id)
                .await?
                .error_message
                .as_deref(),
            Some("gateway preparation failed: existing interface is foreign")
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_success_is_durable_and_idempotent() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-success", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            format!("command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let update = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some("agent-domain-1".to_owned()),
        };
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "REQUESTED"
        );
        assert_eq!(
            store
                .get_provider_reference(request.o3k_server_id, "compute-agent")
                .await?
                .provider_resource_id,
            "agent-domain-1"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_update_rejects_agent_namespace_provider_identity_drift()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-reference-identity", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let command_id = format!("command-{operation_id}");
        bind_command(
            &store,
            command_id.clone(),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        store
            .attach_provider_reference(&ProviderReference {
                resource_id: request.o3k_server_id,
                provider_name: "agent".to_owned(),
                provider_resource_id: "domain-established".to_owned(),
            })
            .await?;
        let update = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some("domain-conflict".to_owned()),
        };
        assert!(matches!(
            journal.apply_agent_update(&update).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_agent_command(&command_id).await?.state,
            AgentCommandState::Pending
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Pending
        );
        assert_eq!(
            store
                .get_provider_reference(request.o3k_server_id, "agent")
                .await?
                .provider_resource_id,
            "domain-established"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_evidence_rejects_foreign_and_stale_updates() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-evidence-fence", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            format!("command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-a",
            "epoch-a",
        )
        .await?;
        let succeeded = AgentOperationUpdate {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            operation_sequence: 2,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some("domain-a".to_owned()),
        };
        assert_eq!(
            journal.apply_agent_update(&succeeded).await?,
            OperationState::Succeeded
        );
        let stale = AgentOperationUpdate {
            operation_sequence: 1,
            state: AgentOperationState::Running,
            error_category: None,
            redacted_message: None,
            provider_resource_id: None,
            ..succeeded.clone()
        };
        assert!(matches!(
            journal.apply_agent_update(&stale).await,
            Err(ReconcileError::InvalidIntent)
        ));
        let foreign = AgentOperationUpdate {
            agent_id: "agent-b".to_owned(),
            agent_epoch: "epoch-b".to_owned(),
            operation_sequence: 3,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Terminal),
            redacted_message: None,
            ..succeeded.clone()
        };
        assert!(matches!(
            journal.apply_agent_update(&foreign).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        Ok(())
    }

    /// Without a registry the fence keeps the strict first-evidence anchor: a
    /// same-agent epoch change is indistinguishable from a dead stream and
    /// must stay rejected. This pins the no-registry fallback so the
    /// registry-aware fence (issue #87) cannot weaken it.
    #[tokio::test]
    async fn agent_evidence_epoch_change_is_rejected_without_registry() -> Result<(), ReconcileError>
    {
        let (journal, store, _) = journal("agent-fence-no-registry", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            "command-1".to_owned(),
            operation_id,
            request.o3k_server_id,
            "agent-a",
            "epoch-a",
        )
        .await?;
        let accepted = AgentCommandAccepted {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id,
            state: AgentOperationState::Accepted,
            operation_sequence: 1,
        };
        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );
        let replayed_under_other_epoch = AgentCommandAccepted {
            agent_epoch: "epoch-b".to_owned(),
            ..accepted.clone()
        };
        assert!(matches!(
            journal
                .apply_agent_acceptance(&replayed_under_other_epoch)
                .await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );
        Ok(())
    }

    /// Issue #87 regression: after a compute-agent crash and restart the agent
    /// re-registers with a fresh per-connection epoch and replays its durable
    /// journal for the in-flight operation. The replay is evidence from the
    /// agent's *current* registered epoch and must be applied — not rejected
    /// because the pre-crash acceptance was anchored to the old epoch.
    #[tokio::test]
    async fn agent_replay_after_reregistration_applies_unknown_outcome()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-reregister-replay", 2).await?;
        let registry = TestAgentRegistry::default();
        registry.register("agent-a", "epoch-a").await;
        let journal = journal.with_agent_registry(Arc::new(registry.clone()));

        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            "command-1".to_owned(),
            operation_id,
            request.o3k_server_id,
            "agent-a",
            "epoch-b",
        )
        .await?;
        let accepted = AgentCommandAccepted {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id,
            state: AgentOperationState::Accepted,
            operation_sequence: 1,
        };
        // Pre-crash: the control plane records the acceptance under epoch-a.
        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );

        // The agent crashes and re-registers; the registry now stores epoch-b.
        registry.register("agent-a", "epoch-b").await;

        // Post-restart replay of the same acceptance under the new epoch must
        // stay idempotent, not be fenced as a foreign stream.
        let replayed_accepted = AgentCommandAccepted {
            agent_epoch: "epoch-b".to_owned(),
            ..accepted.clone()
        };
        assert_eq!(
            journal.apply_agent_acceptance(&replayed_accepted).await?,
            OperationState::Running
        );

        // The journal replay then delivers the crashed create's UnknownOutcome
        // and the operation must converge out of Running.
        let update = AgentOperationUpdate {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-b".to_owned(),
            operation_sequence: 2,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::UnknownOutcome,
            error_category: None,
            redacted_message: None,
            provider_resource_id: None,
        };
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        Ok(())
    }

    /// ASR-015 real-host regression: the agent persists terminal execution
    /// before its E1 stream is lost, then replays that result under E2.  A
    /// preceding terminal observation may win the two-consumer race and make
    /// the operation Succeeded first; the later operation update must still
    /// terminalize the matching durable command instead of leaving it
    /// recoverable forever.
    #[tokio::test]
    async fn terminal_replay_after_reregistration_converges_command_when_operation_is_terminal()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-terminal-reregister-replay", 2).await?;
        let registry = TestAgentRegistry::default();
        registry.register("agent-a", "epoch-a").await;
        let journal = journal.with_agent_registry(Arc::new(registry.clone()));

        let request = request();
        journal.begin_create("project", &request).await?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        store
            .update_resource(
                request.o3k_server_id,
                resource.generation,
                &resource.desired_state,
                server_state_to_storage(ServerState::Active),
                resource.generation,
                Some("domain-a"),
            )
            .await?;
        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(request.o3k_server_id, operation_id, LifecycleAction::Reboot)
            .await?;
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id: "command-1".to_owned(),
                idempotency_key: format!("asr-015-{operation_id}"),
                operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-a".to_owned(),
                agent_epoch: "epoch-a".to_owned(),
                payload_fingerprint_sha256: "f".repeat(64),
                payload: Vec::new(),
                state: AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: Some(operation_id.to_string()),
                provider_resource_id: None,
            })
            .await?;
        let accepted = AgentCommandAccepted {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id,
            state: AgentOperationState::Accepted,
            operation_sequence: 1,
        };
        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );
        let accepted_command = store.get_agent_command("command-1").await?;
        assert_eq!(accepted_command.state, AgentCommandState::Accepted);

        registry.register("agent-a", "epoch-b").await;
        let stale = AgentOperationUpdate {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            operation_sequence: 2,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some("domain-a".to_owned()),
        };
        let operation_before_stale = store.get_operation(operation_id).await?;
        let resource_before_stale = store.get_resource(request.o3k_server_id).await?;
        let command_before_stale = store.get_agent_command("command-1").await?;
        let evidence_before_stale = journal
            .agent_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?
            .clone();
        assert!(matches!(
            journal.apply_agent_update(&stale).await,
            Err(ReconcileError::StaleAgentEvidence)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?,
            operation_before_stale
        );
        assert_eq!(
            store.get_resource(request.o3k_server_id).await?,
            resource_before_stale
        );
        assert_eq!(
            store.get_agent_command("command-1").await?,
            command_before_stale
        );
        assert_eq!(
            journal
                .agent_evidence
                .lock()
                .map_err(|_| ReconcileError::InvalidIntent)?
                .clone(),
            evidence_before_stale
        );

        // This is the real-host ordering: the E2 observation reaches the
        // durable journal before the E2 terminal operation update.
        journal
            .apply_agent_observation(&AgentObservation {
                agent_id: "agent-a".to_owned(),
                agent_epoch: "epoch-b".to_owned(),
                resource_id: request.o3k_server_id,
                provider_resource_id: Some("domain-a".to_owned()),
                state: o3k_provider::InstanceState::Running,
                operation_id,
                operation_state: AgentOperationState::Succeeded,
                observation_sequence: 2,
                observed_at_unix_ms: 2,
                redacted_message: None,
                console_log_bytes: Vec::new(),
                console_log_offset: 0,
                console_log_complete: false,
                console_log_truncated: false,
                block_device: None,
            })
            .await?;
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_agent_command("command-1").await?.state,
            AgentCommandState::Accepted
        );
        let replayed = AgentOperationUpdate {
            agent_epoch: "epoch-b".to_owned(),
            provider_resource_id: Some("domain-a".to_owned()),
            ..stale
        };
        let conflicting = AgentOperationUpdate {
            provider_resource_id: Some("domain-b".to_owned()),
            ..replayed.clone()
        };
        let operation_before_conflict = store.get_operation(operation_id).await?;
        let resource_before_conflict = store.get_resource(request.o3k_server_id).await?;
        let command_before_conflict = store.get_agent_command("command-1").await?;
        let reference_before_conflict = store
            .get_provider_reference(request.o3k_server_id, "compute-agent")
            .await?;
        let agent_fence_before_conflict = journal
            .agent_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?
            .clone();
        let observation_fence_before_conflict = journal
            .observation_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?
            .clone();
        assert!(matches!(
            journal.apply_agent_update(&conflicting).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?,
            operation_before_conflict
        );
        assert_eq!(
            store.get_resource(request.o3k_server_id).await?,
            resource_before_conflict
        );
        assert_eq!(
            store.get_agent_command("command-1").await?,
            command_before_conflict
        );
        assert_eq!(
            store
                .get_provider_reference(request.o3k_server_id, "compute-agent")
                .await?,
            reference_before_conflict
        );
        assert_eq!(
            journal
                .agent_evidence
                .lock()
                .map_err(|_| ReconcileError::InvalidIntent)?
                .clone(),
            agent_fence_before_conflict
        );
        assert_eq!(
            journal
                .observation_evidence
                .lock()
                .map_err(|_| ReconcileError::InvalidIntent)?
                .clone(),
            observation_fence_before_conflict
        );

        // Model a transient store failure after the in-memory E2 fence was
        // advanced but before command projection committed.  Equal evidence
        // must retry the idempotent durable repair, not be discarded merely
        // because its watermark is already present.
        journal
            .agent_evidence
            .lock()
            .map_err(|_| ReconcileError::InvalidIntent)?
            .insert(
                operation_id,
                AgentEvidenceFence {
                    agent_id: replayed.agent_id.clone(),
                    agent_epoch: replayed.agent_epoch.clone(),
                    sequence: replayed.operation_sequence,
                    state: replayed.state,
                    provider_resource_id: "domain-a".to_owned(),
                },
            );
        assert_eq!(
            journal.apply_agent_update(&replayed).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_agent_command("command-1").await?.state,
            AgentCommandState::Succeeded
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        let terminal_command = store.get_agent_command("command-1").await?;
        assert_eq!(terminal_command.command_id, accepted_command.command_id);
        assert_eq!(
            terminal_command.idempotency_key,
            accepted_command.idempotency_key
        );
        assert_eq!(terminal_command.operation_id, accepted_command.operation_id);
        assert_eq!(terminal_command.resource_id, accepted_command.resource_id);
        assert_eq!(terminal_command.agent_id, accepted_command.agent_id);
        assert_eq!(terminal_command.agent_epoch, accepted_command.agent_epoch);
        assert_eq!(
            terminal_command.payload_fingerprint_sha256,
            accepted_command.payload_fingerprint_sha256
        );
        assert_eq!(terminal_command.payload, accepted_command.payload);
        assert_eq!(terminal_command.accepted_sequence, 1);
        assert_eq!(terminal_command.last_sequence, 2);
        assert_eq!(
            terminal_command.provider_resource_id.as_deref(),
            Some("domain-a")
        );

        // Same-epoch replay is idempotent and cannot change durable identity.
        assert_eq!(
            journal.apply_agent_update(&replayed).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_agent_command("command-1").await?,
            terminal_command
        );
        Ok(())
    }

    /// Issue #87 invariant: evidence minted under an epoch that is no longer
    /// the agent's current registered epoch is a dead/stale stream and must be
    /// rejected, even though the same agent legitimately re-registered under a
    /// newer epoch.
    #[tokio::test]
    async fn agent_evidence_from_dead_epoch_is_rejected_after_reregistration()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-fence-dead-epoch", 2).await?;
        let registry = TestAgentRegistry::default();
        registry.register("agent-a", "epoch-b").await;
        let journal = journal.with_agent_registry(Arc::new(registry.clone()));

        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            "command-1".to_owned(),
            operation_id,
            request.o3k_server_id,
            "agent-a",
            "epoch-b",
        )
        .await?;
        let accepted = AgentCommandAccepted {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-b".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id,
            state: AgentOperationState::Accepted,
            operation_sequence: 1,
        };
        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );

        // A stale in-flight update from the agent's previous (dead) epoch must
        // not mutate current state.
        let stale_update = AgentOperationUpdate {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-a".to_owned(),
            operation_sequence: 2,
            operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Terminal),
            redacted_message: None,
            provider_resource_id: None,
        };
        assert!(matches!(
            journal.apply_agent_update(&stale_update).await,
            Err(ReconcileError::StaleAgentEvidence)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );

        // An epoch that was never registered is equally stale.
        let unknown_epoch_update = AgentOperationUpdate {
            agent_epoch: "epoch-c".to_owned(),
            ..stale_update
        };
        assert!(matches!(
            journal.apply_agent_update(&unknown_epoch_update).await,
            Err(ReconcileError::StaleAgentEvidence)
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );
        Ok(())
    }

    /// ASR-015: once E1 passes current-epoch validation, its lease must keep
    /// E2 registration from becoming current until the caller finishes the
    /// durable projection. This closes the check/write interleaving where an
    /// old stream could otherwise write after E2 registration.
    #[tokio::test]
    async fn current_epoch_lease_serializes_projection_with_reregistration()
    -> Result<(), ReconcileError> {
        let (journal, _, _) = journal("agent-epoch-lease", 2).await?;
        let registry = TestAgentRegistry::default();
        registry.register("agent-a", "epoch-a").await;
        let journal = journal.with_agent_registry(Arc::new(registry.clone()));
        let permit = journal
            .fence_agent_evidence(
                Uuid::now_v7(),
                "agent-a",
                "epoch-a",
                1,
                AgentOperationState::Accepted,
                "",
            )
            .await?;

        let replacement_registry = registry.clone();
        let mut replacement = tokio::spawn(async move {
            replacement_registry.register("agent-a", "epoch-b").await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut replacement)
                .await
                .is_err(),
            "replacement epoch became current while E1 projection held its lease"
        );

        drop(permit);
        tokio::time::timeout(std::time::Duration::from_secs(1), replacement)
            .await
            .map_err(|_| ReconcileError::InvalidIntent)?
            .map_err(|_| ReconcileError::InvalidIntent)?;
        assert_eq!(
            o3k_provider::AgentNodeRegistry::snapshot(&registry, "agent-a")
                .await
                .map(|node| node.agent_epoch)
                .as_deref(),
            Some("epoch-b")
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_observation_projects_nova_state_and_replays_without_mutation()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-observation", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_observation_command(
            &store,
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let observation = AgentObservation {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: Some("agent-domain-stopped".to_owned()),
            state: o3k_provider::InstanceState::Stopped,
            operation_id,
            operation_state: AgentOperationState::Succeeded,
            observation_sequence: 1,
            observed_at_unix_ms: 0,
            redacted_message: None,
            console_log_bytes: Vec::new(),
            console_log_offset: 0,
            console_log_complete: false,
            console_log_truncated: false,
            block_device: None,
        };
        journal.apply_agent_observation(&observation).await?;
        let first = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(first.observed_state, "SHUTOFF");
        assert_eq!(first.provider_id.as_deref(), Some("agent-domain-stopped"));
        journal.apply_agent_observation(&observation).await?;
        let replay = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(replay.generation, first.generation);
        assert_eq!(replay.observed_state, "SHUTOFF");
        Ok(())
    }

    #[tokio::test]
    async fn stale_agent_observation_cannot_regress_projected_state() -> Result<(), ReconcileError>
    {
        let (journal, store, _) = journal("agent-observation-order", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_observation_command(
            &store,
            operation_id,
            request.o3k_server_id,
            "compute-1",
            "epoch-1",
        )
        .await?;
        let active = AgentObservation {
            agent_id: "compute-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: Some("agent-domain".to_owned()),
            state: o3k_provider::InstanceState::Running,
            operation_id,
            operation_state: AgentOperationState::Succeeded,
            observation_sequence: 10,
            observed_at_unix_ms: 0,
            redacted_message: None,
            console_log_bytes: Vec::new(),
            console_log_offset: 0,
            console_log_complete: false,
            console_log_truncated: false,
            block_device: None,
        };
        journal.apply_agent_observation(&active).await?;
        let stale = AgentObservation {
            state: o3k_provider::InstanceState::Stopped,
            observation_sequence: 9,
            ..active.clone()
        };
        journal.apply_agent_observation(&stale).await?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "ACTIVE");
        assert_eq!(resource.provider_id.as_deref(), Some("agent-domain"));
        Ok(())
    }

    #[tokio::test]
    async fn agent_observation_rejects_non_succeeded_operation_state() -> Result<(), ReconcileError>
    {
        let (journal, _, _) = journal("agent-observation-invalid", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let observation = AgentObservation {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: None,
            state: o3k_provider::InstanceState::Creating,
            operation_id,
            operation_state: AgentOperationState::Running,
            observation_sequence: 1,
            observed_at_unix_ms: 0,
            redacted_message: None,
            console_log_bytes: Vec::new(),
            console_log_offset: 0,
            console_log_complete: false,
            console_log_truncated: false,
            block_device: None,
        };
        // A non-successful operation state is not a resource observation: the
        // durable state must never be projected from it. Unrepresentable wire
        // states are additionally rejected at the transport boundary, before
        // this journal is reached.
        assert!(matches!(
            journal.apply_agent_observation(&observation).await,
            Err(ReconcileError::InvalidIntent)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn agent_observation_rejects_agent_not_bound_to_durable_command()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-observation-binding", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_observation_command(
            &store,
            operation_id,
            request.o3k_server_id,
            "agent-a",
            "epoch-a",
        )
        .await?;
        let observation = AgentObservation {
            agent_id: "agent-b".to_owned(),
            agent_epoch: "epoch-b".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: Some("foreign-domain".to_owned()),
            state: o3k_provider::InstanceState::Running,
            operation_id,
            operation_state: AgentOperationState::Succeeded,
            observation_sequence: 1,
            observed_at_unix_ms: 0,
            redacted_message: None,
            console_log_bytes: Vec::new(),
            console_log_offset: 0,
            console_log_complete: false,
            console_log_truncated: false,
            block_device: None,
        };
        assert!(matches!(
            journal.apply_agent_observation(&observation).await,
            Err(ReconcileError::InvalidIntent)
        ));
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "REQUESTED"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_failure_persists_the_contract_redacted_provider_reason()
    -> Result<(), ReconcileError> {
        // Contract: the agent redacts secrets and connection information
        // before sending; the control plane persists the reason bounded and
        // sanitized instead of withholding it entirely (issue #485).
        let (journal, store, _) = journal("agent-failure", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            format!("command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let update = failed_update(
            &operation_id.to_string(),
            &request.o3k_server_id.to_string(),
            "agent-1",
            "epoch-1",
            "gateway preparation failed: interface is foreign",
        )?;
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Failed
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.error_category.as_deref(), Some("terminal"));
        assert_eq!(
            operation.error_message.as_deref(),
            Some("gateway preparation failed: interface is foreign")
        );
        Ok(())
    }

    #[tokio::test]
    async fn command_acceptance_is_durable_and_idempotent() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-acceptance", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        bind_command(
            &store,
            "command-1".to_owned(),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        let accepted = AgentCommandAccepted {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id,
            state: AgentOperationState::Accepted,
            operation_sequence: 1,
        };

        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );
        assert_eq!(
            journal.apply_agent_acceptance(&accepted).await?,
            OperationState::Running
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );
        assert_eq!(
            journal
                .events()
                .iter()
                .filter(|event| event.operation_id == operation_id)
                .count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_outcome_is_observed_without_duplicate_create() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown", 2).await?;
        provider.set_failure(FailureInjection::Timeout)?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(provider.instance_count(), 1);
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        Ok(())
    }

    #[tokio::test]
    async fn partial_create_waits_for_observed_running_without_duplicate_create()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("partial-create", 2).await?;
        provider.set_failure(FailureInjection::PartialCompletion)?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Running
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "BUILD");
        assert!(resource.provider_id.is_some());
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );

        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// Issue #611 test seam: the fake provider's idempotency replay returns
    /// the recorded (Accepted/None) operation on the create re-drive; the
    /// real provider re-executes the create (the transfer re-offer completes
    /// it) and advances the operation to Succeeded with a provider resource.
    /// This wrapper advances the recorded operation exactly when the re-drive
    /// reaches `create_instance` with the operation still Accepted.
    struct AdvancingCreateProvider {
        inner: FakeComputeProvider,
        provider_operation_id: Uuid,
    }

    impl AdvancingCreateProvider {
        fn new(inner: FakeComputeProvider, provider_operation_id: Uuid) -> Self {
            Self {
                inner,
                provider_operation_id,
            }
        }
    }

    #[async_trait::async_trait]
    impl o3k_provider::ComputeProvider for AdvancingCreateProvider {
        async fn capabilities(
            &self,
        ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            if let Ok(operation) = self.inner.get_operation(self.provider_operation_id).await
                && operation.state == o3k_provider::OperationState::Accepted
            {
                self.inner.set_operation_state(
                    self.provider_operation_id,
                    o3k_provider::OperationState::Succeeded,
                )?;
                self.inner.set_operation_provider_resource_id(
                    self.provider_operation_id,
                    Some(format!("fake-{}", request.o3k_server_id)),
                )?;
            }
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn inspect_instance(
            &self,
            provider_id: &str,
            resource_id: &str,
            provider_instance_id: &str,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .inspect_instance(
                    provider_id,
                    resource_id,
                    provider_instance_id,
                    operation_id,
                    idempotency_key,
                )
                .await
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: o3k_provider::InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// Wraps the stateful fake provider so every instance is observed in the
    /// Stopped state, modeling the issue-87 crash-between-define-and-start
    /// residue: on restart the domain exists (defined) but was never started,
    /// so the adoption reconcile observes a present instance in SHUTOFF.
    struct StoppedInstanceProvider {
        inner: FakeComputeProvider,
    }

    impl StoppedInstanceProvider {
        fn new(inner: FakeComputeProvider) -> Self {
            Self { inner }
        }

        fn instance_count(&self) -> usize {
            self.inner.instance_count()
        }
    }

    #[async_trait::async_trait]
    impl o3k_provider::ComputeProvider for StoppedInstanceProvider {
        async fn capabilities(
            &self,
        ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
            let mut instance = self.inner.get_instance(provider_instance_id).await?;
            instance.state = o3k_provider::InstanceState::Stopped;
            Ok(instance)
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: o3k_provider::InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// The issue-87 crash-between-define-and-start adoption: the provider
    /// create succeeded and the domain exists, but the instance is observed
    /// Stopped (defined, never started). Presence observation treats any
    /// present instance as a converged create, so the adoption must reach the
    /// terminal Succeeded state projecting SHUTOFF — never stay Running with
    /// no transition path.
    #[tokio::test]
    async fn adopted_create_with_stopped_instance_converges_to_succeeded_shutoff()
    -> Result<(), ReconcileError> {
        let (_, store, provider) = journal("adopted-shutoff", 2).await?;
        let provider = Arc::new(StoppedInstanceProvider::new(provider.as_ref().clone()));
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "SHUTOFF"
        );
        assert_eq!(provider.instance_count(), 1);
        // Idempotent terminality: a second reconcile is a no-op and must
        // never duplicate the create or regress the state.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "SHUTOFF"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// Issue #609 (ASR-021 agent-control-plane-network-interruption): a
    /// retryable provider outcome is by definition not a definitively-known
    /// failure, so exhausting the retry budget must leave the create in
    /// `UnknownOutcome` (error_category `retry_exhausted`, error kept for
    /// diagnosis) — never terminal `Failed` — and the next convergence
    /// re-drive must still converge it to `Succeeded` exactly once when the
    /// provider is healthy. The exhausted create carries no provider
    /// operation identity, so the re-drive observes presence by the durable
    /// placement identity instead of polling (or re-dispatching).
    #[tokio::test]
    async fn retry_budget_exhaustion_leaves_unknown_outcome_and_recovers()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("retry", 2).await?;
        provider.set_failure(FailureInjection::Transient)?;
        let mut request = request();
        // The exhausted create is re-observed by the durable placement
        // identity (SPEC-0021 observe-before-decide), so the intent must
        // name the execution agent.
        request.placement_provider_id = Some("agent-1".to_owned());
        let operation_id = journal.begin_create("project", &request).await?;
        // Dispatch 1: retryable, budget (2) not yet exhausted -> scheduled.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Retryable
        );
        // Dispatch 2: retryable, budget exhausted -> UnknownOutcome, and the
        // exhaustion transition fires the unknown-outcome journal event.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.state, OperationState::UnknownOutcome);
        assert_eq!(operation.error_category.as_deref(), Some("retry_exhausted"));
        assert!(
            operation.error_message.is_some(),
            "the exhausted branch must keep the provider error for diagnosis"
        );
        assert!(journal.events().iter().any(|event| {
            event.operation_id == operation_id && event.kind == JournalEventKind::UnknownObserved
        }));
        // The interruption resolves. The create actually landed at the
        // execution boundary during the outage (only the acknowledgement was
        // lost — a timeout is an unknown outcome, not a failure), so exactly
        // one instance exists at the provider.
        provider.set_failure(FailureInjection::None)?;
        provider.create_instance(request.clone()).await?;
        assert_eq!(provider.instance_count(), 1);
        // The next convergence re-drive observes presence and adopts the
        // landed create: Succeeded, resource ACTIVE, still exactly one
        // instance (no re-dispatch, no duplicate).
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// Issue #611 (ASR-021 agent-control-plane-network-interruption): an
    /// ACCEPTED create whose provider operation never produced a provider
    /// resource (Accepted/Running with no provider_resource_id — the create
    /// provably never executed, e.g. the agent reported an unknown outcome
    /// because the committed artifacts were missing) must be re-driven by the
    /// unknown-outcome recovery — the transfer loop re-offers the missing
    /// artifact and the create converges ACTIVE — instead of being parked
    /// Running forever with no recovery path.
    #[tokio::test]
    async fn accepted_create_without_provider_resource_is_redriven_not_parked()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("accepted-never-executed", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("agent-1".to_owned());
        // The create is accepted and then reported unknown (the interrupted
        // artifact delivery shape), leaving an UnknownOutcome operation with a
        // provider operation identity.
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let operation = store.get_operation(operation_id).await?;
        let provider_operation_id = operation
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        // The provider operation is still Accepted and carries no provider
        // resource: the create provably never executed.
        provider.set_operation_state(
            provider_operation_id,
            o3k_provider::OperationState::Accepted,
        )?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The interruption resolves; the next convergence re-drive must
        // re-dispatch the create (never park it Running) and converge ACTIVE.
        provider.set_failure(FailureInjection::None)?;
        let provider = Arc::new(AdvancingCreateProvider::new(
            provider.as_ref().clone(),
            provider_operation_id,
        ));
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        Ok(())
    }

    /// Issue #610 (ASR-021 agent-control-plane-network-interruption): a
    /// create whose retry budget exhausted before ANY dispatch was accepted —
    /// the durable agent command row is still `pending`, proving the create
    /// never executed — must re-drive the create once the interruption
    /// resolves instead of presence-inspecting it (which would terminalize
    /// the absent create as failed). The re-drive converges ACTIVE exactly
    /// once, mirroring the Running-without-identity sweep shape.
    #[tokio::test]
    async fn exhausted_create_with_pending_command_redrives_after_recovery()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("exhausted-pending-redrive", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("agent-1".to_owned());
        provider.set_failure(FailureInjection::Transient)?;
        let operation_id = journal.begin_create("project", &request).await?;
        // Dispatch 1: retryable, budget (2) not yet exhausted -> scheduled.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Retryable
        );
        // Dispatch 2: retryable, budget exhausted -> UnknownOutcome with no
        // provider operation identity.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.state, OperationState::UnknownOutcome);
        assert!(operation.provider_operation_id.is_none());
        // The durable command row is still `pending` — exactly the residue a
        // mid-transfer control-channel drop leaves behind: the create was
        // provably never accepted by the agent.
        bind_command(
            store.as_ref(),
            format!("create-command-{operation_id}"),
            operation_id,
            request.o3k_server_id,
            "agent-1",
            "epoch-1",
        )
        .await?;
        // The interruption resolves; the next convergence re-drive must
        // re-dispatch the create (never presence-terminalize it) and converge
        // ACTIVE with exactly one instance.
        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }
    /// dispatch is rejected as retryable until the budget exhausts must also
    /// land in `UnknownOutcome` (never terminal `Failed`), and the lifecycle
    /// convergence re-drive must converge it by presence once the provider is
    /// healthy — the instance is still present, so the delete is re-driven
    /// and the goal converges to DELETED with zero residue.
    #[tokio::test]
    async fn delete_retry_exhaustion_stays_unknown_and_presence_converges()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("delete-retry-exhaustion", 2).await?;
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Delete)
            .await?;
        provider.set_failure(FailureInjection::Transient)?;
        // Dispatch 1: retryable, budget (2) not yet exhausted -> scheduled.
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Retryable
        );
        // Dispatch 2: retryable, budget exhausted -> UnknownOutcome, and the
        // exhausted operation carries no provider operation identity.
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.state, OperationState::UnknownOutcome);
        assert_eq!(operation.error_category.as_deref(), Some("retry_exhausted"));
        assert!(operation.provider_operation_id.is_none());
        // The interruption resolves: the delete never executed during the
        // outage, so the instance is still present and the presence-driven
        // re-drive must re-dispatch the delete and converge to DELETED.
        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(provider.instance_count(), 0);
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "DELETED"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_create_records_observed_provider_failure() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-create-failed", 2).await?;
        provider.set_failure(FailureInjection::Timeout)?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider
            .set_operation_state(provider_operation_id, o3k_provider::OperationState::Failed)?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Failed
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Failed
        );
        Ok(())
    }

    /// Genuine unknown-outcome creates converge by observing instance
    /// presence by durable identity: the provider operation carries no
    /// provider resource id, and the presence inspection finds the instance,
    /// so the create finishes without ever re-dispatching the create.
    #[tokio::test]
    async fn unknown_create_converges_when_presence_inspection_finds_instance()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-present", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        provider.set_failure(FailureInjection::None)?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        // Presence observation must never duplicate the create.
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// A presence inspection that provably finds no instance converges the
    /// unknown create to a terminal failure with the resource projected to
    /// error, so clients polling the server stop waiting.
    #[tokio::test]
    async fn unknown_create_converges_to_failed_when_instance_is_absent()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-absent", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let instance_id = provider
            .get_operation(provider_operation_id)
            .await?
            .provider_resource_id
            .ok_or(ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The instance provably does not exist: the create never took effect.
        provider.remove_instance(&instance_id)?;
        provider.set_failure(FailureInjection::None)?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_operation(operation_id)
                .await?
                .error_category
                .as_deref(),
            Some("not_found")
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        Ok(())
    }

    /// A presence inspection whose own outcome is unknown (dispatch timeout,
    /// transport loss) preserves the unknown-outcome semantics: the create is
    /// never marked failed on inspection transport loss and stays re-observable.
    #[tokio::test]
    async fn unknown_create_remains_unknown_when_presence_inspection_is_unknown()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-inspect-unknown", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The inspect dispatch itself remains unknown (Timeout still active).
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        assert_ne!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        Ok(())
    }

    /// When the agent completed the presence inspection while the durable
    /// operation record was still in-flight, the terminal agent command
    /// record is the durable evidence and must converge without a second
    /// dispatch (the race where the agent's terminal update overtakes the
    /// reconciler's in-flight write).
    #[tokio::test]
    async fn unknown_create_converges_from_terminal_agent_command_without_redispatch()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-command", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let instance_id = provider
            .get_operation(provider_operation_id)
            .await?
            .provider_resource_id
            .ok_or(ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;

        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        store
            .insert_operation(&OperationRecord {
                id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                kind: "inspect".to_owned(),
                state: OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id: "inspect-command-1".to_owned(),
                idempotency_key: format!("o3k-inspect-create-{operation_id}"),
                operation_id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                payload_fingerprint_sha256: "f".repeat(64),
                payload: Vec::new(),
                state: AgentCommandState::Succeeded,
                accepted_sequence: 1,
                last_sequence: 2,
                provider_operation_id: Some(inspect_operation_id.to_string()),
                provider_resource_id: Some(instance_id.clone()),
            })
            .await?;
        store
            .attach_provider_reference(&ProviderReference {
                resource_id: request.o3k_server_id,
                provider_name: "compute-agent".to_owned(),
                provider_resource_id: instance_id,
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        // The terminal agent command converged the create without dispatching
        // a second inspection.
        assert_eq!(provider.inspect_dispatch_count(), 0);
        Ok(())
    }

    /// A stored terminal `Failed`/`not_found` inspection record (the crash
    /// window between the inspection converging and the create converging)
    /// must converge the create to absence without any dispatch.
    #[tokio::test]
    async fn unknown_create_converges_from_stored_failed_inspection_without_dispatch()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-stored-failed", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let instance_id = provider
            .get_operation(provider_operation_id)
            .await?
            .provider_resource_id
            .ok_or(ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The instance provably does not exist: the create never took effect.
        provider.remove_instance(&instance_id)?;
        provider.set_failure(FailureInjection::None)?;

        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        store
            .insert_operation(&OperationRecord {
                id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                kind: "inspect".to_owned(),
                state: OperationState::Failed,
                provider_operation_id: Some(inspect_operation_id.to_string()),
                error_category: Some("not_found".to_owned()),
                error_message: Some("presence inspection: instance is absent".to_owned()),
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        assert_eq!(provider.inspect_dispatch_count(), 0);
        Ok(())
    }

    /// The race mirror of the succeeded-command test: a terminal `Failed`
    /// agent command for the in-flight inspection proves absence (the agent
    /// classifies only absent domains as terminal inspect failures) and must
    /// converge the create without a second dispatch.
    #[tokio::test]
    async fn unknown_create_converges_to_absent_from_terminal_failed_agent_command()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-command-failed", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let instance_id = provider
            .get_operation(provider_operation_id)
            .await?
            .provider_resource_id
            .ok_or(ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The instance provably does not exist: the create never took effect.
        provider.remove_instance(&instance_id)?;
        provider.set_failure(FailureInjection::None)?;

        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        store
            .insert_operation(&OperationRecord {
                id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                kind: "inspect".to_owned(),
                state: OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id: "inspect-command-failed".to_owned(),
                idempotency_key: format!("o3k-inspect-create-{operation_id}"),
                operation_id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                payload_fingerprint_sha256: "f".repeat(64),
                payload: Vec::new(),
                state: AgentCommandState::Failed,
                accepted_sequence: 1,
                last_sequence: 2,
                provider_operation_id: Some(inspect_operation_id.to_string()),
                provider_resource_id: None,
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Failed
        );
        assert_eq!(
            store
                .get_operation(operation_id)
                .await?
                .error_category
                .as_deref(),
            Some("not_found")
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        assert_eq!(provider.inspect_dispatch_count(), 0);
        Ok(())
    }
    #[tokio::test]
    async fn unknown_create_redispatches_pending_inspection_record() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-pending", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The instance exists; only the inspection record is stuck in Pending.
        provider.set_failure(FailureInjection::None)?;

        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        store
            .insert_operation(&OperationRecord {
                id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                kind: "inspect".to_owned(),
                state: OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        // Re-observation must never duplicate the create.
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// An inspection the agent already accepted (`Running`, no terminal
    /// evidence yet) is never re-dispatched: the agent journal guarantees
    /// delivery of the terminal update, so the create stays unknown until
    /// that update arrives instead of duplicating the inspection.
    #[tokio::test]
    async fn unknown_create_does_not_redispatch_accepted_inspection_record()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-running", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        provider.set_failure(FailureInjection::None)?;

        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        store
            .insert_operation(&OperationRecord {
                id: inspect_operation_id,
                resource_id: request.o3k_server_id,
                kind: "inspect".to_owned(),
                state: OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        // The instance was never created and the accepted inspection was not
        // duplicated (no dispatch happened at all).
        assert_eq!(provider.instance_count(), 1);
        assert_eq!(provider.inspect_dispatch_count(), 0);
        Ok(())
    }

    /// When a provider reference was recorded meanwhile (the lost-update
    /// window where the agent completed the create), the presence inspection
    /// passes the known provider identity instead of an empty id.
    #[tokio::test]
    async fn unknown_create_uses_known_provider_reference_for_presence_inspection()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-reference", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let instance_id = provider
            .get_operation(provider_operation_id)
            .await?
            .provider_resource_id
            .ok_or(ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        provider.set_failure(FailureInjection::None)?;
        store
            .attach_provider_reference(&ProviderReference {
                resource_id: request.o3k_server_id,
                provider_name: "compute-agent".to_owned(),
                provider_resource_id: instance_id.clone(),
            })
            .await?;

        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        // The inspection was dispatched exactly once and carried the known
        // provider identity recorded in the reference, not an empty id.
        assert_eq!(provider.inspect_dispatch_count(), 1);
        assert_eq!(
            provider.last_inspect_provider_instance_id().as_deref(),
            Some(instance_id.as_str())
        );
        Ok(())
    }

    /// A stored `UnknownOutcome` inspection record (the outcome of a previous
    /// trigger whose dispatch was lost) stays re-observable: the next trigger
    /// re-dispatches the read-only inspection and converges.
    #[tokio::test]
    async fn unknown_create_redispatches_stored_unknown_inspection() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("unknown-presence-stored-unknown", 2).await?;
        let mut request = request();
        request.placement_provider_id = Some("node-a".to_owned());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The first presence observation is itself lost (Timeout still
        // active), leaving a stored UnknownOutcome inspection record.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(provider.inspect_dispatch_count(), 1);
        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{operation_id}").as_bytes(),
        );
        assert_eq!(
            store.get_operation(inspect_operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        // The next trigger re-observes: the read-only inspection is
        // re-dispatched and the instance is found.
        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.inspect_dispatch_count(), 2);
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// A create that never recorded an execution agent cannot be observed by
    /// durable identity: the unknown outcome is preserved (never guessed).
    #[tokio::test]
    async fn unknown_create_without_agent_preserves_unknown_outcome() -> Result<(), ReconcileError>
    {
        let (journal, store, provider) = journal("unknown-presence-no-agent", 2).await?;
        let request = request();
        assert!(request.placement_provider_id.is_none());
        provider.set_failure(FailureInjection::Timeout)?;
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider.set_operation_provider_resource_id(provider_operation_id, None)?;
        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        assert_eq!(provider.inspect_dispatch_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_delete_is_observed_without_repeating_mutation() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("delete-unknown", 2).await?;
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        provider.set_failure(FailureInjection::Timeout)?;
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Delete)
            .await?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "DELETED"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_accepted_delete_gets_one_fresh_idempotent_redrive() -> Result<(), ReconcileError>
    {
        let (journal, store, provider) = journal("delete-stale-accepted", 2).await?;
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Delete)
            .await?;
        provider.set_failure(FailureInjection::StaleAccepted)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Running
        );
        let stale_provider_operation: Uuid = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        store
            .update_operation(
                operation_id,
                OperationState::UnknownOutcome,
                Some(&stale_provider_operation.to_string()),
                Some("unknown_outcome"),
                None,
            )
            .await?;
        provider.set_failure(FailureInjection::None)?;

        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "DELETED"
        );
        assert_eq!(provider.instance_count(), 0);
        Ok(())
    }

    /// Models the REAL `AgentComputeProvider` delete contract for #575: the
    /// dispatch is asynchronous — `delete_instance` returns an Accepted
    /// operation immediately and the agent-side completion is observed only
    /// later through `get_operation`, never inside the dispatch call (the
    /// flaw that hid the bug in `FakeComputeProvider::delete_instance`, which
    /// returns Succeeded synchronously in the no-injection case). The FIRST
    /// delete dispatch for the instance is the #575 stale command whose
    /// execution was lost in the libvirtd restart: it stays Accepted forever
    /// and counts as zero effective executions. Any later dispatch (the
    /// deterministic redrive command) executes once and completes on its
    /// second `get_operation` poll, mirroring the agent event stream
    /// terminalizing the adapter's volatile projection between sweep ticks.
    /// Re-dispatching an already-recorded operation id reuses the recorded
    /// operation without executing again, mirroring
    /// `AgentComputeProvider::reuse_recorded_command` and the agent journal's
    /// command-identity replay.
    #[derive(Clone)]
    struct AsyncAgentDeleteProvider {
        inner: FakeComputeProvider,
        state: Arc<Mutex<AsyncAgentDeleteState>>,
        store: Option<Arc<dyn o3k_store::ComputeRepository>>,
    }

    struct AsyncAgentDeleteState {
        /// Provider operations keyed by the operation id the reconciler
        /// passed to `delete_instance` (the agent adapter keys its volatile
        /// operation projection by the command's operation id).
        operations: HashMap<Uuid, o3k_provider::Operation>,
        /// The first (stale) delete dispatch's operation id; its execution
        /// was lost and it never completes.
        original_operation_id: Option<Uuid>,
        /// The O3K server id captured at create time (the command ledger's
        /// `resource_id` is the server id, never the provider domain name).
        resource_id: Option<Uuid>,
        /// The durable state the stale command projects through
        /// `get_operation` — `Accepted` models the lost-update shape and
        /// `UnknownOutcome` models the rejected-observation shape (the
        /// exact #575 durable row).
        stale_original_state: o3k_provider::OperationState,
        /// Provider instance id per dispatched operation, used to remove the
        /// instance when that operation's execution completes.
        instance_by_operation: HashMap<Uuid, String>,
        /// `get_operation` poll counts per operation.
        polls: HashMap<Uuid, usize>,
        /// Delete commands whose execution actually ran on the "agent".
        delete_executions: usize,
    }

    impl AsyncAgentDeleteProvider {
        fn with_store(
            store: Arc<dyn o3k_store::ComputeRepository>,
            stale_original_state: o3k_provider::OperationState,
        ) -> Self {
            Self {
                inner: FakeComputeProvider::new(),
                state: Arc::new(Mutex::new(AsyncAgentDeleteState {
                    stale_original_state,
                    operations: HashMap::new(),
                    original_operation_id: None,
                    resource_id: None,
                    instance_by_operation: HashMap::new(),
                    polls: HashMap::new(),
                    delete_executions: 0,
                })),
                store: Some(store),
            }
        }

        fn delete_executions(&self) -> usize {
            self.state
                .lock()
                .map(|state| state.delete_executions)
                .unwrap_or_default()
        }

        fn instance_count(&self) -> usize {
            self.inner.instance_count()
        }
    }

    #[async_trait::async_trait]
    impl ComputeProvider for AsyncAgentDeleteProvider {
        async fn capabilities(&self) -> Result<o3k_provider::Capabilities, ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, ProviderError> {
            self.state
                .lock()
                .map_err(|_| ProviderError::Storage)?
                .resource_id = Some(request.o3k_server_id);
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, ProviderError> {
            {
                let state = self.state.lock().map_err(|_| ProviderError::Storage)?;
                if let Some(existing) = state.operations.get(&request.operation_id) {
                    // Idempotent re-dispatch of the same command identity:
                    // reuse the recorded operation without a second execution.
                    return Ok(existing.clone());
                }
            }
            // Mirror `AgentComputeProvider::dispatch_recorded`: persist the
            // durable command row BEFORE dispatch. The ledger's foreign key
            // on `operation_id -> operations(id)` is the #575 real-host
            // constraint: a fresh command identity without a durable
            // operation row fails the insert and the dispatch reports
            // Conflict (run local-5752), so the reconciler must create the
            // re-drive operation row first.
            if let Some(store) = &self.store {
                let resource_id = self
                    .state
                    .lock()
                    .map_err(|_| ProviderError::Storage)?
                    .resource_id
                    .ok_or(ProviderError::InvalidRequest)?;
                let record = o3k_store::AgentCommandRecord {
                    command_id: format!("async-agent-delete-{}", request.operation_id),
                    idempotency_key: request.idempotency_key.clone(),
                    operation_id: request.operation_id,
                    resource_id,
                    agent_id: "node-a".to_owned(),
                    agent_epoch: "epoch-1".to_owned(),
                    payload_fingerprint_sha256: "0".repeat(64),
                    payload: Vec::new(),
                    state: o3k_store::AgentCommandState::Pending,
                    accepted_sequence: 0,
                    last_sequence: 0,
                    provider_operation_id: Some(request.operation_id.to_string()),
                    provider_resource_id: None,
                };
                if store.insert_agent_command(&record).await.is_err() {
                    // The insert failed: either the foreign key rejected a
                    // command identity without an operation row, or a
                    // concurrent dispatch inserted first. Only the former is
                    // a conflict — the latter adopts the surviving row.
                    let adopted = self
                        .state
                        .lock()
                        .map_err(|_| ProviderError::Storage)?
                        .operations
                        .contains_key(&request.operation_id);
                    if !adopted {
                        return Err(ProviderError::Conflict);
                    }
                }
            }
            let mut state = self.state.lock().map_err(|_| ProviderError::Storage)?;
            let operation_id = request.operation_id;
            if let Some(existing) = state.operations.get(&operation_id) {
                return Ok(existing.clone());
            }
            let stale = state.original_operation_id.is_none();
            if stale {
                state.original_operation_id = Some(operation_id);
            } else {
                state.delete_executions += 1;
                state
                    .instance_by_operation
                    .insert(operation_id, request.provider_instance_id.clone());
            }
            let operation = o3k_provider::Operation {
                provider_operation_id: operation_id,
                o3k_operation_id: operation_id,
                state: o3k_provider::OperationState::Accepted,
                error_category: None,
                provider_resource_id: None,
            };
            state.operations.insert(operation_id, operation.clone());
            Ok(operation)
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: o3k_provider::InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, ProviderError> {
            let (operation, instance_to_remove) = {
                let mut state = self.state.lock().map_err(|_| ProviderError::Storage)?;
                let Some(operation) = state.operations.get(&provider_operation_id).cloned() else {
                    return Err(ProviderError::NotFound);
                };
                let polls = state.polls.entry(provider_operation_id).or_insert(0);
                *polls += 1;
                let poll_count = *polls;
                if state.original_operation_id == Some(provider_operation_id) {
                    // The stale command's durable projection: the control
                    // plane projected the operation's own durable state
                    // (`UnknownOutcome` for the rejected-observation shape,
                    // `Accepted` for the lost-update shape), never the
                    // in-flight `Accepted` from the original dispatch.
                    let mut stale = operation;
                    stale.state = state.stale_original_state;
                    (stale, None)
                } else if operation.state != o3k_provider::OperationState::Accepted
                    || poll_count < 2
                {
                    (operation, None)
                } else {
                    // The simulated agent execution completed: the terminal
                    // state and the instance removal are observed now, exactly
                    // as the real agent's terminal events arrive between
                    // sweep ticks.
                    let mut completed = operation.clone();
                    completed.state = o3k_provider::OperationState::Succeeded;
                    state
                        .operations
                        .insert(provider_operation_id, completed.clone());
                    (
                        completed,
                        state.instance_by_operation.remove(&provider_operation_id),
                    )
                }
            };
            if let Some(provider_instance_id) = instance_to_remove {
                let _ = self.inner.remove_instance(&provider_instance_id);
            }
            Ok(operation)
        }
    }

    /// Reproduces the #575 durable state and drives convergence the way the
    /// REAL system does — the `drive_all_lifecycle_convergence` sweep gate
    /// (crates/o3k-compute/src/lib.rs ~805-811) — against a provider fake with
    /// the REAL agent's asynchronous delete contract. The first dispatch is
    /// Accepted with the original provider operation id; the #575 condition
    /// is simulated exactly like the existing regression test (the agent's
    /// non-Succeeded observation was rejected, so the durable operation is
    /// UnknownOutcome with the stale id); then the sweep-gated loop must
    /// redrive the delete with the deterministic fresh command identity and
    /// POLL it to terminal. On current main the redrive arm stores Running
    /// with the redrive id, the sweep gate then skips the operation forever
    /// ("in flight: the agent event stream terminalizes it"), the redrive's
    /// agent evidence is rejected (no operation row carries the redrive id),
    /// and the delete never converges — the resource stays ACTIVE and the
    /// allocation/config-drive media stay held.
    #[tokio::test]
    async fn stale_accepted_delete_converges_under_async_agent_contract()
    -> Result<(), ReconcileError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-delete-async-agent-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(AsyncAgentDeleteProvider::with_store(
            store.clone(),
            o3k_provider::OperationState::Accepted,
        ));
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);

        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;

        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Delete)
            .await?;
        // First dispatch, exactly like the real agent-backed path: the
        // command is accepted asynchronously and the operation is stored
        // Running with the ORIGINAL provider operation id (the agent adapter
        // keys its operation projection by the command's operation id).
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Running
        );
        let stale_provider_operation: Uuid = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        assert_eq!(stale_provider_operation, operation_id);
        // Issue #575 residue: the agent's non-Succeeded observation was
        // rejected by `apply_agent_observation`, leaving the durable
        // operation UnknownOutcome with the stale provider operation id
        // (exactly the existing regression test's shape).
        store
            .update_operation(
                operation_id,
                OperationState::UnknownOutcome,
                Some(&stale_provider_operation.to_string()),
                Some("unknown_outcome"),
                None,
            )
            .await?;

        // Drive convergence EXACTLY like the real sweep: only Pending /
        // UnknownOutcome / Retryable / Running-without-provider-operation-id
        // are re-driven; Running WITH the identity is skipped as in-flight.
        for _ in 0..20 {
            let operation = store.get_operation(operation_id).await?;
            if matches!(
                operation.state,
                OperationState::Succeeded | OperationState::Failed
            ) {
                break;
            }
            let re_drive = matches!(
                operation.state,
                OperationState::Pending
                    | OperationState::UnknownOutcome
                    | OperationState::Retryable
            ) || (operation.state == OperationState::Running
                && operation.provider_operation_id.is_none());
            if !re_drive {
                continue;
            }
            journal.reconcile_lifecycle_once(operation_id).await?;
        }

        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "DELETED"
        );
        assert_eq!(provider.instance_count(), 0);
        // The lost original command never executed; the redrive executed
        // exactly once. Any repeated gate pass re-dispatches the SAME
        // deterministic redrive command identity, which the provider reuses
        // without a second execution — so the count can never exceed one.
        assert_eq!(provider.delete_executions(), 1);
        Ok(())
    }

    /// The exact #575 durable shape (V1): the stale command's durable
    /// projection is `UnknownOutcome` — the control plane rejected the
    /// agent's non-Succeeded observation, the operation row carries the
    /// stale provider operation id, and the agent's journal only replays the
    /// recorded unknown outcome. `observe_lifecycle`'s UnknownOutcome arm
    /// must therefore re-drive the delete when the instance is still present
    /// (previously it returned UnknownOutcome forever and the redrive arm was
    /// unreachable on the real agent-backed path, because the provider
    /// adapter projected the operation's own durable state).
    #[tokio::test]
    async fn stale_unknown_outcome_delete_redrives_and_converges() -> Result<(), ReconcileError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-delete-async-unknown-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(AsyncAgentDeleteProvider::with_store(
            store.clone(),
            o3k_provider::OperationState::UnknownOutcome,
        ));
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);

        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;

        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Delete)
            .await?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Running
        );
        let stale_provider_operation: Uuid = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        store
            .update_operation(
                operation_id,
                OperationState::UnknownOutcome,
                Some(&stale_provider_operation.to_string()),
                Some("unknown_outcome"),
                None,
            )
            .await?;

        for _ in 0..20 {
            let operation = store.get_operation(operation_id).await?;
            if matches!(
                operation.state,
                OperationState::Succeeded | OperationState::Failed
            ) {
                break;
            }
            let re_drive = matches!(
                operation.state,
                OperationState::Pending
                    | OperationState::UnknownOutcome
                    | OperationState::Retryable
            ) || (operation.state == OperationState::Running
                && operation.provider_operation_id.is_none());
            if !re_drive {
                continue;
            }
            journal.reconcile_lifecycle_once(operation_id).await?;
        }

        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "DELETED"
        );
        assert_eq!(provider.instance_count(), 0);
        assert_eq!(provider.delete_executions(), 1);
        Ok(())
    }

    fn failed_update(
        operation_id: &str,
        resource_id: &str,
        agent_id: &str,
        agent_epoch: &str,
        redacted_message: &str,
    ) -> Result<AgentOperationUpdate, ReconcileError> {
        Ok(AgentOperationUpdate {
            agent_id: agent_id.to_owned(),
            agent_epoch: agent_epoch.to_owned(),
            operation_sequence: 1,
            operation_id: Uuid::parse_str(operation_id)
                .map_err(|_| ReconcileError::InvalidIntent)?,
            resource_id: Uuid::parse_str(resource_id).map_err(|_| ReconcileError::InvalidIntent)?,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Terminal),
            redacted_message: Some(redacted_message.to_owned()),
            provider_resource_id: None,
        })
    }

    #[tokio::test]
    async fn unknown_action_is_observed_before_finishing_converged_state()
    -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("action-unknown", 2).await?;
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        provider.set_failure(FailureInjection::Timeout)?;
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Stop)
            .await?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );

        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?
            .parse()
            .map_err(|_| ReconcileError::InvalidIntent)?;
        provider
            .set_operation_state(provider_operation_id, o3k_provider::OperationState::Running)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Running
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running
        );
        provider.set_operation_state(
            provider_operation_id,
            o3k_provider::OperationState::Succeeded,
        )?;

        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "SHUTOFF"
        );
        Ok(())
    }

    /// A second driver reaching `finish` after the first driver already
    /// converged the same operation (the idempotent-retry show path re-driving
    /// a create whose synchronous pass completed in between) must short-circuit
    /// on the first-writer outcome: no re-attach, no resource churn, no error.
    #[tokio::test]
    async fn finish_is_idempotent_when_operation_already_succeeded() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("finish-idempotent", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        let provider_operation_id = store
            .get_operation(operation_id)
            .await?
            .provider_operation_id
            .ok_or(ReconcileError::InvalidIntent)?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        let provider_resource_id = format!("fake-{}", request.o3k_server_id);
        let generation_before = resource.generation;
        assert_eq!(
            journal
                .finish(
                    operation_id,
                    resource.clone(),
                    provider_operation_id.clone(),
                    Some(provider_resource_id.clone()),
                )
                .await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(request.o3k_server_id).await?.generation,
            generation_before,
            "a converged second finish must not touch the resource"
        );
        assert_eq!(
            store
                .get_provider_reference(request.o3k_server_id, "compute")
                .await?
                .provider_resource_id,
            provider_resource_id,
            "the first-writer reference must be unchanged"
        );
        Ok(())
    }

    /// A second driver that reaches `finish` while the first driver's attach
    /// already landed (operation still non-terminal) must treat the matching
    /// provider reference as idempotent and converge the projection.
    #[tokio::test]
    async fn finish_converges_when_provider_reference_already_attached()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("finish-attached", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        store
            .update_operation(operation_id, OperationState::Running, None, None, None)
            .await?;
        let provider_resource_id = format!("fake-{}", request.o3k_server_id);
        store
            .attach_provider_reference(&ProviderReference {
                resource_id: request.o3k_server_id,
                provider_name: "compute".to_owned(),
                provider_resource_id: provider_resource_id.clone(),
            })
            .await?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(
            journal
                .finish(
                    operation_id,
                    resource,
                    "provider-operation-1".to_owned(),
                    Some(provider_resource_id),
                )
                .await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        Ok(())
    }

    /// A provider reference carrying a DIFFERENT provider resource id is a
    /// genuine identity drift, never a converged duplicate: the second driver
    /// must fail closed instead of overwriting the attached identity.
    #[tokio::test]
    async fn finish_rejects_provider_reference_identity_drift() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("finish-drift", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        store
            .update_operation(operation_id, OperationState::Running, None, None, None)
            .await?;
        store
            .attach_provider_reference(&ProviderReference {
                resource_id: request.o3k_server_id,
                provider_name: "compute".to_owned(),
                provider_resource_id: "foreign-domain".to_owned(),
            })
            .await?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert!(matches!(
            journal
                .finish(
                    operation_id,
                    resource,
                    "provider-operation-1".to_owned(),
                    Some(format!("fake-{}", request.o3k_server_id)),
                )
                .await,
            Err(ReconcileError::Store(
                StoreError::ProviderReferenceAlreadyExists
            ))
        ));
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Running,
            "a drifted finish must not project success"
        );
        assert_eq!(
            store
                .get_provider_reference(request.o3k_server_id, "compute")
                .await?
                .provider_resource_id,
            "foreign-domain",
            "the attached identity must be preserved"
        );
        Ok(())
    }

    /// Wraps the stateful fake provider with the agent-registry lifecycle of
    /// the issue-87 empty-registry defect: while the agent is in reconnect
    /// backoff no node is registered, so `create_instance` reports NotFound —
    /// the command can provably never be delivered — and after `register()`
    /// (the agent re-registering on a later sweep tick) the fake behaves
    /// normally.
    struct NotFoundUntilRegisteredProvider {
        inner: FakeComputeProvider,
        registered: std::sync::atomic::AtomicBool,
    }

    impl NotFoundUntilRegisteredProvider {
        fn new() -> Self {
            Self {
                inner: FakeComputeProvider::new(),
                registered: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn register(&self) {
            self.registered
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn instance_count(&self) -> usize {
            self.inner.instance_count()
        }
    }

    #[async_trait::async_trait]
    impl o3k_provider::ComputeProvider for NotFoundUntilRegisteredProvider {
        async fn capabilities(
            &self,
        ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            if !self.registered.load(std::sync::atomic::Ordering::SeqCst) {
                // No agent is registered: `selected_agent` fails before any
                // dispatch, so the create command was never delivered.
                return Err(o3k_provider::ProviderError::NotFound);
            }
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: o3k_provider::InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// The empty-registry dispatch (issue #87): a create driven while no
    /// agent is registered — a preserved agent still in reconnect backoff —
    /// must NOT become terminal Failed. The command was provably never
    /// delivered, so the operation stays `Running` without a provider
    /// operation identity, the exact residue shape the create-convergence
    /// sweep re-drives; once an agent registers on a later sweep tick the
    /// create re-dispatches and converges to ACTIVE. The retry budget is
    /// never consumed by the empty-registry condition (no `retry_or_fail`).
    #[tokio::test]
    async fn create_dispatch_against_empty_registry_is_redriven_not_terminal()
    -> Result<(), ReconcileError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-empty-registry-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(NotFoundUntilRegisteredProvider::new());
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;

        // First sweep tick: the agent is not registered yet (reconnect
        // backoff), so the create cannot be delivered to any agent.
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Running
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.state, OperationState::Running);
        assert!(
            operation.provider_operation_id.is_none(),
            "an undelivered create must not carry a provider operation identity"
        );

        // A later sweep tick after the agent registered re-dispatches the
        // create and converges: the empty-registry condition must never
        // strand the server in a terminal error.
        provider.register();
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        assert_eq!(provider.instance_count(), 1);
        Ok(())
    }

    /// Wraps the stateful fake provider so every lifecycle action is
    /// rejected as an invalid request before any provider mutation, exactly
    /// what the agent provider reports for a genuinely invalid command
    /// (bad payload, unknown action). Pins the reconciler's terminalization
    /// of a real validation rejection: the issue-87 transport-stall
    /// reclassification must never weaken real validation.
    struct RejectingActionProvider {
        inner: FakeComputeProvider,
    }

    impl RejectingActionProvider {
        fn new(inner: FakeComputeProvider) -> Self {
            Self { inner }
        }
    }

    #[async_trait::async_trait]
    impl o3k_provider::ComputeProvider for RejectingActionProvider {
        async fn capabilities(
            &self,
        ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: o3k_provider::DeleteInstanceRequest,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            _provider_instance_id: &str,
            _action: o3k_provider::InstanceAction,
            _operation_id: Uuid,
            _idempotency_key: &str,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            Err(o3k_provider::ProviderError::InvalidRequest)
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// Issue #87 B2 invariant: a lifecycle dispatch rejected as
    /// `InvalidRequest` by the provider — a genuinely invalid command — must
    /// STILL terminalize as `Failed` with `invalid_request`, wiping no
    /// recovery path behind it. The transport-stall fix reclassifies the
    /// stall at the provider boundary; it must not turn real validation
    /// rejections into unknown outcomes.
    #[tokio::test]
    async fn rejected_lifecycle_action_still_terminalizes_as_invalid_request()
    -> Result<(), ReconcileError> {
        let (_, store, inner) = journal("rejected-action", 2).await?;
        let provider = Arc::new(RejectingActionProvider::new(inner.as_ref().clone()));
        let journal = OperationJournal::new(store.clone(), provider.clone(), 2);
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Reboot)
            .await?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Failed
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.state, OperationState::Failed);
        assert_eq!(
            operation.error_category.as_deref(),
            Some("invalid_request"),
            "a genuine validation rejection must keep its classified category"
        );
        assert_eq!(
            operation.error_message.as_deref(),
            Some("provider rejected the request")
        );
        assert!(
            operation.provider_operation_id.is_none(),
            "a rejected request never dispatches, so no provider identity exists"
        );
        // The resource stays ACTIVE: the rejected reboot never mutated it.
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "ACTIVE"
        );
        Ok(())
    }

    /// Issue #87 B2: a lifecycle operation already in `UnknownOutcome` (the
    /// state the transport-stall fix now produces) must ADOPT the agent's
    /// re-delivered terminal observation instead of staying stuck: when the
    /// stalled stream is restored and the agent replays the terminal
    /// observation it produced during the hold, `apply_agent_observation`
    /// promotes the operation to `Succeeded` and projects the resource
    /// ACTIVE — the durable inconsistency (failed operation + succeeded
    /// agent command + ACTIVE resource) never forms.
    #[tokio::test]
    async fn unknown_lifecycle_adopts_late_terminal_observation() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("adopt-late-observation", 2).await?;
        let request = request();
        let create_operation = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(create_operation).await?,
            OperationState::Succeeded
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        let operation_id = Uuid::now_v7();
        provider.set_failure(FailureInjection::Timeout)?;
        journal
            .begin_lifecycle(resource.id, operation_id, LifecycleAction::Reboot)
            .await?;
        bind_observation_command(&store, operation_id, resource.id, "agent-1", "epoch-1").await?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::UnknownOutcome
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::UnknownOutcome
        );
        provider.set_failure(FailureInjection::None)?;
        // The stream is restored; the agent re-delivers the terminal
        // observation it already produced during the hold (the reboot
        // executed: state Running/ACTIVE).
        journal
            .apply_agent_observation(&AgentObservation {
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                resource_id: resource.id,
                provider_resource_id: resource.provider_id.clone(),
                state: o3k_provider::InstanceState::Running,
                operation_id,
                operation_state: AgentOperationState::Succeeded,
                observation_sequence: 1,
                observed_at_unix_ms: 1,
                redacted_message: Some("rebooted".to_owned()),
                console_log_bytes: Vec::new(),
                console_log_offset: 0,
                console_log_complete: false,
                console_log_truncated: false,
                block_device: None,
            })
            .await?;
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(
            operation.state,
            OperationState::Succeeded,
            "the re-delivered terminal observation must be adopted by the unknown lifecycle"
        );
        assert!(
            operation.provider_operation_id.is_some(),
            "the adopted operation keeps its provider operation identity"
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "ACTIVE"
        );
        Ok(())
    }
}
