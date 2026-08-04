use std::sync::Arc;

use async_trait::async_trait;
use o3k_compute_agent::{
    Availability, CreateCommandSpec, LifecycleCommand, NetworkAttachmentSpec, NodeRegistry,
    NodeSnapshot, build_block_device_command, build_create_command, build_lifecycle_command,
};
#[cfg(test)]
use o3k_provider::FakeComputeProvider;
use o3k_provider::{
    BlockDeviceAttachment, BlockDeviceObservation, Capabilities, ComputeProvider,
    ConfigDriveRequest, ConnectorInfo, CreateInstanceRequest, DeleteInstanceRequest, Instance,
    InstanceAction, Operation, ProviderError,
};
use o3k_provider_contract::compute_proto as agent_proto;
use o3k_reconciler::{LifecycleAction, OperationJournal, ReconcileError};
use o3k_scheduler::{Flavor as SchedulerFlavor, Scheduler, SchedulerError};
use o3k_store::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferState,
    ArtifactTransferUpdate, DurableStore, SqliteStore, StoreError, VolumeAttachmentRecord,
};

use prost::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Flavor {
    pub id: Uuid,
    pub name: String,
    pub vcpus: u32,
    pub ram_mib: u64,
    pub disk_gib: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub project_id: String,
    pub flavor_id: Uuid,
    pub image_id: String,
    pub status: String,
    pub key_name: Option<String>,
    pub config_drive: bool,
    pub network_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Keypair {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: String,
    pub name: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCreateInput {
    pub user_id: String,
    pub project_id: String,
    pub name: String,
    pub image_id: String,
    pub flavor_id: Uuid,
    pub network_ids: Vec<String>,
    pub key_name: Option<String>,
    pub config_drive: Option<ConfigDriveRequest>,
    pub idempotency_key: String,
}

#[derive(Debug, Error)]
pub enum ComputeError {
    #[error("compute resource was not found")]
    NotFound,
    #[error("compute request conflicts with existing state")]
    Conflict,
    #[error("compute request is invalid")]
    InvalidRequest,
    #[error("compute store error")]
    Store(#[from] StoreError),
    #[error("compute reconciliation error")]
    Reconcile(#[from] ReconcileError),
    #[error("compute provider error")]
    Provider(#[from] ProviderError),
    #[error("compute scheduler error")]
    Scheduler(#[from] SchedulerError),
}

#[derive(Clone)]
pub struct ComputeService {
    store: Arc<SqliteStore>,
    provider: Arc<ProviderBackend>,
    journal: OperationJournal<SqliteStore, ProviderBackend>,
    scheduler: Option<Scheduler>,
    agent_registry: Option<NodeRegistry>,
}

#[derive(Clone)]
pub struct ProviderBackend(Arc<dyn ComputeProvider>);

/// Fully resolved, immutable inputs required by the compute-agent create
/// command.  The control plane may construct this value from its image,
/// network, and config-drive services, but the agent provider never guesses
/// paths, checksums, addresses, or flavor values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCreateInputs {
    pub flavor_id: String,
    pub image_artifact_id: String,
    pub image_sha256: String,
    pub image_format: String,
    pub disk_gib: u64,
    pub config_drive_artifact_id: String,
    pub config_drive_sha256: String,
    pub network_attachments: Vec<NetworkAttachmentSpec>,
}

/// Verified bytes that must be present on the agent before a create command
/// is dispatched. Implementations must source these bytes from managed,
/// digest-checked stores; paths are intentionally not part of this contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCreateArtifact {
    pub artifact_id: String,
    pub kind: agent_proto::ArtifactKind,
    pub sha256: String,
    pub format: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait CreateArtifactResolver: Send + Sync {
    async fn resolve_artifacts(
        &self,
        request: &CreateInstanceRequest,
        agent: &NodeSnapshot,
        inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError>;
}

#[derive(Debug, Default)]
pub struct UnconfiguredCreateArtifactResolver;

#[async_trait]
impl CreateArtifactResolver for UnconfiguredCreateArtifactResolver {
    async fn resolve_artifacts(
        &self,
        _request: &CreateInstanceRequest,
        _agent: &NodeSnapshot,
        _inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError> {
        Err(ProviderError::InvalidRequest)
    }
}

/// Resolves control-plane-owned resources into the bounded protocol inputs
/// required by an agent. Implementations must return verified references and
/// digests; returning placeholder values is intentionally not supported.
#[async_trait]
pub trait ResolvedCreateResolver: Send + Sync {
    async fn resolve(
        &self,
        request: &CreateInstanceRequest,
        agent: &NodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError>;
}

/// A resolver used by profiles that have not yet wired image/config-drive/
/// network realization. It fails closed, making the missing integration
/// explicit instead of sending fabricated protocol data to a host.
#[derive(Debug, Default)]
pub struct UnconfiguredResolvedCreateResolver;

#[async_trait]
impl ResolvedCreateResolver for UnconfiguredResolvedCreateResolver {
    async fn resolve(
        &self,
        _request: &CreateInstanceRequest,
        _agent: &NodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError> {
        Err(ProviderError::InvalidRequest)
    }
}

#[derive(Debug, Clone)]
struct AgentBinding {
    resource_id: String,
    agent_id: String,
    agent_epoch: String,
    provider_resource_id: Option<String>,
}

#[derive(Default)]
struct AgentProviderState {
    operations: HashMap<Uuid, Operation>,
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
    registry: NodeRegistry,
    resolver: Arc<dyn ResolvedCreateResolver>,
    artifact_resolver: Arc<dyn CreateArtifactResolver>,
    state: Arc<RwLock<AgentProviderState>>,
    store: Option<Arc<SqliteStore>>,
    command_timeout: Duration,
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
        store: Option<Arc<SqliteStore>>,
    ) -> Self {
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        let mut events = registry.subscribe_events();
        let event_state = state.clone();
        let event_store = store.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        apply_agent_provider_event(&event_state, event_store.as_ref(), event).await
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
        command: o3k_provider_contract::compute_proto::Command,
        operation_id: Uuid,
        timeout: Duration,
    ) -> Result<o3k_provider_contract::compute_proto::BlockDeviceObservation, ProviderError> {
        self.persist_pending_command(&command, operation_id).await?;
        self.registry
            .dispatch_command_and_wait(command, timeout)
            .await
            .map_err(|error| match error {
                o3k_compute_agent::AgentError::Protocol(message)
                    if message.contains("observation timed out") =>
                {
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
            if resource.observed_state.eq_ignore_ascii_case("DELETED") {
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
        if snapshot.desired_state
            != o3k_provider_contract::compute_proto::AdministrativeState::Enabled as i32
        {
            return Err(ProviderError::Retryable);
        }
        Ok(snapshot)
    }

    async fn dispatch(
        &self,
        command: o3k_provider_contract::compute_proto::Command,
    ) -> Result<(), ProviderError> {
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
        command: o3k_provider_contract::compute_proto::Command,
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
                let recorded = o3k_provider_contract::compute_proto::Command::decode(
                    existing.payload.as_slice(),
                )
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
        command: o3k_provider_contract::compute_proto::Command,
        operation_id: Uuid,
    ) -> Result<Operation, ProviderError> {
        let operation = self.accepted_operation(operation_id).await?;
        if let Err(error) = self.dispatch(command).await {
            self.state.write().await.operations.remove(&operation_id);
            return Err(error);
        }
        Ok(operation)
    }

    async fn persist_pending_command(
        &self,
        command: &o3k_provider_contract::compute_proto::Command,
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
    store: Arc<SqliteStore>,
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
        let inputs = match resolver.resolve(&request, &agent).await {
            Ok(inputs) => inputs,
            Err(error) => {
                tracing::warn!(transfer_id = %transfer.transfer_id, %error, "artifact transfer source recovery failed");
                continue;
            }
        };
        let artifacts = match artifact_resolver
            .resolve_artifacts(&request, &agent, &inputs)
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

fn map_agent_error(error: o3k_compute_agent::AgentError) -> ProviderError {
    match error {
        o3k_compute_agent::AgentError::Protocol(message) => {
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
        o3k_compute_agent::AgentError::Transport(_)
        | o3k_compute_agent::AgentError::IdentityStore(_)
        | o3k_compute_agent::AgentError::TlsMaterial
        | o3k_compute_agent::AgentError::InvalidConfiguration(_) => ProviderError::Retryable,
    }
}

fn artifact_kind_name(kind: agent_proto::ArtifactKind) -> &'static str {
    match kind {
        agent_proto::ArtifactKind::ImageBase => "image_base",
        agent_proto::ArtifactKind::ConfigDriveIso => "config_drive_iso",
        agent_proto::ArtifactKind::Unspecified => "unspecified",
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

fn operation_state_from_proto(state: i32) -> Option<o3k_provider::OperationState> {
    use o3k_provider_contract::compute_proto::OperationState as WireState;
    match state {
        value if value == WireState::Accepted as i32 => {
            Some(o3k_provider::OperationState::Accepted)
        }
        value if value == WireState::Running as i32 => Some(o3k_provider::OperationState::Running),
        value if value == WireState::Succeeded as i32 => {
            Some(o3k_provider::OperationState::Succeeded)
        }
        value if value == WireState::Failed as i32 => Some(o3k_provider::OperationState::Failed),
        value if value == WireState::UnknownOutcome as i32 => {
            Some(o3k_provider::OperationState::UnknownOutcome)
        }
        _ => None,
    }
}

fn error_category_from_proto(category: i32) -> Option<o3k_provider::ErrorCategory> {
    use o3k_provider_contract::compute_proto::ErrorCategory as WireCategory;
    match category {
        value if value == WireCategory::InvalidRequest as i32 => {
            Some(o3k_provider::ErrorCategory::InvalidRequest)
        }
        value if value == WireCategory::Conflict as i32 => {
            Some(o3k_provider::ErrorCategory::Conflict)
        }
        value if value == WireCategory::Capacity as i32 => {
            Some(o3k_provider::ErrorCategory::Capacity)
        }
        value if value == WireCategory::NotFound as i32 => {
            Some(o3k_provider::ErrorCategory::NotFound)
        }
        value if value == WireCategory::Retryable as i32 => {
            Some(o3k_provider::ErrorCategory::Retryable)
        }
        value if value == WireCategory::UnknownOutcome as i32 => {
            Some(o3k_provider::ErrorCategory::UnknownOutcome)
        }
        value if value == WireCategory::Terminal as i32 => {
            Some(o3k_provider::ErrorCategory::Terminal)
        }
        _ => None,
    }
}

fn provider_error_category_from_name(name: &str) -> Option<o3k_provider::ErrorCategory> {
    match name {
        "invalid_request" => Some(o3k_provider::ErrorCategory::InvalidRequest),
        "conflict" => Some(o3k_provider::ErrorCategory::Conflict),
        "capacity" => Some(o3k_provider::ErrorCategory::Capacity),
        "not_found" => Some(o3k_provider::ErrorCategory::NotFound),
        "retryable" => Some(o3k_provider::ErrorCategory::Retryable),
        "unknown_outcome" => Some(o3k_provider::ErrorCategory::UnknownOutcome),
        "terminal" => Some(o3k_provider::ErrorCategory::Terminal),
        _ => None,
    }
}

fn durable_inspect_error(error: &ProviderError) -> (o3k_store::OperationState, &'static str) {
    match error {
        ProviderError::Retryable | ProviderError::Storage => {
            (o3k_store::OperationState::Retryable, "retryable")
        }
        ProviderError::UnknownOutcome { .. } | ProviderError::StaleState => {
            (o3k_store::OperationState::UnknownOutcome, "unknown_outcome")
        }
        ProviderError::InvalidRequest => (o3k_store::OperationState::Failed, "invalid_request"),
        ProviderError::NotFound => (o3k_store::OperationState::Failed, "not_found"),
        ProviderError::Conflict => (o3k_store::OperationState::Failed, "conflict"),
        ProviderError::Capacity => (o3k_store::OperationState::Failed, "capacity"),
        ProviderError::Terminal => (o3k_store::OperationState::Failed, "terminal"),
        ProviderError::UnsupportedBlockDevice(_) => (o3k_store::OperationState::Failed, "terminal"),
    }
}

fn instance_state_from_proto(state: i32) -> Option<o3k_provider::InstanceState> {
    use o3k_provider_contract::compute_proto::ResourceState as WireState;
    match state {
        value if value == WireState::Creating as i32 => Some(o3k_provider::InstanceState::Creating),
        value if value == WireState::Running as i32 => Some(o3k_provider::InstanceState::Running),
        value if value == WireState::Stopped as i32 => Some(o3k_provider::InstanceState::Stopped),
        value if value == WireState::Deleting as i32 => Some(o3k_provider::InstanceState::Deleting),
        value if value == WireState::Deleted as i32 => Some(o3k_provider::InstanceState::Deleted),
        value if value == WireState::Error as i32 => Some(o3k_provider::InstanceState::Error),
        _ => None,
    }
}

fn instance_state_from_observed(value: &str) -> Option<o3k_provider::InstanceState> {
    match value.to_ascii_uppercase().as_str() {
        "ACTIVE" | "RUNNING" => Some(o3k_provider::InstanceState::Running),
        "SHUTOFF" | "STOPPED" => Some(o3k_provider::InstanceState::Stopped),
        "BUILD" | "CREATING" | "REQUESTED" => Some(o3k_provider::InstanceState::Creating),
        "DELETING" => Some(o3k_provider::InstanceState::Deleting),
        "DELETED" => Some(o3k_provider::InstanceState::Deleted),
        "ERROR" => Some(o3k_provider::InstanceState::Error),
        _ => None,
    }
}

async fn apply_artifact_status(
    store: &SqliteStore,
    status: &agent_proto::ArtifactStatus,
) -> Result<(), StoreError> {
    let transfer = store.get_artifact_transfer(&status.transfer_id).await?;
    let operation_id = Uuid::parse_str(&status.operation_id).map_err(|_| {
        StoreError::ArtifactTransferConflict("invalid operation identity".to_owned())
    })?;
    let resource_id = Uuid::parse_str(&status.resource_id).map_err(|_| {
        StoreError::ArtifactTransferConflict("invalid resource identity".to_owned())
    })?;
    if transfer.command_id != status.command_id
        || transfer.operation_id != operation_id
        || transfer.resource_id != resource_id
        || transfer.agent_id != status.agent_id
    {
        return Err(StoreError::ArtifactTransferConflict(
            "artifact status identity conflicts with durable state".to_owned(),
        ));
    }
    let state = match agent_proto::ArtifactTransferState::try_from(status.state) {
        Ok(agent_proto::ArtifactTransferState::Offered) => ArtifactTransferState::Offered,
        Ok(agent_proto::ArtifactTransferState::Receiving) => ArtifactTransferState::Receiving,
        Ok(agent_proto::ArtifactTransferState::Committed) => ArtifactTransferState::Committed,
        Ok(agent_proto::ArtifactTransferState::Rejected) => ArtifactTransferState::Rejected,
        Ok(agent_proto::ArtifactTransferState::Expired) => ArtifactTransferState::Expired,
        _ => {
            return Err(StoreError::ArtifactTransferConflict(
                "artifact status state is not recoverable".to_owned(),
            ));
        }
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

async fn apply_agent_provider_event(
    state: &Arc<RwLock<AgentProviderState>>,
    store: Option<&Arc<SqliteStore>>,
    event: o3k_compute_agent::AgentEvent,
) {
    let mut state = state.write().await;
    match event {
        o3k_compute_agent::AgentEvent::CommandAccepted(accepted) => {
            if let Some(store) = store
                && let Err(error) = store
                    .update_agent_command(
                        &accepted.command_id,
                        AgentCommandState::Accepted,
                        accepted.operation_sequence,
                        accepted.operation_sequence,
                        Some(&accepted.operation_id),
                        None,
                    )
                    .await
            {
                tracing::debug!(%error, command_id = %accepted.command_id, "agent command acceptance was not durably projected");
            }
            if let Ok(operation_id) = Uuid::parse_str(&accepted.operation_id)
                && let Some(operation) = state.operations.get_mut(&operation_id)
                && let Some(next) = operation_state_from_proto(accepted.state)
            {
                operation.state = next;
            }
        }
        o3k_compute_agent::AgentEvent::Operation(update) => {
            if let Some(store) = store
                && let Ok(operation_id) = Uuid::parse_str(&update.operation_id)
                && let Ok(command) = store.get_agent_command_by_operation(operation_id).await
            {
                let state = match operation_state_from_proto(update.state) {
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
                        (!update.provider_resource_id.is_empty())
                            .then_some(update.provider_resource_id.as_str()),
                    )
                    .await
                {
                    tracing::debug!(%error, operation_id = %update.operation_id, "agent operation was not durably projected");
                }
            }
            if let Ok(operation_id) = Uuid::parse_str(&update.operation_id)
                && let Some(operation) = state.operations.get_mut(&operation_id)
            {
                if let Some(next) = operation_state_from_proto(update.state) {
                    operation.state = next;
                }
                operation.error_category = error_category_from_proto(update.error_category);
                if !update.provider_resource_id.is_empty() {
                    operation.provider_resource_id = Some(update.provider_resource_id);
                }
            }
        }
        o3k_compute_agent::AgentEvent::Observation(observation) => {
            let Some(instance_state) = instance_state_from_proto(observation.state) else {
                return;
            };
            let provider_id = observation.provider_resource_id.clone();
            if !provider_id.is_empty() {
                if let Some(store) = store
                    && let Ok(resource_id) = Uuid::parse_str(&observation.resource_id)
                {
                    let reference = o3k_store::ProviderReference {
                        resource_id,
                        provider_name: "agent".to_owned(),
                        provider_resource_id: provider_id.clone(),
                    };
                    if let Err(error) = store.attach_provider_reference(&reference).await
                        && !matches!(error, StoreError::ProviderReferenceAlreadyExists)
                    {
                        tracing::debug!(%error, resource_id = %observation.resource_id, "agent provider reference was not durably projected");
                    }
                }
                state.bindings.insert(
                    provider_id.clone(),
                    AgentBinding {
                        resource_id: observation.resource_id.clone(),
                        agent_id: observation.agent_id.clone(),
                        agent_epoch: observation.agent_epoch.clone(),
                        provider_resource_id: Some(provider_id.clone()),
                    },
                );
                state.instances.insert(
                    provider_id.clone(),
                    Instance {
                        provider_instance_id: provider_id.clone(),
                        o3k_server_id: Uuid::parse_str(&observation.resource_id)
                            .unwrap_or(Uuid::nil()),
                        state: instance_state,
                        observed_message: (!observation.redacted_message.is_empty())
                            .then_some(observation.redacted_message.clone()),
                    },
                );
            }
            if let Ok(operation_id) = Uuid::parse_str(&observation.operation_id)
                && let Some(operation) = state.operations.get_mut(&operation_id)
            {
                if let Some(next) = operation_state_from_proto(observation.operation_state) {
                    operation.state = next;
                }
                if !provider_id.is_empty() {
                    operation.provider_resource_id = Some(provider_id);
                }
            }
        }
        o3k_compute_agent::AgentEvent::Error(error) => {
            if let Ok(operation_id) = Uuid::parse_str(&error.operation_id)
                && let Some(operation) = state.operations.get_mut(&operation_id)
            {
                operation.state = if error.retryable {
                    o3k_provider::OperationState::Retryable
                } else {
                    o3k_provider::OperationState::Failed
                };
                operation.error_category = error_category_from_proto(error.category);
            }
        }
        o3k_compute_agent::AgentEvent::ArtifactAck(_ack) => {
            // The foreground create path owns the durable commit after its
            // waiter receives this acknowledgement. Persisting the same
            // transition here races that writer on SQLite. ArtifactStatus
            // events remain the asynchronous recovery projection.
        }
        o3k_compute_agent::AgentEvent::ArtifactStatus(status) => {
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
/// and makes the provider unschedulable; capability flags and disk formats are
/// never treated as capacity.
pub fn agent_inventory(
    capabilities: &o3k_provider_contract::compute_proto::Capabilities,
) -> BTreeMap<String, o3k_placement::Inventory> {
    BTreeMap::from([
        (
            o3k_placement::VCPU.to_owned(),
            o3k_placement::Inventory {
                total: u64::from(capabilities.max_vcpus),
                reserved: 0,
                allocation_ratio: 1.0,
                used: 0,
            },
        ),
        (
            o3k_placement::MEMORY_MB.to_owned(),
            o3k_placement::Inventory {
                total: capabilities.max_memory_mib,
                reserved: 0,
                allocation_ratio: 1.0,
                used: 0,
            },
        ),
        (
            o3k_placement::DISK_GB.to_owned(),
            o3k_placement::Inventory {
                total: capabilities.max_disk_gb,
                reserved: 0,
                allocation_ratio: 1.0,
                used: 0,
            },
        ),
    ])
}

fn agent_provider_state(
    snapshot: &o3k_compute_agent::NodeSnapshot,
) -> o3k_placement::ProviderState {
    use o3k_compute_agent::Availability;
    use o3k_provider_contract::compute_proto::AdministrativeState;

    if snapshot.availability != Availability::Available
        || snapshot.desired_state == AdministrativeState::Disabled as i32
        || snapshot.capabilities.max_vcpus == 0
        || snapshot.capabilities.max_memory_mib == 0
        || snapshot.capabilities.max_disk_gb == 0
    {
        o3k_placement::ProviderState::Unavailable
    } else if snapshot.desired_state == AdministrativeState::Draining as i32 {
        o3k_placement::ProviderState::Draining
    } else {
        o3k_placement::ProviderState::Enabled
    }
}

/// Synchronizes the current authenticated agent snapshots into Placement.
/// The stable agent ID is the Placement provider ID, so reconnects update the
/// same provider and preserve durable allocations.
pub async fn sync_agent_inventory(
    registry: &NodeRegistry,
    placement: &o3k_placement::PlacementLedger,
) -> Result<(), SchedulerError> {
    for snapshot in registry.all().await {
        placement.sync_provider(
            &snapshot.agent_id,
            agent_inventory(&snapshot.capabilities),
            agent_provider_state(&snapshot),
        )?;
    }
    Ok(())
}

/// Starts the bounded periodic inventory publisher used by `o3kd`.
pub fn spawn_agent_inventory_publisher(
    registry: NodeRegistry,
    placement: o3k_placement::PlacementLedger,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = sync_agent_inventory(&registry, &placement).await {
                tracing::warn!(%error, "agent inventory publication failed");
            }
        }
    })
}

impl<P: ComputeProvider + 'static> From<Arc<P>> for ProviderBackend {
    fn from(provider: Arc<P>) -> Self {
        Self(provider)
    }
}

#[async_trait]
impl ComputeProvider for AgentComputeProvider {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
        let mut nodes = self.registry.all().await;
        nodes.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        let node = nodes
            .into_iter()
            .find(|node| {
                node.availability == Availability::Available
                    && node.desired_state
                        == o3k_provider_contract::compute_proto::AdministrativeState::Enabled as i32
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
        let resolved = self
            .resolver
            .resolve(&request, &agent)
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
            .resolve_artifacts(&request, &agent, &artifact_inputs)
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
                agent_proto::ArtifactKind::ImageBase,
                &artifact_inputs.image_artifact_id,
                &artifact_inputs.image_sha256,
                artifact_inputs.image_format.as_str(),
            ),
            (
                agent_proto::ArtifactKind::ConfigDriveIso,
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
                || artifact.kind == agent_proto::ArtifactKind::Unspecified
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
            let chunk_size = o3k_compute_agent::MAX_ARTIFACT_CHUNK_BYTES as u64;
            let chunk_count = u32::try_from(size_bytes.div_ceil(chunk_size)).map_err(|_| {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    artifact_kind = artifact_kind_name(artifact.kind),
                    "create artifact chunk count is invalid"
                );
                ProviderError::InvalidRequest
            })?;
            let transfer_id = o3k_compute_agent::deterministic_artifact_transfer_id(
                &command.command_id,
                artifact.kind,
                &artifact.artifact_id,
            );
            let offer = agent_proto::ArtifactOffer {
                transfer_id,
                command_id: command.command_id.clone(),
                operation_id: command.operation_id.clone(),
                resource_id: command.resource_id.clone(),
                agent_id: agent.agent_id.clone(),
                artifact_id: artifact.artifact_id,
                kind: artifact.kind as i32,
                sha256: artifact.sha256,
                size_bytes,
                format: artifact.format,
                chunk_size_bytes: o3k_compute_agent::MAX_ARTIFACT_CHUNK_BYTES as u32,
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
            o3k_compute_agent::BlockDeviceCommand::CollectConnector,
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
            host: observation.host_name,
            ip: observation.ip_address,
            platform: "x86_64".to_owned(),
            os_type: "linux".to_owned(),
            multipath: false,
            initiator: (!observation.initiator.is_empty()).then_some(observation.initiator),
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
            o3k_compute_agent::BlockDeviceCommand::Attach {
                device: o3k_provider_contract::compute_proto::AttachDiskCommand {
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
        Ok(BlockDeviceObservation {
            volume_id: observation.volume_id,
            attachment_id: observation.attachment_id,
            driver_volume_type: observation.driver_volume_type,
            device_path: (!observation.device_path.is_empty()).then_some(observation.device_path),
            host_path: (!observation.host_path.is_empty()).then_some(observation.host_path),
            attached: observation.attached,
            found: observation.found,
        })
    }

    async fn detach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            o3k_compute_agent::BlockDeviceCommand::Detach {
                device: o3k_provider_contract::compute_proto::DetachDiskCommand {
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
        Ok(BlockDeviceObservation {
            volume_id: observation.volume_id,
            attachment_id: observation.attachment_id,
            driver_volume_type: observation.driver_volume_type,
            device_path: (!observation.device_path.is_empty()).then_some(observation.device_path),
            host_path: (!observation.host_path.is_empty()).then_some(observation.host_path),
            attached: observation.attached,
            found: observation.found,
        })
    }

    async fn observe_block_device(
        &self,
        resource_id: Uuid,
        volume_id: &str,
    ) -> Result<Option<BlockDeviceObservation>, ProviderError> {
        let (agent, binding_resource) = self.agent_for_server(resource_id).await?;
        let operation_id = Uuid::now_v7();
        let command = build_block_device_command(
            o3k_compute_agent::BlockDeviceCommand::Observe {
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
        Ok(Some(BlockDeviceObservation {
            volume_id: observation.volume_id,
            attachment_id: observation.attachment_id,
            driver_volume_type: observation.driver_volume_type,
            device_path: (!observation.device_path.is_empty()).then_some(observation.device_path),
            host_path: (!observation.host_path.is_empty()).then_some(observation.host_path),
            attached: observation.attached,
            found: observation.found,
        }))
    }
}

#[async_trait]
impl ComputeProvider for ProviderBackend {
    async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
        self.0.capabilities().await
    }
    async fn create_instance(
        &self,
        request: CreateInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        self.0.create_instance(request).await
    }
    async fn get_instance(&self, id: &str) -> Result<Instance, ProviderError> {
        self.0.get_instance(id).await
    }
    async fn inspect_instance(
        &self,
        provider_id: &str,
        resource_id: &str,
        id: &str,
        operation_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Operation, ProviderError> {
        self.0
            .inspect_instance(provider_id, resource_id, id, operation_id, idempotency_key)
            .await
    }
    async fn delete_instance(
        &self,
        request: DeleteInstanceRequest,
    ) -> Result<Operation, ProviderError> {
        self.0.delete_instance(request).await
    }
    async fn action_instance(
        &self,
        id: &str,
        action: InstanceAction,
        operation_id: Uuid,
        key: &str,
    ) -> Result<Operation, ProviderError> {
        self.0.action_instance(id, action, operation_id, key).await
    }
    async fn get_operation(&self, id: Uuid) -> Result<Operation, ProviderError> {
        self.0.get_operation(id).await
    }
}

fn requests_match_with_keypair_migration(
    existing: &CreateInstanceRequest,
    requested: &CreateInstanceRequest,
) -> bool {
    if existing.keypair_id.is_some() || requested.keypair_id.is_none() {
        return false;
    }
    let migrated = CreateInstanceRequest {
        keypair_id: requested.keypair_id,
        ..existing.clone()
    };
    migrated == *requested
}

impl ComputeService {
    #[must_use]
    pub fn new<P>(store: Arc<SqliteStore>, provider: Arc<P>) -> Self
    where
        Arc<P>: Into<ProviderBackend>,
    {
        let provider = Arc::new(provider.into());
        let journal = OperationJournal::new(store.clone(), provider.clone(), 3);
        Self {
            store,
            provider,
            journal,
            scheduler: None,
            agent_registry: None,
        }
    }

    #[must_use]
    pub fn with_scheduler(mut self, scheduler: Scheduler) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Restricts scheduler candidates to agents that are currently registered,
    /// alive, and administratively enabled. The registry is intentionally
    /// optional so direct fake-provider operation keeps its existing behavior.
    #[must_use]
    pub fn with_agent_registry(mut self, registry: NodeRegistry) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    #[must_use]
    pub fn provider(&self) -> Arc<ProviderBackend> {
        self.provider.clone()
    }

    /// Applies a live authenticated agent result through the durable journal.
    /// The control-plane event consumer owns subscription and retry policy.
    pub async fn apply_agent_update(
        &self,
        update: &o3k_provider_contract::compute_proto::OperationUpdate,
    ) -> Result<o3k_store::OperationState, ComputeError> {
        Ok(self.journal.apply_agent_update(update).await?)
    }

    pub async fn apply_agent_acceptance(
        &self,
        accepted: &o3k_provider_contract::compute_proto::CommandAccepted,
    ) -> Result<o3k_store::OperationState, ComputeError> {
        Ok(self.journal.apply_agent_acceptance(accepted).await?)
    }

    /// Applies an authenticated provider observation to the durable resource
    /// projection. This is separate from operation progress because a command
    /// may succeed while the provider remains stopped, deleting, or errored.
    pub async fn apply_agent_observation(
        &self,
        observation: &o3k_provider_contract::compute_proto::Observation,
    ) -> Result<(), ComputeError> {
        Ok(self.journal.apply_agent_observation(observation).await?)
    }

    /// Starts the in-memory event bridge used by the control-plane binary.
    /// The journal remains the recovery authority; this task only applies live
    /// updates received from an authenticated agent connection.
    pub fn spawn_agent_event_consumer(
        &self,
        registry: NodeRegistry,
    ) -> tokio::task::JoinHandle<()> {
        let mut events = registry.subscribe_events();
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(o3k_compute_agent::AgentEvent::Operation(update)) => {
                        if let Err(error) = service.apply_agent_update(&update).await {
                            tracing::warn!(%error, "agent operation update rejected");
                        }
                    }
                    Ok(o3k_compute_agent::AgentEvent::CommandAccepted(accepted)) => {
                        if let Err(error) = service.apply_agent_acceptance(&accepted).await {
                            tracing::warn!(%error, "agent command acceptance rejected");
                        }
                    }
                    Ok(o3k_compute_agent::AgentEvent::Observation(observation)) => {
                        let current_epoch = registry
                            .snapshot(&observation.agent_id)
                            .await
                            .map(|node| node.agent_epoch);
                        if current_epoch.as_deref() != Some(observation.agent_epoch.as_str()) {
                            tracing::warn!(
                                agent_id = %observation.agent_id,
                                agent_epoch = %observation.agent_epoch,
                                current_epoch = ?current_epoch,
                                "ignored observation from a replaced agent epoch"
                            );
                            continue;
                        }
                        if let Err(error) = service.apply_agent_observation(&observation).await {
                            tracing::warn!(
                                %error,
                                operation_id = %observation.operation_id,
                                resource_id = %observation.resource_id,
                                agent_id = %observation.agent_id,
                                agent_epoch = %observation.agent_epoch,
                                operation_state = observation.operation_state,
                                state = observation.state,
                                provider_resource_id = %observation.provider_resource_id,
                                observation_sequence = observation.observation_sequence,
                                "agent resource observation rejected"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!(
                            count,
                            "agent event consumer lagged; durable recovery required"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::warn!("agent event stream closed");
                        break;
                    }
                }
            }
        })
    }

    #[must_use]
    pub fn flavors(&self) -> Vec<Flavor> {
        vec![
            Flavor {
                id: Uuid::from_u128(1),
                name: "test.small".to_owned(),
                vcpus: 1,
                ram_mib: 512,
                disk_gib: 10,
            },
            Flavor {
                id: Uuid::from_u128(2),
                name: "test.medium".to_owned(),
                vcpus: 2,
                ram_mib: 2048,
                disk_gib: 20,
            },
        ]
    }

    pub async fn flavors_for_project(&self, project_id: &str) -> Result<Vec<Flavor>, ComputeError> {
        let mut flavors = self.flavors();
        for resource in self
            .store
            .list_resources(project_id, "compute_flavor")
            .await?
        {
            if resource.observed_state == "DELETED" {
                continue;
            }
            let flavor: Flavor = serde_json::from_str(&resource.desired_state)
                .map_err(|_| ComputeError::Conflict)?;
            flavors.push(flavor);
        }
        Ok(flavors)
    }

    pub async fn create_flavor(
        &self,
        project_id: &str,
        name: String,
        vcpus: u32,
        ram_mib: u64,
        disk_gib: u64,
    ) -> Result<Flavor, ComputeError> {
        if project_id.trim().is_empty() || name.trim().is_empty() || vcpus == 0 || ram_mib == 0 {
            return Err(ComputeError::InvalidRequest);
        }
        if self
            .flavors_for_project(project_id)
            .await?
            .iter()
            .any(|flavor| flavor.name == name)
        {
            return Err(ComputeError::Conflict);
        }
        let flavor = Flavor {
            id: Uuid::now_v7(),
            name,
            vcpus,
            ram_mib,
            disk_gib,
        };
        let resource = o3k_store::ResourceRecord {
            id: flavor.id,
            kind: "compute_flavor".to_owned(),
            project_id: project_id.to_owned(),
            generation: 1,
            observed_generation: 1,
            desired_state: serde_json::to_string(&flavor).map_err(|_| ComputeError::Conflict)?,
            observed_state: "ACTIVE".to_owned(),
            provider_id: None,
        };
        self.store.insert_resource(&resource).await?;
        Ok(flavor)
    }

    pub async fn flavor_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<Flavor, ComputeError> {
        if let Some(flavor) = self.flavors().into_iter().find(|flavor| flavor.id == id) {
            return Ok(flavor);
        }
        self.flavors_for_project(project_id)
            .await?
            .into_iter()
            .find(|flavor| flavor.id == id)
            .ok_or(ComputeError::NotFound)
    }

    pub async fn delete_flavor(&self, project_id: &str, id: Uuid) -> Result<(), ComputeError> {
        if self.flavors().into_iter().any(|flavor| flavor.id == id) {
            return Err(ComputeError::Conflict);
        }
        let resource = self
            .store
            .get_resource(id)
            .await
            .map_err(|error| match error {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })?;
        if resource.kind != "compute_flavor" || resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let flavor: Flavor =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        for server in self
            .store
            .list_resources(project_id, "compute_instance")
            .await?
        {
            if !server.observed_state.eq_ignore_ascii_case("DELETED")
                && serde_json::from_str::<serde_json::Value>(&server.desired_state)
                    .ok()
                    .is_some_and(|value| {
                        value
                            .get("flavor_id")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| value == flavor.id.to_string())
                            || (value.get("flavor_id").is_none()
                                && value.get("vcpus").and_then(serde_json::Value::as_u64)
                                    == Some(u64::from(flavor.vcpus))
                                && value.get("memory_mib").and_then(serde_json::Value::as_u64)
                                    == Some(flavor.ram_mib))
                    })
            {
                return Err(ComputeError::Conflict);
            }
        }
        self.store
            .update_resource(
                id,
                resource.generation,
                &resource.desired_state,
                "DELETED",
                resource.observed_generation,
                None,
            )
            .await?;
        Ok(())
    }

    pub fn flavor(&self, id: Uuid) -> Result<Flavor, ComputeError> {
        self.flavors()
            .into_iter()
            .find(|flavor| flavor.id == id)
            .ok_or(ComputeError::NotFound)
    }

    pub async fn create_server(
        &self,
        project_id: &str,
        name: String,
        image_id: String,
        flavor_id: Uuid,
        network_ids: Vec<String>,
        idempotency_key: String,
    ) -> Result<Server, ComputeError> {
        self.create_server_for_user(ServerCreateInput {
            user_id: String::new(),
            project_id: project_id.to_owned(),
            name,
            image_id,
            flavor_id,
            network_ids,
            key_name: None,
            config_drive: None,
            idempotency_key,
        })
        .await
    }

    pub async fn create_server_for_user(
        &self,
        input: ServerCreateInput,
    ) -> Result<Server, ComputeError> {
        let ServerCreateInput {
            user_id,
            project_id,
            name,
            image_id,
            flavor_id,
            network_ids,
            key_name,
            config_drive,
            idempotency_key,
        } = input;
        if name.trim().is_empty()
            || image_id.trim().is_empty()
            || network_ids.is_empty()
            || network_ids.iter().any(|id| id.trim().is_empty())
            || idempotency_key.trim().is_empty()
        {
            return Err(ComputeError::InvalidRequest);
        }
        let keypair = match key_name.as_deref() {
            Some(name) => Some(
                self.store
                    .get_keypair(&user_id, &project_id, name)
                    .await
                    .map_err(|error| match error {
                        StoreError::KeypairNotFound => ComputeError::NotFound,
                        other => ComputeError::Store(other),
                    })?,
            ),
            None => None,
        };
        let flavor = self.flavor_for_project(&project_id, flavor_id).await?;
        let server_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:server:{project_id}:{idempotency_key}").as_bytes(),
        );
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:operation:{project_id}:{idempotency_key}").as_bytes(),
        );
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: server_id,
            project_id: project_id.to_owned(),
            name: name.clone(),
            vcpus: flavor.vcpus,
            memory_mib: flavor.ram_mib,
            flavor_id: flavor.id.to_string(),
            disk_gib: flavor.disk_gib,
            image_id: Some(image_id.clone()),
            key_name: key_name.clone(),
            keypair_id: keypair.as_ref().map(|value| value.id),
            network_ids: network_ids.clone(),
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: config_drive.clone(),
            idempotency_key: idempotency_key.clone(),
        };
        match self.store.get_resource(server_id).await {
            Ok(existing) => {
                let existing_request: CreateInstanceRequest =
                    serde_json::from_str(&existing.desired_state)
                        .map_err(|_| ComputeError::Conflict)?;
                let existing_request = CreateInstanceRequest {
                    placement_provider_id: None,
                    placement_allocation_id: None,
                    ..existing_request
                };
                let legacy_keypair_intent =
                    requests_match_with_keypair_migration(&existing_request, &request);
                if existing_request == request || legacy_keypair_intent {
                    if existing.observed_state == "DELETED" {
                        return Err(ComputeError::NotFound);
                    }
                    if legacy_keypair_intent {
                        let desired_state =
                            serde_json::to_string(&request).map_err(|_| ComputeError::Conflict)?;
                        self.store
                            .update_resource(
                                existing.id,
                                existing.generation,
                                &desired_state,
                                &existing.observed_state,
                                existing.observed_generation,
                                existing.provider_id.as_deref(),
                            )
                            .await?;
                    }
                    let attached = self.store.get_server_keypair_name(server_id).await?;
                    let mut repaired_association = false;
                    if attached != key_name {
                        if attached.is_none() {
                            if let Some(keypair) = keypair.as_ref() {
                                self.store
                                    .attach_server_keypair(server_id, keypair.id)
                                    .await?;
                                repaired_association = true;
                            } else {
                                return Err(ComputeError::Conflict);
                            }
                        } else {
                            return Err(ComputeError::Conflict);
                        }
                    }
                    if repaired_association {
                        match self
                            .journal
                            .reconcile_once(existing_request.operation_id)
                            .await
                        {
                            Ok(o3k_store::OperationState::Failed) => {
                                self.store.detach_server_keypair(server_id).await?;
                                return Err(ComputeError::Conflict);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                self.store.detach_server_keypair(server_id).await?;
                                return Err(ComputeError::Reconcile(error));
                            }
                        }
                    }
                    return self.show_server(&project_id, server_id).await;
                }
                return Err(ComputeError::Conflict);
            }
            Err(StoreError::ResourceNotFound) => {}
            Err(error) => return Err(ComputeError::Store(error)),
        }
        // A name conflict is deterministic control-plane state. Reject it
        // before reserving Placement capacity; the second check below still
        // protects against a concurrent create racing this read.
        if self
            .list_servers(&project_id)
            .await?
            .iter()
            .any(|server| server.name == name && server.status != "DELETED")
        {
            return Err(ComputeError::Conflict);
        }
        let scheduler_flavor = SchedulerFlavor {
            vcpus: flavor.vcpus as u64,
            memory_mb: flavor.ram_mib,
            disk_gb: flavor.disk_gib,
        };
        let placement = match (self.scheduler.as_ref(), self.agent_registry.as_ref()) {
            (Some(scheduler), Some(registry)) => {
                let eligible = registry
                    .all()
                    .await
                    .into_iter()
                    .filter(|node| {
                        node.availability == Availability::Available
                            && node.desired_state
                                == o3k_provider_contract::compute_proto::AdministrativeState::Enabled
                                    as i32
                    })
                    .map(|node| node.agent_id)
                    .collect::<BTreeSet<_>>();
                Some(scheduler.schedule_for_agents(
                    &eligible,
                    &server_id.to_string(),
                    scheduler_flavor,
                )?)
            }
            (Some(scheduler), None) => {
                Some(scheduler.schedule(&server_id.to_string(), scheduler_flavor)?)
            }
            (None, _) => None,
        };
        let request = CreateInstanceRequest {
            placement_provider_id: placement
                .as_ref()
                .map(|decision| decision.provider_id.clone()),
            placement_allocation_id: placement
                .as_ref()
                .map(|decision| decision.allocation_id.clone()),
            ..request
        };
        let servers = match self.list_servers(&project_id).await {
            Ok(servers) => servers,
            Err(error) => {
                if let Some(decision) = placement.as_ref() {
                    self.release_placement_decision(decision)?;
                }
                return Err(error);
            }
        };
        if servers
            .iter()
            .any(|server| server.name == name && server.status != "DELETED")
        {
            if let Some(decision) = placement.as_ref() {
                self.release_placement_decision(decision)?;
            }
            return Err(ComputeError::Conflict);
        }
        let request = CreateInstanceRequest {
            network_ids,
            ..request
        };
        let id = request.o3k_server_id;
        match self.journal.begin_create(&project_id, &request).await {
            Ok(_) => {}
            Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {
                let existing = self.store.get_resource(id).await?;
                let existing_request: CreateInstanceRequest =
                    serde_json::from_str(&existing.desired_state)
                        .map_err(|_| ComputeError::Conflict)?;
                let owns_persisted_placement = placement.as_ref().is_some_and(|decision| {
                    existing_request.placement_provider_id.as_deref()
                        == Some(decision.provider_id.as_str())
                        && existing_request.placement_allocation_id.as_deref()
                            == Some(decision.allocation_id.as_str())
                });
                if let Some(decision) = placement.as_ref()
                    && !owns_persisted_placement
                {
                    self.release_placement_decision(decision)?;
                }
                let legacy_keypair_intent =
                    requests_match_with_keypair_migration(&existing_request, &request);
                if existing_request != request && !legacy_keypair_intent {
                    return Err(ComputeError::Conflict);
                }
                if existing.observed_state == "DELETED" {
                    return Err(ComputeError::NotFound);
                }
                if legacy_keypair_intent {
                    let desired_state =
                        serde_json::to_string(&request).map_err(|_| ComputeError::Conflict)?;
                    self.store
                        .update_resource(
                            existing.id,
                            existing.generation,
                            &desired_state,
                            &existing.observed_state,
                            existing.observed_generation,
                            existing.provider_id.as_deref(),
                        )
                        .await?;
                }
                let attached = self.store.get_server_keypair_name(id).await?;
                let mut repaired_association = false;
                if attached != request.key_name {
                    if attached.is_none() {
                        if let Some(keypair) = keypair.as_ref() {
                            self.store.attach_server_keypair(id, keypair.id).await?;
                            repaired_association = true;
                        } else {
                            return Err(ComputeError::Conflict);
                        }
                    } else {
                        return Err(ComputeError::Conflict);
                    }
                }
                if repaired_association {
                    match self.journal.reconcile_once(request.operation_id).await {
                        Ok(o3k_store::OperationState::Failed) => {
                            self.store.detach_server_keypair(id).await?;
                            return Err(ComputeError::Conflict);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            self.store.detach_server_keypair(id).await?;
                            return Err(ComputeError::Reconcile(error));
                        }
                    }
                }
                return self.show_server(&project_id, id).await;
            }
            Err(error) => return Err(ComputeError::Reconcile(error)),
        }
        if let Some(keypair) = keypair {
            self.store.attach_server_keypair(id, keypair.id).await?;
        }
        let reconcile_state = match self.journal.reconcile_once(request.operation_id).await {
            Ok(state) => state,
            Err(error) => {
                self.store.detach_server_keypair(id).await?;
                tracing::warn!(
                    operation_id = %request.operation_id,
                    resource_id = %id,
                    error = %error,
                    "server create reconciliation returned an error"
                );
                return Err(ComputeError::Reconcile(error));
            }
        };
        if reconcile_state == o3k_store::OperationState::Failed {
            if let Ok(operation) = self.store.get_operation(request.operation_id).await {
                tracing::warn!(
                    operation_id = %request.operation_id,
                    resource_id = %id,
                    error_category = ?operation.error_category,
                    error_message = ?operation.error_message,
                    "server create reconciliation failed"
                );
            }
            self.store.detach_server_keypair(id).await?;
            if let (Some(scheduler), Some(provider_id), Some(allocation_id)) = (
                self.scheduler.as_ref(),
                request.placement_provider_id.as_deref(),
                request.placement_allocation_id.as_deref(),
            ) {
                scheduler.release_terminal(&o3k_scheduler::ScheduleDecision {
                    provider_id: provider_id.to_owned(),
                    allocation_id: allocation_id.to_owned(),
                    allocation: o3k_placement::Allocation {
                        provider_id: provider_id.to_owned(),
                        consumer_id: id.to_string(),
                        resources: std::collections::BTreeMap::new(),
                    },
                })?;
            }
            return Err(ComputeError::Conflict);
        }
        self.show_server(&project_id, id).await
    }

    pub async fn list_servers(&self, project_id: &str) -> Result<Vec<Server>, ComputeError> {
        let flavors = self.flavors_for_project(project_id).await?;
        let resources = self
            .store
            .list_resources(project_id, "compute_instance")
            .await?;
        let mut servers = Vec::new();
        for resource in resources {
            if let Ok(mut server) = server_from_resource(resource, &flavors)
                && server.status != "DELETED"
            {
                server.key_name = self.store.get_server_keypair_name(server.id).await?;
                servers.push(server);
            }
        }
        Ok(servers)
    }

    pub async fn show_server(&self, project_id: &str, id: Uuid) -> Result<Server, ComputeError> {
        let resource = self
            .store
            .get_resource(id)
            .await
            .map_err(|error| match error {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let flavors = self.flavors_for_project(project_id).await?;
        let mut server =
            server_from_resource(resource, &flavors).map_err(|_| ComputeError::InvalidRequest)?;
        if server.status == "DELETED" {
            return Err(ComputeError::NotFound);
        }
        server.key_name = self.store.get_server_keypair_name(server.id).await?;
        Ok(server)
    }

    pub async fn attach_volume(
        &self,
        project_id: &str,
        server_id: Uuid,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;

        let device = match device {
            Some(d) if !d.trim().is_empty() => {
                if d.starts_with("/dev/") {
                    d
                } else {
                    format!("/dev/{d}")
                }
            }
            _ => {
                let existing = self.store.list_volume_attachments(server_id).await?;
                let count = existing.len();
                let letter = (b'b' + count as u8) as char;
                format!("/dev/vd{letter}")
            }
        };

        let record = VolumeAttachmentRecord {
            id: volume_id,
            server_id,
            volume_id,
            device,
            tag,
            delete_on_termination,
            created_at: format!("{:?}", SystemTime::now()),
        };

        self.store
            .insert_volume_attachment(&record)
            .await
            .map_err(|err| match err {
                StoreError::ResourceAlreadyExists => ComputeError::Conflict,
                other => ComputeError::Store(other),
            })?;

        Ok(record)
    }

    pub async fn list_volume_attachments(
        &self,
        project_id: &str,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        Ok(self.store.list_volume_attachments(server_id).await?)
    }

    pub async fn get_volume_attachment(
        &self,
        project_id: &str,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.store
            .get_volume_attachment(server_id, attachment_id)
            .await?
            .ok_or(ComputeError::NotFound)
    }

    pub async fn detach_volume(
        &self,
        project_id: &str,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.store
            .delete_volume_attachment(server_id, attachment_id)
            .await
            .map_err(|err| match err {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })
    }

    /// Revalidates and inspects an already-created server through the
    /// provider boundary. This is deliberately read-only: an existing
    /// Placement allocation is checked, never recreated, before the provider
    /// receives an inspect request.
    pub async fn inspect_server(
        &self,
        project_id: &str,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<Operation, ComputeError> {
        let resource = self
            .store
            .get_resource(id)
            .await
            .map_err(|error| match error {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        let provider_id = intent
            .placement_provider_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        let allocation_id = intent
            .placement_allocation_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        if let Some(scheduler) = &self.scheduler {
            scheduler.validate_allocation(provider_id, allocation_id, &id.to_string())?;
        } else {
            return Err(ComputeError::Conflict);
        }
        let _reference = match self.store.get_provider_reference(id, "compute").await {
            Ok(reference) => reference,
            Err(StoreError::ProviderReferenceNotFound) => self
                .store
                .get_provider_reference(id, "compute-agent")
                .await
                .map_err(|error| match error {
                    StoreError::ProviderReferenceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?,
            Err(other) => return Err(ComputeError::Store(other)),
        };
        if idempotency_key.trim().is_empty() {
            return Err(ComputeError::InvalidRequest);
        }
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect:{project_id}:{id}:{idempotency_key}").as_bytes(),
        );
        let existing = self.store.get_operation(operation_id).await.ok();
        if let Some(record) = existing.as_ref()
            && matches!(
                record.state,
                o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
            )
        {
            let state = match record.state {
                o3k_store::OperationState::Succeeded => o3k_provider::OperationState::Succeeded,
                o3k_store::OperationState::Failed => o3k_provider::OperationState::Failed,
                _ => unreachable!("terminal state checked above"),
            };
            return Ok(Operation {
                provider_operation_id: record
                    .provider_operation_id
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .unwrap_or(operation_id),
                o3k_operation_id: operation_id,
                state,
                error_category: record
                    .error_category
                    .as_deref()
                    .and_then(provider_error_category_from_name),
                provider_resource_id: Some(_reference.provider_resource_id.clone()),
            });
        }
        if existing.is_none() {
            self.store
                .insert_operation(&o3k_store::OperationRecord {
                    id: operation_id,
                    resource_id: id,
                    kind: "inspect".to_owned(),
                    state: o3k_store::OperationState::Pending,
                    provider_operation_id: None,
                    error_category: None,
                    error_message: None,
                })
                .await?;
        }
        let result = self
            .provider
            .inspect_instance(
                provider_id,
                &id.to_string(),
                &_reference.provider_resource_id,
                operation_id,
                idempotency_key,
            )
            .await;
        match result {
            Ok(operation) => {
                let durable_state = match operation.state {
                    o3k_provider::OperationState::Succeeded => o3k_store::OperationState::Succeeded,
                    o3k_provider::OperationState::Failed => o3k_store::OperationState::Failed,
                    o3k_provider::OperationState::UnknownOutcome => {
                        o3k_store::OperationState::UnknownOutcome
                    }
                    _ => o3k_store::OperationState::Running,
                };
                self.store
                    .update_operation(
                        operation_id,
                        durable_state,
                        Some(&operation.provider_operation_id.to_string()),
                        None,
                        None,
                    )
                    .await?;
                Ok(operation)
            }
            Err(error) => {
                let (durable_state, category) = durable_inspect_error(&error);
                self.store
                    .update_operation(
                        operation_id,
                        durable_state,
                        None,
                        Some(category),
                        Some(&error.to_string()),
                    )
                    .await?;
                Err(error.into())
            }
        }
    }

    pub async fn create_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: String,
        public_key: String,
    ) -> Result<Keypair, ComputeError> {
        validate_keypair_name(&name)?;
        let (key_type, fingerprint, public_key) =
            o3k_store::validate_public_key(&public_key).map_err(ComputeError::Store)?;
        let record = o3k_store::KeypairRecord {
            id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:keypair:{user_id}:{project_id}:{name}").as_bytes(),
            ),
            user_id: user_id.to_owned(),
            project_id: project_id.to_owned(),
            name,
            key_type,
            public_key,
            fingerprint,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ComputeError::InvalidRequest)?
                .as_secs()
                .to_string(),
        };
        self.store
            .insert_keypair(&record)
            .await
            .map_err(|error| match error {
                StoreError::KeypairAlreadyExists => ComputeError::Conflict,
                other => ComputeError::Store(other),
            })?;
        Ok(keypair_from_record(record))
    }

    pub async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<Keypair>, ComputeError> {
        Ok(self
            .store
            .list_keypairs(user_id, project_id)
            .await?
            .into_iter()
            .map(keypair_from_record)
            .collect())
    }

    pub async fn show_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<Keypair, ComputeError> {
        self.store
            .get_keypair(user_id, project_id, name)
            .await
            .map(keypair_from_record)
            .map_err(|error| match error {
                StoreError::KeypairNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })
    }

    pub async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), ComputeError> {
        self.store
            .delete_keypair(user_id, project_id, name)
            .await
            .map_err(|error| match error {
                StoreError::KeypairNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })
    }

    /// Returns the placement agent bound to a project-owned server.
    pub async fn placement_provider_id(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<Option<String>, ComputeError> {
        let resource = self.store.get_resource(id).await?;
        if resource.kind != "compute_instance" || resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        Ok(request.placement_provider_id)
    }

    pub async fn delete_server(&self, project_id: &str, id: Uuid) -> Result<(), ComputeError> {
        let resource = self
            .store
            .get_resource(id)
            .await
            .map_err(|error| match error {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        if resource.observed_state == "DELETED" {
            let intent: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
                .map_err(|_| ComputeError::Conflict)?;
            self.release_placement_allocation(id, &intent)?;
            self.store.detach_server_keypair(id).await?;
            return Ok(());
        }
        if resource.provider_id.is_none() {
            return Err(ComputeError::Conflict);
        }
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:delete:{project_id}:{id}:{}", resource.generation).as_bytes(),
        );
        match self
            .journal
            .begin_lifecycle(id, operation_id, LifecycleAction::Delete)
            .await
        {
            Ok(_) | Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {}
            Err(error) => return Err(ComputeError::Reconcile(error)),
        }
        if self
            .reconcile_lifecycle_until_terminal(operation_id)
            .await?
            != o3k_store::OperationState::Succeeded
        {
            return Err(ComputeError::Conflict);
        }
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        self.release_placement_allocation(id, &intent)?;
        self.store.detach_server_keypair(id).await?;
        Ok(())
    }

    fn release_placement_allocation(
        &self,
        server_id: Uuid,
        intent: &CreateInstanceRequest,
    ) -> Result<(), ComputeError> {
        if let (Some(scheduler), Some(provider_id), Some(allocation_id)) = (
            self.scheduler.as_ref(),
            intent.placement_provider_id.as_deref(),
            intent.placement_allocation_id.as_deref(),
        ) {
            scheduler.release_terminal(&o3k_scheduler::ScheduleDecision {
                provider_id: provider_id.to_owned(),
                allocation_id: allocation_id.to_owned(),
                allocation: o3k_placement::Allocation {
                    provider_id: provider_id.to_owned(),
                    consumer_id: server_id.to_string(),
                    resources: std::collections::BTreeMap::new(),
                },
            })?;
        }
        Ok(())
    }

    fn release_placement_decision(
        &self,
        decision: &o3k_scheduler::ScheduleDecision,
    ) -> Result<(), ComputeError> {
        if let Some(scheduler) = self.scheduler.as_ref() {
            scheduler.release_terminal(decision)?;
        }
        Ok(())
    }

    pub async fn action(
        &self,
        project_id: &str,
        id: Uuid,
        action: InstanceAction,
    ) -> Result<Server, ComputeError> {
        let resource = self
            .store
            .get_resource(id)
            .await
            .map_err(|error| match error {
                StoreError::ResourceNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        if resource.provider_id.is_none() {
            return Err(ComputeError::Conflict);
        }
        let target = match (action, resource.observed_state.as_str()) {
            (InstanceAction::Start, "stopped" | "SHUTOFF") => "ACTIVE",
            (InstanceAction::Stop, "active" | "ACTIVE") => "SHUTOFF",
            (InstanceAction::Reboot, "active" | "ACTIVE" | "stopped" | "SHUTOFF") => "ACTIVE",
            _ => return Err(ComputeError::Conflict),
        };
        let lifecycle_action = match action {
            InstanceAction::Start => LifecycleAction::Start,
            InstanceAction::Stop => LifecycleAction::Stop,
            InstanceAction::Reboot => LifecycleAction::Reboot,
        };
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:action:{project_id}:{id}:{target}:{}",
                resource.generation
            )
            .as_bytes(),
        );
        match self
            .journal
            .begin_lifecycle(id, operation_id, lifecycle_action)
            .await
        {
            Ok(_) | Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {}
            Err(error) => return Err(ComputeError::Reconcile(error)),
        }
        if self
            .reconcile_lifecycle_until_terminal(operation_id)
            .await?
            != o3k_store::OperationState::Succeeded
        {
            return Err(ComputeError::Conflict);
        }
        self.show_server(project_id, id).await
    }

    /// Drives a lifecycle operation to a terminal state. Agent-backed
    /// providers complete commands asynchronously, so a single reconcile pass
    /// almost always returns `Running`; polling briefly preserves the
    /// synchronous action contract without inventing new API semantics.
    /// Passes are idempotent, so transient store races with the live
    /// observation consumer are retried within the same bounded budget;
    /// only a terminal outcome or a deterministic intent error ends the wait.
    async fn reconcile_lifecycle_until_terminal(
        &self,
        operation_id: Uuid,
    ) -> Result<o3k_store::OperationState, ComputeError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut last_error = None;
        loop {
            match self.journal.reconcile_lifecycle_once(operation_id).await {
                Ok(
                    state @ (o3k_store::OperationState::Succeeded
                    | o3k_store::OperationState::Failed),
                ) => return Ok(state),
                Ok(_) => {}
                Err(ReconcileError::InvalidIntent) => {
                    return Err(ComputeError::Reconcile(ReconcileError::InvalidIntent));
                }
                Err(error) => last_error = Some(ComputeError::Reconcile(error)),
            }
            if std::time::Instant::now() >= deadline {
                return match last_error {
                    Some(error) => Err(error),
                    None => Ok(o3k_store::OperationState::Running),
                };
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn server_from_resource(
    resource: o3k_store::ResourceRecord,
    flavors: &[Flavor],
) -> Result<Server, ()> {
    let request: CreateInstanceRequest =
        serde_json::from_str(&resource.desired_state).map_err(|_| ())?;
    let flavor = if request.flavor_id.trim().is_empty() {
        flavors
            .iter()
            .find(|flavor| flavor.vcpus == request.vcpus && flavor.ram_mib == request.memory_mib)
    } else {
        let flavor_id = request.flavor_id.parse::<Uuid>().map_err(|_| ())?;
        flavors.iter().find(|flavor| flavor.id == flavor_id)
    }
    .ok_or(())?;
    Ok(Server {
        id: resource.id,
        name: request.name,
        project_id: resource.project_id,
        flavor_id: flavor.id,
        image_id: request.image_id.unwrap_or_default(),
        status: resource.observed_state.to_ascii_uppercase(),
        key_name: None,
        config_drive: request.config_drive.is_some(),
        network_ids: request.network_ids,
    })
}

fn validate_keypair_name(name: &str) -> Result<(), ComputeError> {
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ComputeError::InvalidRequest);
    }
    Ok(())
}

fn keypair_from_record(record: o3k_store::KeypairRecord) -> Keypair {
    Keypair {
        id: record.id,
        user_id: record.user_id,
        project_id: record.project_id,
        name: record.name,
        key_type: record.key_type,
        public_key: record.public_key,
        fingerprint: record.fingerprint,
        created_at: record.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider_contract::compute_proto as proto;
    use std::path::PathBuf;

    async fn service(label: &str) -> Result<ComputeService, ComputeError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Ok(ComputeService::new(
            Arc::new(SqliteStore::connect_file(&path).await?),
            Arc::new(FakeComputeProvider::new()),
        ))
    }

    #[tokio::test]
    async fn inspect_server_validates_existing_placement_without_reallocation()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-inspect-service-{}.sqlite",
            std::process::id()
        ));
        let placement_path = PathBuf::from(format!(
            "/tmp/o3k-compute-inspect-placement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store = Arc::new(SqliteStore::connect_file(&database_path).await?);
        let placement = o3k_placement::PlacementLedger::open(&placement_path)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement.register_provider(
            "node-a",
            std::collections::BTreeMap::from([
                (
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: 4,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::MEMORY_MB.to_owned(),
                    o3k_placement::Inventory {
                        total: 4096,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::DISK_GB.to_owned(),
                    o3k_placement::Inventory {
                        total: 100,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ]),
        )?;
        let service = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()))
            .with_scheduler(Scheduler::new(placement.clone()));
        let server = service
            .create_server(
                "project-a",
                "inspectable".to_owned(),
                "image-1".to_owned(),
                Uuid::from_u128(1),
                vec!["port-1".to_owned()],
                "inspectable-request".to_owned(),
            )
            .await?;
        let before = placement.provider("node-a")?;
        let inspected = service
            .inspect_server("project-a", server.id, "inspectable-request")
            .await?;
        assert_eq!(inspected.state, o3k_provider::OperationState::Succeeded);
        let repeated = service
            .inspect_server("project-a", server.id, "inspectable-request")
            .await?;
        assert_eq!(repeated.o3k_operation_id, inspected.o3k_operation_id);
        let second_request = service
            .inspect_server("project-a", server.id, "inspectable-request-2")
            .await?;
        assert_ne!(second_request.o3k_operation_id, inspected.o3k_operation_id);
        assert_eq!(before, placement.provider("node-a")?);
        assert!(matches!(
            service
                .inspect_server("project-b", server.id, "inspectable-request")
                .await,
            Err(ComputeError::NotFound)
        ));
        std::fs::remove_file(database_path)?;
        std::fs::remove_dir_all(placement_path)?;
        Ok(())
    }

    #[tokio::test]
    async fn custom_flavors_are_project_scoped_durable_and_delete_safely()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-flavor-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(SqliteStore::connect_file(&path).await?);
        let service = ComputeService::new(store, Arc::new(FakeComputeProvider::new()));
        let flavor = service
            .create_flavor("project-a", "custom.small".to_owned(), 1, 1024, 5)
            .await?;
        let same_dimensions = service
            .create_flavor("project-a", "custom.small-alias".to_owned(), 1, 1024, 5)
            .await?;
        assert!(
            service
                .flavors_for_project("project-b")
                .await?
                .iter()
                .all(|value| value.id != flavor.id)
        );
        assert!(
            service
                .flavors_for_project("project-a")
                .await?
                .iter()
                .any(|value| value == &flavor)
        );
        let reopened = ComputeService::new(
            Arc::new(SqliteStore::connect_file(&path).await?),
            Arc::new(FakeComputeProvider::new()),
        );
        assert_eq!(
            reopened.flavor_for_project("project-a", flavor.id).await?,
            flavor
        );
        let server = reopened
            .create_server(
                "project-a",
                "custom-flavor-server".to_owned(),
                "image-1".to_owned(),
                same_dimensions.id,
                vec!["network-1".to_owned()],
                "custom-flavor-request".to_owned(),
            )
            .await?;
        assert_eq!(
            reopened
                .show_server("project-a", server.id)
                .await?
                .flavor_id,
            same_dimensions.id
        );
        let persisted_intent: CreateInstanceRequest =
            serde_json::from_str(&reopened.store.get_resource(server.id).await?.desired_state)?;
        assert_eq!(persisted_intent.flavor_id, same_dimensions.id.to_string());
        assert_eq!(persisted_intent.disk_gib, same_dimensions.disk_gib);
        reopened.delete_flavor("project-a", flavor.id).await?;
        reopened.delete_server("project-a", server.id).await?;
        reopened
            .delete_flavor("project-a", same_dimensions.id)
            .await?;
        assert!(matches!(
            reopened.flavor_for_project("project-a", flavor.id).await,
            Err(ComputeError::NotFound)
        ));
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn agent_inventory_requires_explicit_disk_capacity() {
        let capabilities = proto::Capabilities {
            max_vcpus: 4,
            max_memory_mib: 4096,
            disk_formats: vec!["qcow2".to_owned()],
            ..Default::default()
        };
        let inventory = agent_inventory(&capabilities);
        assert_eq!(inventory[o3k_placement::VCPU].total, 4);
        assert_eq!(inventory[o3k_placement::MEMORY_MB].total, 4096);
        assert_eq!(inventory[o3k_placement::DISK_GB].total, 0);
    }

    #[tokio::test]
    async fn authenticated_agent_inventory_is_published_and_state_fenced()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = PathBuf::from(format!(
            "/tmp/o3k-placement-agent-inventory-{}",
            Uuid::now_v7()
        ));
        let placement = o3k_placement::PlacementLedger::open(&root)?;
        let registry = NodeRegistry::default();
        registry
            .register(&proto::RegisterRequest {
                agent_id: "agent-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                software_version: "test".to_owned(),
                host_label: "host-a".to_owned(),
                supported_versions: vec![o3k_compute_agent::PROTOCOL_VERSION],
                capabilities: Some(proto::Capabilities {
                    max_vcpus: 4,
                    max_memory_mib: 4096,
                    max_disk_gb: 20,
                    ..Default::default()
                }),
            })
            .await?;

        sync_agent_inventory(&registry, &placement).await?;
        let provider = placement.provider("agent-a")?;
        assert_eq!(provider.state, o3k_placement::ProviderState::Enabled);
        assert_eq!(provider.inventories[o3k_placement::VCPU].total, 4);
        assert_eq!(provider.inventories[o3k_placement::MEMORY_MB].total, 4096);
        assert_eq!(provider.inventories[o3k_placement::DISK_GB].total, 20);

        placement.allocate(
            "agent-a",
            "allocation-1",
            "server-1",
            BTreeMap::from([
                (o3k_placement::VCPU.to_owned(), 1),
                (o3k_placement::MEMORY_MB.to_owned(), 512),
                (o3k_placement::DISK_GB.to_owned(), 1),
            ]),
            provider.generation,
        )?;
        registry
            .set_desired_state("agent-a", proto::AdministrativeState::Draining)
            .await?;
        sync_agent_inventory(&registry, &placement).await?;
        let refreshed = placement.provider("agent-a")?;
        assert_eq!(refreshed.state, o3k_placement::ProviderState::Draining);
        assert_eq!(refreshed.allocations.len(), 1);
        assert_eq!(refreshed.inventories[o3k_placement::VCPU].used, 1);

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn action_waits_for_asynchronous_provider_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-async-action-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(SqliteStore::connect_file(&path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let service = ComputeService::new(store.clone(), provider.clone());
        let flavor = service.flavors()[0].id;
        let server = service
            .create_server(
                "project-a",
                "server".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "request-1".to_owned(),
            )
            .await?;
        // An agent-backed provider reports Running until the asynchronous
        // command completes. The action must keep reconciling instead of
        // returning a conflict while the provider is still converging.
        provider.set_failure(o3k_provider::FailureInjection::PartialCompletion)?;
        let resource = store.get_resource(server.id).await?;
        let action_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:action:project-a:{}:SHUTOFF:{}",
                server.id, resource.generation
            )
            .as_bytes(),
        );
        let flipping_provider = provider.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let _ = flipping_provider
                .set_operation_state(action_operation_id, o3k_provider::OperationState::Succeeded);
        });
        let stopped = service
            .action("project-a", server.id, InstanceAction::Stop)
            .await?;
        assert_eq!(stopped.status, "SHUTOFF");
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[tokio::test]
    async fn server_lifecycle_and_actions_are_project_scoped() -> Result<(), ComputeError> {
        let service = service("lifecycle").await?;
        let flavor = service.flavors()[0].id;
        let server = service
            .create_server(
                "project-a",
                "server".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "request-1".to_owned(),
            )
            .await?;
        let retry = service
            .create_server(
                "project-a",
                "server".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "request-1".to_owned(),
            )
            .await?;
        assert_eq!(retry.id, server.id);
        assert!(matches!(
            service
                .create_server(
                    "project-a",
                    "different-name".to_owned(),
                    "image-1".to_owned(),
                    flavor,
                    vec!["network-1".to_owned()],
                    "request-1".to_owned(),
                )
                .await,
            Err(ComputeError::Conflict)
        ));
        assert_eq!(server.status, "ACTIVE");
        assert_eq!(
            service
                .action("project-a", server.id, InstanceAction::Stop)
                .await?
                .status,
            "SHUTOFF"
        );
        assert_eq!(
            service
                .action("project-a", server.id, InstanceAction::Start)
                .await?
                .status,
            "ACTIVE"
        );
        assert!(matches!(
            service.show_server("project-b", server.id).await,
            Err(ComputeError::NotFound)
        ));
        service.delete_server("project-a", server.id).await?;
        assert!(matches!(
            service.show_server("project-a", server.id).await,
            Err(ComputeError::NotFound)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn keypair_crud_is_user_scoped_and_server_validation_precedes_provider()
    -> Result<(), ComputeError> {
        let service = service("keypairs").await?;
        let public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test".to_owned();
        let keypair = service
            .create_keypair(
                "user-a",
                "project-a",
                "test-key".to_owned(),
                public_key.clone(),
            )
            .await?;
        assert_eq!(service.list_keypairs("user-a", "project-a").await?.len(), 1);
        assert!(matches!(
            service
                .show_keypair("user-b", "project-a", "test-key")
                .await,
            Err(ComputeError::NotFound)
        ));
        assert!(matches!(
            service
                .create_keypair("user-a", "project-a", "test-key".to_owned(), public_key)
                .await,
            Err(ComputeError::Conflict)
        ));
        assert!(matches!(
            service
                .create_server_for_user(ServerCreateInput {
                    user_id: "user-a".to_owned(),
                    project_id: "project-a".to_owned(),
                    name: "server".to_owned(),
                    image_id: "image".to_owned(),
                    flavor_id: service.flavors()[0].id,
                    network_ids: vec!["network".to_owned()],
                    key_name: Some("missing".to_owned()),
                    config_drive: None,
                    idempotency_key: "request".to_owned(),
                })
                .await,
            Err(ComputeError::NotFound)
        ));
        let server = service
            .create_server_for_user(ServerCreateInput {
                user_id: "user-a".to_owned(),
                project_id: "project-a".to_owned(),
                name: "server".to_owned(),
                image_id: "image".to_owned(),
                flavor_id: service.flavors()[0].id,
                network_ids: vec!["network".to_owned()],
                key_name: Some("test-key".to_owned()),
                config_drive: None,
                idempotency_key: "request-2".to_owned(),
            })
            .await?;
        assert_eq!(server.key_name.as_deref(), Some("test-key"));
        assert_eq!(
            service
                .show_server("project-a", server.id)
                .await?
                .key_name
                .as_deref(),
            Some("test-key")
        );
        assert!(matches!(
            service
                .create_server_for_user(ServerCreateInput {
                    user_id: "user-a".to_owned(),
                    project_id: "project-a".to_owned(),
                    name: "server".to_owned(),
                    image_id: "image".to_owned(),
                    flavor_id: service.flavors()[0].id,
                    network_ids: vec!["network".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "request-2".to_owned(),
                })
                .await,
            Err(ComputeError::Conflict)
        ));
        assert!(matches!(
            service
                .delete_keypair("user-a", "project-a", &keypair.name)
                .await,
            Err(ComputeError::Store(StoreError::KeypairInUse))
        ));
        service.delete_server("project-a", server.id).await?;
        service
            .delete_keypair("user-a", "project-a", &keypair.name)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_update_forwarding_uses_durable_journal() -> Result<(), ComputeError> {
        let service = service("agent-forwarding").await?;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "agent-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["network-1".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "agent-forwarding".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        let update = o3k_provider_contract::compute_proto::OperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: request.operation_id.to_string(),
            resource_id: request.o3k_server_id.to_string(),
            state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            provider_resource_id: "agent-domain-1".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            service.apply_agent_update(&update).await?,
            o3k_store::OperationState::Succeeded
        );
        assert_eq!(
            service.apply_agent_update(&update).await?,
            o3k_store::OperationState::Succeeded
        );
        assert_eq!(
            service
                .store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "requested"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_observation_forwarding_projects_stopped_server() -> Result<(), ComputeError> {
        let service = service("agent-observation-forwarding").await?;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "observed-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["network-1".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "agent-observation-forwarding".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        let observation = o3k_provider_contract::compute_proto::Observation {
            resource_id: request.o3k_server_id.to_string(),
            provider_resource_id: "agent-domain-stopped".to_owned(),
            operation_id: request.operation_id.to_string(),
            operation_state: o3k_provider_contract::compute_proto::OperationState::Succeeded as i32,
            state: o3k_provider_contract::compute_proto::ResourceState::Stopped as i32,
            ..Default::default()
        };
        service.apply_agent_observation(&observation).await?;
        assert_eq!(
            service
                .show_server("project-a", request.o3k_server_id)
                .await?
                .status,
            "SHUTOFF"
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduler_binding_is_persisted_idempotently_and_released_on_delete()
    -> Result<(), ComputeError> {
        let placement_root =
            PathBuf::from(format!("/tmp/o3k-placement-compute-{}", Uuid::now_v7()));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider(
                "node-a",
                std::collections::BTreeMap::from([
                    (
                        o3k_placement::VCPU.to_owned(),
                        o3k_placement::Inventory {
                            total: 8,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::MEMORY_MB.to_owned(),
                        o3k_placement::Inventory {
                            total: 8192,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::DISK_GB.to_owned(),
                        o3k_placement::Inventory {
                            total: 100,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service = service("scheduler")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()));
        let flavor = service.flavors()[0].id;
        let server = service
            .create_server(
                "project-a",
                "scheduled".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "request-scheduled".to_owned(),
            )
            .await?;
        let resource = service.store.get_resource(server.id).await?;
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        assert_eq!(intent.placement_provider_id.as_deref(), Some("node-a"));
        assert_eq!(
            intent.placement_allocation_id.as_deref(),
            Some(format!("allocation-{}", server.id).as_str())
        );
        assert_eq!(
            placement
                .provider("node-a")
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            1
        );
        let retry = service
            .create_server(
                "project-a",
                "scheduled".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "request-scheduled".to_owned(),
            )
            .await?;
        assert_eq!(retry.id, server.id);
        service.delete_server("project-a", server.id).await?;
        assert_eq!(
            placement
                .provider("node-a")
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            0
        );
        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_server_names_release_each_new_placement_allocation()
    -> Result<(), ComputeError> {
        let placement_root = PathBuf::from(format!(
            "/tmp/o3k-placement-duplicate-name-{}",
            Uuid::now_v7()
        ));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider(
                "node-a",
                std::collections::BTreeMap::from([
                    (
                        o3k_placement::VCPU.to_owned(),
                        o3k_placement::Inventory {
                            total: 2,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::MEMORY_MB.to_owned(),
                        o3k_placement::Inventory {
                            total: 1024,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::DISK_GB.to_owned(),
                        o3k_placement::Inventory {
                            total: 20,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service = service("duplicate-name")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()));
        let flavor = service.flavors()[0].id;
        service
            .create_server(
                "project-a",
                "duplicate-name".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "initial-request".to_owned(),
            )
            .await?;
        let generation_after_initial_create = placement
            .provider("node-a")
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
            .generation;

        for attempt in 0..3 {
            assert!(matches!(
                service
                    .create_server(
                        "project-a",
                        "duplicate-name".to_owned(),
                        "image-1".to_owned(),
                        flavor,
                        vec!["network-1".to_owned()],
                        format!("conflicting-request-{attempt}"),
                    )
                    .await,
                Err(ComputeError::Conflict)
            ));
            assert_eq!(
                placement
                    .provider("node-a")
                    .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                    .allocations
                    .len(),
                1
            );
            assert_eq!(
                placement
                    .provider("node-a")
                    .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                    .generation,
                generation_after_initial_create
            );
        }

        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn create_race_releases_placement_not_owned_by_winner() -> Result<(), ComputeError> {
        let placement_root =
            PathBuf::from(format!("/tmp/o3k-placement-create-race-{}", Uuid::now_v7()));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let inventory = |vcpus| {
            std::collections::BTreeMap::from([
                (
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: vcpus,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::MEMORY_MB.to_owned(),
                    o3k_placement::Inventory {
                        total: 512,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::DISK_GB.to_owned(),
                    o3k_placement::Inventory {
                        total: 10,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ])
        };
        placement
            .register_provider("node-a", inventory(2))
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider("node-b", inventory(3))
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service = service("create-race")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()));
        let flavor = service.flavors()[0].id;
        let left = service.create_server(
            "project-a",
            "racing".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            "same-request".to_owned(),
        );
        let right = service.create_server(
            "project-a",
            "racing".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            "same-request".to_owned(),
        );
        let (left, right) = tokio::join!(left, right);
        assert!(
            left.is_ok() || right.is_ok(),
            "both creates failed: {left:?} {right:?}"
        );
        assert!(
            left.is_ok() && (right.is_ok() || matches!(right, Err(ComputeError::Conflict)))
                || right.is_ok() && matches!(left, Err(ComputeError::Conflict)),
            "unexpected race results: {left:?} {right:?}"
        );
        let allocation_count = placement
            .providers()
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
            .into_iter()
            .map(|provider| provider.allocations.len())
            .sum::<usize>();
        assert_eq!(allocation_count, 1);
        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn existing_resource_conflict_does_not_acquire_placement_allocation()
    -> Result<(), ComputeError> {
        let placement_root = PathBuf::from(format!(
            "/tmp/o3k-placement-existing-resource-{}",
            Uuid::now_v7()
        ));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider(
                "node-a",
                std::collections::BTreeMap::from([
                    (
                        o3k_placement::VCPU.to_owned(),
                        o3k_placement::Inventory {
                            total: 2,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::MEMORY_MB.to_owned(),
                        o3k_placement::Inventory {
                            total: 1024,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::DISK_GB.to_owned(),
                        o3k_placement::Inventory {
                            total: 20,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service = service("existing-resource")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()));
        let flavor = service.flavors()[0].clone();
        let idempotency_key = "existing-resource-request";
        let server_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:server:project-a:{idempotency_key}").as_bytes(),
        );
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:operation:project-a:{idempotency_key}").as_bytes(),
        );
        let existing_request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: server_id,
            project_id: "project-a".to_owned(),
            name: "existing-name".to_owned(),
            vcpus: flavor.vcpus,
            memory_mib: flavor.ram_mib,
            flavor_id: flavor.id.to_string(),
            disk_gib: flavor.disk_gib,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["network-1".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: idempotency_key.to_owned(),
        };
        service
            .store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 0,
                desired_state: serde_json::to_string(&existing_request)
                    .map_err(|_| ComputeError::Conflict)?,
                observed_state: "requested".to_owned(),
                provider_id: None,
            })
            .await?;

        assert!(matches!(
            service
                .create_server(
                    "project-a",
                    "different-name".to_owned(),
                    "image-1".to_owned(),
                    flavor.id,
                    vec!["network-1".to_owned()],
                    idempotency_key.to_owned(),
                )
                .await,
            Err(ComputeError::Conflict)
        ));
        assert_eq!(
            placement
                .provider("node-a")
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            0
        );

        std::fs::remove_dir_all(placement_root).map_err(|error| {
            ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(error),
            ))
        })?;
        Ok(())
    }

    #[tokio::test]
    async fn deleted_server_retries_failed_placement_release() -> Result<(), ComputeError> {
        let placement_root = PathBuf::from(format!(
            "/tmp/o3k-placement-delete-release-{}",
            Uuid::now_v7()
        ));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider(
                "node-a",
                std::collections::BTreeMap::from([
                    (
                        o3k_placement::VCPU.to_owned(),
                        o3k_placement::Inventory {
                            total: 2,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::MEMORY_MB.to_owned(),
                        o3k_placement::Inventory {
                            total: 1024,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::DISK_GB.to_owned(),
                        o3k_placement::Inventory {
                            total: 20,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service = service("delete-release")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()));
        let server = service
            .create_server(
                "project-a",
                "delete-release".to_owned(),
                "image-1".to_owned(),
                service.flavors()[0].id,
                vec!["network-1".to_owned()],
                "delete-release-request".to_owned(),
            )
            .await?;

        std::fs::remove_file(placement_root.join("placement.json")).map_err(|error| {
            ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(error),
            ))
        })?;
        std::fs::create_dir(placement_root.join("placement.json")).map_err(|error| {
            ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(error),
            ))
        })?;

        assert!(matches!(
            service.delete_server("project-a", server.id).await,
            Err(ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(_)
            )))
        ));
        assert_eq!(
            placement
                .provider("node-a")
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            1
        );
        assert_eq!(
            service.store.get_resource(server.id).await?.observed_state,
            "DELETED"
        );

        std::fs::remove_dir(placement_root.join("placement.json")).map_err(|error| {
            ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(error),
            ))
        })?;
        service.delete_server("project-a", server.id).await?;
        assert_eq!(
            placement
                .provider("node-a")
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            0
        );

        std::fs::remove_dir_all(placement_root).map_err(|error| {
            ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Storage(error),
            ))
        })?;
        Ok(())
    }

    #[tokio::test]
    async fn registry_gate_excludes_unavailable_draining_and_disabled_agents()
    -> Result<(), ComputeError> {
        let placement_root =
            PathBuf::from(format!("/tmp/o3k-placement-registry-{}", Uuid::now_v7()));
        let placement = o3k_placement::PlacementLedger::open(&placement_root)
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let inventory = || {
            std::collections::BTreeMap::from([
                (
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: 8,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::MEMORY_MB.to_owned(),
                    o3k_placement::Inventory {
                        total: 8192,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::DISK_GB.to_owned(),
                    o3k_placement::Inventory {
                        total: 100,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ])
        };
        for agent in ["unavailable", "draining", "disabled", "enabled"] {
            placement
                .register_provider(agent, inventory())
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        }

        let registry = NodeRegistry::default();
        for agent in ["unavailable", "draining", "disabled"] {
            registry
                .register(&proto::RegisterRequest {
                    agent_id: agent.to_owned(),
                    agent_epoch: "epoch".to_owned(),
                    software_version: "test".to_owned(),
                    host_label: agent.to_owned(),
                    supported_versions: vec![o3k_compute_agent::PROTOCOL_VERSION],
                    capabilities: Some(proto::Capabilities::default()),
                })
                .await
                .map_err(|_| ComputeError::Conflict)?;
        }
        registry.mark_unavailable(std::time::Duration::ZERO).await;
        registry
            .register(&proto::RegisterRequest {
                agent_id: "enabled".to_owned(),
                agent_epoch: "epoch".to_owned(),
                software_version: "test".to_owned(),
                host_label: "enabled".to_owned(),
                supported_versions: vec![o3k_compute_agent::PROTOCOL_VERSION],
                capabilities: Some(proto::Capabilities::default()),
            })
            .await
            .map_err(|_| ComputeError::Conflict)?;
        registry
            .set_desired_state("draining", proto::AdministrativeState::Draining)
            .await
            .map_err(|_| ComputeError::Conflict)?;
        registry
            .set_desired_state("disabled", proto::AdministrativeState::Disabled)
            .await
            .map_err(|_| ComputeError::Conflict)?;

        let service = service("registry-gate")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()))
            .with_agent_registry(registry);
        let server = service
            .create_server(
                "project-a",
                "registry-gated".to_owned(),
                "image-1".to_owned(),
                service.flavors()[0].id,
                vec!["network-1".to_owned()],
                "registry-gated-request".to_owned(),
            )
            .await?;
        let resource = service.store.get_resource(server.id).await?;
        let request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        assert_eq!(request.placement_provider_id.as_deref(), Some("enabled"));

        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[derive(Debug, Default)]
    struct TestResolvedCreateResolver;

    #[async_trait]
    impl ResolvedCreateResolver for TestResolvedCreateResolver {
        async fn resolve(
            &self,
            _request: &CreateInstanceRequest,
            _agent: &NodeSnapshot,
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

    fn registered_agent(id: &str) -> proto::RegisterRequest {
        proto::RegisterRequest {
            agent_id: id.to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            software_version: "test".to_owned(),
            host_label: id.to_owned(),
            supported_versions: vec![o3k_compute_agent::PROTOCOL_VERSION],
            capabilities: Some(proto::Capabilities {
                agent_provider_name: "o3k-compute".to_owned(),
                agent_provider_version: "test".to_owned(),
                max_vcpus: 8,
                max_memory_mib: 16_384,
                max_disk_gb: 100,
                lifecycle_actions: vec!["start".to_owned(), "stop".to_owned()],
                ..Default::default()
            }),
        }
    }

    #[tokio::test]
    async fn agent_provider_reads_capabilities_from_selected_registered_agent()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let provider =
            AgentComputeProvider::new(registry, Arc::new(UnconfiguredResolvedCreateResolver));
        let capabilities = provider.capabilities().await?;
        assert_eq!(capabilities.provider_name, "o3k-compute");
        assert!(
            capabilities
                .capabilities
                .iter()
                .any(|value| value == "start")
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_rehydrates_instance_binding_from_durable_store()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
        let server_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: server_id,
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: "flavor-1".to_owned(),
            disk_gib: 10,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("allocation-a".to_owned()),
            config_drive: None,
            idempotency_key: "request-a".to_owned(),
        };
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: serde_json::to_string(&request)?,
                observed_state: "ACTIVE".to_owned(),
                provider_id: Some("domain-a".to_owned()),
            })
            .await?;
        store
            .attach_provider_reference(&o3k_store::ProviderReference {
                resource_id: server_id,
                provider_name: "agent".to_owned(),
                provider_resource_id: "domain-a".to_owned(),
            })
            .await?;
        let provider = AgentComputeProvider::new_with_store(
            registry,
            Arc::new(UnconfiguredResolvedCreateResolver),
            Some(store),
        );
        let instance = provider.get_instance("domain-a").await?;
        assert_eq!(instance.o3k_server_id, server_id);
        assert_eq!(instance.state, o3k_provider::InstanceState::Running);
        Ok(())
    }

    #[tokio::test]
    async fn agent_lifecycle_commands_use_the_o3k_server_id_as_resource_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
        let server_id = Uuid::now_v7();
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: server_id,
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: "flavor-1".to_owned(),
            disk_gib: 10,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("allocation-a".to_owned()),
            config_drive: None,
            idempotency_key: "request-a".to_owned(),
        };
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: serde_json::to_string(&request)?,
                observed_state: "ACTIVE".to_owned(),
                provider_id: Some("domain-a".to_owned()),
            })
            .await?;
        store
            .attach_provider_reference(&o3k_store::ProviderReference {
                resource_id: server_id,
                provider_name: "agent".to_owned(),
                provider_resource_id: "domain-a".to_owned(),
            })
            .await?;
        let provider = AgentComputeProvider::new_with_store(
            registry,
            Arc::new(UnconfiguredResolvedCreateResolver),
            Some(store.clone()),
        );
        // Lifecycle commands must carry the O3K server id, not the provider
        // (libvirt domain) name: the agent derives the domain name from the
        // server id, and the durable command store requires a UUID. Dispatch
        // fails without a live stream, but the durable record proves the
        // command identity that would be sent.
        let stop_operation_id = Uuid::now_v7();
        store
            .insert_operation(&o3k_store::OperationRecord {
                id: stop_operation_id,
                resource_id: server_id,
                kind: "lifecycle:stop".to_owned(),
                state: o3k_store::OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        let stop = provider
            .action_instance(
                "domain-a",
                o3k_provider::InstanceAction::Stop,
                stop_operation_id,
                "stop-a",
            )
            .await;
        let stop_error = match stop {
            Err(error) => error,
            Ok(_) => return Err("stop dispatch unexpectedly succeeded without a stream".into()),
        };
        assert!(
            matches!(stop_error, ProviderError::Retryable),
            "stop failed with {stop_error:?}"
        );
        let record = store
            .get_agent_command_by_operation(stop_operation_id)
            .await?;
        let command = proto::Command::decode(record.payload.as_slice())?;
        assert_eq!(command.resource_id, server_id.to_string());
        assert!(matches!(
            command.action,
            Some(proto::command::Action::Stop(_))
        ));
        let delete_operation_id = Uuid::now_v7();
        store
            .insert_operation(&o3k_store::OperationRecord {
                id: delete_operation_id,
                resource_id: server_id,
                kind: "lifecycle:delete".to_owned(),
                state: o3k_store::OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        let delete = provider
            .delete_instance(DeleteInstanceRequest {
                operation_id: delete_operation_id,
                provider_instance_id: "domain-a".to_owned(),
                idempotency_key: "delete-a".to_owned(),
            })
            .await;
        let delete_error = match delete {
            Err(error) => error,
            Ok(_) => {
                return Err("delete dispatch unexpectedly succeeded without a stream".into());
            }
        };
        assert!(
            matches!(delete_error, ProviderError::Retryable),
            "delete failed with {delete_error:?}"
        );
        let record = store
            .get_agent_command_by_operation(delete_operation_id)
            .await?;
        let command = proto::Command::decode(record.payload.as_slice())?;
        assert_eq!(command.resource_id, server_id.to_string());
        assert!(matches!(
            command.action,
            Some(proto::command::Action::Delete(_))
        ));
        // A reconcile retry of the same operation must reuse the durable
        // command payload. Rebuilding it would drift the embedded deadline
        // and conflict with the durable record instead of replaying.
        let retry = provider
            .delete_instance(DeleteInstanceRequest {
                operation_id: delete_operation_id,
                provider_instance_id: "domain-a".to_owned(),
                idempotency_key: "delete-a".to_owned(),
            })
            .await;
        let retry_error = match retry {
            Err(error) => error,
            Ok(_) => return Err("delete retry unexpectedly succeeded without a stream".into()),
        };
        assert!(
            matches!(retry_error, ProviderError::Retryable),
            "delete retry failed with {retry_error:?}"
        );
        let replayed = store
            .get_agent_command_by_operation(delete_operation_id)
            .await?;
        assert_eq!(replayed.payload, record.payload);
        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_requires_placement_and_never_invents_resolved_inputs()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let provider =
            AgentComputeProvider::new(registry, Arc::new(UnconfiguredResolvedCreateResolver));
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "request-a".to_owned(),
        };
        assert_eq!(
            provider.create_instance(request).await,
            Err(ProviderError::InvalidRequest)
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_rejects_create_without_verified_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let provider = AgentComputeProvider::new(registry, Arc::new(TestResolvedCreateResolver));
        let operation_id = Uuid::now_v7();
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("allocation-a".to_owned()),
            config_drive: None,
            idempotency_key: "request-a".to_owned(),
        };
        assert_eq!(
            provider.create_instance(request).await,
            Err(ProviderError::InvalidRequest)
        );
        assert_eq!(
            provider.get_operation(operation_id).await,
            Err(ProviderError::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_rejects_config_drive_without_backend_capability()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&registered_agent("node-a")).await?;
        let provider = AgentComputeProvider::new(registry, Arc::new(TestResolvedCreateResolver));
        let operation_id = Uuid::now_v7();
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server-a".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-a".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-a".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("allocation-a".to_owned()),
            config_drive: Some(ConfigDriveRequest {
                user_data: b"#cloud-config\n".to_vec(),
                vendor_data: None,
                ssh_public_key: "ssh-ed25519 AAAA".to_owned(),
            }),
            idempotency_key: "request-a".to_owned(),
        };
        assert_eq!(
            provider.create_instance(request).await,
            Err(ProviderError::InvalidRequest)
        );
        assert_eq!(
            provider.get_operation(operation_id).await,
            Err(ProviderError::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_provider_projects_observations_and_agent_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        let operation_id = Uuid::now_v7();
        state.write().await.operations.insert(
            operation_id,
            Operation {
                provider_operation_id: operation_id,
                o3k_operation_id: operation_id,
                state: o3k_provider::OperationState::Accepted,
                error_category: None,
                provider_resource_id: None,
            },
        );
        let update = proto::OperationUpdate {
            operation_id: operation_id.to_string(),
            resource_id: "server-a".to_owned(),
            state: proto::OperationState::Succeeded as i32,
            operation_sequence: 1,
            provider_resource_id: "domain-a".to_owned(),
            ..Default::default()
        };
        apply_agent_provider_event(
            &state,
            None,
            o3k_compute_agent::AgentEvent::Operation(update),
        )
        .await;
        apply_agent_provider_event(
            &state,
            None,
            o3k_compute_agent::AgentEvent::Observation(Box::new(proto::Observation {
                agent_id: "node-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                resource_id: "server-a".to_owned(),
                provider_resource_id: "domain-a".to_owned(),
                state: proto::ResourceState::Running as i32,
                operation_id: operation_id.to_string(),
                operation_state: proto::OperationState::Succeeded as i32,
                observation_sequence: 1,
                observed_at_unix_ms: 1,
                redacted_message: "running".to_owned(),
                ..Default::default()
            })),
        )
        .await;
        let provider = AgentComputeProvider {
            registry: NodeRegistry::default(),
            resolver: Arc::new(UnconfiguredResolvedCreateResolver),
            state: state.clone(),
            store: None,
            artifact_resolver: Arc::new(UnconfiguredCreateArtifactResolver),
            command_timeout: Duration::from_secs(30),
        };
        assert_eq!(
            provider.get_instance("domain-a").await?.state,
            o3k_provider::InstanceState::Running
        );
        assert_eq!(
            provider
                .get_operation(operation_id)
                .await?
                .provider_resource_id
                .as_deref(),
            Some("domain-a")
        );
        apply_agent_provider_event(
            &state,
            None,
            o3k_compute_agent::AgentEvent::Error(proto::ProtocolError {
                category: proto::ErrorCategory::Retryable as i32,
                code: "agent-retry".to_owned(),
                redacted_message: "retry".to_owned(),
                operation_id: operation_id.to_string(),
                retryable: true,
                command_id: String::new(),
            }),
        )
        .await;
        assert_eq!(
            provider.get_operation(operation_id).await?.state,
            o3k_provider::OperationState::Retryable
        );
        Ok(())
    }

    #[tokio::test]
    async fn artifact_status_rebinds_epoch_and_rejects_identity_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
        let operation_id = Uuid::now_v7();
        let resource_id = Uuid::now_v7();
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
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
                state: o3k_store::OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        store
            .insert_artifact_transfer(&ArtifactTransferRecord {
                transfer_id: "transfer-1".to_owned(),
                command_id: "command-1".to_owned(),
                operation_id,
                resource_id,
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                artifact_id: "artifact-1".to_owned(),
                artifact_kind: "image_base".to_owned(),
                sha256: sha256.to_owned(),
                size_bytes: 8,
                expires_at_unix_ms: i64::MAX,
                format: "raw".to_owned(),
                chunk_size_bytes: 4,
                chunk_count: 2,
                state: ArtifactTransferState::Offered,
                contiguous_bytes: 0,
                next_chunk_index: 0,
                retry_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
            })
            .await?;
        let state = Arc::new(RwLock::new(AgentProviderState::default()));
        apply_agent_provider_event(
            &state,
            Some(&store),
            o3k_compute_agent::AgentEvent::ArtifactStatus(agent_proto::ArtifactStatus {
                transfer_id: "transfer-1".to_owned(),
                command_id: "command-1".to_owned(),
                operation_id: operation_id.to_string(),
                resource_id: resource_id.to_string(),
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-2".to_owned(),
                contiguous_bytes: 4,
                next_chunk_index: 1,
                state: agent_proto::ArtifactTransferState::Receiving as i32,
            }),
        )
        .await;
        let transfer = store.get_artifact_transfer("transfer-1").await?;
        assert_eq!(transfer.agent_epoch, "epoch-2");
        assert_eq!(transfer.state, ArtifactTransferState::Receiving);
        assert_eq!(transfer.contiguous_bytes, 4);

        apply_agent_provider_event(
            &state,
            Some(&store),
            o3k_compute_agent::AgentEvent::ArtifactStatus(agent_proto::ArtifactStatus {
                transfer_id: "transfer-1".to_owned(),
                command_id: "different-command".to_owned(),
                operation_id: operation_id.to_string(),
                resource_id: resource_id.to_string(),
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-2".to_owned(),
                contiguous_bytes: 8,
                next_chunk_index: 2,
                state: agent_proto::ArtifactTransferState::Committed as i32,
            }),
        )
        .await;
        let unchanged = store.get_artifact_transfer("transfer-1").await?;
        assert_eq!(unchanged.state, ArtifactTransferState::Receiving);
        assert_eq!(unchanged.contiguous_bytes, 4);
        Ok(())
    }

    #[tokio::test]
    async fn store_not_found_does_not_dispatch_provider_mutation() -> Result<(), ComputeError> {
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-notfound-{}.sqlite", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let store = Arc::new(SqliteStore::connect_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let service = ComputeService::new(store, provider.clone());

        let non_existent_id = Uuid::now_v7();
        assert!(matches!(
            service.delete_server("project-a", non_existent_id).await,
            Err(ComputeError::NotFound)
        ));
        assert!(matches!(
            service
                .inspect_server("project-a", non_existent_id, "key-1")
                .await,
            Err(ComputeError::NotFound)
        ));
        assert_eq!(provider.instance_count(), 0);

        let _ = std::fs::remove_file(&database_path);
        Ok(())
    }

    /// Regression test: the inspect probe must use the durable project *ID* "bootstrap-project",
    /// not the project *name* "admin" from the CLI/token context.
    ///
    /// The bootstrap token encodes:
    ///   "project": "admin"                  ← project name (display name only)
    ///   "project_id": "bootstrap-project"   ← durable ID used by compute service
    ///
    /// Passing the project name instead of the project ID causes the compute service to return
    /// NotFound on every inspect call because `resource.project_id != project_id` at the
    /// project isolation guard inside `inspect_server`.
    #[tokio::test]
    async fn inspect_probe_requires_project_id_not_project_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-projid-{}.sqlite", Uuid::now_v7()));
        let placement_path =
            std::env::temp_dir().join(format!("o3k-compute-projid-pl-{}", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store = Arc::new(SqliteStore::connect_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let placement = o3k_placement::PlacementLedger::open(&placement_path)?;
        placement.register_provider(
            "node-a",
            std::collections::BTreeMap::from([
                (
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: 4,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::MEMORY_MB.to_owned(),
                    o3k_placement::Inventory {
                        total: 4096,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::DISK_GB.to_owned(),
                    o3k_placement::Inventory {
                        total: 100,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ]),
        )?;
        let service = ComputeService::new(store, provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()));

        // Create a server under the durable project ID used by TestLab.
        let server = service
            .create_server(
                "bootstrap-project",
                "testlab-server".to_owned(),
                "cirros-image".to_owned(),
                Uuid::from_u128(1),
                vec!["net-1".to_owned()],
                "testlab-create-key".to_owned(),
            )
            .await?;

        // Passing the correct project ID ("bootstrap-project") succeeds.
        let result = service
            .inspect_server("bootstrap-project", server.id, "testlab-inspect-key")
            .await?;
        assert_eq!(
            result.state,
            o3k_provider::OperationState::Succeeded,
            "inspect with correct project ID must succeed"
        );

        // Passing the project *name* "admin" (as was hard-coded in the probe by mistake)
        // must return NotFound and must not dispatch to the provider.
        let calls_before = provider.instance_count();
        assert!(
            matches!(
                service
                    .inspect_server("admin", server.id, "wrong-project-name-key")
                    .await,
                Err(ComputeError::NotFound)
            ),
            "inspect with project name 'admin' instead of project ID must return NotFound"
        );
        assert_eq!(
            provider.instance_count(),
            calls_before,
            "wrong project ID must not dispatch a provider mutation"
        );

        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        Ok(())
    }

    /// Regression test: project isolation is preserved — a server created in
    /// "bootstrap-project" is invisible to a caller using a different project ID.
    #[tokio::test]
    async fn inspect_probe_project_isolation_rejects_foreign_project()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-isolation-{}.sqlite", Uuid::now_v7()));
        let placement_path =
            std::env::temp_dir().join(format!("o3k-compute-isolation-pl-{}", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store = Arc::new(SqliteStore::connect_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let placement = o3k_placement::PlacementLedger::open(&placement_path)?;
        placement.register_provider(
            "node-a",
            std::collections::BTreeMap::from([
                (
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: 4,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::MEMORY_MB.to_owned(),
                    o3k_placement::Inventory {
                        total: 4096,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    o3k_placement::DISK_GB.to_owned(),
                    o3k_placement::Inventory {
                        total: 100,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ]),
        )?;
        let service = ComputeService::new(store, provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()));

        let server = service
            .create_server(
                "bootstrap-project",
                "isolation-server".to_owned(),
                "cirros-image".to_owned(),
                Uuid::from_u128(1),
                vec!["net-1".to_owned()],
                "isolation-create-key".to_owned(),
            )
            .await?;

        // A caller using a different project ID must not reach the server.
        let calls_before = provider.instance_count();
        assert!(
            matches!(
                service
                    .inspect_server("other-project", server.id, "isolation-inspect-key")
                    .await,
                Err(ComputeError::NotFound)
            ),
            "inspect from a foreign project must return NotFound"
        );
        assert_eq!(
            provider.instance_count(),
            calls_before,
            "foreign project inspect must not dispatch a provider mutation"
        );

        // The owning project can still inspect successfully.
        let owner_result = service
            .inspect_server("bootstrap-project", server.id, "isolation-owner-key")
            .await?;
        assert_eq!(
            owner_result.state,
            o3k_provider::OperationState::Succeeded,
            "inspect from the owning project must succeed"
        );

        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        Ok(())
    }
}
