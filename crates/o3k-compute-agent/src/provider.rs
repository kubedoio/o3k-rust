//! Application-side compute provider that dispatches all host lifecycle
//! commands through a selected, authenticated agent connection.
//!
//! This module is the transport adapter for compute execution: it converts
//! `o3k_provider::ComputeProvider` calls into wire commands, drives artifact
//! offers over the fenced control stream, and projects authenticated agent
//! events back into the provider vocabulary. The durable O3K journal remains
//! the recovery authority; the in-memory projection here only mirrors it.
//!
//! The control plane constructs a `ComputeProvider` from this adapter at the
//! composition root; application services depend only on the
//! `o3k_provider::ComputeProvider` port.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use o3k_domain::ServerState;
use o3k_provider::{
    BlockDeviceAttachment, BlockDeviceObservation, Capabilities, ComputeProvider, ConnectorInfo,
    CreateArtifactResolver, CreateInstanceRequest, DeleteInstanceRequest, Instance, InstanceAction,
    Operation, ProviderError, ResolvedCreateResolver, UnconfiguredCreateArtifactResolver,
};
use o3k_provider_contract::compute_proto as agent_proto;
use o3k_store::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferState,
    ArtifactTransferUpdate, ComputeRepository, StoreError, server_state_from_storage,
};
use prost::Message;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    AgentError, Availability, BlockDeviceCommand, CreateCommandSpec, LifecycleCommand,
    MAX_ARTIFACT_CHUNK_BYTES, NodeRegistry, NodeSnapshot, agent_snapshot,
    build_block_device_command, build_create_command, build_lifecycle_command,
    deterministic_artifact_transfer_id,
};
#[derive(Debug, Clone)]
pub(crate) struct AgentBinding {
    resource_id: String,
    agent_id: String,
    agent_epoch: String,
    provider_resource_id: Option<String>,
}

#[derive(Default)]
pub(crate) struct AgentProviderState {
    /// In-memory projection of durable agent operations; the journal is the
    /// recovery authority.
    pub(crate) operations: HashMap<Uuid, Operation>,
    instances: HashMap<String, Instance>,
    bindings: HashMap<String, AgentBinding>,
}

/// ComputeProvider adapter that sends all host lifecycle commands through a
/// selected, authenticated NodeRegistry connection.
///
/// The adapter deliberately returns an accepted provider operation after the
/// command is put on the fenced control stream. Later operation and
/// observation events update the adapter's projection; the durable O3K
/// journal remains the recovery authority.
#[derive(Clone)]
pub struct AgentComputeProvider {
    pub(crate) registry: NodeRegistry,
    pub(crate) resolver: Arc<dyn ResolvedCreateResolver>,
    pub(crate) artifact_resolver: Arc<dyn CreateArtifactResolver>,
    pub(crate) state: Arc<RwLock<AgentProviderState>>,
    pub(crate) store: Option<Arc<dyn ComputeRepository>>,
    pub(crate) command_timeout: Duration,
}

impl AgentComputeProvider {
    #[must_use]
    pub fn new(registry: NodeRegistry, resolver: Arc<dyn ResolvedCreateResolver>) -> Self {
        Self::new_with_store(registry, resolver, None)
    }

