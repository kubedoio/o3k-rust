use std::sync::Arc;

use async_trait::async_trait;
pub use o3k_domain::{Server, ServerId, ServerState};
#[cfg(test)]
use o3k_provider::FakeComputeProvider;
use o3k_provider::{
    AgentAdministrativeState, AgentAvailability, AgentCapabilities, AgentNodeRegistry,
    AgentNodeSnapshot, BlockDeviceAttachment, BlockDeviceObservation, Capabilities,
    ComputeProvider, ConfigDriveRequest, ConnectorInfo, CreateInstanceRequest,
    DeleteInstanceRequest, Instance, InstanceAction, Operation, ProviderError,
    VolumeAttachmentProvider,
};
use o3k_reconciler::{LifecycleAction, OperationJournal, ReconcileError};
use o3k_scheduler::{Flavor as SchedulerFlavor, Scheduler, SchedulerError};
use o3k_store::{
    ComputeRepository, StoreError, VolumeAttachmentRecord, server_state_from_storage,
    server_state_to_storage,
};

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

pub mod attachment;

pub use attachment::AttachmentOrchestrator;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Flavor {
    pub id: Uuid,
    pub name: String,
    pub vcpus: u32,
    pub ram_mib: u64,
    pub disk_gib: u64,
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
    #[error("compute service is unavailable or misconfigured")]
    Unavailable,
}

#[derive(Clone)]
pub struct ComputeService {
    store: Arc<dyn ComputeRepository>,
    provider: Arc<ProviderBackend>,
    journal: OperationJournal<dyn ComputeRepository, ProviderBackend>,
    scheduler: Option<Scheduler>,
    agent_registry: Option<Arc<dyn AgentNodeRegistry>>,
    cinder: Option<Arc<dyn VolumeAttachmentProvider>>,
    attachments: AttachmentOrchestrator,
    binding_projector: Option<Arc<dyn PortBindingProjector>>,
}

#[derive(Clone)]
pub struct ProviderBackend(Arc<dyn ComputeProvider>);

