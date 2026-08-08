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

    async fn dispatch(&self, command: agent_proto::Command) -> Result<(), ProviderError> {
        self.registry
            .dispatch_command(command)
            .await
            .map_err(map_agent_error)
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
        if let Err(error) = self.dispatch(command).await {
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
        if store.get_operation(operation_id).await.is_err() {
            let _ = store
                .insert_operation(&o3k_store::OperationRecord {
                    id: operation_id,
                    resource_id: record.resource_id,
                    kind: "command".to_owned(),
                    state: o3k_store::OperationState::Running,
                    provider_operation_id: Some(operation_id.to_string()),
                    error_category: None,
                    error_message: None,
                })
                .await;
        }
        let existing = store
            .insert_agent_command(&record)
            .await
            .map_err(|_| ProviderError::Conflict)?;
        Ok(Some(existing))
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
            if let Some(provider_id) = provider_id.as_deref() {
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
        if let Some(binding) = binding.as_ref()
            && (binding.agent_id != provider_id || binding.agent_epoch != agent.agent_epoch)
        {
            return Err(ProviderError::StaleState);
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