    #[must_use]
    pub fn new_with_store(
        registry: NodeRegistry,
        resolver: Arc<dyn ResolvedCreateResolver>,
        store: Option<Arc<dyn ComputeRepository>>,
    ) -> Self {
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        let mut events = registry.subscribe_events();
        let event_state = state.clone();
        let event_store = store.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        apply_agent_provider_event(&event_state, event_store.as_deref(), event)
                            .await
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        tracing::warn!(
                            "agent provider event projection lagged; durable recovery is required"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Self {
            registry,
            resolver,
            artifact_resolver: Arc::new(UnconfiguredCreateArtifactResolver),
            state,
            store,
            command_timeout: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Resolves the fenced agent bound to a server resource.
    pub async fn agent_for_server(
        &self,
        resource_id: Uuid,
    ) -> Result<(NodeSnapshot, String), ProviderError> {
        let binding = {
            let state = self.state.read().await;
            state
                .bindings
                .values()
                .find(|binding| binding.resource_id == resource_id.to_string())
                .cloned()
        };
        let binding = binding.ok_or(ProviderError::NotFound)?;
        let agent = self.selected_agent(&binding.agent_id).await?;
        if agent.agent_epoch != binding.agent_epoch {
            return Err(ProviderError::StaleState);
        }
        Ok((agent, binding.resource_id))
    }

    /// Persists and dispatches a block-device command, waiting for the bound
    /// observation. A timeout is an unknown outcome requiring observation
    /// before retry.
    pub async fn dispatch_block_device_and_wait(
        &self,
        command: agent_proto::Command,
        operation_id: Uuid,
        timeout: Duration,
    ) -> Result<o3k_provider::BlockDeviceObservation, ProviderError> {
        self.persist_pending_command(&command, operation_id).await?;
        self.registry
            .dispatch_command_and_wait(command, timeout)
            .await
            .map_err(|error| match error {
                AgentError::Protocol(message) if message.contains("observation timed out") => {
                    ProviderError::UnknownOutcome { operation_id }
                }
                other => map_agent_error(other),
            })?
            .block_device
            .ok_or(ProviderError::Terminal)
    }

    #[must_use]
    pub fn with_artifact_resolver(mut self, resolver: Arc<dyn CreateArtifactResolver>) -> Self {
        self.artifact_resolver = resolver.clone();
        if let Some(recovery_store) = self.store.clone() {
            let recovery_registry = self.registry.clone();
            let recovery_resolver = self.resolver.clone();
            let recovery_timeout = self.command_timeout;
            tokio::spawn(async move {
                recover_artifact_transfers(
                    recovery_registry,
                    recovery_resolver,
                    resolver,
                    recovery_store,
                    recovery_timeout,
                )
                .await;
            });
        }
        self
    }

    /// Rebuilds the provider's volatile instance/binding projection from the
    /// durable resource ledger after a daemon restart or agent reconnect.
    /// Provider references and the currently authenticated agent epoch are
    /// both required; stale or conflicting ownership evidence is rejected.
    pub async fn rehydrate(&self) -> Result<(), ProviderError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let resources = store
            .list_resources_by_kind("compute_instance")
            .await
            .map_err(|_| ProviderError::Retryable)?;
        let mut instances = HashMap::new();
        let mut bindings = HashMap::new();
        for resource in resources {
            if matches!(
                server_state_from_storage(&resource.observed_state),
                Ok(ServerState::Deleted)
            ) {
                continue;
            }
            let Some(provider_id) = resource.provider_id.clone() else {
                continue;
            };
            let reference = store
                .get_provider_reference(resource.id, "agent")
                .await
                .map_err(|_| ProviderError::Conflict)?;
            if reference.provider_resource_id != provider_id {
                return Err(ProviderError::Conflict);
            }
            let request: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
                .map_err(|_| ProviderError::Conflict)?;
            let Some(agent_id) = request.placement_provider_id.as_deref() else {
                return Err(ProviderError::Conflict);
            };
            let Some(agent) = self.registry.snapshot(agent_id).await else {
                continue;
            };
            if agent.agent_epoch.trim().is_empty() {
                return Err(ProviderError::Conflict);
            }
            let state = instance_state_from_observed(&resource.observed_state)
                .ok_or(ProviderError::Conflict)?;
            instances.insert(
                provider_id.clone(),
                Instance {
                    provider_instance_id: provider_id.clone(),
                    o3k_server_id: resource.id,
                    state,
                    observed_message: None,
                },
            );
            bindings.insert(
                provider_id,
                AgentBinding {
                    resource_id: resource.id.to_string(),
                    agent_id: agent.agent_id,
                    agent_epoch: agent.agent_epoch,
                    provider_resource_id: resource.provider_id,
                },
            );
        }
        let mut state = self.state.write().await;
        state
            .instances
            .retain(|provider_id, _| instances.contains_key(provider_id));
        state
            .bindings
            .retain(|provider_id, _| bindings.contains_key(provider_id));
        state.instances.extend(instances);
        state.bindings.extend(bindings);
        Ok(())
    }

    async fn selected_agent(&self, provider_id: &str) -> Result<NodeSnapshot, ProviderError> {
        let snapshot = self
            .registry
            .snapshot(provider_id)
            .await
            .ok_or(ProviderError::NotFound)?;
        if snapshot.availability != Availability::Available {
            return Err(ProviderError::Retryable);
        }
        if snapshot.desired_state != agent_proto::AdministrativeState::Enabled as i32 {
            return Err(ProviderError::Retryable);
        }
        Ok(snapshot)
    }

    async fn dispatch(
        &self,
        command: agent_proto::Command,
        operation_id: Uuid,
    ) -> Result<(), ProviderError> {
        self.registry
            .dispatch_command(command)
            .await
            .map_err(|error| match error {
                AgentError::Protocol(message)
                    if message
                        .to_ascii_lowercase()
                        .contains("deadline has expired") =>
                {
                    // A durably recorded command whose embedded deadline
                    // expired during a re-dispatch may already have been
                    // delivered and executed while the control stream was
                    // stalled: the outcome is unknown, never a rejected
                    // request (issue #87 B2 — the agent accepted and
                    // executed the reboot while the acceptance and terminal
                    // observation were dropped). The reconciler's
                    // UnknownOutcome path observes the operation before
                    // retrying and adopts the re-delivered terminal evidence
                    // instead of terminalizing the operation.
                    ProviderError::UnknownOutcome { operation_id }
                }
                other => map_agent_error(other),
            })
    }

    async fn accepted_operation(&self, operation_id: Uuid) -> Result<Operation, ProviderError> {
        let operation = Operation {
            provider_operation_id: operation_id,
            o3k_operation_id: operation_id,
            state: o3k_provider::OperationState::Accepted,
            error_category: None,
            provider_resource_id: None,
        };
        self.state
            .write()
            .await
            .operations
            .insert(operation_id, operation.clone());
        Ok(operation)
    }

    async fn dispatch_recorded(
        &self,
        command: agent_proto::Command,
        operation_id: Uuid,
    ) -> Result<Operation, ProviderError> {
        if let Some(store) = &self.store {
            if let Ok(existing) = store.get_agent_command_by_operation(operation_id).await {
                // A repeated reconcile pass for the same operation must reuse
                // the durable command payload: rebuilding it would drift the
                // embedded deadline and break the agent journal's idempotent
                // replay with an identity conflict.
                if matches!(
                    existing.state,
                    AgentCommandState::Succeeded | AgentCommandState::Failed
                ) {
                    return self.get_operation(operation_id).await;
                }
                let recorded = agent_proto::Command::decode(existing.payload.as_slice())
                    .map_err(|_| ProviderError::Storage)?;
                return self.dispatch_accepted(recorded, operation_id).await;
            }
            let record = AgentCommandRecord {
                command_id: command.command_id.clone(),
                idempotency_key: command.idempotency_key.clone(),
                operation_id,
                resource_id: Uuid::parse_str(&command.resource_id)
                    .map_err(|_| ProviderError::InvalidRequest)?,
                agent_id: command.agent_id.clone(),
                agent_epoch: command.agent_epoch.clone(),
                payload_fingerprint_sha256: command.payload_fingerprint_sha256.clone(),
                payload: command.encode_to_vec(),
                state: AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: Some(operation_id.to_string()),
                provider_resource_id: None,
            };
            store
                .insert_agent_command(&record)
                .await
                .map_err(|_| ProviderError::Conflict)?;
        }
        self.dispatch_accepted(command, operation_id).await
    }

    async fn dispatch_accepted(
        &self,
        command: agent_proto::Command,
        operation_id: Uuid,
    ) -> Result<Operation, ProviderError> {
        let operation = self.accepted_operation(operation_id).await?;
        if let Err(error) = self.dispatch(command, operation_id).await {
            self.state.write().await.operations.remove(&operation_id);
            return Err(error);
        }
        Ok(operation)
    }

    pub async fn persist_pending_command(
        &self,
        command: &agent_proto::Command,
        operation_id: Uuid,
    ) -> Result<Option<AgentCommandRecord>, ProviderError> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let resource_id =
            Uuid::parse_str(&command.resource_id).map_err(|_| ProviderError::InvalidRequest)?;
        crate::persist_command_record(store.as_ref(), command, operation_id, resource_id)
            .await
            .map_err(|_| ProviderError::Conflict)
    }
}

async fn recover_artifact_transfers(
    registry: NodeRegistry,
    resolver: Arc<dyn ResolvedCreateResolver>,
    artifact_resolver: Arc<dyn CreateArtifactResolver>,
    store: Arc<dyn ComputeRepository>,
    timeout: Duration,
) {
    let transfers = match store.list_recoverable_artifact_transfers().await {
        Ok(transfers) => transfers,
        Err(error) => {
            tracing::warn!(%error, "artifact transfer recovery listing failed");
            return;
        }
    };
    for transfer in transfers {
        if transfer.expires_at_unix_ms <= unix_ms() {
            if let Err(error) = store
                .update_artifact_transfer(
                    &transfer.transfer_id,
                    &transfer.agent_epoch,
                    ArtifactTransferUpdate {
                        state: ArtifactTransferState::Expired,
                        contiguous_bytes: transfer.contiguous_bytes,
                        next_chunk_index: transfer.next_chunk_index,
                        retry_count: transfer.retry_count,
                    },
                )
                .await
            {
                tracing::warn!(
                    transfer_id = %transfer.transfer_id,
                    %error,
                    "expired artifact transfer could not be fenced"
                );
            }
            continue;
        }
        let Some(agent) = registry.snapshot(&transfer.agent_id).await else {
            continue;
        };
        let transfer = if agent.agent_epoch != transfer.agent_epoch {
            match store
                .rebind_artifact_transfer_epoch(
                    &transfer.transfer_id,
                    &transfer.agent_epoch,
                    &agent.agent_epoch,
                )
                .await
            {
                Ok(transfer) => transfer,
                Err(error) => {
                    tracing::warn!(
                        transfer_id = %transfer.transfer_id,
                        %error,
                        "artifact transfer recovery could not rebind agent epoch"
                    );
                    continue;
                }
            }
        } else {
            transfer
        };
        let resource = match store.get_resource(transfer.resource_id).await {
            Ok(resource) => resource,
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer intent missing");
                continue;
            }
        };
        let request: CreateInstanceRequest = match serde_json::from_str(&resource.desired_state) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer intent is invalid");
                continue;
            }
        };
        let app_agent = agent_snapshot(&agent);
        let inputs = match resolver.resolve(&request, &app_agent).await {
            Ok(inputs) => inputs,
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer source recovery failed");
                continue;
            }
        };
        let artifacts = match artifact_resolver
            .resolve_artifacts(&request, &app_agent, &inputs)
            .await
        {
            Ok(artifacts) => artifacts,
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer bytes recovery failed");
                continue;
            }
        };
        let Some(artifact) = artifacts.into_iter().find(|artifact| {
            artifact.artifact_id == transfer.artifact_id
                && artifact.sha256 == transfer.sha256
                && artifact.format == transfer.format
                && artifact_kind_name(artifact.kind) == transfer.artifact_kind
        }) else {
            tracing::warn!(transfer_id = %transfer.transfer_id, "artifact transfer identity has no matching source");
            continue;
        };
        let kind = match transfer.artifact_kind.as_str() {
            "image_base" => agent_proto::ArtifactKind::ImageBase,
            "config_drive_iso" => agent_proto::ArtifactKind::ConfigDriveIso,
            _ => continue,
        };
        let offer = agent_proto::ArtifactOffer {
            transfer_id: transfer.transfer_id.clone(),
            command_id: transfer.command_id.clone(),
            operation_id: transfer.operation_id.to_string(),
            resource_id: transfer.resource_id.to_string(),
            agent_id: transfer.agent_id.clone(),
            artifact_id: transfer.artifact_id.clone(),
            kind: kind as i32,
            sha256: transfer.sha256.clone(),
            size_bytes: transfer.size_bytes,
            format: transfer.format.clone(),
            chunk_size_bytes: transfer.chunk_size_bytes as u32,
            chunk_count: transfer.chunk_count as u32,
            expires_at_unix_ms: transfer.expires_at_unix_ms,
        };
        let Ok(start_chunk_index) = u32::try_from(transfer.next_chunk_index) else {
            tracing::warn!(transfer_id = %transfer.transfer_id, "artifact transfer resume index is invalid");
            continue;
        };
        match registry
            .dispatch_artifact_and_wait_from(offer, artifact.bytes, start_chunk_index, timeout)
            .await
        {
            Ok(_) => {
                let _ = store
                    .update_artifact_transfer(
                        &transfer.transfer_id,
                        &transfer.agent_epoch,
                        ArtifactTransferUpdate {
                            state: ArtifactTransferState::Committed,
                            contiguous_bytes: transfer.size_bytes,
                            next_chunk_index: transfer.chunk_count,
                            retry_count: transfer.retry_count,
                        },
                    )
                    .await;
            }
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer recovery dispatch failed")
            }
        }
    }
}

fn map_agent_error(error: AgentError) -> ProviderError {
    match error {
        AgentError::Protocol(message) => {
            let message = message.to_ascii_lowercase();
            if message.contains("unavailable")
                || message.contains("closed")
                || message.contains("timeout")
            {
                ProviderError::Retryable
            } else if message.contains("fenced") || message.contains("not registered") {
                ProviderError::StaleState
            } else {
                ProviderError::InvalidRequest
            }
        }
        AgentError::Transport(_)
        | AgentError::IdentityStore(_)
        | AgentError::TlsMaterial
        | AgentError::InvalidConfiguration(_) => ProviderError::Retryable,
    }
}

