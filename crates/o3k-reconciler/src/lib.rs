use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use o3k_provider::{
    ComputeProvider, CreateInstanceRequest, OperationState as ProviderOperationState, ProviderError,
};
use o3k_provider_contract::compute_proto as agent_proto;
use o3k_store::{
    DurableStore, OperationRecord, OperationState, ProviderReference, ResourceRecord, StoreError,
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
            let observed_state = if operation.kind == "lifecycle:delete" {
                "DELETED"
            } else if operation.kind == "create" {
                "active"
            } else {
                resource.observed_state.as_str()
            };
            self.store
                .update_resource(
                    resource_id,
                    resource.generation,
                    &resource.desired_state,
                    observed_state,
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
            Ok(provider_operation) => match provider_operation.state {
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
            },
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
                    o3k_provider::InstanceState::Stopped => "STOPPED",
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
                self.finish(
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
        self.event(operation.id, resource.id, JournalEventKind::UnknownObserved);
        if provider_operation.state == ProviderOperationState::UnknownOutcome {
            if let Some(resource_id) = provider_operation.provider_resource_id {
                if self.provider.get_instance(&resource_id).await.is_ok() {
                    return self
                        .finish(
                            operation.id,
                            resource,
                            provider_id.to_owned(),
                            Some(resource_id),
                        )
                        .await;
                }
            }
            return Ok(OperationState::UnknownOutcome);
        }
        if provider_operation.state == ProviderOperationState::Succeeded {
            return self
                .finish(
                    operation.id,
                    resource,
                    provider_id.to_owned(),
                    provider_operation.provider_resource_id,
                )
                .await;
        }
        Ok(OperationState::Retryable)
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
            "active"
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

        provider.set_failure(FailureInjection::None)?;
        assert_eq!(
            journal.reconcile_lifecycle_once(operation_id).await?,
            OperationState::Succeeded
        );
        assert_eq!(
            store.get_resource(resource.id).await?.observed_state,
            "STOPPED"
        );
        Ok(())
    }
}
