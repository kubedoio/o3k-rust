use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use o3k_provider::{
    ComputeProvider, CreateInstanceRequest, OperationState as ProviderOperationState, ProviderError,
};
use o3k_provider_contract::compute_proto as agent_proto;
use o3k_store::{
    DurableStore, ObservationUpdate, OperationRecord, OperationState, ProviderReference,
    ResourceRecord, StoreError,
};
use thiserror::Error;
use uuid::Uuid;

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
}

pub struct OperationJournal<S, P: ?Sized> {
    store: Arc<S>,
    provider: Arc<P>,
    max_attempts: u8,
    attempts: Arc<Mutex<HashMap<Uuid, u8>>>,
    events: Arc<Mutex<Vec<JournalEvent>>>,
}

impl<S, P: ?Sized> Clone for OperationJournal<S, P> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            provider: self.provider.clone(),
            max_attempts: self.max_attempts,
            attempts: self.attempts.clone(),
            events: self.events.clone(),
        }
    }
}

impl<S, P: ?Sized> OperationJournal<S, P>
where
    S: DurableStore + 'static,
    P: ComputeProvider + 'static,
{
    pub fn new(store: Arc<S>, provider: Arc<P>, max_attempts: u8) -> Self {
        Self {
            store,
            provider,
            max_attempts: max_attempts.max(1),
            attempts: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(Vec::new())),
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
            observed_state: "requested".to_owned(),
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
    /// used by the provider reconciliation loop. Agent messages are deliberately
    /// not persisted: only a stable category is retained for operator safety.
    pub async fn apply_agent_update(
        &self,
        update: &agent_proto::OperationUpdate,
    ) -> Result<OperationState, ReconcileError> {
        let operation_id =
            Uuid::parse_str(&update.operation_id).map_err(|_| ReconcileError::InvalidIntent)?;
        let resource_id =
            Uuid::parse_str(&update.resource_id).map_err(|_| ReconcileError::InvalidIntent)?;
        let operation = self.store.get_operation(operation_id).await?;
        if operation.resource_id != resource_id {
            return Err(ReconcileError::InvalidIntent);
        }
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }

        let state = agent_proto::OperationState::try_from(update.state)
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let durable_state = match state {
            agent_proto::OperationState::Accepted | agent_proto::OperationState::Running => {
                OperationState::Running
            }
            agent_proto::OperationState::Succeeded => OperationState::Succeeded,
            agent_proto::OperationState::Failed => OperationState::Failed,
            agent_proto::OperationState::UnknownOutcome => OperationState::UnknownOutcome,
            agent_proto::OperationState::Unspecified => return Err(ReconcileError::InvalidIntent),
        };
        let error_category = if durable_state == OperationState::Failed {
            Some(agent_error_category(update.error_category)?)
        } else {
            None
        };
        let provider_operation_id = operation.provider_operation_id.as_deref();
        self.store
            .update_operation(
                operation_id,
                durable_state,
                provider_operation_id,
                error_category,
                (durable_state == OperationState::Failed).then_some("agent operation failed"),
            )
            .await?;

        if durable_state == OperationState::Succeeded {
            let resource = self.store.get_resource(resource_id).await?;
            let provider_id = (!update.provider_resource_id.is_empty())
                .then_some(update.provider_resource_id.as_str())
                .or(resource.provider_id.as_deref());
            if let Some(provider_resource_id) = provider_id {
                match self
                    .store
                    .get_provider_reference(resource_id, "compute-agent")
                    .await
                {
                    Ok(existing) if existing.provider_resource_id == provider_resource_id => {}
                    Ok(_) => return Err(StoreError::ProviderReferenceAlreadyExists.into()),
                    Err(StoreError::ProviderReferenceNotFound) => {
                        self.store
                            .attach_provider_reference(&ProviderReference {
                                resource_id,
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
                    resource_id,
                    resource.generation,
                    &resource.desired_state,
                    &resource.observed_state,
                    resource.generation,
                    provider_id,
                )
                .await?;
        }
        self.event(
            operation_id,
            resource_id,
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
        accepted: &agent_proto::CommandAccepted,
    ) -> Result<OperationState, ReconcileError> {
        let operation_id =
            Uuid::parse_str(&accepted.operation_id).map_err(|_| ReconcileError::InvalidIntent)?;
        let operation = self.store.get_operation(operation_id).await?;
        if matches!(
            operation.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            return Ok(operation.state);
        }
        match agent_proto::OperationState::try_from(accepted.state)
            .map_err(|_| ReconcileError::InvalidIntent)?
        {
            agent_proto::OperationState::Accepted | agent_proto::OperationState::Running => {}
            _ => return Err(ReconcileError::InvalidIntent),
        }
        self.store
            .update_operation(
                operation_id,
                OperationState::Running,
                operation.provider_operation_id.as_deref(),
                operation.error_category.as_deref(),
                operation.error_message.as_deref(),
            )
            .await?;
        self.event(
            operation_id,
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
        observation: &agent_proto::Observation,
    ) -> Result<(), ReconcileError> {
        let operation_id = Uuid::parse_str(&observation.operation_id)
            .map_err(|_| ReconcileError::InvalidIntent)?;
        let resource_id =
            Uuid::parse_str(&observation.resource_id).map_err(|_| ReconcileError::InvalidIntent)?;
        let operation = self.store.get_operation(operation_id).await?;
        if operation.resource_id != resource_id {
            return Err(ReconcileError::InvalidIntent);
        }
        let operation_state = agent_proto::OperationState::try_from(observation.operation_state)
            .map_err(|_| ReconcileError::InvalidIntent)?;
        if operation_state != agent_proto::OperationState::Succeeded {
            return Err(ReconcileError::InvalidIntent);
        }
        let observed_state = agent_resource_state(observation.state)?;
        let resource = self.store.get_resource(resource_id).await?;
        let provider_id = (!observation.provider_resource_id.is_empty())
            .then_some(observation.provider_resource_id.as_str())
            .or(resource.provider_id.as_deref());
        if let Some(provider_resource_id) = provider_id {
            match self
                .store
                .get_provider_reference(resource_id, "compute-agent")
                .await
            {
                Ok(existing) if existing.provider_resource_id == provider_resource_id => {}
                Ok(_) => return Err(StoreError::ProviderReferenceAlreadyExists.into()),
                Err(StoreError::ProviderReferenceNotFound) => {
                    self.store
                        .attach_provider_reference(&ProviderReference {
                            resource_id,
                            provider_name: "compute-agent".to_owned(),
                            provider_resource_id: provider_resource_id.to_owned(),
                        })
                        .await?;
                }
                Err(error) => return Err(error.into()),
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
            .update_resource_from_observation(resource_id, &update)
            .await?;
        if updated.generation == resource.generation {
            return Ok(());
        }
        self.event(operation_id, resource_id, JournalEventKind::UnknownObserved);
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
                        Some(match error.category() {
                            o3k_provider::ErrorCategory::InvalidRequest => "invalid_request",
                            o3k_provider::ErrorCategory::NotFound => "not_found",
                            o3k_provider::ErrorCategory::Conflict => "conflict",
                            o3k_provider::ErrorCategory::Capacity => "capacity",
                            o3k_provider::ErrorCategory::Retryable => "retryable",
                            o3k_provider::ErrorCategory::UnknownOutcome => "unknown_outcome",
                            o3k_provider::ErrorCategory::Terminal => "terminal",
                        }),
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
            "DELETED".to_owned()
        } else {
            match self.provider.get_instance(&provider_id).await {
                Ok(instance) => match instance.state {
                    o3k_provider::InstanceState::Running => "ACTIVE",
                    o3k_provider::InstanceState::Stopped => "SHUTOFF",
                    o3k_provider::InstanceState::Creating => "BUILD",
                    o3k_provider::InstanceState::Deleting => "DELETING",
                    o3k_provider::InstanceState::Deleted => "DELETED",
                    o3k_provider::InstanceState::Error => "ERROR",
                }
                .to_owned(),
                Err(ProviderError::NotFound) if action == LifecycleAction::Delete => {
                    "DELETED".to_owned()
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
        match self.provider.create_instance(request).await {
            Ok(provider_operation) => {
                validate_provider_operation_owner(operation_id, &provider_operation)?;
                self.finish_create(
                    operation_id,
                    resource,
                    provider_operation.provider_operation_id.to_string(),
                    provider_operation.provider_resource_id,
                )
                .await
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
                Ok(OperationState::UnknownOutcome)
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

    async fn finish_create(
        &self,
        operation_id: Uuid,
        resource: ResourceRecord,
        provider_operation_id: String,
        provider_resource_id: Option<String>,
    ) -> Result<OperationState, ReconcileError> {
        let provider_resource_id = provider_resource_id.ok_or(ReconcileError::InvalidIntent)?;
        let instance = self.provider.get_instance(&provider_resource_id).await?;
        let observed_state = match instance.state {
            o3k_provider::InstanceState::Running => "active",
            o3k_provider::InstanceState::Creating => "BUILD",
            o3k_provider::InstanceState::Stopped => "SHUTOFF",
            o3k_provider::InstanceState::Deleting => "DELETING",
            o3k_provider::InstanceState::Deleted => "DELETED",
            o3k_provider::InstanceState::Error => "ERROR",
        };
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
                "active",
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
        let attempts = {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|_| ReconcileError::RetryExhausted)?;
            let value = attempts.entry(operation_id).or_insert(0);
            *value = value.saturating_add(1);
            *value
        };
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

fn agent_error_category(value: i32) -> Result<&'static str, ReconcileError> {
    let category =
        agent_proto::ErrorCategory::try_from(value).map_err(|_| ReconcileError::InvalidIntent)?;
    match category {
        agent_proto::ErrorCategory::InvalidRequest => Ok("invalid_request"),
        agent_proto::ErrorCategory::Unauthenticated => Ok("unauthenticated"),
        agent_proto::ErrorCategory::Unauthorized => Ok("unauthorized"),
        agent_proto::ErrorCategory::Conflict => Ok("conflict"),
        agent_proto::ErrorCategory::Capacity => Ok("capacity"),
        agent_proto::ErrorCategory::NotFound => Ok("not_found"),
        agent_proto::ErrorCategory::Retryable => Ok("retryable"),
        agent_proto::ErrorCategory::UnknownOutcome => Ok("unknown_outcome"),
        agent_proto::ErrorCategory::Terminal => Ok("terminal"),
        agent_proto::ErrorCategory::Unspecified => Err(ReconcileError::InvalidIntent),
    }
}

fn agent_resource_state(value: i32) -> Result<&'static str, ReconcileError> {
    match agent_proto::ResourceState::try_from(value).map_err(|_| ReconcileError::InvalidIntent)? {
        agent_proto::ResourceState::Creating => Ok("BUILD"),
        agent_proto::ResourceState::Running => Ok("ACTIVE"),
        agent_proto::ResourceState::Stopped => Ok("SHUTOFF"),
        agent_proto::ResourceState::Deleting => Ok("DELETING"),
        agent_proto::ResourceState::Deleted => Ok("DELETED"),
        agent_proto::ResourceState::Error => Ok("ERROR"),
        agent_proto::ResourceState::Unspecified => Err(ReconcileError::InvalidIntent),
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

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider::{FailureInjection, FakeComputeProvider};
    use o3k_store::SqliteStore;
    use std::path::PathBuf;

    fn request() -> CreateInstanceRequest {
        CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project".to_owned(),
            name: "journal-test".to_owned(),
            vcpus: 1,
            memory_mib: 128,
            image_id: None,
            network_ids: Vec::new(),
            placement_provider_id: None,
            placement_allocation_id: None,
            idempotency_key: "journal-test-key".to_owned(),
        }
    }

    async fn journal(
        label: &str,
        max_attempts: u8,
    ) -> Result<
        (
            OperationJournal<SqliteStore, FakeComputeProvider>,
            Arc<SqliteStore>,
            Arc<FakeComputeProvider>,
        ),
        ReconcileError,
    > {
        let path = PathBuf::from(format!(
            "/tmp/o3k-reconciler-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(SqliteStore::connect_file(&path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        Ok((
            OperationJournal::new(store.clone(), provider.clone(), max_attempts),
            store,
            provider,
        ))
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
            "active"
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
        let store = Arc::new(SqliteStore::connect_file(&path).await?);
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
        let store = Arc::new(SqliteStore::connect_file(&path).await?);
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
    async fn agent_success_is_durable_and_idempotent() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-success", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let update = o3k_provider_contract::compute_proto::OperationUpdate {
            operation_id: operation_id.to_string(),
            resource_id: request.o3k_server_id.to_string(),
            state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            provider_resource_id: "agent-domain-1".to_owned(),
            ..Default::default()
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
            "requested"
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
    async fn agent_observation_projects_nova_state_and_replays_without_mutation()
    -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-observation", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let observation = o3k_provider_contract::compute_proto::Observation {
            resource_id: request.o3k_server_id.to_string(),
            provider_resource_id: "agent-domain-stopped".to_owned(),
            operation_id: operation_id.to_string(),
            operation_state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            state: o3k_provider_contract::compute_proto::ResourceState::Stopped as i32,
            ..Default::default()
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
        let active = o3k_provider_contract::compute_proto::Observation {
            agent_id: "compute-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id.to_string(),
            provider_resource_id: "agent-domain".to_owned(),
            operation_id: operation_id.to_string(),
            operation_state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            state: o3k_provider_contract::compute_proto::ResourceState::Running as i32,
            observation_sequence: 10,
            ..Default::default()
        };
        journal.apply_agent_observation(&active).await?;
        let stale = o3k_provider_contract::compute_proto::Observation {
            state: o3k_provider_contract::compute_proto::ResourceState::Stopped as i32,
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
    async fn agent_observation_rejects_unspecified_resource_state() -> Result<(), ReconcileError> {
        let (journal, _, _) = journal("agent-observation-invalid", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let observation = o3k_provider_contract::compute_proto::Observation {
            resource_id: request.o3k_server_id.to_string(),
            operation_id: operation_id.to_string(),
            operation_state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            ..Default::default()
        };
        assert!(matches!(
            journal.apply_agent_observation(&observation).await,
            Err(ReconcileError::InvalidIntent)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn agent_failure_persists_category_without_provider_message() -> Result<(), ReconcileError>
    {
        let (journal, store, _) = journal("agent-failure", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let update = o3k_provider_contract::compute_proto::OperationUpdate {
            operation_id: operation_id.to_string(),
            resource_id: request.o3k_server_id.to_string(),
            state: o3k_provider_contract::compute_proto::OperationState::Failed as i32,
            error_category: o3k_provider_contract::compute_proto::ErrorCategory::Terminal as i32,
            redacted_message: "secret-provider-detail".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            journal.apply_agent_update(&update).await?,
            OperationState::Failed
        );
        let operation = store.get_operation(operation_id).await?;
        assert_eq!(operation.error_category.as_deref(), Some("terminal"));
        assert_eq!(
            operation.error_message.as_deref(),
            Some("agent operation failed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn command_acceptance_is_durable_and_idempotent() -> Result<(), ReconcileError> {
        let (journal, store, _) = journal("agent-acceptance", 2).await?;
        let request = request();
        let operation_id = journal.begin_create("project", &request).await?;
        let accepted = agent_proto::CommandAccepted {
            command_id: "command-1".to_owned(),
            operation_id: operation_id.to_string(),
            state: agent_proto::OperationState::Accepted as i32,
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
            3
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
            "active"
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
}