/// Projects terminal compute outcomes into the durable port binding state
/// owned by the network control plane. Implementations are provided by the
/// composition root (`o3kd` wires the network service); a `None` projector
/// leaves binding state at the dispatch intent. Projection is best-effort:
/// a failed projection is logged, never a compute failure.
#[async_trait]
pub trait PortBindingProjector: Send + Sync {
    /// A terminal create outcome for the server owning `port_id`:
    /// `succeeded` projects `bound`, otherwise `error`.
    async fn project_create_outcome(
        &self,
        project_id: &str,
        port_id: &str,
        succeeded: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// The server owning `port_id` reached terminal deletion; the binding
    /// must be cleared so the port is reusable.
    async fn unbind_port(
        &self,
        project_id: &str,
        port_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Projects one authenticated agent capability snapshot into the inventory
/// shape required by the scheduler. Missing capacity is represented as zero
/// and makes the provider unschedulable; capability flags and disk formats are
/// never treated as capacity.
pub fn agent_inventory(
    capabilities: &AgentCapabilities,
) -> BTreeMap<String, o3k_placement::Inventory> {
    BTreeMap::from([
        (
            o3k_placement::VCPU.to_owned(),
            o3k_placement::Inventory {
                total: capabilities.max_vcpus,
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

fn agent_provider_state(snapshot: &AgentNodeSnapshot) -> o3k_placement::ProviderState {
    if snapshot.availability != AgentAvailability::Available
        || snapshot.administrative_state == AgentAdministrativeState::Disabled
        || snapshot.capabilities.max_vcpus == 0
        || snapshot.capabilities.max_memory_mib == 0
        || snapshot.capabilities.max_disk_gb == 0
    {
        o3k_placement::ProviderState::Unavailable
    } else if snapshot.administrative_state == AgentAdministrativeState::Draining {
        o3k_placement::ProviderState::Draining
    } else {
        o3k_placement::ProviderState::Enabled
    }
}

/// Synchronizes the current authenticated agent snapshots into Placement.
/// The stable agent ID is the Placement provider ID, so reconnects update the
/// same provider and preserve durable allocations.
pub async fn sync_agent_inventory(
    registry: &dyn AgentNodeRegistry,
    placement: &o3k_placement::PlacementLedger,
) -> Result<(), SchedulerError> {
    for snapshot in registry.all().await {
        placement
            .sync_provider(
                &snapshot.agent_id,
                agent_inventory(&snapshot.capabilities),
                agent_provider_state(&snapshot),
            )
            .await?;
    }
    Ok(())
}

/// Starts the bounded periodic inventory publisher used by `o3kd`.
pub fn spawn_agent_inventory_publisher(
    registry: Arc<dyn AgentNodeRegistry>,
    placement: o3k_placement::PlacementLedger,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = sync_agent_inventory(registry.as_ref(), &placement).await {
                tracing::warn!(%error, "agent inventory publication failed");
            }
        }
    })
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

impl<P: ComputeProvider + 'static> From<Arc<P>> for ProviderBackend {
    fn from(provider: Arc<P>) -> Self {
        Self(provider)
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
    async fn collect_connector(&self, resource_id: Uuid) -> Result<ConnectorInfo, ProviderError> {
        self.0.collect_connector(resource_id).await
    }
    async fn attach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        self.0.attach_block_device(resource_id, device).await
    }
    async fn detach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        self.0.detach_block_device(resource_id, device).await
    }
    async fn observe_block_device(
        &self,
        resource_id: Uuid,
        volume_id: &str,
    ) -> Result<Option<BlockDeviceObservation>, ProviderError> {
        self.0.observe_block_device(resource_id, volume_id).await
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
    pub fn new<P>(store: Arc<dyn ComputeRepository>, provider: Arc<P>) -> Self
    where
        Arc<P>: Into<ProviderBackend>,
    {
        let provider = Arc::new(provider.into());
        let journal = OperationJournal::new(store.clone(), provider.clone(), 3);
        let attachments = AttachmentOrchestrator::new(store.clone(), provider.clone(), None);
        Self {
            store,
            provider,
            journal,
            scheduler: None,
            agent_registry: None,
            cinder: None,
            attachments,
            binding_projector: None,
        }
    }

    /// Configures the projector that reflects terminal create/delete outcomes
    /// into the durable port binding state of the network control plane.
    #[must_use]
    pub fn with_binding_projector(
        mut self,
        binding_projector: Arc<dyn PortBindingProjector>,
    ) -> Self {
        self.binding_projector = Some(binding_projector);
        self
    }

    /// Configures the external volume-attachment provider used for the
    /// durable attachment lifecycle. External-hosted volume attachment
    /// requires it; the concrete adapter is selected at the composition root.
    #[must_use]
    pub fn with_attachment_provider(mut self, provider: Arc<dyn VolumeAttachmentProvider>) -> Self {
        self.cinder = Some(provider.clone());
        self.attachments =
            AttachmentOrchestrator::new(self.store.clone(), self.provider.clone(), Some(provider));
        self
    }

    #[must_use]
    pub fn attachment_orchestrator(&self) -> AttachmentOrchestrator {
        self.attachments.clone()
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
    pub fn with_agent_registry(mut self, registry: Arc<dyn AgentNodeRegistry>) -> Self {
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
        update: &o3k_provider::AgentOperationUpdate,
    ) -> Result<o3k_store::OperationState, ComputeError> {
        let state = self.journal.apply_agent_update(update).await?;
        if matches!(
            state,
            o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
        ) {
            self.project_terminal_binding_outcome(update.operation_id.to_string().as_str(), state)
                .await;
        }
        if state == o3k_store::OperationState::Failed {
            self.compensate_failed_create(update.operation_id).await?;
        }
        Ok(state)
    }

    /// Reflects a terminal operation outcome into the durable port binding
    /// state of the network control plane. The server's ports are read from
    /// the durable desired-state snapshot, and the binding host comes from
    /// the intent the network service recorded at dispatch. Projection is
    /// best-effort and idempotent: it is a side observation, never a compute
    /// failure, and a replayed terminal update projects the same state again.
    /// Integrity anomalies (a missing operation or resource, or an
    /// unparseable desired-state snapshot) are surfaced as warnings instead
    /// of failing the compute path.
    async fn project_terminal_binding_outcome(
        &self,
        operation_id: &str,
        state: o3k_store::OperationState,
    ) {
        let Some(projector) = self.binding_projector.as_ref() else {
            return;
        };
        let Ok(operation_id) = Uuid::parse_str(operation_id) else {
            tracing::warn!(
                operation_id = %operation_id,
                "port binding outcome skipped: operation id is not a UUID"
            );
            return;
        };
        let Ok(operation) = self.store.get_operation(operation_id).await else {
            tracing::warn!(
                operation_id = %operation_id,
                "port binding outcome skipped: operation is missing from the durable store"
            );
            return;
        };
        let Ok(resource) = self.store.get_resource(operation.resource_id).await else {
            tracing::warn!(
                operation_id = %operation_id,
                resource_id = %operation.resource_id,
                "port binding outcome skipped: server resource is missing from the durable store"
            );
            return;
        };
        let Ok(request) = serde_json::from_str::<CreateInstanceRequest>(&resource.desired_state)
        else {
            tracing::warn!(
                operation_id = %operation_id,
                resource_id = %operation.resource_id,
                "port binding outcome skipped: server create intent is corrupt"
            );
            return;
        };
        for port_id in &request.network_ids {
            let outcome = match operation.kind.as_str() {
                "create" => {
                    projector
                        .project_create_outcome(
                            &request.project_id,
                            port_id,
                            state == o3k_store::OperationState::Succeeded,
                        )
                        .await
                }
                "lifecycle:delete" if state == o3k_store::OperationState::Succeeded => {
                    projector.unbind_port(&request.project_id, port_id).await
                }
                _ => continue,
            };
            if let Err(error) = outcome {
                tracing::warn!(
                    operation_id = %operation_id,
                    resource_id = %operation.resource_id,
                    port_id = %port_id,
                    error = %error,
                    "port binding outcome projection rejected"
                );
            }
        }
    }

    /// Clears the binding of every port named by the server's durable create
    /// intent. Used when a delete reached terminal success, including the
    /// already-deleted shortcut, where the delete completed in a previous
    /// run. Best-effort and idempotent like `project_terminal_binding_outcome`.
    async fn unbind_ports_from_intent(&self, request: &CreateInstanceRequest) {
        let Some(projector) = self.binding_projector.as_ref() else {
            return;
        };
        for port_id in &request.network_ids {
            if let Err(error) = projector.unbind_port(&request.project_id, port_id).await {
                tracing::warn!(
                    resource_id = %request.o3k_server_id,
                    port_id = %port_id,
                    error = %error,
                    "port unbind projection rejected"
                );
            }
        }
    }

    /// Applies the same reverse-order compensation as the synchronous create
    /// path when a create operation is terminal Failed after the API request
    /// already returned. Compensation is idempotent: keypair detach is a
    /// delete-if-present and the placement allocation is released only when
    /// it is still held, so replayed deliveries and repeated convergence
    /// triggers are safe.
    async fn compensate_failed_create(&self, operation_id: Uuid) -> Result<(), ComputeError> {
        let operation = self.store.get_operation(operation_id).await?;
        if operation.kind != "create" {
            return Ok(());
        }
        let resource = self.store.get_resource(operation.resource_id).await?;
        self.store.detach_server_keypair(resource.id).await?;
        let request: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
            .map_err(|_| ComputeError::InvalidRequest)?;
        if let (Some(scheduler), Some(provider_id), Some(allocation_id)) = (
            self.scheduler.as_ref(),
            request.placement_provider_id.as_deref(),
            request.placement_allocation_id.as_deref(),
        ) && scheduler
            .validate_allocation(provider_id, allocation_id, &resource.id.to_string())
            .await
            .is_ok()
        {
            self.release_placement_allocation(resource.id, &request)
                .await?;
        }
        Ok(())
    }

    pub async fn apply_agent_acceptance(
        &self,
        accepted: &o3k_provider::AgentCommandAccepted,
    ) -> Result<o3k_store::OperationState, ComputeError> {
        Ok(self.journal.apply_agent_acceptance(accepted).await?)
    }

    /// Applies an authenticated provider observation to the durable resource
    /// projection. This is separate from operation progress because a command
    /// may succeed while the provider remains stopped, deleting, or errored.
    pub async fn apply_agent_observation(
        &self,
        observation: &o3k_provider::AgentObservation,
    ) -> Result<(), ComputeError> {
        Ok(self.journal.apply_agent_observation(observation).await?)
    }

    /// Starts the in-memory event bridge used by the control-plane binary.
    /// The journal remains the recovery authority; this task only applies live
    /// updates received from an authenticated agent connection.
    pub fn spawn_agent_event_consumer(
        &self,
        registry: Arc<dyn AgentNodeRegistry>,
    ) -> tokio::task::JoinHandle<()> {
        let mut events = registry.subscribe_events();
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(o3k_provider::AgentEvent::Operation(update)) => {
                        if let Err(error) = service.apply_agent_update(&update).await {
                            tracing::warn!(error = ?error, "agent operation update rejected");
                        }
                    }
                    Ok(o3k_provider::AgentEvent::CommandAccepted(accepted)) => {
                        if let Err(error) = service.apply_agent_acceptance(&accepted).await {
                            tracing::warn!(error = ?error, "agent command acceptance rejected");
                        }
                    }
                    Ok(o3k_provider::AgentEvent::Observation(observation)) => {
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
                                error = ?error,
                                operation_id = %observation.operation_id,
                                resource_id = %observation.resource_id,
                                agent_id = %observation.agent_id,
                                agent_epoch = %observation.agent_epoch,
                                operation_state = ?observation.operation_state,
                                state = ?observation.state,
                                provider_resource_id = ?observation.provider_resource_id,
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

    /// Drives durable attachment recovery after restart or an unknown outcome.
    ///
    /// The attachment orchestrator persists every phase before executing an
    /// external side effect. On restart, in-flight or unknown-outcome records
    /// must converge by observing the Cinder and compute boundaries rather than
    /// re-running mutations blindly. This bounded periodic task is the
    /// production caller for `AttachmentOrchestrator::reconcile`.
    pub fn spawn_attachment_reconciler(&self, interval_secs: u64) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = service.attachment_orchestrator().reconcile().await {
                    tracing::warn!(%error, "attachment reconcile pass failed");
                }
            }
        })
    }

    /// Periodically drives create convergence for servers left in a state
    /// that nothing else will ever advance: `Pending`, `UnknownOutcome`, or
    /// `Running` without a provider operation identity (issue-87 S1 residue —
    /// a crash between persisting `Running` and dispatching the create).
    /// After a control-plane restart the lazy show path alone would leave
    /// such a server stuck in REQUESTED (and its placement allocation leaked)
    /// until a client polls it; this bounded periodic task is the recovery
    /// authority. Each pass is lazy and idempotent: terminal and accepted
    /// operations are skipped by `drive_create_convergence`, and the
    /// reconciler reuses in-flight and terminal provider work by the
    /// deterministic operation identity.
    pub fn spawn_create_convergence_reconciler(
        &self,
        interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = service.drive_all_create_convergence().await {
                    tracing::warn!(%error, "create convergence reconcile pass failed");
                }
            }
        })
    }

    /// Drives create convergence for every durable compute instance. The
    /// per-resource drive is lazy and bounded, so healthy servers are skipped
    /// and a stuck server converges regardless of which project owns it.
    async fn drive_all_create_convergence(&self) -> Result<(), ComputeError> {
        let resources = self
            .store
            .list_resources_by_kind("compute_instance")
            .await?;
        for resource in resources {
            self.drive_create_convergence(&resource).await;
        }
        Ok(())
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
            if !matches!(
                server_state_from_storage(&server.observed_state),
                Ok(ServerState::Deleted)
            ) && serde_json::from_str::<serde_json::Value>(&server.desired_state)
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
                    if matches!(
                        server_state_from_storage(&existing.observed_state),
                        Ok(ServerState::Deleted)
                    ) {
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
                                self.project_terminal_binding_outcome(
                                    existing_request.operation_id.to_string().as_str(),
                                    o3k_store::OperationState::Failed,
                                )
                                .await;
                                return Err(ComputeError::Conflict);
                            }
                            Ok(o3k_store::OperationState::Succeeded) => {
                                self.project_terminal_binding_outcome(
                                    existing_request.operation_id.to_string().as_str(),
                                    o3k_store::OperationState::Succeeded,
                                )
                                .await;
                            }
                            Ok(_) => {}
                            Err(error) => {
                                self.store.detach_server_keypair(server_id).await?;
                                return Err(ComputeError::Reconcile(error));
                            }
                        }
                    }
                    return self
                        .show_server(&project_id, ServerId::from_uuid(server_id))
                        .await;
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
            .any(|server| server.name == name && server.state != ServerState::Deleted)
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
                        node.availability == AgentAvailability::Available
                            && node.administrative_state == AgentAdministrativeState::Enabled
                    })
                    .map(|node| node.agent_id)
                    .collect::<BTreeSet<_>>();
                Some(
                    self.schedule_server(
                        scheduler,
                        Some(&eligible),
                        &server_id.to_string(),
                        scheduler_flavor,
                    )
                    .await?,
                )
            }
            (Some(scheduler), None) => Some(
                self.schedule_server(scheduler, None, &server_id.to_string(), scheduler_flavor)
                    .await?,
            ),
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
                    self.release_placement_decision(decision).await?;
                }
                return Err(error);
            }
        };
        if servers
            .iter()
            .any(|server| server.name == name && server.state != ServerState::Deleted)
        {
            // A racing identical request may have persisted this server while
            // the schedule was in flight; its allocation idempotently backs
            // that live row and must not be released. Only a decision that is
            // not owned by a live row carrying the same placement binding is
            // released here (a name conflict from a different request, or a
            // decision that fell back to a provider the persisted row does
            // not reference).
            let owns_live_server = async {
                let Some(decision) = placement.as_ref() else {
                    return false;
                };
                let Ok(server_id) = Uuid::parse_str(&decision.allocation.consumer_id) else {
                    return false;
                };
                let Ok(resource) = self.store.get_resource(server_id).await else {
                    return false;
                };
                resource.kind == "compute_instance"
                    && server_state_from_storage(&resource.observed_state).ok()
                        != Some(ServerState::Deleted)
                    && serde_json::from_str::<CreateInstanceRequest>(&resource.desired_state)
                        .map(|intent| {
                            intent.placement_provider_id.as_deref()
                                == Some(decision.provider_id.as_str())
                                && intent.placement_allocation_id.as_deref()
                                    == Some(decision.allocation_id.as_str())
                        })
                        .unwrap_or(false)
            }
            .await;
            if let Some(decision) = placement.as_ref()
                && !owns_live_server
            {
                self.release_placement_decision(decision).await?;
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
                    self.release_placement_decision(decision).await?;
                }
                let legacy_keypair_intent =
                    requests_match_with_keypair_migration(&existing_request, &request);
                if existing_request != request && !legacy_keypair_intent {
                    return Err(ComputeError::Conflict);
                }
                if matches!(
                    server_state_from_storage(&existing.observed_state),
                    Ok(ServerState::Deleted)
                ) {
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
                            self.project_terminal_binding_outcome(
                                request.operation_id.to_string().as_str(),
                                o3k_store::OperationState::Failed,
                            )
                            .await;
                            return Err(ComputeError::Conflict);
                        }
                        Ok(o3k_store::OperationState::Succeeded) => {
                            self.project_terminal_binding_outcome(
                                request.operation_id.to_string().as_str(),
                                o3k_store::OperationState::Succeeded,
                            )
                            .await;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            self.store.detach_server_keypair(id).await?;
                            return Err(ComputeError::Reconcile(error));
                        }
                    }
                }
                return self.show_server(&project_id, ServerId::from_uuid(id)).await;
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
        if matches!(
            reconcile_state,
            o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
        ) {
            self.project_terminal_binding_outcome(
                request.operation_id.to_string().as_str(),
                reconcile_state,
            )
            .await;
        }
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
                scheduler
                    .release_terminal(&o3k_scheduler::ScheduleDecision {
                        provider_id: provider_id.to_owned(),
                        allocation_id: allocation_id.to_owned(),
                        allocation: o3k_placement::Allocation {
                            provider_id: provider_id.to_owned(),
                            consumer_id: id.to_string(),
                            resources: std::collections::BTreeMap::new(),
                        },
                    })
                    .await?;
            }
            return Err(ComputeError::Conflict);
        }
        self.show_server(&project_id, ServerId::from_uuid(id)).await
    }

    pub async fn list_servers(&self, project_id: &str) -> Result<Vec<Server>, ComputeError> {
        let flavors = self.flavors_for_project(project_id).await?;
        let resources = self
            .store
            .list_resources(project_id, "compute_instance")
            .await?;
        let mut servers = Vec::new();
        for resource in resources {
            let resource_id = resource.id;
            let mut server = match server_from_resource(resource, &flavors) {
                Ok(server) => server,
                Err(ServerProjectionError::CorruptState(corrupt)) => {
                    // Corrupt rows are skipped, not misclassified: the
                    // conversion failed closed. Surface the integrity failure
                    // so an operator can repair the durable ledger.
                    tracing::warn!(%resource_id, %corrupt, "server lifecycle state is corrupt; row skipped");
                    continue;
                }
                Err(ServerProjectionError::Unresolvable) => continue,
            };
            if server.state != ServerState::Deleted {
                server.key_name = self
                    .store
                    .get_server_keypair_name(server.id.as_uuid())
                    .await?;
                servers.push(server);
            }
        }
        Ok(servers)
    }

    pub async fn show_server(
        &self,
        project_id: &str,
        id: ServerId,
    ) -> Result<Server, ComputeError> {
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        // The show path is the poll surface for `openstack server create
        // --wait`: a create operation left non-terminal after the synchronous
        // pass must be re-driven here or the server stays in BUILD forever.
        // The drive is lazy, bounded, and idempotent; ownership was validated
        // above, so no provider dispatch can happen for a foreign project.
        self.drive_create_convergence(&resource).await;
        // Re-read the durable state: the convergence drive may have projected
        // a terminal outcome onto the resource.
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        let flavors = self.flavors_for_project(project_id).await?;
        let mut server = match server_from_resource(resource, &flavors) {
            Ok(server) => server,
            Err(ServerProjectionError::CorruptState(corrupt)) => {
                return Err(ComputeError::Store(corrupt));
            }
            Err(ServerProjectionError::Unresolvable) => {
                return Err(ComputeError::InvalidRequest);
            }
        };
        if server.state == ServerState::Deleted {
            return Err(ComputeError::NotFound);
        }
        server.key_name = self
            .store
            .get_server_keypair_name(server.id.as_uuid())
            .await?;
        Ok(server)
    }

    /// Drives durable create convergence for a server whose create operation
    /// is stuck in a state that nothing else will ever advance: `Pending` (a
    /// crash between persisting the intent and the synchronous pass),
    /// `UnknownOutcome` (dispatch timeout, transport loss), or `Running`
    /// without a provider operation identity (a crash between the
    /// Pending→Running persist in `reconcile_once` and the dispatch reaching
    /// the provider — issue-87 S1 residue). Without this driver the server
    /// would stay in BUILD forever after the synchronous pass in
    /// `create_server`, and a genuine unknown outcome only converges by
    /// observing instance presence at the execution boundary (issue #481
    /// criterion 3).
    ///
    /// A `Running` operation that carries a provider operation identity is
    /// deliberately NOT driven: the provider has accepted the command (the
    /// identity is attached only after a successful dispatch) and its
    /// terminal update arrives through the agent event stream, and a
    /// concurrent re-drive from the poll path would race the synchronous
    /// finisher on the same operation records (duplicate reference attach /
    /// stale generation). A `Running` operation WITHOUT the identity was
    /// never accepted, so it is re-driven like `Pending`; re-dispatch is
    /// safe because the agent journal dedups by command id/operation/
    /// idempotency key + fingerprint and never re-executes an accepted
    /// command. The drive is lazy (read-triggered), bounded (terminal and
    /// accepted operations are not re-driven), and idempotent (the
    /// reconciler reuses in-flight and terminal provider work by the
    /// deterministic operation identity). Errors are surfaced as warnings so
    /// the read path stays available; a converged failure applies the same
    /// reverse-order compensation as the asynchronous agent-failure path.
    async fn drive_create_convergence(&self, resource: &o3k_store::ResourceRecord) {
        let Ok(request) = serde_json::from_str::<CreateInstanceRequest>(&resource.desired_state)
        else {
            return;
        };
        let Ok(operation) = self.store.get_operation(request.operation_id).await else {
            return;
        };
        let re_drive = matches!(
            operation.state,
            o3k_store::OperationState::Pending | o3k_store::OperationState::UnknownOutcome
        ) || (operation.state == o3k_store::OperationState::Running
            && operation.provider_operation_id.is_none());
        if !re_drive {
            return;
        }
        let state = match self.journal.reconcile_once(request.operation_id).await {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(
                    operation_id = %request.operation_id,
                    resource_id = %resource.id,
                    error = %error,
                    "server create convergence pass failed; server state is unchanged"
                );
                return;
            }
        };
        match state {
            o3k_store::OperationState::Failed => {
                self.project_terminal_binding_outcome(
                    request.operation_id.to_string().as_str(),
                    state,
                )
                .await;
                if let Err(error) = self.compensate_failed_create(request.operation_id).await {
                    tracing::warn!(
                        operation_id = %request.operation_id,
                        resource_id = %resource.id,
                        error = %error,
                        "server create failure compensation failed"
                    );
                }
                // A terminal create failure must render ERROR on the poll
                // surface, or `--wait` keeps showing BUILD forever. The
                // reconciler projects ERROR internally only for presence
                // absence; every other failure path (dispatch rejection,
                // retry budget exhaustion, provider-reported failure) needs
                // the drive to project it. The update is idempotent: the
                // resource is only touched when it is not already ERROR.
                let Ok(resource) = self.store.get_resource(resource.id).await else {
                    return;
                };
                if resource.observed_state != server_state_to_storage(ServerState::Error)
                    && let Err(error) = self
                        .store
                        .update_resource(
                            resource.id,
                            resource.generation,
                            &resource.desired_state,
                            server_state_to_storage(ServerState::Error),
                            resource.generation,
                            resource.provider_id.as_deref(),
                        )
                        .await
                {
                    tracing::warn!(
                        operation_id = %request.operation_id,
                        resource_id = %resource.id,
                        error = %error,
                        "server create failure projection to ERROR failed"
                    );
                }
            }
            o3k_store::OperationState::Succeeded => {
                self.project_terminal_binding_outcome(
                    request.operation_id.to_string().as_str(),
                    state,
                )
                .await;
            }
            _ => {}
        }
    }

    pub async fn attach_volume(
        &self,
        project_id: &str,
        server_id: ServerId,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.attachments
            .attach(
                project_id,
                server_id.as_uuid(),
                volume_id,
                device,
                tag,
                delete_on_termination,
            )
            .await
    }

    pub async fn list_volume_attachments(
        &self,
        project_id: &str,
        server_id: ServerId,
    ) -> Result<Vec<VolumeAttachmentRecord>, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        let records = self
            .store
            .list_volume_attachments(server_id.as_uuid())
            .await?;
        Ok(records
            .into_iter()
            .filter(|r| r.status != "detached")
            .collect())
    }

    pub async fn get_volume_attachment(
        &self,
        project_id: &str,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.store
            .get_volume_attachment(server_id.as_uuid(), attachment_id)
            .await?
            .ok_or(ComputeError::NotFound)
    }

    pub async fn detach_volume(
        &self,
        project_id: &str,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.attachments
            .detach(project_id, server_id.as_uuid(), attachment_id)
            .await
    }

    /// Revalidates and inspects an already-created server through the
    /// provider boundary. This is deliberately read-only: an existing
    /// Placement allocation is checked, never recreated, before the provider
    /// receives an inspect request.
    pub async fn inspect_server(
        &self,
        project_id: &str,
        id: ServerId,
        idempotency_key: &str,
    ) -> Result<Operation, ComputeError> {
        let resource =
            self.store
                .get_resource(id.as_uuid())
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
            scheduler
                .validate_allocation(provider_id, allocation_id, &id.to_string())
                .await?;
        } else {
            return Err(ComputeError::Conflict);
        }
        let _reference = match self
            .store
            .get_provider_reference(id.as_uuid(), "compute")
            .await
        {
            Ok(reference) => reference,
            Err(StoreError::ProviderReferenceNotFound) => self
                .store
                .get_provider_reference(id.as_uuid(), "compute-agent")
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
                    resource_id: id.as_uuid(),
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
        id: ServerId,
    ) -> Result<Option<String>, ComputeError> {
        let resource = self.store.get_resource(id.as_uuid()).await?;
        if resource.kind != "compute_instance" || resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        Ok(request.placement_provider_id)
    }

    pub async fn delete_server(&self, project_id: &str, id: ServerId) -> Result<(), ComputeError> {
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        // The destructive path must fail closed on corrupt lifecycle state:
        // deleting a row whose state cannot be decoded would dispatch a
        // provider delete on an unknown instance and overwrite the evidence
        // needed for repair. The decode error is propagated before any
        // lifecycle operation begins; only a decodable `Deleted` row takes
        // the already-deleted shortcut.
        let observed =
            server_state_from_storage(&resource.observed_state).map_err(ComputeError::Store)?;
        if observed == ServerState::Deleted {
            let intent: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
                .map_err(|_| ComputeError::Conflict)?;
            self.release_placement_allocation(id.as_uuid(), &intent)
                .await?;
            self.store.detach_server_keypair(id.as_uuid()).await?;
            // The delete reached terminal success in a previous run; clear
            // any binding that was not yet unbound.
            self.unbind_ports_from_intent(&intent).await;
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
            .begin_lifecycle(id.as_uuid(), operation_id, LifecycleAction::Delete)
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
        self.release_placement_allocation(id.as_uuid(), &intent)
            .await?;
        self.store.detach_server_keypair(id.as_uuid()).await?;
        // Terminal delete success means the agent removed the host execution;
        // the ports are reusable and must no longer claim a binding.
        self.project_terminal_binding_outcome(
            operation_id.to_string().as_str(),
            o3k_store::OperationState::Succeeded,
        )
        .await;
        Ok(())
    }

    async fn release_placement_allocation(
        &self,
        server_id: Uuid,
        intent: &CreateInstanceRequest,
    ) -> Result<(), ComputeError> {
        if let (Some(scheduler), Some(provider_id), Some(allocation_id)) = (
            self.scheduler.as_ref(),
            intent.placement_provider_id.as_deref(),
            intent.placement_allocation_id.as_deref(),
        ) {
            scheduler
                .release_terminal(&o3k_scheduler::ScheduleDecision {
                    provider_id: provider_id.to_owned(),
                    allocation_id: allocation_id.to_owned(),
                    allocation: o3k_placement::Allocation {
                        provider_id: provider_id.to_owned(),
                        consumer_id: server_id.to_string(),
                        resources: std::collections::BTreeMap::new(),
                    },
                })
                .await?;
        }
        Ok(())
    }

    async fn release_placement_decision(
        &self,
        decision: &o3k_scheduler::ScheduleDecision,
    ) -> Result<(), ComputeError> {
        if let Some(scheduler) = self.scheduler.as_ref() {
            scheduler.release_terminal(decision).await?;
        }
        Ok(())
    }

    /// Schedules a create request. The ledger reports `InvalidAllocation`
    /// when the `allocation-{server_id}` intent key for this server collided
    /// with a concurrent identical request: the intent was consumed and this
    /// call acquired no capacity. The racing request holds (or will hold) the
    /// allocation, so the collision surfaces as a Conflict without releasing
    /// anything; request-level validation errors are already fenced by the
    /// ledger and the scheduler before this point.
    async fn schedule_server(
        &self,
        scheduler: &Scheduler,
        selected_agents: Option<&BTreeSet<String>>,
        server_id: &str,
        flavor: SchedulerFlavor,
    ) -> Result<o3k_scheduler::ScheduleDecision, ComputeError> {
        let attempt = match selected_agents {
            Some(agents) => {
                scheduler
                    .schedule_for_agents(agents, server_id, flavor)
                    .await
            }
            None => scheduler.schedule(server_id, flavor).await,
        };
        match attempt {
            Err(o3k_scheduler::SchedulerError::Placement(
                o3k_placement::PlacementError::InvalidAllocation,
            )) => Err(ComputeError::Conflict),
            result => result.map_err(ComputeError::from),
        }
    }

    pub async fn action(
        &self,
        project_id: &str,
        id: ServerId,
        action: InstanceAction,
    ) -> Result<Server, ComputeError> {
        let resource =
            self.store
                .get_resource(id.as_uuid())
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
        // Action applicability is decided on the canonical lifecycle state,
        // decoded fail-closed from the durable observed value. The target
        // state feeds the deterministic journal identity through its storage
        // encoding, so durable operation ids are unchanged.
        let current = server_state_from_storage(&resource.observed_state)
            .map_err(|_| ComputeError::Conflict)?;
        let target = match (action, current) {
            (InstanceAction::Start, ServerState::Stopped) => ServerState::Active,
            (InstanceAction::Stop, ServerState::Active) => ServerState::Stopped,
            (InstanceAction::Reboot, ServerState::Active | ServerState::Stopped) => {
                ServerState::Active
            }
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
                "o3k:action:{project_id}:{id}:{}:{}",
                server_state_to_storage(target),
                resource.generation
            )
            .as_bytes(),
        );
        match self
            .journal
            .begin_lifecycle(id.as_uuid(), operation_id, lifecycle_action)
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

/// Why a durable resource row could not be projected into a canonical
/// `Server`. Distinguishing corrupt lifecycle state from an unresolvable
/// intent matters: corruption is a server-side integrity failure (500),
/// while an unparseable create intent or missing flavor is the pre-existing
/// invalid-request category (400).
enum ServerProjectionError {
    /// The persisted server lifecycle state is not a decodable canonical
    /// state. Carries the store corruption error for reporting.
    CorruptState(StoreError),
    /// The create intent cannot be parsed or its flavor cannot be resolved.
    Unresolvable,
}

fn server_from_resource(
    resource: o3k_store::ResourceRecord,
    flavors: &[Flavor],
) -> Result<Server, ServerProjectionError> {
    let request: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
        .map_err(|_| ServerProjectionError::Unresolvable)?;
    let flavor = if request.flavor_id.trim().is_empty() {
        flavors
            .iter()
            .find(|flavor| flavor.vcpus == request.vcpus && flavor.ram_mib == request.memory_mib)
    } else {
        let flavor_id = request
            .flavor_id
            .parse::<Uuid>()
            .map_err(|_| ServerProjectionError::Unresolvable)?;
        flavors.iter().find(|flavor| flavor.id == flavor_id)
    }
    .ok_or(ServerProjectionError::Unresolvable)?;
    let state = server_state_from_storage(&resource.observed_state)
        .map_err(ServerProjectionError::CorruptState)?;
    Ok(Server {
        id: ServerId::from_uuid(resource.id),
        name: request.name,
        project_id: resource.project_id,
        flavor_id: flavor.id,
        image_id: request.image_id.unwrap_or_default(),
        state,
        key_name: None,
        config_drive: request.config_drive.is_some(),
        network_ids: request.network_ids,
        host: request.placement_provider_id,
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
    use o3k_provider::{
        AgentAdministrativeState, AgentAvailability, AgentCapabilities, AgentErrorCategory,
        AgentNodeSnapshot, AgentObservation, AgentOperationState, AgentOperationUpdate,
        FailureInjection,
    };
    use std::path::PathBuf;

    /// Stateful in-memory agent registry used to test application scheduling
    /// and inventory behavior without wire types. The snapshots are
    /// application-level values, so the tests exercise exactly what the
    /// transport adapter would publish after its boundary conversion.
    #[derive(Clone, Default)]
    struct FakeAgentRegistry {
        nodes: Arc<tokio::sync::RwLock<BTreeMap<String, AgentNodeSnapshot>>>,
    }

    #[async_trait]
    impl AgentNodeRegistry for FakeAgentRegistry {
        async fn all(&self) -> Vec<AgentNodeSnapshot> {
            self.nodes.read().await.values().cloned().collect()
        }

        async fn snapshot(&self, agent_id: &str) -> Option<AgentNodeSnapshot> {
            self.nodes.read().await.get(agent_id).cloned()
        }

        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<o3k_provider::AgentEvent> {
            let (_, receiver) = tokio::sync::broadcast::channel(1);
            receiver
        }
    }

    impl FakeAgentRegistry {
        async fn upsert(&self, node: AgentNodeSnapshot) {
            self.nodes.write().await.insert(node.agent_id.clone(), node);
        }

        async fn set_unavailable(&self, agent_id: &str) {
            if let Some(node) = self.nodes.write().await.get_mut(agent_id) {
                node.availability = AgentAvailability::Unavailable;
            }
        }
    }

    fn agent_node(agent_id: &str, vcpus: u64, memory_mib: u64, disk_gib: u64) -> AgentNodeSnapshot {
        agent_node_with_state(
            agent_id,
            AgentAdministrativeState::Enabled,
            vcpus,
            memory_mib,
            disk_gib,
        )
    }

    fn agent_node_with_state(
        agent_id: &str,
        administrative_state: AgentAdministrativeState,
        vcpus: u64,
        memory_mib: u64,
        disk_gib: u64,
    ) -> AgentNodeSnapshot {
        AgentNodeSnapshot {
            agent_id: agent_id.to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            availability: AgentAvailability::Available,
            administrative_state,
            capabilities: AgentCapabilities {
                agent_provider_name: "o3k-compute".to_owned(),
                agent_provider_version: "test".to_owned(),
                max_vcpus: vcpus,
                max_memory_mib: memory_mib,
                max_disk_gb: disk_gib,
                lifecycle_actions: vec!["start".to_owned(), "stop".to_owned()],
                console_log: true,
                flags: Vec::new(),
            },
        }
    }

    #[derive(Debug, PartialEq, Eq, Clone)]
    enum ProjectorCall {
        CreateOutcome {
            project: String,
            port: String,
            succeeded: bool,
        },
        Unbind {
            project: String,
            port: String,
        },
    }

    #[derive(Default)]
    struct RecordingProjector {
        calls: std::sync::Mutex<Vec<ProjectorCall>>,
    }

    #[async_trait]
    impl PortBindingProjector for RecordingProjector {
        async fn project_create_outcome(
            &self,
            project_id: &str,
            port_id: &str,
            succeeded: bool,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.calls
                .lock()
                .map_err(|_| "recording projector lock poisoned".to_owned())?
                .push(ProjectorCall::CreateOutcome {
                    project: project_id.to_owned(),
                    port: port_id.to_owned(),
                    succeeded,
                });
            Ok(())
        }

        async fn unbind_port(
            &self,
            project_id: &str,
            port_id: &str,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.calls
                .lock()
                .map_err(|_| "recording projector lock poisoned".to_owned())?
                .push(ProjectorCall::Unbind {
                    project: project_id.to_owned(),
                    port: port_id.to_owned(),
                });
            Ok(())
        }
    }

    fn projector_calls(projector: &RecordingProjector) -> Vec<ProjectorCall> {
        projector
            .calls
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    async fn service(label: &str) -> Result<ComputeService, ComputeError> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Ok(ComputeService::new(
            Arc::new(o3k_store::testkit::open_file(&path).await?),
            Arc::new(FakeComputeProvider::new()),
        ))
    }

    #[tokio::test]
    async fn corrupt_persisted_server_state_fails_closed() -> Result<(), ComputeError> {
        let service = service("corrupt-state").await?;
        let corrupt_id = Uuid::now_v7();
        let flavor = service.flavors()[0].id;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: corrupt_id,
            project_id: "project-a".to_owned(),
            name: "corrupt".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: flavor.to_string(),
            disk_gib: 10,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: Vec::new(),
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("alloc-1".to_owned()),
            config_drive: None,
            idempotency_key: "corrupt-state".to_owned(),
        };
        service
            .store
            .insert_resource(&o3k_store::ResourceRecord {
                id: corrupt_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: serde_json::to_string(&request)
                    .map_err(|_| ComputeError::Conflict)?,
                observed_state: "garbage-state".to_owned(),
                provider_id: Some("node-a".to_owned()),
            })
            .await?;
        // The corrupt value must never be misclassified as a valid lifecycle
        // state: show fails closed as a server-side integrity failure, list
        // skips the row, actions reject it.
        assert!(matches!(
            service
                .show_server("project-a", ServerId::from_uuid(corrupt_id))
                .await,
            Err(ComputeError::Store(StoreError::Corrupt(_)))
        ));
        assert!(
            !service
                .list_servers("project-a")
                .await?
                .iter()
                .any(|server| server.id.as_uuid() == corrupt_id)
        );
        assert!(matches!(
            service
                .action(
                    "project-a",
                    ServerId::from_uuid(corrupt_id),
                    InstanceAction::Stop
                )
                .await,
            Err(ComputeError::Conflict)
        ));
        // The destructive path must also fail closed: delete rejects the
        // corrupt row instead of dispatching a provider delete on an unknown
        // instance and overwriting the evidence needed for repair.
        assert!(matches!(
            service
                .delete_server("project-a", ServerId::from_uuid(corrupt_id))
                .await,
            Err(ComputeError::Store(StoreError::Corrupt(_)))
        ));
        assert_eq!(
            service.store.get_resource(corrupt_id).await?.observed_state,
            "garbage-state"
        );
        Ok(())
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
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_path, placement_repository)
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider(
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
            )
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
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
        let before = placement
            .provider("node-a")
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
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
        assert_eq!(
            before,
            placement
                .provider("node-a")
                .await
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
        );
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
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&path).await?);
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
            Arc::new(o3k_store::testkit::open_file(&path).await?),
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
        let persisted_intent: CreateInstanceRequest = serde_json::from_str(
            &reopened
                .store
                .get_resource(server.id.as_uuid())
                .await?
                .desired_state,
        )?;
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
        let capabilities = AgentCapabilities {
            agent_provider_name: "o3k-compute".to_owned(),
            agent_provider_version: "test".to_owned(),
            max_vcpus: 4,
            max_memory_mib: 4096,
            max_disk_gb: 0,
            lifecycle_actions: Vec::new(),
            console_log: false,
            flags: Vec::new(),
        };
        let inventory = agent_inventory(&capabilities);
        assert_eq!(inventory[o3k_placement::VCPU].total, 4);
        assert_eq!(inventory[o3k_placement::MEMORY_MB].total, 4096);
        assert_eq!(inventory[o3k_placement::DISK_GB].total, 0);
    }

    #[tokio::test]
    async fn agent_inventory_is_published_and_state_fenced()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = PathBuf::from(format!(
            "/tmp/o3k-placement-agent-inventory-{}",
            Uuid::now_v7()
        ));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&root, placement_repository).await?;
        let registry = FakeAgentRegistry::default();
        registry.upsert(agent_node("agent-a", 4, 4096, 20)).await;

        sync_agent_inventory(&registry, &placement).await?;
        let provider = placement.provider("agent-a").await?;
        assert_eq!(provider.state, o3k_placement::ProviderState::Enabled);
        assert_eq!(provider.inventories[o3k_placement::VCPU].total, 4);
        assert_eq!(provider.inventories[o3k_placement::MEMORY_MB].total, 4096);
        assert_eq!(provider.inventories[o3k_placement::DISK_GB].total, 20);

        placement
            .allocate(
                "agent-a",
                "allocation-1",
                "server-1",
                BTreeMap::from([
                    (o3k_placement::VCPU.to_owned(), 1),
                    (o3k_placement::MEMORY_MB.to_owned(), 512),
                    (o3k_placement::DISK_GB.to_owned(), 1),
                ]),
                provider.generation,
            )
            .await?;
        registry
            .upsert(agent_node_with_state(
                "agent-a",
                AgentAdministrativeState::Draining,
                4,
                4096,
                20,
            ))
            .await;
        sync_agent_inventory(&registry, &placement).await?;
        let refreshed = placement.provider("agent-a").await?;
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
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&path).await?);
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
        let resource = store.get_resource(server.id.as_uuid()).await?;
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
        assert_eq!(stopped.state, ServerState::Stopped);
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
        assert_eq!(server.state, ServerState::Active);
        assert_eq!(
            service
                .action("project-a", server.id, InstanceAction::Stop)
                .await?
                .state,
            ServerState::Stopped
        );
        assert_eq!(
            service
                .action("project-a", server.id, InstanceAction::Start)
                .await?
                .state,
            ServerState::Active
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
        let update = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: request.operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some("agent-domain-1".to_owned()),
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
            "REQUESTED"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_failed_create_update_marks_error_and_compensates_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-agent-failed-{}.sqlite",
            std::process::id()
        ));
        let placement_path = PathBuf::from(format!(
            "/tmp/o3k-compute-agent-failed-placement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement =
            o3k_placement::PlacementLedger::open(&placement_path, placement_repository).await?;
        placement
            .register_provider(
                "node-a",
                std::collections::BTreeMap::from([(
                    o3k_placement::VCPU.to_owned(),
                    o3k_placement::Inventory {
                        total: 4,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                )]),
            )
            .await?;
        let service = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()))
            .with_scheduler(Scheduler::new(placement.clone()));
        let keypair = service
            .create_keypair(
                "user-a",
                "project-a",
                "agent-failed-key".to_owned(),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test".to_owned(),
            )
            .await?;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "failed-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: Some("agent-failed-key".to_owned()),
            keypair_id: Some(keypair.id),
            network_ids: vec!["network-1".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("alloc-1".to_owned()),
            config_drive: None,
            idempotency_key: "agent-failed".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .attach_server_keypair(request.o3k_server_id, keypair.id)
            .await?;
        let generation = placement.provider("node-a").await?.generation;
        placement
            .allocate(
                "node-a",
                "alloc-1",
                &request.o3k_server_id.to_string(),
                std::collections::BTreeMap::from([(o3k_placement::VCPU.to_owned(), 1_u64)]),
                generation,
            )
            .await?;

        let update = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: request.operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Terminal),
            redacted_message: None,
            provider_resource_id: None,
        };
        assert_eq!(
            service.apply_agent_update(&update).await?,
            o3k_store::OperationState::Failed
        );
        assert_eq!(
            service
                .store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        assert_eq!(
            service
                .store
                .get_server_keypair_name(request.o3k_server_id)
                .await?,
            None
        );
        assert!(placement.provider("node-a").await?.allocations.is_empty());
        // A replayed delivery of the same terminal update compensates safely.
        assert_eq!(
            service.apply_agent_update(&update).await?,
            o3k_store::OperationState::Failed
        );
        assert_eq!(
            service
                .store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ERROR"
        );
        std::fs::remove_file(database_path)?;
        std::fs::remove_dir_all(placement_path)?;
        Ok(())
    }

    /// Builds a service with a real scheduler/placement ledger and a server
    /// create intent whose operation is left in `UnknownOutcome`: the create
    /// dispatch timed out, so the provider operation carries no durable
    /// resource identity and only presence observation can converge it
    /// (issue #481 criterion 3).
    #[allow(clippy::type_complexity)]
    async fn unknown_outcome_create_fixture<P>(
        label: &str,
        provider: Arc<P>,
    ) -> Result<
        (
            ComputeService,
            Arc<dyn ComputeRepository>,
            o3k_placement::PlacementLedger,
            CreateInstanceRequest,
            Uuid,
            String,
        ),
        Box<dyn std::error::Error>,
    >
    where
        P: ComputeProvider + 'static,
    {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-{}.sqlite",
            std::process::id()
        ));
        let placement_path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-placement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement =
            o3k_placement::PlacementLedger::open(&placement_path, placement_repository).await?;
        placement
            .register_provider(
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
            )
            .await?;
        let service = ComputeService::new(store.clone(), provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()));
        let keypair = service
            .create_keypair(
                "user-a",
                "project-a",
                format!("{label}-key"),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test".to_owned(),
            )
            .await?;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: format!("{label}-server"),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: Some(format!("{label}-key")),
            keypair_id: Some(keypair.id),
            network_ids: vec!["port-1".to_owned()],
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("alloc-1".to_owned()),
            config_drive: None,
            idempotency_key: format!("{label}-request"),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .attach_server_keypair(request.o3k_server_id, keypair.id)
            .await?;
        let generation = placement.provider("node-a").await?.generation;
        placement
            .allocate(
                "node-a",
                "alloc-1",
                &request.o3k_server_id.to_string(),
                std::collections::BTreeMap::from([(o3k_placement::VCPU.to_owned(), 1_u64)]),
                generation,
            )
            .await?;
        // The caller keeps the injected timeout active, so the synchronous
        // pass leaves the create operation unknown with no resource identity.
        let reconcile_state = service
            .journal
            .reconcile_once(request.operation_id)
            .await
            .map_err(ComputeError::Reconcile)?;
        assert_eq!(reconcile_state, o3k_store::OperationState::UnknownOutcome);
        let operation = service.store.get_operation(request.operation_id).await?;
        let provider_operation_id = operation
            .provider_operation_id
            .ok_or("create provider operation id is missing")?
            .parse::<Uuid>()?;
        let provider_operation = provider.get_operation(provider_operation_id).await?;
        let instance_id = provider_operation
            .provider_resource_id
            .ok_or("create provider resource id is missing")?;
        Ok((
            service,
            store,
            placement,
            request,
            provider_operation_id,
            instance_id,
        ))
    }

    /// A create left in UnknownOutcome whose presence inspection provably
    /// finds no instance converges to ERROR on the show path with the same
    /// reverse-order compensation as the async agent-failure path.
    #[tokio::test]
    async fn unknown_outcome_create_converges_to_error_and_compensates_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, store, placement, request, provider_operation_id, instance_id) =
            unknown_outcome_create_fixture("presence-absent", fake.clone()).await?;
        fake.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The instance provably does not exist: the create never took effect.
        fake.remove_instance(&instance_id)?;
        fake.set_failure(FailureInjection::None)?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Error);
        let operation = store.get_operation(request.operation_id).await?;
        assert_eq!(operation.state, o3k_store::OperationState::Failed);
        assert_eq!(operation.error_category.as_deref(), Some("not_found"));
        // Reverse-order compensation: keypair detached, placement released.
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            None
        );
        assert!(placement.provider("node-a").await?.allocations.is_empty());
        // A repeated show stays terminal without re-driving the failure.
        let repeated = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(repeated.state, ServerState::Error);
        Ok(())
    }

    /// A create left in UnknownOutcome whose presence inspection finds the
    /// instance converges to ACTIVE on the show path with the provider
    /// resource identity recorded and dependencies retained.
    #[tokio::test]
    async fn unknown_outcome_create_converges_to_active_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, store, placement, request, provider_operation_id, _instance_id) =
            unknown_outcome_create_fixture("presence-present", fake.clone()).await?;
        fake.set_operation_provider_resource_id(provider_operation_id, None)?;
        fake.set_failure(FailureInjection::None)?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Active);
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert!(resource.provider_id.is_some());
        assert_eq!(
            store.get_operation(request.operation_id).await?.state,
            o3k_store::OperationState::Succeeded
        );
        // The converged create keeps its keypair and placement allocation.
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            Some("presence-present-key".to_owned())
        );
        assert!(!placement.provider("node-a").await?.allocations.is_empty());
        Ok(())
    }

    /// A create whose re-drive fails deterministically (not an unknown
    /// outcome) converges to a terminal ERROR on the show path: the drive
    /// projects the failure onto the resource so the poll surface renders
    /// ERROR instead of BUILD forever, and applies the same reverse-order
    /// compensation as the async agent-failure path. The operation is seeded
    /// `Pending` (the crash window between persisting the intent and the
    /// synchronous pass), which is the only stuck state the drive re-drives
    /// besides `UnknownOutcome` — a `Running` operation converges through
    /// the agent event stream and is never re-driven from the poll path.
    #[tokio::test]
    async fn unknown_outcome_create_terminal_failure_projects_error_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, store, placement, request, provider_operation_id, _instance_id) =
            unknown_outcome_create_fixture("presence-terminal", fake.clone()).await?;
        fake.set_operation_provider_resource_id(provider_operation_id, None)?;
        // Every re-drive after the accepted create fails terminally (a
        // deterministic rejection), so the drive's terminal-failure
        // projection and compensation are exercised.
        fake.set_failure(FailureInjection::TerminalOnRedrive)?;
        // The synchronous pass never ran to completion (crash window): the
        // operation is stuck in Pending and the re-drive below fails
        // terminally instead of reporting an unknown outcome.
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Pending,
                None,
                None,
                None,
            )
            .await?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Error);
        let operation = store.get_operation(request.operation_id).await?;
        assert_eq!(operation.state, o3k_store::OperationState::Failed);
        assert_eq!(operation.error_category.as_deref(), Some("terminal"));
        // Reverse-order compensation: keypair detached, placement released.
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            None
        );
        assert!(placement.provider("node-a").await?.allocations.is_empty());
        // A repeated show stays terminal.
        let repeated = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(repeated.state, ServerState::Error);
        Ok(())
    }

    /// The show path drives convergence lazily: repeated polls reuse the
    /// in-flight presence inspection (same deterministic operation identity)
    /// instead of dispatching duplicates, and the terminal agent evidence
    /// converges the next poll without another dispatch.
    #[tokio::test]
    async fn repeated_show_polls_do_not_duplicate_presence_inspection()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, _store, _placement, request, provider_operation_id, instance_id) =
            unknown_outcome_create_fixture("presence-inflight", fake.clone()).await?;
        fake.set_operation_provider_resource_id(provider_operation_id, None)?;
        // The presence inspection dispatches but stays accepted (in-flight).
        fake.set_failure(FailureInjection::InspectAccepted)?;

        // The first poll drives the presence inspection; it stays in-flight.
        let first = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(first.state, ServerState::Requested);
        assert_eq!(fake.inspect_dispatch_count(), 1);
        // A repeated poll must reuse the in-flight inspection, not re-dispatch.
        let second = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(second.state, ServerState::Requested);
        assert_eq!(fake.inspect_dispatch_count(), 1);

        // The agent completes the inspection: terminal update + observation.
        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{}", request.operation_id).as_bytes(),
        );
        let update = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: inspect_operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Succeeded,
            error_category: None,
            redacted_message: None,
            provider_resource_id: Some(instance_id.clone()),
        };
        assert_eq!(
            service.apply_agent_update(&update).await?,
            o3k_store::OperationState::Succeeded
        );
        let observation = AgentObservation {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: Some(instance_id),
            state: o3k_provider::InstanceState::Running,
            operation_id: inspect_operation_id,
            operation_state: AgentOperationState::Succeeded,
            observation_sequence: 2,
            observed_at_unix_ms: 0,
            redacted_message: None,
            console_log_bytes: Vec::new(),
            console_log_offset: 0,
            console_log_complete: false,
            console_log_truncated: false,
            block_device: None,
        };
        service.apply_agent_observation(&observation).await?;
        // The next poll converges to ACTIVE without another dispatch.
        let converged = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(converged.state, ServerState::Active);
        assert_eq!(fake.inspect_dispatch_count(), 1);
        Ok(())
    }

    /// Seeds the issue-87 S1 crash residue: the create intent is durable, the
    /// operation is `Running` with no provider operation identity (the
    /// Pending→Running transition in `reconcile_once` persisted, then o3kd
    /// died before the dispatch reached the provider), and — unless a command
    /// record is inserted by the caller — no agent command row exists.
    #[allow(clippy::type_complexity)]
    async fn crash_before_dispatch_fixture(
        label: &str,
        provider: Arc<FakeComputeProvider>,
    ) -> Result<
        (
            ComputeService,
            Arc<dyn ComputeRepository>,
            CreateInstanceRequest,
        ),
        Box<dyn std::error::Error>,
    > {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&path).await?);
        let service = ComputeService::new(store.clone(), provider);
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: format!("{label}-server"),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: Vec::new(),
            placement_provider_id: Some("node-a".to_owned()),
            placement_allocation_id: Some("alloc-1".to_owned()),
            config_drive: None,
            idempotency_key: format!("{label}-request"),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        // The synchronous pass persisted `Running` and died before the
        // provider returned anything: no provider operation identity, no
        // agent command record (issue-87 S1 residue).
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Running,
                None,
                None,
                None,
            )
            .await?;
        Ok((service, store, request))
    }

    /// The issue-87 S1 residue: a create operation durably `Running` with no
    /// provider operation identity and no agent command record (o3kd died
    /// between the Pending→Running persist in `reconcile_once` and the
    /// dispatch reaching the provider). The provider never accepted a
    /// command, so nothing else can ever advance the operation; the show path
    /// must re-drive it to convergence.
    #[tokio::test]
    async fn running_create_without_provider_operation_is_redriven_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("s1-residue", fake.clone()).await?;
        assert_eq!(fake.instance_count(), 0);

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Active);
        let operation = store.get_operation(request.operation_id).await?;
        assert_eq!(operation.state, o3k_store::OperationState::Succeeded);
        assert!(
            operation.provider_operation_id.is_some(),
            "the re-drive must attach the provider operation identity"
        );
        assert_eq!(
            fake.instance_count(),
            1,
            "the re-drive must actually reach the provider"
        );
        Ok(())
    }

    /// The issue-87 S1 residue with the agent command record persisted before
    /// the crash (the insert-before-send window): the operation is `Running`,
    /// no provider operation identity, and the command row is still
    /// `pending`. Nothing else re-sends a pending row; the show path must
    /// re-drive it exactly like the no-row residue. The agent journal dedups
    /// by command id/operation/idempotency key + fingerprint, so a re-dispatch
    /// of an already-accepted command is safe and never re-executes.
    #[tokio::test]
    async fn running_create_with_pending_agent_command_is_redriven_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("s1-pending-row", fake.clone()).await?;
        store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "node-a".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                payload_fingerprint_sha256: "0".repeat(64),
                payload: Vec::new(),
                state: o3k_store::AgentCommandState::Pending,
                accepted_sequence: 0,
                last_sequence: 0,
                provider_operation_id: None,
                provider_resource_id: None,
            })
            .await?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Active);
        assert_eq!(
            store.get_operation(request.operation_id).await?.state,
            o3k_store::OperationState::Succeeded
        );
        assert_eq!(
            fake.instance_count(),
            1,
            "the re-drive must reach the provider even with a pending row"
        );
        Ok(())
    }

    /// The accepted-command invariant (#542): a `Running` create WITH a
    /// provider operation identity was accepted by the provider, and its
    /// terminal update arrives through the agent event stream. The show path
    /// must NOT re-drive it — a re-dispatch would race the event stream on
    /// the same operation records. The seeded durable shape is exactly the
    /// accepted window, and a poll must leave it untouched: any re-dispatch
    /// would have converged the create to Succeeded/ACTIVE on the fresh fake.
    #[tokio::test]
    async fn running_create_with_provider_operation_is_not_redriven_on_show()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("s1-accepted", fake.clone()).await?;
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Running,
                Some(&request.operation_id.to_string()),
                None,
                None,
            )
            .await?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Requested);
        let operation = store.get_operation(request.operation_id).await?;
        assert_eq!(operation.state, o3k_store::OperationState::Running);
        assert_eq!(
            operation.provider_operation_id,
            Some(request.operation_id.to_string())
        );
        assert_eq!(
            fake.instance_count(),
            0,
            "an accepted create must never be re-dispatched"
        );
        Ok(())
    }

    /// The periodic create-convergence sweep must recover the issue-87 S1
    /// residue after a control-plane restart WITHOUT any API call: the lazy
    /// show path alone would leave the server stuck in REQUESTED until a
    /// client polls it.
    #[tokio::test]
    async fn create_convergence_sweep_recovers_crash_before_dispatch_without_api_call()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("s1-sweep", fake.clone()).await?;
        let task = service.spawn_create_convergence_reconciler(1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let operation = store.get_operation(request.operation_id).await?;
            if operation.state == o3k_store::OperationState::Succeeded {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "create convergence sweep did not converge the S1 residue"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(fake.instance_count(), 1);
        assert_eq!(
            store
                .get_resource(request.o3k_server_id)
                .await?
                .observed_state,
            "ACTIVE"
        );
        task.abort();
        let _ = task.await;
        Ok(())
    }

    /// Project isolation is preserved on the lazy convergence path: a foreign
    /// project cannot trigger the presence inspection dispatch.
    #[tokio::test]
    async fn foreign_project_show_cannot_trigger_presence_dispatch()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, _store, _placement, request, provider_operation_id, _instance_id) =
            unknown_outcome_create_fixture("presence-isolation", fake.clone()).await?;
        fake.set_operation_provider_resource_id(provider_operation_id, None)?;
        fake.set_failure(FailureInjection::None)?;

        assert!(matches!(
            service
                .show_server("project-b", ServerId::from_uuid(request.o3k_server_id))
                .await,
            Err(ComputeError::NotFound)
        ));
        assert_eq!(
            fake.inspect_dispatch_count(),
            0,
            "foreign project show must not dispatch a provider mutation"
        );
        // The owning project still converges normally.
        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Active);
        assert_eq!(fake.inspect_dispatch_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_agent_updates_project_create_binding_outcomes()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-binding-projection-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let projector = Arc::new(RecordingProjector::default());
        let service = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()))
            .with_binding_projector(projector.clone());
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "binding-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-1".to_owned(), "port-2".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "binding-projection".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        let failed = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: request.operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Terminal),
            redacted_message: None,
            provider_resource_id: None,
        };
        assert_eq!(
            service.apply_agent_update(&failed).await?,
            o3k_store::OperationState::Failed
        );
        assert_eq!(
            projector_calls(&projector),
            vec![
                ProjectorCall::CreateOutcome {
                    project: "project-a".to_owned(),
                    port: "port-1".to_owned(),
                    succeeded: false,
                },
                ProjectorCall::CreateOutcome {
                    project: "project-a".to_owned(),
                    port: "port-2".to_owned(),
                    succeeded: false,
                },
            ]
        );
        // A replayed terminal update projects the same outcome again; the
        // projection is an idempotent side observation.
        service.apply_agent_update(&failed).await?;
        assert_eq!(projector_calls(&projector).len(), 4);

        // Terminal states are sticky in the journal: a later succeeded
        // delivery of the same operation returns the terminal failed state
        // and projects the same error outcome.
        let succeeded = AgentOperationUpdate {
            operation_sequence: 2,
            state: AgentOperationState::Succeeded,
            ..failed.clone()
        };
        assert_eq!(
            service.apply_agent_update(&succeeded).await?,
            o3k_store::OperationState::Failed
        );
        let calls = projector_calls(&projector);
        assert_eq!(calls.len(), 6);
        assert!(calls[4..].iter().all(|call| matches!(
            call,
            ProjectorCall::CreateOutcome {
                succeeded: false,
                ..
            }
        )));
        std::fs::remove_file(database_path)?;
        Ok(())
    }

    #[tokio::test]
    async fn create_and_delete_project_binding_outcomes_through_fake_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-binding-lifecycle-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let projector = Arc::new(RecordingProjector::default());
        let service = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()))
            .with_binding_projector(projector.clone());
        let flavor = service
            .create_flavor("project-a", "tiny".to_owned(), 1, 512, 1)
            .await?;
        let server = service
            .create_server(
                "project-a",
                "bound-server".to_owned(),
                "image-1".to_owned(),
                flavor.id,
                vec!["port-1".to_owned()],
                "binding-lifecycle".to_owned(),
            )
            .await?;
        // The fake provider completes create synchronously; the reconcile
        // path projects the terminal outcome.
        assert_eq!(
            projector_calls(&projector),
            vec![ProjectorCall::CreateOutcome {
                project: "project-a".to_owned(),
                port: "port-1".to_owned(),
                succeeded: true,
            }]
        );
        service.delete_server("project-a", server.id).await?;
        assert!(
            projector_calls(&projector).contains(&ProjectorCall::Unbind {
                project: "project-a".to_owned(),
                port: "port-1".to_owned(),
            })
        );
        // Deleting again takes the already-deleted shortcut and unbinds
        // idempotently.
        service.delete_server("project-a", server.id).await?;
        assert_eq!(
            projector_calls(&projector)
                .iter()
                .filter(|call| matches!(call, ProjectorCall::Unbind { .. }))
                .count(),
            2
        );
        std::fs::remove_file(database_path)?;
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
        let observation = AgentObservation {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: request.o3k_server_id,
            provider_resource_id: Some("agent-domain-stopped".to_owned()),
            state: o3k_provider::InstanceState::Stopped,
            operation_id: request.operation_id,
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
        service.apply_agent_observation(&observation).await?;
        assert_eq!(
            service
                .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
                .await?
                .state,
            ServerState::Stopped
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduler_binding_is_persisted_idempotently_and_released_on_delete()
    -> Result<(), ComputeError> {
        let placement_root =
            PathBuf::from(format!("/tmp/o3k-placement-compute-{}", Uuid::now_v7()));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
            .await
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
        let resource = service.store.get_resource(server.id.as_uuid()).await?;
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        assert_eq!(intent.placement_provider_id.as_deref(), Some("node-a"));
        assert_eq!(server.host.as_deref(), Some("node-a"));
        assert_eq!(
            intent.placement_allocation_id.as_deref(),
            Some(format!("allocation-{}", server.id).as_str())
        );
        assert_eq!(
            placement
                .provider("node-a")
                .await
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
                .await
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
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
            .await
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
            .await
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
                    .await
                    .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                    .allocations
                    .len(),
                1
            );
            assert_eq!(
                placement
                    .provider("node-a")
                    .await
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
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement
            .register_provider("node-b", inventory(3))
            .await
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
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
            .into_iter()
            .map(|provider| provider.allocations.len())
            .sum::<usize>();
        assert_eq!(allocation_count, 1);
        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_scheduler_attempts_do_not_over_allocate() -> Result<(), ComputeError> {
        // Two independent compute services over two store instances on one
        // file database: provider capacity and allocations are shared through
        // the repository, so the atomic allocation commit gates both create
        // paths and exactly one may acquire the VCPU capacity. The losing
        // service surfaces the scheduler's NoValidHost mapping to the API.
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-concurrent-{}.sqlite", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let placement_root =
            std::env::temp_dir().join(format!("o3k-compute-concurrent-pl-{}", Uuid::now_v7()));
        let raw_store_a = o3k_store::testkit::open_file(&database_path).await?;
        let raw_store_b = o3k_store::testkit::open_file(&database_path).await?;
        let store_a: Arc<dyn ComputeRepository> = Arc::new(raw_store_a.clone());
        let store_b: Arc<dyn ComputeRepository> = Arc::new(raw_store_b.clone());
        let repository_a: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store_a);
        let repository_b: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store_b);
        let placement_a = o3k_placement::PlacementLedger::open(&placement_root, repository_a)
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let placement_b = o3k_placement::PlacementLedger::open(&placement_root, repository_b)
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        placement_a
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
                            total: 2048,
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
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        let service_a = ComputeService::new(store_a, Arc::new(FakeComputeProvider::new()))
            .with_scheduler(Scheduler::new(placement_a.clone()));
        let service_b = ComputeService::new(store_b, Arc::new(FakeComputeProvider::new()))
            .with_scheduler(Scheduler::new(placement_b));
        let flavor = service_a.flavors()[1].id;
        let left = service_a.create_server(
            "project-a",
            "concurrent-a".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            "concurrent-a-request".to_owned(),
        );
        let right = service_b.create_server(
            "project-a",
            "concurrent-b".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            "concurrent-b-request".to_owned(),
        );
        let (left, right) = tokio::join!(left, right);
        let mut created = 0;
        let mut rejected = 0;
        for result in [left, right] {
            match result {
                Ok(_) => created += 1,
                Err(ComputeError::Scheduler(SchedulerError::NoValidHost)) => rejected += 1,
                Err(error) => return Err(error),
            }
        }
        assert_eq!(created, 1);
        assert_eq!(rejected, 1);
        // The final state is deterministic: exactly one durable allocation
        // and the reported VCPU usage reflects it on both stores.
        let provider = placement_a
            .provider("node-a")
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        assert_eq!(provider.allocations.len(), 1);
        assert_eq!(provider.inventories[o3k_placement::VCPU].used, 2);
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn existing_resource_conflict_does_not_acquire_placement_allocation()
    -> Result<(), ComputeError> {
        let placement_root = PathBuf::from(format!(
            "/tmp/o3k-placement-existing-resource-{}",
            Uuid::now_v7()
        ));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
            .await
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
                .await
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

    /// Test-only placement repository that fails a bounded number of
    /// `release_allocation` calls at the repository port. The legacy
    /// file-backed ledger test broke persistence by placing a directory at
    /// the `placement.json` path; with the repository-backed ledger the
    /// equivalent injection point is the port boundary itself. Every other
    /// method delegates to the in-memory adapter.
    struct FailingReleaseRepository {
        inner: o3k_store::testkit::TestStore,
        fail_releases: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl o3k_store::PlacementRepository for FailingReleaseRepository {
        async fn get_provider(
            &self,
            provider_id: &str,
        ) -> Result<Option<o3k_store::PlacementProviderRecord>, o3k_store::StoreError> {
            self.inner.get_provider(provider_id).await
        }
        async fn list_providers(
            &self,
        ) -> Result<Vec<o3k_store::PlacementProviderRecord>, o3k_store::StoreError> {
            self.inner.list_providers().await
        }
        async fn register_provider(
            &self,
            node_id: &str,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner.register_provider(node_id, inventories).await
        }
        async fn sync_provider(
            &self,
            node_id: &str,
            state: &str,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner.sync_provider(node_id, state, inventories).await
        }
        async fn refresh_inventories(
            &self,
            provider_id: &str,
            expected_generation: u64,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner
                .refresh_inventories(provider_id, expected_generation, inventories)
                .await
        }
        async fn set_provider_state(
            &self,
            provider_id: &str,
            state: &str,
        ) -> Result<(), o3k_store::StoreError> {
            self.inner.set_provider_state(provider_id, state).await
        }
        async fn commit_allocation(
            &self,
            provider_id: &str,
            expected_generation: u64,
            allocation: &o3k_store::PlacementAllocationRecord,
        ) -> Result<o3k_store::PlacementAllocationRecord, o3k_store::StoreError> {
            self.inner
                .commit_allocation(provider_id, expected_generation, allocation)
                .await
        }
        async fn release_allocation(
            &self,
            provider_id: &str,
            allocation_id: &str,
        ) -> Result<(), o3k_store::StoreError> {
            if self
                .fail_releases
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                return Err(o3k_store::StoreError::ResourceNotFound);
            }
            self.inner
                .release_allocation(provider_id, allocation_id)
                .await
        }
        async fn upsert_intent(
            &self,
            intent: &o3k_store::PlacementIntentRecord,
        ) -> Result<o3k_store::PlacementIntentRecord, o3k_store::StoreError> {
            self.inner.upsert_intent(intent).await
        }
        async fn get_intent(
            &self,
            allocation_id: &str,
        ) -> Result<Option<o3k_store::PlacementIntentRecord>, o3k_store::StoreError> {
            self.inner.get_intent(allocation_id).await
        }
        async fn list_intents(
            &self,
        ) -> Result<Vec<o3k_store::PlacementIntentRecord>, o3k_store::StoreError> {
            self.inner.list_intents().await
        }
        async fn delete_intent(&self, allocation_id: &str) -> Result<(), o3k_store::StoreError> {
            self.inner.delete_intent(allocation_id).await
        }
        async fn reconcile_consumers(
            &self,
            durable_consumer_ids: &[String],
        ) -> Result<o3k_store::PlacementReconcileRecord, o3k_store::StoreError> {
            self.inner.reconcile_consumers(durable_consumer_ids).await
        }
        async fn import_provider(
            &self,
            provider: &o3k_store::PlacementProviderRecord,
        ) -> Result<(), o3k_store::StoreError> {
            self.inner.import_provider(provider).await
        }
    }

    #[tokio::test]
    async fn deleted_server_retries_failed_placement_release() -> Result<(), ComputeError> {
        let placement_root = PathBuf::from(format!(
            "/tmp/o3k-placement-delete-release-{}",
            Uuid::now_v7()
        ));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let fail_releases = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(FailingReleaseRepository {
                inner: placement_store,
                fail_releases: fail_releases.clone(),
            });
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
            .await
            .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        // The compute service is constructed once; the scheduler reaches the
        // repository through the failing wrapper, so the release failure is
        // injected before the first delete and the retry succeeds.
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

        // The first delete reaches terminal server deletion but the placement
        // release fails once at the repository boundary (the repository-backed
        // mapping of the legacy journal write failure). The allocation is
        // retained so a retry can release it.
        assert!(matches!(
            service.delete_server("project-a", server.id).await,
            Err(ComputeError::Scheduler(SchedulerError::Placement(
                o3k_placement::PlacementError::Store(_)
            )))
        ));
        assert_eq!(
            placement
                .provider("node-a")
                .await
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            1
        );
        assert_eq!(
            service
                .store
                .get_resource(server.id.as_uuid())
                .await?
                .observed_state,
            "DELETED"
        );

        // The retried delete takes the already-deleted shortcut and releases
        // the retained allocation.
        service.delete_server("project-a", server.id).await?;
        assert_eq!(
            placement
                .provider("node-a")
                .await
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?
                .allocations
                .len(),
            0
        );

        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn registry_gate_excludes_unavailable_draining_and_disabled_agents()
    -> Result<(), ComputeError> {
        let placement_root =
            PathBuf::from(format!("/tmp/o3k-placement-registry-{}", Uuid::now_v7()));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&placement_root, placement_repository)
            .await
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
                .await
                .map_err(|error| ComputeError::Scheduler(SchedulerError::Placement(error)))?;
        }

        let registry = FakeAgentRegistry::default();
        registry
            .upsert(agent_node_with_state(
                "unavailable",
                AgentAdministrativeState::Enabled,
                8,
                8192,
                100,
            ))
            .await;
        registry.set_unavailable("unavailable").await;
        registry
            .upsert(agent_node_with_state(
                "draining",
                AgentAdministrativeState::Draining,
                8,
                8192,
                100,
            ))
            .await;
        registry
            .upsert(agent_node_with_state(
                "disabled",
                AgentAdministrativeState::Disabled,
                8,
                8192,
                100,
            ))
            .await;
        registry
            .upsert(agent_node_with_state(
                "enabled",
                AgentAdministrativeState::Enabled,
                8,
                8192,
                100,
            ))
            .await;

        let service = service("registry-gate")
            .await?
            .with_scheduler(Scheduler::new(placement.clone()))
            .with_agent_registry(Arc::new(registry));
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
        let resource = service.store.get_resource(server.id.as_uuid()).await?;
        let request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        assert_eq!(request.placement_provider_id.as_deref(), Some("enabled"));

        let _ = std::fs::remove_dir_all(placement_root);
        Ok(())
    }

    #[tokio::test]
    async fn store_not_found_does_not_dispatch_provider_mutation() -> Result<(), ComputeError> {
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-notfound-{}.sqlite", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let service = ComputeService::new(store, provider.clone());

        let non_existent_id = Uuid::now_v7();
        assert!(matches!(
            service
                .delete_server("project-a", ServerId::from_uuid(non_existent_id))
                .await,
            Err(ComputeError::NotFound)
        ));
        assert!(matches!(
            service
                .inspect_server("project-a", ServerId::from_uuid(non_existent_id), "key-1")
                .await,
            Err(ComputeError::NotFound)
        ));
        assert_eq!(provider.instance_count(), 0);

        let _ = std::fs::remove_file(&database_path);
        Ok(())
    }

    /// Regression test: the inspect probe must use the durable project *ID* "eba29e2d-53de-461d-ae91-ede7402713cb",
    /// not the project *name* "admin" from the CLI/token context.
    ///
    /// The bootstrap token encodes:
    ///   "project": "admin"                  ← project name (display name only)
    ///   "project_id": "eba29e2d-53de-461d-ae91-ede7402713cb"   ← durable ID used by compute service
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
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement =
            o3k_placement::PlacementLedger::open(&placement_path, placement_repository).await?;
        placement
            .register_provider(
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
            )
            .await?;
        let service = ComputeService::new(store, provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()));

        // Create a server under the durable project ID used by TestLab.
        let server = service
            .create_server(
                "eba29e2d-53de-461d-ae91-ede7402713cb",
                "testlab-server".to_owned(),
                "cirros-image".to_owned(),
                Uuid::from_u128(1),
                vec!["net-1".to_owned()],
                "testlab-create-key".to_owned(),
            )
            .await?;

        // Passing the correct project ID ("eba29e2d-53de-461d-ae91-ede7402713cb") succeeds.
        let result = service
            .inspect_server(
                "eba29e2d-53de-461d-ae91-ede7402713cb",
                server.id,
                "testlab-inspect-key",
            )
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
    /// "eba29e2d-53de-461d-ae91-ede7402713cb" is invisible to a caller using a different project ID.
    #[tokio::test]
    async fn inspect_probe_project_isolation_rejects_foreign_project()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path =
            std::env::temp_dir().join(format!("o3k-compute-isolation-{}.sqlite", Uuid::now_v7()));
        let placement_path =
            std::env::temp_dir().join(format!("o3k-compute-isolation-pl-{}", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement =
            o3k_placement::PlacementLedger::open(&placement_path, placement_repository).await?;
        placement
            .register_provider(
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
            )
            .await?;
        let service = ComputeService::new(store, provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()));

        let server = service
            .create_server(
                "eba29e2d-53de-461d-ae91-ede7402713cb",
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
            .inspect_server(
                "eba29e2d-53de-461d-ae91-ede7402713cb",
                server.id,
                "isolation-owner-key",
            )
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

    #[tokio::test]
    async fn attachment_reconciler_starts_and_shuts_down_cleanly()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = service("reconcile").await?;
        let task = service.spawn_attachment_reconciler(1);
        // The reconciler is a bounded background task that periodically calls
        // AttachmentOrchestrator::reconcile. Prove it starts and can be aborted
        // cleanly; the convergence behavior itself is covered by the
        // attachment.rs restart tests.
        task.abort();
        let _ = task.await;
        Ok(())
    }
}