fn artifact_kind_name(kind: o3k_provider::ArtifactKind) -> &'static str {
    match kind {
        o3k_provider::ArtifactKind::ImageBase => "image_base",
        o3k_provider::ArtifactKind::ConfigDriveIso => "config_drive_iso",
    }
}

fn unix_ms_after(duration: Duration) -> i64 {
    unix_ms().saturating_add(duration.as_millis().min(i64::MAX as u128) as i64)
}

fn unix_ms() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis().min(i64::MAX as u128) as i64
}

fn agent_operation_state(
    state: o3k_provider::AgentOperationState,
) -> Option<o3k_provider::OperationState> {
    use o3k_provider::AgentOperationState as AgentState;
    Some(match state {
        AgentState::Accepted => o3k_provider::OperationState::Accepted,
        AgentState::Running => o3k_provider::OperationState::Running,
        AgentState::Succeeded => o3k_provider::OperationState::Succeeded,
        AgentState::Failed => o3k_provider::OperationState::Failed,
        AgentState::UnknownOutcome => o3k_provider::OperationState::UnknownOutcome,
    })
}

fn agent_error_category(
    category: Option<o3k_provider::AgentErrorCategory>,
) -> Option<o3k_provider::ErrorCategory> {
    use o3k_provider::AgentErrorCategory as AgentCategory;
    category.and_then(|category| match category {
        AgentCategory::InvalidRequest => Some(o3k_provider::ErrorCategory::InvalidRequest),
        AgentCategory::Conflict => Some(o3k_provider::ErrorCategory::Conflict),
        AgentCategory::Capacity => Some(o3k_provider::ErrorCategory::Capacity),
        AgentCategory::NotFound => Some(o3k_provider::ErrorCategory::NotFound),
        AgentCategory::Retryable => Some(o3k_provider::ErrorCategory::Retryable),
        AgentCategory::UnknownOutcome => Some(o3k_provider::ErrorCategory::UnknownOutcome),
        AgentCategory::Terminal => Some(o3k_provider::ErrorCategory::Terminal),
        AgentCategory::Unauthenticated | AgentCategory::Unauthorized => None,
    })
}

/// Decodes a durable observed value into the provider's own instance-state
/// vocabulary for the agent provider's rehydrate projection. The durable
/// value is decoded through the canonical fail-closed store decoder first, so
/// the provider vocabulary is a projection of the canonical model rather than
/// a second decoder of persisted strings.
pub(crate) fn instance_state_from_observed(value: &str) -> Option<o3k_provider::InstanceState> {
    let state = server_state_from_storage(value).ok()?;
    match state {
        ServerState::Requested
        | ServerState::Building
        | ServerState::Stopping
        | ServerState::Starting
        | ServerState::Rebooting => Some(o3k_provider::InstanceState::Creating),
        ServerState::Active => Some(o3k_provider::InstanceState::Running),
        ServerState::Stopped => Some(o3k_provider::InstanceState::Stopped),
        ServerState::Deleting => Some(o3k_provider::InstanceState::Deleting),
        ServerState::Deleted => Some(o3k_provider::InstanceState::Deleted),
        ServerState::Error => Some(o3k_provider::InstanceState::Error),
    }
}

pub(crate) async fn apply_artifact_status(
    store: &dyn ComputeRepository,
    status: &o3k_provider::AgentArtifactStatus,
) -> Result<(), StoreError> {
    let transfer = store.get_artifact_transfer(&status.transfer_id).await?;
    if transfer.command_id != status.command_id
        || transfer.operation_id != status.operation_id
        || transfer.resource_id != status.resource_id
        || transfer.agent_id != status.agent_id
    {
        return Err(StoreError::ArtifactTransferConflict(
            "artifact status identity conflicts with durable state".to_owned(),
        ));
    }
    let state = match status.state {
        o3k_provider::ArtifactTransferState::Offered => ArtifactTransferState::Offered,
        o3k_provider::ArtifactTransferState::Receiving => ArtifactTransferState::Receiving,
        o3k_provider::ArtifactTransferState::Committed => ArtifactTransferState::Committed,
        o3k_provider::ArtifactTransferState::Rejected => ArtifactTransferState::Rejected,
        o3k_provider::ArtifactTransferState::Expired => ArtifactTransferState::Expired,
    };
    if transfer.agent_epoch != status.agent_epoch {
        if matches!(
            transfer.state,
            ArtifactTransferState::Committed
                | ArtifactTransferState::Rejected
                | ArtifactTransferState::Expired
        ) {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }
        store
            .rebind_artifact_transfer_epoch(
                &transfer.transfer_id,
                &transfer.agent_epoch,
                &status.agent_epoch,
            )
            .await?;
    }
    store
        .update_artifact_transfer(
            &status.transfer_id,
            &status.agent_epoch,
            ArtifactTransferUpdate {
                state,
                contiguous_bytes: status.contiguous_bytes,
                next_chunk_index: u64::from(status.next_chunk_index),
                retry_count: transfer.retry_count,
            },
        )
        .await
        .map(|_| ())
}

pub(crate) async fn apply_agent_provider_event(
    state: &Arc<RwLock<AgentProviderState>>,
    store: Option<&dyn ComputeRepository>,
    event: o3k_provider::AgentEvent,
) {
    let mut state = state.write().await;
    match event {
        o3k_provider::AgentEvent::CommandAccepted(accepted) => {
            if let Some(store) = store
                && let Err(error) = store
                    .update_agent_command(
                        &accepted.command_id,
                        AgentCommandState::Accepted,
                        accepted.operation_sequence,
                        accepted.operation_sequence,
                        Some(&accepted.operation_id.to_string()),
                        None,
                    )
                    .await
            {
                tracing::debug!(%error, command_id = %accepted.command_id, "agent command acceptance was not durably projected");
            }
            if let Some(operation) = state.operations.get_mut(&accepted.operation_id)
                && let Some(next) = agent_operation_state(accepted.state)
            {
                operation.state = next;
            }
        }
        o3k_provider::AgentEvent::Operation(update) => {
            if let Some(store) = store
                && let Ok(command) = store
                    .get_agent_command_by_operation(update.operation_id)
                    .await
            {
                let state = match agent_operation_state(update.state) {
                    Some(o3k_provider::OperationState::Succeeded) => AgentCommandState::Succeeded,
                    Some(o3k_provider::OperationState::Retryable) => AgentCommandState::Retryable,
                    Some(o3k_provider::OperationState::UnknownOutcome) => {
                        AgentCommandState::UnknownOutcome
                    }
                    Some(o3k_provider::OperationState::Failed) => AgentCommandState::Failed,
                    _ => AgentCommandState::Running,
                };
                if let Err(error) = store
                    .update_agent_command(
                        &command.command_id,
                        state,
                        command.accepted_sequence,
                        update.operation_sequence,
                        command.provider_operation_id.as_deref(),
                        update.provider_resource_id.as_deref(),
                    )
                    .await
                {
                    tracing::debug!(%error, operation_id = %update.operation_id, "agent operation was not durably projected");
                }
            }
            if let Some(operation) = state.operations.get_mut(&update.operation_id) {
                if let Some(next) = agent_operation_state(update.state) {
                    operation.state = next;
                }
                operation.error_category = agent_error_category(update.error_category);
                if let Some(provider_resource_id) = update.provider_resource_id {
                    operation.provider_resource_id = Some(provider_resource_id);
                }
            }
        }
        o3k_provider::AgentEvent::Observation(observation) => {
            let instance_state = observation.state;
            let provider_id = observation.provider_resource_id.clone();
            // Mirror the journal's `apply_agent_observation` gate: only a
            // Succeeded observation may project a durable provider reference
            // and a volatile instance/binding. A terminal failed presence
            // inspection (domain provably absent) still carries the
            // deterministic libvirt domain name in `provider_resource_id`;
            // attaching a reference from that absence evidence would make
            // the UnknownOutcome create sweep resolve a phantom resource
            // identity and drive `finish_create` against a never-created
            // domain forever instead of converging the absence
            // (issue #87).
            if observation.operation_state == o3k_provider::AgentOperationState::Succeeded
                && let Some(provider_id) = provider_id.as_deref()
            {
                if let Some(store) = store {
                    let reference = o3k_store::ProviderReference {
                        resource_id: observation.resource_id,
                        provider_name: "agent".to_owned(),
                        provider_resource_id: provider_id.to_owned(),
                    };
                    if let Err(error) = store.attach_provider_reference(&reference).await
                        && !matches!(error, StoreError::ProviderReferenceAlreadyExists)
                    {
                        tracing::debug!(%error, resource_id = %observation.resource_id, "agent provider reference was not durably projected");
                    }
                }
                state.bindings.insert(
                    provider_id.to_owned(),
                    AgentBinding {
                        resource_id: observation.resource_id.to_string(),
                        agent_id: observation.agent_id.clone(),
                        agent_epoch: observation.agent_epoch.clone(),
                        provider_resource_id: Some(provider_id.to_owned()),
                    },
                );
                state.instances.insert(
                    provider_id.to_owned(),
                    Instance {
                        provider_instance_id: provider_id.to_owned(),
                        o3k_server_id: observation.resource_id,
                        state: instance_state,
                        observed_message: observation.redacted_message.clone(),
                    },
                );
            }
            if let Some(operation) = state.operations.get_mut(&observation.operation_id) {
                if let Some(next) = agent_operation_state(observation.operation_state) {
                    operation.state = next;
                }
                if let Some(provider_id) = provider_id {
                    operation.provider_resource_id = Some(provider_id);
                }
            }
        }
        o3k_provider::AgentEvent::Error(error) => {
            if let Some(operation_id) = error.operation_id
                && let Some(operation) = state.operations.get_mut(&operation_id)
            {
                operation.state = if error.retryable {
                    o3k_provider::OperationState::Retryable
                } else {
                    o3k_provider::OperationState::Failed
                };
                operation.error_category = agent_error_category(error.category);
            }
        }
        o3k_provider::AgentEvent::ArtifactAck(_ack) => {
            // The foreground create path owns the durable commit after its
            // waiter receives this acknowledgement. Persisting the same
            // transition here races that writer on SQLite. ArtifactStatus
            // events remain the asynchronous recovery projection.
        }
        o3k_provider::AgentEvent::ArtifactStatus(status) => {
            if let Some(store) = store
                && let Err(error) = apply_artifact_status(store, &status).await
            {
                tracing::debug!(%error, transfer_id = %status.transfer_id, "agent artifact status rejected");
            }
        }
    }
}

