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
    DurableStore, ObservationUpdate, OperationRecord, OperationState, ProviderReference,
    ResourceRecord, StoreError, server_state_to_storage,
};
use thiserror::Error;
use uuid::Uuid;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceDisposition {
    New,
    Duplicate,
    Stale,
}

pub struct OperationJournal<S: ?Sized, P: ?Sized> {
    store: Arc<S>,
    provider: Arc<P>,
    max_attempts: u8,
    events: Arc<Mutex<Vec<JournalEvent>>>,
    agent_evidence: Arc<Mutex<HashMap<Uuid, AgentEvidenceFence>>>,
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
    ) -> Result<EvidenceDisposition, ReconcileError> {
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
        if let Some(registry) = &self.agent_registry {
            match registry.snapshot(agent_id).await {
                Some(node) if node.agent_epoch != agent_epoch => {
                    return Err(ReconcileError::StaleAgentEvidence);
                }
                None => return Err(ReconcileError::StaleAgentEvidence),
                Some(_) => {}
            }
        }
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
        match evidence.get(&operation_id) {
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
        }
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
            .insert_resource_and_operation(&resource, &operation)
            .await?;
        self.event(
            request.operation_id,
            request.o3k_server_id,
            JournalEventKind::IntentPersisted,
        );
        Ok(operation.id)
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
        let disposition = self
            .fence_agent_evidence(
                update.operation_id,
                &update.agent_id,
                &update.agent_epoch,
                update.operation_sequence,
                update.state,
                update.provider_resource_id.as_deref().unwrap_or(""),
            )
            .await?;
        if disposition != EvidenceDisposition::New {
            return Ok(operation.state);
        }
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        let durable_state = match update.state {
            AgentOperationState::Accepted | AgentOperationState::Running => OperationState::Running,
            AgentOperationState::Succeeded => OperationState::Succeeded,
            AgentOperationState::Failed => OperationState::Failed,
            AgentOperationState::UnknownOutcome => OperationState::UnknownOutcome,
        };
        if operation.state == OperationState::UnknownOutcome
            && matches!(durable_state, OperationState::Running)
        {
            return Err(ReconcileError::InvalidIntent);
        }
        let error_category = if durable_state == OperationState::Failed {
            Some(agent_error_category(update.error_category)?)
        } else {
            None
        };
        // Persist the agent-reported, contract-redacted failure reason so the
        // durable record carries an actionable cause; it is bounded and
        // sanitized before storage. Unknown-outcome and non-failure updates
        // keep no message.
        let error_message = (durable_state == OperationState::Failed).then(|| {
            bounded_agent_failure_message(update.redacted_message.as_deref().unwrap_or(""))
        });
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
        Ok(durable_state)
    }

    /// Commits an authenticated command acceptance before the agent executes
    /// the command. Duplicate acceptances are idempotent because the durable
    /// operation is simply kept in `running` state.
    pub async fn apply_agent_acceptance(
        &self,
        accepted: &AgentCommandAccepted,
    ) -> Result<OperationState, ReconcileError> {
        let operation = self.store.get_operation(accepted.operation_id).await?;
        let disposition = self
            .fence_agent_evidence(
                accepted.operation_id,
                &accepted.agent_id,
                &accepted.agent_epoch,
                accepted.operation_sequence,
                accepted.state,
                "",
            )
            .await?;
        if disposition != EvidenceDisposition::New {
            return Ok(operation.state);
        }
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        match accepted.state {
            AgentOperationState::Accepted | AgentOperationState::Running => {}
            _ => return Err(ReconcileError::InvalidIntent),
        }
        if operation.state == OperationState::UnknownOutcome {
            return Err(ReconcileError::InvalidIntent);
        }
        self.store
            .update_operation(
                accepted.operation_id,
                OperationState::Running,
                operation.provider_operation_id.as_deref(),
                operation.error_category.as_deref(),
                operation.error_message.as_deref(),
            )
            .await?;
        self.event(
            accepted.operation_id,
            operation.resource_id,
            JournalEventKind::ProviderStarted,
        );
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
        if observation.operation_state != AgentOperationState::Succeeded {
            tracing::debug!(
                operation_id=%observation.operation_id,
                operation_state=?observation.operation_state,
                "apply_agent_observation: operation_state is not Succeeded"
            );
            return Err(ReconcileError::InvalidIntent);
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
        if action == LifecycleAction::Delete
            && matches!(
                self.provider.get_instance(&provider_id).await,
                Err(ProviderError::NotFound)
            )
        {
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
        validate_provider_operation_owner(operation.id, &provider_operation)?;
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
                }
                Ok(OperationState::UnknownOutcome)
            }
            ProviderOperationState::Retryable => {
                self.retry_or_fail(operation.id, resource.id, ProviderError::Retryable)
                    .await
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
                validate_provider_operation_owner(operation.id, &provider_operation)?;
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
            return self.observe_unknown(operation, resource).await;
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
        Ok(OperationState::Running)
    }

    async fn finish(
        &self,
        operation_id: Uuid,
        resource: ResourceRecord,
        provider_operation_id: String,
        provider_resource_id: Option<String>,
    ) -> Result<OperationState, ReconcileError> {
        if let Some(provider_resource_id) = provider_resource_id.as_deref() {
            self.store
                .attach_provider_reference(&ProviderReference {
                    resource_id: resource.id,
                    provider_name: "compute".to_owned(),
                    provider_resource_id: provider_resource_id.to_owned(),
                })
                .await?;
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
        self.store
            .update_resource(
                resource.id,
                resource.generation,
                &resource.desired_state,
                server_state_to_storage(ServerState::Active),
                resource.generation,
                provider_resource_id.as_deref(),
            )
            .await?;
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
            self.store
                .update_operation(
                    operation_id,
                    OperationState::Failed,
                    None,
                    Some("retry_exhausted"),
                    Some(&error.to_string()),
                )
                .await?;
            self.event(operation_id, resource_id, JournalEventKind::Failed);
            Ok(OperationState::Failed)
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

fn valid_agent_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider::{FailureInjection, FakeComputeProvider};
    use o3k_store::testkit::TestStore;
    use o3k_store::{AgentCommandRecord, AgentCommandState};
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

    /// Minimal in-memory node registry used to simulate agent registration and
    /// re-registration. A re-registration replaces the stored epoch, mirroring
    /// `NodeRegistry::register` in o3k-compute-agent.
    #[derive(Clone, Default)]
    struct TestAgentRegistry {
        nodes: Arc<tokio::sync::RwLock<HashMap<String, o3k_provider::AgentNodeSnapshot>>>,
    }

    #[async_trait::async_trait]
    impl o3k_provider::AgentNodeRegistry for TestAgentRegistry {
        async fn all(&self) -> Vec<o3k_provider::AgentNodeSnapshot> {
            self.nodes.read().await.values().cloned().collect()
        }

        async fn snapshot(&self, agent_id: &str) -> Option<o3k_provider::AgentNodeSnapshot> {
            self.nodes.read().await.get(agent_id).cloned()
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
    async fn agent_evidence_rejects_foreign_and_stale_updates() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-evidence-fence", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
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
        assert_eq!(
            journal.apply_agent_update(&stale).await?,
            OperationState::Succeeded
        );
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

    #[tokio::test]
    async fn agent_observation_projects_nova_state_and_replays_without_mutation()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-observation", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
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
    async fn agent_failure_persists_the_contract_redacted_provider_reason()
    -> Result<(), ReconcileError> {
        // Contract: the agent redacts secrets and connection information
        // before sending; the control plane persists the reason bounded and
        // sanitized instead of withholding it entirely (issue #485).
        let (journal, store, _) = journal("agent-failure", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
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

    #[tokio::test]
    async fn retry_budget_becomes_visible_failure() -> Result<(), ReconcileError> {
        let (journal, store, provider) = journal("retry", 2).await?;
        provider.set_failure(FailureInjection::Transient)?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        assert_eq!(
            journal.reconcile_once(operation_id).await?,
            OperationState::Retryable
        );
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
}