/// Projects one authenticated agent capability snapshot into the inventory
/// shape required by the scheduler. Missing capacity is represented as zero

#[async_trait]
impl ComputeProvider for AgentComputeProvider {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
        let mut nodes = self.registry.all().await;
        nodes.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        let node = nodes
            .into_iter()
            .find(|node| {
                node.availability == Availability::Available
                    && node.desired_state == agent_proto::AdministrativeState::Enabled as i32
            })
            .ok_or(ProviderError::Retryable)?;
        Ok(Capabilities {
            provider_name: node.capabilities.agent_provider_name,
            provider_version: node.capabilities.agent_provider_version,
            capabilities: node
                .capabilities
                .lifecycle_actions
                .into_iter()
                .chain(
                    node.capabilities
                        .console_log
                        .then_some("compute.console_log".to_owned()),
                )
                .collect(),
        })
    }

    async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            operation_id = %request.operation_id,
            "agent create provider entered"
        );
        let provider_id = request.placement_provider_id.as_deref().ok_or_else(|| {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                "create request has no placement provider identity"
            );
            ProviderError::InvalidRequest
        })?;
        let agent = self.selected_agent(provider_id).await.map_err(|error| {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                error = %error,
                "agent create agent selection rejected"
            );
            error
        })?;
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            agent_id = %agent.agent_id,
            "agent create agent selected"
        );
        if request.config_drive.is_some()
            && !agent
                .capabilities
                .flags
                .iter()
                .any(|flag| flag.name == "config_drive" && flag.supported)
        {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                agent_id = %agent.agent_id,
                capability_flags = ?agent
                    .capabilities
                    .flags
                    .iter()
                    .map(|flag| format!("{}={}", flag.name, flag.supported))
                    .collect::<Vec<_>>(),
                "create request requires an unsupported config-drive capability"
            );
            return Err(ProviderError::InvalidRequest);
        }
        let app_agent = agent_snapshot(&agent);
        let resolved = self
            .resolver
            .resolve(&request, &app_agent)
            .await
            .map_err(|error| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    error = %error,
                    "create resolver rejected request"
                );
                error
            })?;
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            "agent create inputs resolved"
        );
        let image_id = request.image_id.as_deref().ok_or_else(|| {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                "create request has no image identity"
            );
            ProviderError::InvalidRequest
        })?;
        let artifact_inputs = resolved.clone();
        let command = build_create_command(CreateCommandSpec {
            agent_id: agent.agent_id.clone(),
            agent_epoch: agent.agent_epoch.clone(),
            project_id: request.project_id.clone(),
            operation_id: request.operation_id.to_string(),
            resource_id: request.o3k_server_id.to_string(),
            idempotency_key: request.idempotency_key.clone(),
            deadline_unix_ms: unix_ms_after(self.command_timeout),
            image_id: image_id.to_owned(),
            flavor_id: resolved.flavor_id,
            image_artifact_id: resolved.image_artifact_id,
            image_sha256: resolved.image_sha256,
            image_format: resolved.image_format,
            vcpus: request.vcpus,
            memory_mib: request.memory_mib,
            disk_gib: resolved.disk_gib,
            config_drive_artifact_id: resolved.config_drive_artifact_id,
            config_drive_sha256: resolved.config_drive_sha256,
            network_attachments: resolved.network_attachments.clone(),
        })
        .map_err(|error| {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                error = %error,
                "create command construction rejected request"
            );
            map_agent_error(error)
        })?;
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            "agent create command built"
        );
        if let Some(existing) = self
            .persist_pending_command(&command, request.operation_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    error = %error,
                    "agent create pending command persistence rejected"
                );
                error
            })?
            && matches!(
                existing.state,
                AgentCommandState::Succeeded | AgentCommandState::Failed
            )
        {
            return self.get_operation(request.operation_id).await;
        }
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            "agent create pending command persisted"
        );
        let artifacts = self
            .artifact_resolver
            .resolve_artifacts(&request, &app_agent, &artifact_inputs)
            .await
            .map_err(|error| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    error = %error,
                    "create artifact resolver rejected request"
                );
                error
            })?;
        tracing::warn!(
            resource_id = %request.o3k_server_id,
            artifact_count = artifacts.len(),
            "agent create artifacts resolved"
        );
        if artifacts.len() != 2 {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                artifact_count = artifacts.len(),
                "create artifact resolver returned an unexpected artifact count"
            );
            return Err(ProviderError::InvalidRequest);
        }
        let required = [
            (
                o3k_provider::ArtifactKind::ImageBase,
                &artifact_inputs.image_artifact_id,
                &artifact_inputs.image_sha256,
                artifact_inputs.image_format.as_str(),
            ),
            (
                o3k_provider::ArtifactKind::ConfigDriveIso,
                &artifact_inputs.config_drive_artifact_id,
                &artifact_inputs.config_drive_sha256,
                "iso",
            ),
        ];
        let mut seen = [false; 2];
        for artifact in artifacts {
            let expected_index = required
                .iter()
                .position(|(kind, artifact_id, _, _)| {
                    *kind == artifact.kind && artifact_id.as_str() == artifact.artifact_id
                })
                .ok_or_else(|| {
                    tracing::warn!(
                        resource_id = %request.o3k_server_id,
                        artifact_kind = artifact_kind_name(artifact.kind),
                        "create artifact did not match a required reference"
                    );
                    ProviderError::InvalidRequest
                })?;
            if seen[expected_index] {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    artifact_kind = artifact_kind_name(artifact.kind),
                    "create artifact was duplicated"
                );
                return Err(ProviderError::InvalidRequest);
            }
            seen[expected_index] = true;
            let expected = &required[expected_index];
            let size_bytes = u64::try_from(artifact.bytes.len()).map_err(|_| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    artifact_kind = artifact_kind_name(artifact.kind),
                    "create artifact size could not be represented"
                );
                ProviderError::InvalidRequest
            })?;
            if size_bytes == 0
                || artifact.artifact_id.trim().is_empty()
                || artifact.sha256.len() != 64
                || artifact.format.trim().is_empty()
                || artifact.sha256 != *expected.2
                || artifact.format != expected.3
            {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    artifact_kind = artifact_kind_name(artifact.kind),
                    "create artifact metadata or digest validation failed"
                );
                return Err(ProviderError::InvalidRequest);
            }
            let chunk_size = MAX_ARTIFACT_CHUNK_BYTES as u64;
            let chunk_count = u32::try_from(size_bytes.div_ceil(chunk_size)).map_err(|_| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    artifact_kind = artifact_kind_name(artifact.kind),
                    "create artifact chunk count is invalid"
                );
                ProviderError::InvalidRequest
            })?;
            let wire_kind = match artifact.kind {
                o3k_provider::ArtifactKind::ImageBase => agent_proto::ArtifactKind::ImageBase,
                o3k_provider::ArtifactKind::ConfigDriveIso => {
                    agent_proto::ArtifactKind::ConfigDriveIso
                }
            };
            let transfer_id = deterministic_artifact_transfer_id(
                &command.command_id,
                wire_kind,
                &artifact.artifact_id,
            );
            let offer = agent_proto::ArtifactOffer {
                transfer_id,
                command_id: command.command_id.clone(),
                operation_id: command.operation_id.clone(),
                resource_id: command.resource_id.clone(),
                agent_id: agent.agent_id.clone(),
                artifact_id: artifact.artifact_id,
                kind: wire_kind as i32,
                sha256: artifact.sha256,
                size_bytes,
                format: artifact.format,
                chunk_size_bytes: MAX_ARTIFACT_CHUNK_BYTES as u32,
                chunk_count,
                expires_at_unix_ms: command.deadline_unix_ms,
            };
            if let Some(store) = &self.store {
                let transfer = ArtifactTransferRecord {
                    transfer_id: offer.transfer_id.clone(),
                    command_id: offer.command_id.clone(),
                    operation_id: request.operation_id,
                    resource_id: request.o3k_server_id,
                    agent_id: offer.agent_id.clone(),
                    agent_epoch: agent.agent_epoch.clone(),
                    artifact_id: offer.artifact_id.clone(),
                    artifact_kind: artifact_kind_name(artifact.kind).to_owned(),
                    sha256: offer.sha256.clone(),
                    size_bytes: offer.size_bytes,
                    expires_at_unix_ms: offer.expires_at_unix_ms,
                    format: offer.format.clone(),
                    chunk_size_bytes: offer.chunk_size_bytes as u64,
                    chunk_count: offer.chunk_count as u64,
                    state: ArtifactTransferState::Offered,
                    contiguous_bytes: 0,
                    next_chunk_index: 0,
                    retry_count: 0,
                    created_at: String::new(),
                    updated_at: String::new(),
                };
                let existing = match store.insert_artifact_transfer(&transfer).await {
                    Ok(existing) => existing,
                    Err(StoreError::ArtifactTransferConflict(_)) => {
                        let previous = store
                            .get_artifact_transfer(&transfer.transfer_id)
                            .await
                            .map_err(|_| ProviderError::Conflict)?;
                        if previous.state == ArtifactTransferState::Committed {
                            previous
                        } else if previous.command_id == transfer.command_id
                            && previous.operation_id == transfer.operation_id
                            && previous.resource_id == transfer.resource_id
                            && previous.agent_id == transfer.agent_id
                            && previous.artifact_id == transfer.artifact_id
                            && previous.artifact_kind == transfer.artifact_kind
                            && previous.sha256 == transfer.sha256
                            && previous.size_bytes == transfer.size_bytes
                            && previous.format == transfer.format
                            && previous.chunk_size_bytes == transfer.chunk_size_bytes
                            && previous.chunk_count == transfer.chunk_count
                        {
                            store
                                .rebind_artifact_transfer_epoch(
                                    &transfer.transfer_id,
                                    &previous.agent_epoch,
                                    &transfer.agent_epoch,
                                )
                                .await
                                .map_err(|_| ProviderError::Conflict)?
                        } else {
                            return Err(ProviderError::Conflict);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            resource_id = %request.o3k_server_id,
                            artifact_kind = artifact_kind_name(artifact.kind),
                            error = %error,
                            "artifact transfer insert failed"
                        );
                        return Err(ProviderError::Conflict);
                    }
                };
                if existing.state == ArtifactTransferState::Committed {
                    continue;
                }
                if matches!(
                    existing.state,
                    ArtifactTransferState::Rejected | ArtifactTransferState::Expired
                ) {
                    return Err(ProviderError::Conflict);
                }
            }
            self.registry
                .dispatch_artifact_and_wait(offer.clone(), artifact.bytes, self.command_timeout)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        resource_id = %request.o3k_server_id,
                        artifact_kind = artifact_kind_name(artifact.kind),
                        error = %error,
                        "create artifact transfer was rejected"
                    );
                    map_agent_error(error)
                })?;
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                artifact_kind = artifact_kind_name(artifact.kind),
                "agent create artifact transfer committed"
            );
            if let Some(store) = &self.store {
                store
                    .update_artifact_transfer(
                        &offer.transfer_id,
                        &agent.agent_epoch,
                        ArtifactTransferUpdate {
                            state: ArtifactTransferState::Committed,
                            contiguous_bytes: offer.size_bytes,
                            next_chunk_index: offer.chunk_count as u64,
                            retry_count: 0,
                        },
                    )
                    .await
                    .map_err(|error| {
                        tracing::warn!(
                            resource_id = %request.o3k_server_id,
                            artifact_kind = artifact_kind_name(artifact.kind),
                            error = %error,
                            "artifact transfer commit update failed"
                        );
                        ProviderError::Conflict
                    })?;
            }
        }
        if seen != [true, true] {
            tracing::warn!(
                resource_id = %request.o3k_server_id,
                image_seen = seen[0],
                config_drive_seen = seen[1],
                "create artifacts did not include every required artifact"
            );
            return Err(ProviderError::InvalidRequest);
        }
        let operation = self
            .dispatch_recorded(command, request.operation_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    error = %error,
                    "create command dispatch was rejected"
                );
                error
            })?;
        self.state.write().await.bindings.insert(
            request.o3k_server_id.to_string(),
            AgentBinding {
                resource_id: request.o3k_server_id.to_string(),
                agent_id: agent.agent_id,
                agent_epoch: agent.agent_epoch,
                provider_resource_id: None,
            },
        );
        Ok(operation)
    }

    async fn get_instance(&self, id: &str) -> Result<Instance, ProviderError> {
        self.rehydrate().await?;
        self.state
            .read()
            .await
            .instances
            .get(id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }

    async fn inspect_instance(
        &self,
        provider_id: &str,
        resource_id: &str,
        provider_instance_id: &str,
        operation_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Operation, ProviderError> {
        if idempotency_key.trim().is_empty() {
            return Err(ProviderError::InvalidRequest);
        }
        let binding = {
            let state = self.state.read().await;
            state.bindings.get(resource_id).cloned().or_else(|| {
                state
                    .bindings
                    .values()
                    .find(|binding| binding.resource_id == resource_id)
                    .cloned()
            })
        };
        if let Some(binding) = binding.as_ref()
            && let Some(expected) = binding.provider_resource_id.as_deref()
            && expected != provider_instance_id
        {
            return Err(ProviderError::Conflict);
        }
        let agent = self.selected_agent(provider_id).await?;
        if let Some(binding) = binding.as_ref() {
            if binding.agent_id != provider_id {
                return Err(ProviderError::StaleState);
            }
            if binding.agent_epoch != agent.agent_epoch {
                // The registry is authoritative for the agent's current
                // epoch: every registration replaces the stored epoch (minted
                // per connection), so a binding carrying the pre-restart
                // epoch is a legitimate same-agent restart, not a dead
                // stream — the #552 journal-evidence rationale. Re-anchor the
                // in-memory binding so the presence inspection dispatches
                // against the current agent instead of failing as
                // StaleState forever. A binding owned by a DIFFERENT agent is
                // still rejected above; the wire dispatch additionally fences
                // the command by the current registered epoch.
                let mut state = self.state.write().await;
                match state.bindings.get_mut(resource_id) {
                    Some(current) => current.agent_epoch = agent.agent_epoch.clone(),
                    None => {
                        if let Some(current) = state
                            .bindings
                            .values_mut()
                            .find(|binding| binding.resource_id == resource_id)
                        {
                            current.agent_epoch = agent.agent_epoch.clone();
                        }
                    }
                }
            }
        }
        let mut command = build_lifecycle_command(
            LifecycleCommand::Inspect,
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            resource_id,
        )
        .map_err(map_agent_error)?;
        command.idempotency_key = idempotency_key.to_owned();
        self.dispatch_recorded(command, operation_id).await
    }

    async fn delete_instance(
        &self,
        request: DeleteInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        self.rehydrate().await?;
        let binding = self
            .state
            .read()
            .await
            .bindings
            .get(&request.provider_instance_id)
            .cloned();
        let binding = binding.ok_or(ProviderError::NotFound)?;
        let agent = self.selected_agent(&binding.agent_id).await?;
        if agent.agent_epoch != binding.agent_epoch {
            return Err(ProviderError::StaleState);
        }
        // The command resource identity is the O3K server id, never the
        // provider (libvirt domain) name: the agent derives the domain from
        // the server id and the durable command store requires a UUID.
        let command = build_lifecycle_command(
            LifecycleCommand::Delete,
            &agent.agent_id,
            &agent.agent_epoch,
            &request.operation_id.to_string(),
            &binding.resource_id,
        )
        .map_err(map_agent_error)?;
        self.dispatch_recorded(command, request.operation_id).await
    }

    async fn action_instance(
        &self,
        id: &str,
        action: InstanceAction,
        operation_id: Uuid,
        _idempotency_key: &str,
    ) -> Result<Operation, ProviderError> {
        self.rehydrate().await?;
        let binding = self
            .state
            .read()
            .await
            .bindings
            .get(id)
            .cloned()
            .ok_or(ProviderError::NotFound)?;
        let agent = self.selected_agent(&binding.agent_id).await?;
        if agent.agent_epoch != binding.agent_epoch {
            return Err(ProviderError::StaleState);
        }
        let command_action = match action {
            InstanceAction::Start => LifecycleCommand::Start,
            InstanceAction::Stop => LifecycleCommand::Stop,
            InstanceAction::Reboot => LifecycleCommand::HardReboot,
        };
        let command = build_lifecycle_command(
            command_action,
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            &binding.resource_id,
        )
        .map_err(map_agent_error)?;
        self.dispatch_recorded(command, operation_id).await
    }

    async fn get_operation(&self, id: Uuid) -> Result<Operation, ProviderError> {
        if let Some(store) = &self.store
            && let Ok(record) = store.get_operation(id).await
        {
            let provider_resource_id =
                if let Ok(command) = store.get_agent_command_by_operation(id).await {
                    let reference = match store
                        .get_provider_reference(command.resource_id, "compute")
                        .await
                    {
                        Ok(reference) => Some(reference),
                        Err(_) => store
                            .get_provider_reference(command.resource_id, "agent")
                            .await
                            .ok(),
                    };
                    reference.map(|reference| reference.provider_resource_id)
                } else {
                    None
                };
            let state = match record.state {
                o3k_store::OperationState::Pending => o3k_provider::OperationState::Accepted,
                o3k_store::OperationState::Running => o3k_provider::OperationState::Running,
                o3k_store::OperationState::Succeeded => o3k_provider::OperationState::Succeeded,
                o3k_store::OperationState::Retryable => o3k_provider::OperationState::Retryable,
                o3k_store::OperationState::UnknownOutcome => {
                    o3k_provider::OperationState::UnknownOutcome
                }
                o3k_store::OperationState::Failed => o3k_provider::OperationState::Failed,
            };
            return Ok(Operation {
                provider_operation_id: record
                    .provider_operation_id
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .unwrap_or(id),
                o3k_operation_id: id,
                state,
                error_category: record
                    .error_category
                    .as_deref()
                    .and_then(|value| match value {
                        "invalid_request" => Some(o3k_provider::ErrorCategory::InvalidRequest),
                        "conflict" => Some(o3k_provider::ErrorCategory::Conflict),
                        "capacity" => Some(o3k_provider::ErrorCategory::Capacity),
                        "not_found" => Some(o3k_provider::ErrorCategory::NotFound),
                        "retryable" => Some(o3k_provider::ErrorCategory::Retryable),
                        "unknown_outcome" => Some(o3k_provider::ErrorCategory::UnknownOutcome),
                        "terminal" => Some(o3k_provider::ErrorCategory::Terminal),
                        _ => None,
                    }),
                provider_resource_id,
            });
        }
        self.state
            .read()
            .await
            .operations
            .get(&id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }

    async fn collect_connector(&self, resource_id: Uuid) -> Result<ConnectorInfo, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            BlockDeviceCommand::CollectConnector,
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            &binding_resource,
        )
        .map_err(map_agent_error)?;
        let observation = self
            .dispatch_block_device_and_wait(command, operation_id, self.command_timeout)
            .await?;
        Ok(ConnectorInfo {
            host: observation.host_name.clone().unwrap_or_default(),
            ip: observation.ip_address.clone().unwrap_or_default(),
            platform: "x86_64".to_owned(),
            os_type: "linux".to_owned(),
            multipath: false,
            initiator: observation.initiator.clone(),
        })
    }

    async fn attach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            BlockDeviceCommand::Attach {
                device: agent_proto::AttachDiskCommand {
                    volume_id: device.volume_id.clone(),
                    attachment_id: device.attachment_id.clone(),
                    driver_volume_type: device.driver_volume_type.clone(),
                    target_iqn: device.target_iqn.clone().unwrap_or_default(),
                    target_portal: device.target_portal.clone().unwrap_or_default(),
                    target_lun: device.target_lun.unwrap_or(0),
                    device_path: device.local_path.clone().unwrap_or_default(),
                    multipath: device.multipath,
                    initiator: device.initiator.clone().unwrap_or_default(),
                    auth_method: device.auth_method.clone().unwrap_or_default(),
                    auth_username: device.auth_username.clone().unwrap_or_default(),
                    auth_password: device.auth_password.clone().unwrap_or_default(),
                },
            },
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            &binding_resource,
        )
        .map_err(map_agent_error)?;
        let observation = self
            .dispatch_block_device_and_wait(command, operation_id, self.command_timeout)
            .await?;
        Ok(observation)
    }

    async fn detach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            BlockDeviceCommand::Detach {
                device: agent_proto::DetachDiskCommand {
                    volume_id: device.volume_id.clone(),
                    attachment_id: device.attachment_id.clone(),
                    driver_volume_type: device.driver_volume_type.clone(),
                    target_iqn: device.target_iqn.clone().unwrap_or_default(),
                    target_portal: device.target_portal.clone().unwrap_or_default(),
                    target_lun: device.target_lun.unwrap_or(0),
                    device_path: device.local_path.clone().unwrap_or_default(),
                    multipath: device.multipath,
                    initiator: device.initiator.clone().unwrap_or_default(),
                },
            },
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            &binding_resource,
        )
        .map_err(map_agent_error)?;
        let observation = self
            .dispatch_block_device_and_wait(command, operation_id, self.command_timeout)
            .await?;
        Ok(observation)
    }

    async fn observe_block_device(
        &self,
        resource_id: Uuid,
        volume_id: &str,
    ) -> Result<Option<BlockDeviceObservation>, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            BlockDeviceCommand::Observe {
                volume_id: volume_id.to_owned(),
                attachment_id: String::new(),
            },
            &agent.agent_id,
            &agent.agent_epoch,
            &operation_id.to_string(),
            &binding_resource,
        )
        .map_err(map_agent_error)?;
        let observation = self
            .dispatch_block_device_and_wait(command, operation_id, self.command_timeout)
            .await?;
        if !observation.found {
            return Ok(None);
        }
        Ok(Some(observation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider::{
        AgentNodeSnapshot, AgentObservation, AgentOperationState, NetworkAttachmentSpec,
        ResolvedCreateInputs, ResolvedCreateResolver, UnconfiguredResolvedCreateResolver,
    };

    fn register_request(id: &str, epoch: &str) -> agent_proto::RegisterRequest {
        agent_proto::RegisterRequest {
            agent_id: id.to_owned(),
            agent_epoch: epoch.to_owned(),
            software_version: "test".to_owned(),
            host_label: id.to_owned(),
            supported_versions: vec![crate::PROTOCOL_VERSION],
            capabilities: Some(agent_proto::Capabilities {
                architecture: "x86_64".to_owned(),
                agent_provider_name: "o3k-compute".to_owned(),
                agent_provider_version: "test".to_owned(),
                ..Default::default()
            }),
        }
    }

    #[derive(Debug, Default)]
    struct TestResolvedCreateResolver;

    #[async_trait]
    impl ResolvedCreateResolver for TestResolvedCreateResolver {
        async fn resolve(
            &self,
            _request: &CreateInstanceRequest,
            _agent: &AgentNodeSnapshot,
        ) -> Result<ResolvedCreateInputs, ProviderError> {
            Ok(ResolvedCreateInputs {
                flavor_id: "flavor.test".to_owned(),
                image_artifact_id: "artifact.test".to_owned(),
                image_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_owned(),
                image_format: "qcow2".to_owned(),
                disk_gib: 10,
                config_drive_artifact_id: "config-drive.test".to_owned(),
                config_drive_sha256:
                    "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".to_owned(),
                network_attachments: vec![NetworkAttachmentSpec {
                    port_id: "port.test".to_owned(),
                    mac: "52:54:00:12:34:56".to_owned(),
                    fixed_ipv4: "192.0.2.10".to_owned(),
                    subnet_cidr: "192.0.2.0/24".to_owned(),
                    gateway_ipv4: "192.0.2.1".to_owned(),
                }],
            })
        }
    }

    /// Seeds the in-memory binding exactly as the create dispatch leaves it
    /// (see `create_instance`): keyed by the O3K server id, carrying the
    /// create-time agent epoch, with no provider resource identity yet.
    async fn seed_create_binding(
        provider: &AgentComputeProvider,
        server_id: Uuid,
        agent_id: &str,
        agent_epoch: &str,
    ) {
        provider.state.write().await.bindings.insert(
            server_id.to_string(),
            AgentBinding {
                resource_id: server_id.to_string(),
                agent_id: agent_id.to_owned(),
                agent_epoch: agent_epoch.to_owned(),
                provider_resource_id: None,
            },
        );
    }

    /// Seeds the in-memory binding exactly as a create observation leaves it
    /// for lifecycle dispatch (see `apply_agent_provider_event`): keyed by
    /// the provider (libvirt domain) identity, carrying the create-time
    /// agent epoch and the O3K server identity as the resource.
    async fn seed_lifecycle_binding(
        provider: &AgentComputeProvider,
        provider_instance_id: &str,
        resource_id: &str,
        agent_id: &str,
        agent_epoch: &str,
    ) {
        provider.state.write().await.bindings.insert(
            provider_instance_id.to_owned(),
            AgentBinding {
                resource_id: resource_id.to_owned(),
                agent_id: agent_id.to_owned(),
                agent_epoch: agent_epoch.to_owned(),
                provider_resource_id: Some(provider_instance_id.to_owned()),
            },
        );
    }

    /// Issue #87 crash-restart defect: the agent crashes and re-registers
    /// under a fresh per-connection epoch while the in-memory `AgentBinding`
    /// still carries the pre-crash epoch. `inspect_instance` must not reject
    /// the presence inspection as `StaleState` before dispatching anything:
    /// the registry is authoritative for the current epoch (the #552
    /// rationale), so the same agent's operation resolves against the current
    /// registration. The dispatch itself cannot complete in-process without a
    /// live control stream, so the observable contract is that the failure is
    /// a dispatch attempt (`Retryable`), never a stale-binding rejection.
    #[tokio::test]
    async fn inspect_after_agent_reregistration_dispatches_against_current_epoch()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry
            .register(&register_request("node-a", "epoch-1"))
            .await?;
        let provider =
            AgentComputeProvider::new(registry.clone(), Arc::new(TestResolvedCreateResolver));
        let server_id = Uuid::now_v7();
        seed_create_binding(&provider, server_id, "node-a", "epoch-1").await;
        // The agent crashed and re-registered; the registry now stores the
        // fresh per-connection epoch, mirroring NodeRegistry::register.
        registry
            .register(&register_request("node-a", "epoch-2"))
            .await?;
        let result = provider
            .inspect_instance(
                "node-a",
                &server_id.to_string(),
                "",
                Uuid::now_v7(),
                "inspect-create-test",
            )
            .await;
        match result {
            Err(ProviderError::Retryable) => {}
            other => {
                return Err(format!(
                    "inspect must dispatch against the current agent, got {other:?}"
                )
                .into());
            }
        }
        // The binding was re-anchored to the current registered epoch.
        let binding = provider
            .state
            .read()
            .await
            .bindings
            .get(&server_id.to_string())
            .cloned();
        assert_eq!(
            binding.as_ref().map(|binding| binding.agent_epoch.as_str()),
            Some("epoch-2")
        );
        Ok(())
    }

    /// Issue #87 invariant: an inspection requested for a DIFFERENT agent
    /// than the one the binding belongs to must still be rejected as
    /// `StaleState`; the re-anchor is limited to the agent the operation
    /// actually belongs to and must not mutate the binding on rejection.
    #[tokio::test]
    async fn inspect_for_a_different_agent_is_still_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry
            .register(&register_request("node-a", "epoch-1"))
            .await?;
        registry
            .register(&register_request("node-b", "epoch-9"))
            .await?;
        let provider =
            AgentComputeProvider::new(registry.clone(), Arc::new(TestResolvedCreateResolver));
        let server_id = Uuid::now_v7();
        seed_create_binding(&provider, server_id, "node-a", "epoch-1").await;
        registry
            .register(&register_request("node-a", "epoch-2"))
            .await?;
        assert_eq!(
            provider
                .inspect_instance(
                    "node-b",
                    &server_id.to_string(),
                    "",
                    Uuid::now_v7(),
                    "inspect-create-test",
                )
                .await,
            Err(ProviderError::StaleState)
        );
        // The rejected inspection must not have re-anchored the binding.
        let binding = provider
            .state
            .read()
            .await
            .bindings
            .get(&server_id.to_string())
            .cloned();
        assert_eq!(
            binding.as_ref().map(|binding| binding.agent_epoch.as_str()),
            Some("epoch-1")
        );
        Ok(())
    }

    /// Seeds the durable records of an UnknownOutcome create exactly as the
    /// issue-87 crash-restart residue leaves them: a BUILD resource with no
    /// provider identity, a create operation in `UnknownOutcome`, and the
    /// agent command record whose resource identity is the O3K server id.
    async fn seed_unknown_outcome_create(
        store: &dyn ComputeRepository,
        operation_id: Uuid,
        resource_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: resource_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "{}".to_owned(),
                observed_state: "BUILD".to_owned(),
                provider_id: None,
            })
            .await?;
        store
            .insert_operation(&o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "compute_create".to_owned(),
                state: o3k_store::OperationState::UnknownOutcome,
                provider_operation_id: Some(operation_id.to_string()),
                error_category: Some("unknown_outcome".to_owned()),
                error_message: None,
            })
            .await?;
        store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: "command-create".to_owned(),
                idempotency_key: "create-request".to_owned(),
                operation_id,
                resource_id,
                agent_id: "node-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                payload_fingerprint_sha256: String::new(),
                payload: Vec::new(),
                state: o3k_store::AgentCommandState::UnknownOutcome,
                accepted_sequence: 1,
                last_sequence: 1,
                provider_operation_id: Some(operation_id.to_string()),
                provider_resource_id: None,
            })
            .await?;
        Ok(())
    }

    /// Issue #87 crash-recovery defect: the agent settles the presence
    /// inspection for an UnknownOutcome create as a terminal Failed/NotFound
    /// when the domain provably was never created. That absence evidence
    /// still carries the stable libvirt domain name in `provider_resource_id`
    /// (the name is derived from the server identity, not from existence).
    /// The observation handler must NOT project it: the journal's
    /// `apply_agent_observation` rejects non-Succeeded observations by
    /// design, and a durable provider reference attached from absence
    /// evidence would make the create sweep resolve a phantom resource
    /// identity (`get_operation` reads the "agent" reference) and drive
    /// `finish_create` against a never-created domain forever — the
    /// `server create convergence pass failed` loop — instead of reaching
    /// `converge_absent_create`.
    #[tokio::test]
    async fn absence_observation_does_not_project_a_provider_reference()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let operation_id = Uuid::now_v7();
        let resource_id = Uuid::now_v7();
        // The agent derives the stable domain name from the server identity
        // (o3k-libvirt `stable_domain_name`), so a provably absent domain
        // still has a deterministic name.
        let domain_name = "o3k-0123456789abcdef0123";
        seed_unknown_outcome_create(store.as_ref(), operation_id, resource_id).await?;
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        apply_agent_provider_event(
            &state,
            Some(store.as_ref()),
            o3k_provider::AgentEvent::Observation(Box::new(AgentObservation {
                agent_id: "node-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                resource_id,
                provider_resource_id: Some(domain_name.to_owned()),
                state: o3k_provider::InstanceState::Error,
                operation_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("o3k:inspect-create:{operation_id}").as_bytes(),
                ),
                operation_state: AgentOperationState::Failed,
                observation_sequence: 1,
                observed_at_unix_ms: 1,
                redacted_message: Some("requested domain was not found".to_owned()),
                console_log_bytes: Vec::new(),
                console_log_offset: 0,
                console_log_complete: false,
                console_log_truncated: false,
                block_device: None,
            })),
        )
        .await;
        // Absence evidence must not become a durable provider reference...
        assert!(
            matches!(
                store.get_provider_reference(resource_id, "agent").await,
                Err(StoreError::ProviderReferenceNotFound)
            ),
            "a failed/not_found observation must not attach a provider reference"
        );
        // ...must not project a volatile instance or binding for a domain
        // that was provably never created...
        let provider = AgentComputeProvider {
            registry: NodeRegistry::default(),
            resolver: Arc::new(UnconfiguredResolvedCreateResolver),
            state: state.clone(),
            store: Some(store.clone()),
            artifact_resolver: Arc::new(UnconfiguredCreateArtifactResolver),
            command_timeout: Duration::from_secs(30),
        };
        assert!(
            provider.state.read().await.instances.is_empty(),
            "failed observation must not project a volatile instance"
        );
        assert!(
            provider.state.read().await.bindings.is_empty(),
            "failed observation must not project a volatile binding"
        );
        // ...and the create's provider operation must keep no resource
        // identity, so the reconciler sweep routes the UnknownOutcome create
        // to `observe_create_presence` (whose durable Failed/not_found
        // inspection converges the create as absent) instead of
        // `finish_create` against the phantom domain name.
        assert_eq!(
            provider
                .get_operation(operation_id)
                .await?
                .provider_resource_id,
            None
        );
        Ok(())
    }

    /// Issue #87 invariant: a SUCCEEDED observation still projects the
    /// durable provider reference, the volatile instance, and the binding —
    /// the domain was verified to exist, and the presence inspection that
    /// finds the instance converges the create to success through this
    /// identity.
    #[tokio::test]
    async fn succeeded_observation_projects_the_provider_reference()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let operation_id = Uuid::now_v7();
        let resource_id = Uuid::now_v7();
        let domain_name = "o3k-0123456789abcdef0123";
        seed_unknown_outcome_create(store.as_ref(), operation_id, resource_id).await?;
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        apply_agent_provider_event(
            &state,
            Some(store.as_ref()),
            o3k_provider::AgentEvent::Observation(Box::new(AgentObservation {
                agent_id: "node-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                resource_id,
                provider_resource_id: Some(domain_name.to_owned()),
                state: o3k_provider::InstanceState::Running,
                operation_id,
                operation_state: AgentOperationState::Succeeded,
                observation_sequence: 1,
                observed_at_unix_ms: 1,
                redacted_message: Some("running".to_owned()),
                console_log_bytes: Vec::new(),
                console_log_offset: 0,
                console_log_complete: false,
                console_log_truncated: false,
                block_device: None,
            })),
        )
        .await;
        let reference = store.get_provider_reference(resource_id, "agent").await?;
        assert_eq!(reference.provider_resource_id, domain_name);
        let provider = AgentComputeProvider {
            registry: NodeRegistry::default(),
            resolver: Arc::new(UnconfiguredResolvedCreateResolver),
            state: state.clone(),
            store: Some(store.clone()),
            artifact_resolver: Arc::new(UnconfiguredCreateArtifactResolver),
            command_timeout: Duration::from_secs(30),
        };
        let instance = provider
            .state
            .read()
            .await
            .instances
            .get(domain_name)
            .cloned()
            .ok_or("succeeded observation must project the volatile instance")?;
        assert_eq!(instance.o3k_server_id, resource_id);
        assert_eq!(instance.state, o3k_provider::InstanceState::Running);
        let binding = provider
            .state
            .read()
            .await
            .bindings
            .get(domain_name)
            .cloned()
            .ok_or("succeeded observation must project the binding")?;
        assert_eq!(binding.resource_id, resource_id.to_string());
        Ok(())
    }

    /// Seeds the durable records of an in-flight lifecycle operation exactly
    /// as the reconcile loop leaves them during a transport stall: an ACTIVE
    /// resource with a provider identity and a valid create intent (the
    /// rehydrate projection decodes it), the attached "agent" provider
    /// reference (the create observation attached it), the lifecycle
    /// operation in `Running`, and (in the caller's step) the pending agent
    /// command. The operation row is the FK target of
    /// `agent_commands.operation_id`.
    async fn seed_lifecycle_operation(
        store: &dyn ComputeRepository,
        operation_id: Uuid,
        resource_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: resource_id,
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: "flavor.test".to_owned(),
            disk_gib: 10,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("alloc-a".to_owned()),
            config_drive: None,
            idempotency_key: "create-a".to_owned(),
        };
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: resource_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 3,
                observed_generation: 3,
                desired_state: serde_json::to_string(&request).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "request serialization")
                })?,
                observed_state: "ACTIVE".to_owned(),
                provider_id: Some("domain-a".to_owned()),
            })
            .await?;
        store
            .attach_provider_reference(&o3k_store::ProviderReference {
                resource_id,
                provider_name: "agent".to_owned(),
                provider_resource_id: "domain-a".to_owned(),
            })
            .await?;
        store
            .insert_operation(&o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:reboot".to_owned(),
                state: o3k_store::OperationState::Running,
                provider_operation_id: Some(operation_id.to_string()),
                error_category: None,
                error_message: None,
            })
            .await?;
        Ok(())
    }

    /// Issue #87 B2 regression (agent-control-plane-network-interruption):
    /// a re-dispatch of a durably recorded lifecycle command whose embedded
    /// deadline has expired — the residue of an accepted in-flight command
    /// during a stalled control stream — must be classified as an unknown
    /// outcome, never as a rejected request. The command may already have
    /// been delivered and executed while the stream was down (in the
    /// real-host failure the agent accepted and executed the reboot and
    /// re-delivered the terminal observation after the restore); the
    /// reconciler must observe the operation before retrying instead of
    /// terminalizing it as failed/invalid_request.
    #[tokio::test]
    async fn lifecycle_redispatch_past_command_deadline_is_unknown_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry
            .register(&register_request("node-a", "epoch-1"))
            .await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = AgentComputeProvider::new_with_store(
            registry.clone(),
            Arc::new(TestResolvedCreateResolver),
            Some(store.clone()),
        );
        let server_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        seed_lifecycle_binding(
            &provider,
            "domain-a",
            &server_id.to_string(),
            "node-a",
            "epoch-1",
        )
        .await;
        seed_lifecycle_operation(store.as_ref(), operation_id, server_id).await?;
        // The stalled-stream residue: the command was durably recorded as
        // Pending (its acceptance never reached the control plane) and the
        // reconcile re-drive happens after the embedded 10s deadline
        // expired. The deadline is not part of the canonical payload, so the
        // recorded fingerprint stays valid and only the deadline fence fires.
        let mut command = build_lifecycle_command(
            LifecycleCommand::HardReboot,
            "node-a",
            "epoch-1",
            &operation_id.to_string(),
            &server_id.to_string(),
        )?;
        command.deadline_unix_ms = unix_ms().saturating_sub(1);
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id: command.command_id.clone(),
                idempotency_key: command.idempotency_key.clone(),
                operation_id,
                resource_id: server_id,
                agent_id: command.agent_id.clone(),
                agent_epoch: command.agent_epoch.clone(),
                payload_fingerprint_sha256: command.payload_fingerprint_sha256.clone(),
                payload: command.encode_to_vec(),
                state: AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: Some(operation_id.to_string()),
                provider_resource_id: None,
            })
            .await?;
        let result = provider
            .action_instance(
                "domain-a",
                o3k_provider::InstanceAction::Reboot,
                operation_id,
                "reboot-a",
            )
            .await;
        match result {
            Err(ProviderError::UnknownOutcome {
                operation_id: unknown,
            }) if unknown == operation_id => {}
            other => {
                return Err(format!(
                    "a stalled-stream lifecycle re-dispatch must be an unknown outcome, got {other:?}"
                )
                .into());
            }
        }
        // The accepted projection must not leak, and the durable command
        // record must be untouched, ready for the reconciler's
        // observe-before-retry pass.
        assert!(
            provider.state.read().await.operations.is_empty(),
            "a failed dispatch must not leave an accepted operation projection"
        );
        assert_eq!(
            store
                .get_agent_command_by_operation(operation_id)
                .await?
                .state,
            AgentCommandState::Pending
        );
        Ok(())
    }

    /// Issue #87 B2 invariant: a genuinely invalid command payload — a
    /// durable record whose fingerprint does not match its canonical payload
    /// — must STILL be rejected as `InvalidRequest` (the reconciler's
    /// terminal failure path). The transport-stall reclassification never
    /// weakens real validation.
    #[tokio::test]
    async fn lifecycle_redispatch_with_corrupt_payload_is_still_invalid_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry
            .register(&register_request("node-a", "epoch-1"))
            .await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = AgentComputeProvider::new_with_store(
            registry.clone(),
            Arc::new(TestResolvedCreateResolver),
            Some(store.clone()),
        );
        let server_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        seed_lifecycle_binding(
            &provider,
            "domain-a",
            &server_id.to_string(),
            "node-a",
            "epoch-1",
        )
        .await;
        seed_lifecycle_operation(store.as_ref(), operation_id, server_id).await?;
        let mut command = build_lifecycle_command(
            LifecycleCommand::HardReboot,
            "node-a",
            "epoch-1",
            &operation_id.to_string(),
            &server_id.to_string(),
        )?;
        // The embedded deadline stays live so the fingerprint fence is the
        // only validation that can fire.
        command.payload_fingerprint_sha256 = "f".repeat(64);
        store
            .insert_agent_command(&AgentCommandRecord {
                command_id: command.command_id.clone(),
                idempotency_key: command.idempotency_key.clone(),
                operation_id,
                resource_id: server_id,
                agent_id: command.agent_id.clone(),
                agent_epoch: command.agent_epoch.clone(),
                payload_fingerprint_sha256: command.payload_fingerprint_sha256.clone(),
                payload: command.encode_to_vec(),
                state: AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: Some(operation_id.to_string()),
                provider_resource_id: None,
            })
            .await?;
        let result = provider
            .action_instance(
                "domain-a",
                o3k_provider::InstanceAction::Reboot,
                operation_id,
                "reboot-a",
            )
            .await;
        assert_eq!(
            result,
            Err(ProviderError::InvalidRequest),
            "a corrupt command payload must stay a rejected request"
        );
        assert!(
            provider.state.read().await.operations.is_empty(),
            "a rejected dispatch must not leave an accepted operation projection"
        );
        Ok(())
    }
}
