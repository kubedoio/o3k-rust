//! Compute application service: server CRUD, keypairs, flavors,
//! convergence sweeps, agent event dispatch, binding projection.
//!
//! ## Responsibility
//!
//! `o3k-compute` implements the compute-domain use cases. It drives the
//! reconciler's durable state machine from the application layer and
//! handles compute-specific post-processing (port binding projection,
//! config-drive cleanup, failed-create compensation, inventory sync).
//!
//! ## Boundary
//!
//! The durable reconciliation state machine (operation journal, evidence
//! fencing, retry budget) lives in `o3k-reconciler`. `o3k-compute` calls
//! into it; it does not duplicate journaling or state-machine logic.
//!
//! ## Sub-modules
//!
//! - `types` — compute-domain types (flavors, keypairs, errors)
//! - `attachment` — Cinder volume attachment orchestration
//!
//! See also: `o3k-reconciler` for the reconciliation state machine.

use std::sync::Arc;

use async_trait::async_trait;
pub use o3k_domain::{Server, ServerId, ServerState};
#[cfg(test)]
use o3k_kernel::LimitValue;
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditSink, AuthContext, AuthorizationRequest, Authorizer,
    LimitKey, NoopAuditSink, OwnershipScope, ResourceAmount, ResourceId, ResourceTarget,
    ResourceType, ScopeId, ServiceNamespace, StaticAuthorizer,
};
#[cfg(test)]
use o3k_provider::FakeComputeProvider;
use o3k_provider::{
    AgentAdministrativeState, AgentAvailability, AgentCapabilities, AgentNodeRegistry,
    AgentNodeSnapshot, BlockDeviceAttachment, BlockDeviceObservation, Capabilities,
    ComputeProvider, ConnectorInfo, CreateInstanceRequest, DeleteInstanceRequest, Instance,
    InstanceAction, Operation, ProviderError, VolumeAttachmentProvider,
};
use o3k_reconciler::{LifecycleAction, OperationJournal, ReconcileError};
use o3k_scheduler::{Flavor as SchedulerFlavor, Scheduler, SchedulerError};
use o3k_store::{
    ComputeRepository, StoreError, VolumeAttachmentRecord, server_state_from_storage,
    server_state_to_storage,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub mod attachment;

pub use attachment::AttachmentOrchestrator;

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

pub mod types;
pub use types::*;

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
    config_drive_cleaner: Option<o3k_config_drive::ConfigDriveStore>,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
    coordination: Option<(
        Arc<dyn o3k_store::CoordinationRepository>,
        o3k_store::ControllerId,
        o3k_store::ControllerEpoch,
    )>,
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
pub mod inventory;
pub use inventory::{agent_inventory, spawn_agent_inventory_publisher, sync_agent_inventory};

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

impl ProviderBackend {
    pub async fn attach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        self.0.attach_block_device(resource_id, device).await
    }

    pub async fn detach_block_device(
        &self,
        resource_id: Uuid,
        device: &BlockDeviceAttachment,
    ) -> Result<BlockDeviceObservation, ProviderError> {
        self.0.detach_block_device(resource_id, device).await
    }

    pub async fn observe_block_device(
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
            config_drive_cleaner: None,
            authorizer: Arc::new(StaticAuthorizer::standard()),
            audit_sink: Arc::new(NoopAuditSink),
            coordination: None,
        }
    }

    #[must_use]
    pub fn with_coordination(
        mut self,
        coordination: Arc<dyn o3k_store::CoordinationRepository>,
        controller_id: o3k_store::ControllerId,
        controller_epoch: o3k_store::ControllerEpoch,
    ) -> Self {
        self.coordination = Some((coordination, controller_id, controller_epoch));
        self
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_audit_sink(mut self, audit_sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = audit_sink;
        self
    }

    /// Configures the control-plane config-drive store whose per-instance
    /// media is reaped when a server delete reaches terminal success.
    /// Reaping is best-effort and idempotent; without this builder the
    /// cleanup is a no-op, so tests and hosts that do not own a config-drive
    /// store are unchanged.
    #[must_use]
    pub fn with_config_drive_cleaner(mut self, store: o3k_config_drive::ConfigDriveStore) -> Self {
        self.config_drive_cleaner = Some(store);
        self
    }

    /// Best-effort removal of the per-instance config-drive media owned by
    /// this control plane once a server delete is known terminal. A failed
    /// cleanup is logged, never a compute failure: the leak verifier catches
    /// residue separately, and the cleanup is idempotent so a replayed
    /// terminal update reaps nothing more than the first one.
    fn cleanup_config_drive_best_effort(&self, server_id: &str) {
        let Some(store) = self.config_drive_cleaner.as_ref() else {
            return;
        };
        if let Err(error) = store.cleanup(server_id) {
            tracing::warn!(
                server_id = %server_id,
                error = %error,
                "config-drive cleanup failed; the delete outcome is unaffected"
            );
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
    /// The same registry backs the journal's evidence fence: without it the
    /// fence stays anchored to each operation's first evidence epoch (issue
    /// #87 crash-restart replay needs the registry's current epoch to accept
    /// a re-registered agent's replay while rejecting dead epochs).
    #[must_use]
    pub fn with_agent_registry(mut self, registry: Arc<dyn AgentNodeRegistry>) -> Self {
        self.journal = self.journal.clone().with_agent_registry(registry.clone());
        self.agent_registry = Some(registry);
        self
    }

    #[must_use]
    pub fn provider(&self) -> Arc<ProviderBackend> {
        self.provider.clone()
    }

    /// Reports whether the explicitly configured external-Cinder attachment
    /// provider enables the hosted attachment API profile.
    #[must_use]
    pub fn cinder_configured(&self) -> bool {
        self.attachments.cinder_configured()
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
    /// state of the network control plane, and reaps the per-instance
    /// config-drive media when a delete reached terminal success. The
    /// server's ports are read from the durable desired-state snapshot, and
    /// the binding host comes from the intent the network service recorded at
    /// dispatch. Projection and reaping are best-effort and idempotent: they
    /// are side observations, never compute failures, and a replayed terminal
    /// update projects and reaps the same state again. Integrity anomalies (a
    /// missing operation or resource, or an unparseable desired-state
    /// snapshot) are surfaced as warnings instead of failing the compute path.
    async fn project_terminal_binding_outcome(
        &self,
        operation_id: &str,
        state: o3k_store::OperationState,
    ) {
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
        // Terminal successful delete reaps the per-instance config-drive
        // media owned by this control plane (best-effort, idempotent, and
        // independent of the binding projector).
        if operation.kind == "lifecycle:delete" && state == o3k_store::OperationState::Succeeded {
            self.cleanup_config_drive_best_effort(&operation.resource_id.to_string());
        }
        let Some(projector) = self.binding_projector.as_ref() else {
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
                if let Some((coordination, controller_id, controller_epoch)) = &service.coordination
                {
                    let work_key = "reconcile:volume_attachments";
                    match coordination
                        .acquire_work_lease(
                            work_key,
                            "reconciler",
                            controller_id,
                            controller_epoch,
                            Duration::from_secs(15),
                        )
                        .await
                    {
                        Ok(o3k_store::LeaseAcquireOutcome::Acquired { lease }) => {
                            if let Err(error) = service.attachment_orchestrator().reconcile().await
                            {
                                tracing::warn!(%error, "attachment reconcile pass failed");
                            }
                            let _ = coordination
                                .release_work_lease(
                                    work_key,
                                    controller_id,
                                    controller_epoch,
                                    lease.fencing_token,
                                )
                                .await;
                        }
                        Ok(o3k_store::LeaseAcquireOutcome::Busy { .. }) => {
                            tracing::debug!(
                                "attachment reconcile is currently leased by another controller; skipping"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to acquire attachment reconcile lease");
                        }
                    }
                } else if let Err(error) = service.attachment_orchestrator().reconcile().await {
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
                tracing::debug!("create convergence sweep tick");
                if let Err(error) = service.drive_all_create_convergence().await {
                    tracing::warn!(%error, "create convergence reconcile pass failed");
                }
            }
        })
    }

    /// Drives create convergence for every durable compute instance, then
    /// expires artifact transfers abandoned by operations that have already
    /// reached a terminal state (issue #88). The per-resource drive is lazy
    /// and bounded, so healthy servers are skipped and a stuck server
    /// converges regardless of which project owns it.
    async fn drive_all_create_convergence(&self) -> Result<(), ComputeError> {
        let resources = self
            .store
            .list_resources_by_kind("compute_instance")
            .await?;
        for resource in resources {
            if let Some((coordination, controller_id, controller_epoch)) = &self.coordination {
                let work_key = format!("convergence:create:{}", resource.id);
                match coordination
                    .acquire_work_lease(
                        &work_key,
                        "convergence",
                        controller_id,
                        controller_epoch,
                        Duration::from_secs(15),
                    )
                    .await
                {
                    Ok(o3k_store::LeaseAcquireOutcome::Acquired { lease }) => {
                        self.drive_create_convergence(&resource).await;
                        let _ = coordination
                            .release_work_lease(
                                &work_key,
                                controller_id,
                                controller_epoch,
                                lease.fencing_token,
                            )
                            .await;
                    }
                    Ok(o3k_store::LeaseAcquireOutcome::Busy { .. }) => {
                        tracing::debug!(
                            resource_id = %resource.id,
                            "create convergence is currently leased by another controller; skipping"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            resource_id = %resource.id,
                            %error,
                            "failed to acquire create convergence lease; skipping"
                        );
                    }
                }
            } else {
                self.drive_create_convergence(&resource).await;
            }
        }
        // Issue #88: an operation can reach a terminal state while its
        // artifact handshake rows are still `offered`/`receiving` (an agent
        // crash can strand them, and a terminalized operation is never driven
        // again), so no per-operation path ever advances them. This per-pass
        // sweep expires exactly those rows. Best-effort and idempotent:
        // repeated passes expire nothing, committed/rejected/expired rows are
        // never touched, and a failure is a warning, not a sweep abort.
        if let Err(error) = self.store.expire_transfers_of_terminal_operations().await {
            tracing::warn!(%error, "artifact transfer expiry sweep failed");
        }
        Ok(())
    }

    /// Periodically drives lifecycle convergence for operations left in a
    /// state that nothing else will ever advance. A lifecycle operation can
    /// be stranded non-terminal by an unknown delete/action outcome
    /// (issue-88 B1: the delete undefine raced a libvirtd restart, the agent
    /// reported unknown, the API's synchronous 10s poll has long returned,
    /// and the event stream rejects non-Succeeded observations), and no path
    /// ever calls `reconcile_lifecycle_once` again — the resource stays
    /// ACTIVE, the API delete retry 409s, and every owned residue (op row,
    /// command row, allocation, config-drive media) is held. This bounded
    /// periodic task is the recovery authority, mirroring the
    /// create-convergence sweep. Each pass is lazy and idempotent: terminal
    /// operations are not listed, in-flight operations are skipped, and
    /// re-dispatches reuse the durable command row and the deterministic
    /// `o3k-operation-{id}` idempotency key.
    pub fn spawn_lifecycle_convergence_reconciler(
        &self,
        interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = service.drive_all_lifecycle_convergence().await {
                    tracing::warn!(%error, "lifecycle convergence reconcile pass failed");
                }
            }
        })
    }

    /// Drives lifecycle convergence for every non-terminal lifecycle
    /// operation. The per-operation drive is lazy and bounded, so healthy
    /// operations are skipped and a stranded operation converges regardless
    /// of which project owns it.
    async fn drive_all_lifecycle_convergence(&self) -> Result<(), ComputeError> {
        let operations = self.store.list_non_terminal_lifecycle_operations().await?;
        for operation in operations {
            // Issue #88 B1: re-drive exactly the states nothing else will
            // ever advance — `Pending`, `UnknownOutcome` (observed, not
            // re-dispatched: presence inspection and adoption decide),
            // `Retryable` (the #572 retry-scheduling addition), and
            // `Running` without a provider operation identity (a crash
            // between persisting `Running` and dispatching). A `Running`
            // operation WITH the identity was accepted and is in flight: the
            // agent event stream terminalizes it, and a re-dispatch would
            // race it on the same operation records.
            let re_drive = matches!(
                operation.state,
                o3k_store::OperationState::Pending
                    | o3k_store::OperationState::UnknownOutcome
                    | o3k_store::OperationState::Retryable
            ) || (operation.state == o3k_store::OperationState::Running
                && operation.provider_operation_id.is_none());
            if !re_drive {
                continue;
            }
            if let Some((coordination, controller_id, controller_epoch)) = &self.coordination {
                let work_key = format!("operation:{}", operation.id);
                match coordination
                    .acquire_work_lease(
                        &work_key,
                        "operation",
                        controller_id,
                        controller_epoch,
                        Duration::from_secs(15),
                    )
                    .await
                {
                    Ok(o3k_store::LeaseAcquireOutcome::Acquired { lease }) => {
                        if let Err(error) =
                            self.journal.reconcile_lifecycle_once(operation.id).await
                        {
                            tracing::warn!(
                                operation_id = %operation.id,
                                resource_id = %operation.resource_id,
                                error = %error,
                                "server lifecycle convergence pass failed; server state is unchanged"
                            );
                        }
                        let _ = coordination
                            .release_work_lease(
                                &work_key,
                                controller_id,
                                controller_epoch,
                                lease.fencing_token,
                            )
                            .await;
                    }
                    Ok(o3k_store::LeaseAcquireOutcome::Busy { .. }) => {
                        tracing::debug!(
                            operation_id = %operation.id,
                            "lifecycle operation is currently leased by another controller; skipping"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            operation_id = %operation.id,
                            %error,
                            "failed to acquire lifecycle operation lease; skipping"
                        );
                    }
                }
            } else if let Err(error) = self.journal.reconcile_lifecycle_once(operation.id).await {
                tracing::warn!(
                    operation_id = %operation.id,
                    resource_id = %operation.resource_id,
                    error = %error,
                    "server lifecycle convergence pass failed; server state is unchanged"
                );
            }
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

    pub async fn flavors_for_auth(&self, auth: &AuthContext) -> Result<Vec<Flavor>, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ListFlavors").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ListFlavors".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "flavor").map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        self.flavors_for_project(auth.effective_scope().id().as_str())
            .await
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

    pub async fn create_flavor_for_auth(
        &self,
        auth: &AuthContext,
        name: String,
        vcpus: u32,
        ram_mib: u64,
        disk_gib: u64,
    ) -> Result<Flavor, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "CreateFlavor").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "CreateFlavor".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "flavor").map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        match self
            .create_flavor(
                auth.effective_scope().id().as_str(),
                name,
                vcpus,
                ram_mib,
                disk_gib,
            )
            .await
        {
            Ok(flavor) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "flavor").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "flavor".to_owned())
                        }),
                        ResourceId::new(flavor.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(flavor)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
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

    pub async fn flavor_for_auth(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<Flavor, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ReadFlavor").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ReadFlavor".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "flavor").map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.flavor_for_project(auth.effective_scope().id().as_str(), id)
            .await
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

    pub async fn delete_flavor_for_auth(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "DeleteFlavor").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "DeleteFlavor".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "flavor").map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .delete_flavor(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "flavor").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "flavor".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
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

    pub async fn create_server_for_auth(
        &self,
        auth: &AuthContext,
        input: ServerCreateInput,
    ) -> Result<Server, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "CreateServer").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "CreateServer".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        match self.create_server_for_user(input).await {
            Ok(server) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "server").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "server".to_owned())
                        }),
                        ResourceId::new(server.id.as_uuid().to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(server)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_server_for_auth_canonical(
        &self,
        auth: &AuthContext,
        input: ServerCreateInput,
        context: o3k_reconciler::CanonicalMutationContext,
    ) -> Result<MutationReceipt<Server>, ComputeError> {
        // CANONICAL INVARIANT: the canonical context's action must match the
        // expected mutation (InvalidRequest — a structural wiring error),
        // while the actor and owner_scope must match the authenticated request
        // (Unauthorized — an authorization failure).
        if context.action.to_string() != "compute:CreateServer" {
            return Err(ComputeError::InvalidRequest);
        }
        if context.actor != auth.principal().id().as_str()
            || context.owner_scope.id().as_str() != auth.effective_scope().id().as_str()
        {
            return Err(ComputeError::Unauthorized);
        }
        let action = context.action.clone();
        let target = ResourceTarget::collection(
            ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
            Some(auth.effective_scope().id().clone()),
        );
        if !self
            .authorizer
            .authorize(&AuthorizationRequest {
                auth_context: auth,
                action: action.clone(),
                resource_target: target,
            })
            .is_allowed()
        {
            return Err(ComputeError::Unauthorized);
        }
        let accepted = self
            .create_server_for_user_with_context(input, Some(&context))
            .await?;
        Ok(MutationReceipt {
            resource: accepted.server,
            operation_id: accepted.operation_id,
            operation_state: accepted.operation_state,
            replayed: accepted.replayed,
        })
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
        Ok(self
            .create_server_for_user_with_context(input, None)
            .await?
            .server)
    }

    /// Returns the deterministic canonical identity used by the Nova create
    /// adapter before the durable server intent exists.
    pub fn server_id_for_create(project_id: &str, idempotency_key: &str) -> Uuid {
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:server:{project_id}:{idempotency_key}").as_bytes(),
        )
    }

    async fn create_server_for_user_with_context(
        &self,
        input: ServerCreateInput,
        canonical: Option<&o3k_reconciler::CanonicalMutationContext>,
    ) -> Result<CreateMutationReceipt, ComputeError> {
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
        let server_id = Self::server_id_for_create(&project_id, &idempotency_key);
        let existing_res = self.store.get_resource(server_id).await;
        let operation_id = match &existing_res {
            Ok(existing) => {
                let observed = server_state_from_storage(&existing.observed_state).ok();
                if observed == Some(ServerState::Deleted) {
                    Uuid::new_v5(
                        &Uuid::NAMESPACE_URL,
                        format!(
                            "o3k:operation:revive:{project_id}:{idempotency_key}:{}",
                            existing.observed_generation
                        )
                        .as_bytes(),
                    )
                } else if let Ok(req) =
                    serde_json::from_str::<CreateInstanceRequest>(&existing.desired_state)
                {
                    req.operation_id
                } else {
                    Uuid::new_v5(
                        &Uuid::NAMESPACE_URL,
                        format!("o3k:operation:{project_id}:{idempotency_key}").as_bytes(),
                    )
                }
            }
            _ => Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:operation:{project_id}:{idempotency_key}").as_bytes(),
            ),
        };
        let scope = OwnershipScope::project(ScopeId::new_unchecked(project_id.clone()), None, None);
        let amounts = vec![
            ResourceAmount::new_unchecked(LimitKey::compute_servers(), 1),
            ResourceAmount::new_unchecked(LimitKey::compute_vcpus(), flavor.vcpus as u64),
            ResourceAmount::new_unchecked(LimitKey::compute_memory_mb(), flavor.ram_mib),
            ResourceAmount::new_unchecked(LimitKey::compute_disk_gb(), flavor.disk_gib),
        ];
        let quota_res = self
            .store
            .reserve_quota(&scope, &operation_id.to_string(), &amounts)
            .await
            .map_err(|err| match err {
                StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => ComputeError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                StoreError::ReservationConflict(_) => ComputeError::Conflict,
                other => ComputeError::Store(other),
            })?;

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
        // A durable row in terminal `Deleted` state is a completed lifecycle
        // over the same deterministic identity, not an in-flight create: the
        // name is free (`list_servers` excludes Deleted rows) and the caller
        // is starting a NEW lifecycle. The tombstone is remembered and the
        // normal schedule+persist flow revives the row under a fresh
        // lifecycle operation identity below. Every other durable state keeps
        // the retry semantics of ADR-0014 (byte-equivalent intent converges,
        // a differing intent conflicts).
        // CANONICAL INVARIANT: a canonical mutation resolves the idempotency
        // identity before any legacy existing-resource shortcut or revive
        // semantics.  The canonical store-level `create_or_replay_*` is the
        // sole authority for live replay and conflict detection.  Legacy
        // deterministic resource handling (omit+revive, keypair migration,
        // reply-equivalence) must NOT run when CanonicalMutationContext is
        // present — doing so would expose a non-canonical Operation or
        // revive a tombstone without canonical metadata.
        let mut revived_from: Option<o3k_store::ResourceRecord> = None;
        if canonical.is_none() {
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
                    // Deliberate vs HEAD: corrupt observed state now fails
                    // closed as `ComputeError::Store` instead of being silently
                    // treated as a live row.
                    let observed = server_state_from_storage(&existing.observed_state)
                        .map_err(ComputeError::Store)?;
                    if observed == ServerState::Deleted {
                        revived_from = Some(existing);
                    } else {
                        let legacy_keypair_intent =
                            requests_match_with_keypair_migration(&existing_request, &request);
                        // A recreation revive (issue #613 blocker B) persisted a
                        // fresh lifecycle operation identity for the same caller
                        // intent; a retry of that recreation rebuilds the
                        // pre-revive request and differs from the persisted
                        // intent only in operation_id and idempotency_key. The
                        // durable intent wins for that exact shape (the retry
                        // converges on the persisted row and its operation);
                        // every other difference keeps the ADR-0014 conflict
                        // semantics.
                        let revive_equivalent = existing_request != request
                            && !legacy_keypair_intent
                            && CreateInstanceRequest {
                                operation_id: existing_request.operation_id,
                                idempotency_key: existing_request.idempotency_key.clone(),
                                ..request.clone()
                            } == existing_request;
                        if !(existing_request == request
                            || legacy_keypair_intent
                            || revive_equivalent)
                        {
                            return Err(ComputeError::Conflict);
                        }
                        if legacy_keypair_intent {
                            let desired_state = serde_json::to_string(&request)
                                .map_err(|_| ComputeError::Conflict)?;
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
                        let server = self
                            .show_server(&project_id, ServerId::from_uuid(server_id))
                            .await?;
                        let operation = self
                            .store
                            .get_operation(existing_request.operation_id)
                            .await?;
                        return Ok(CreateMutationReceipt {
                            server,
                            operation_id: operation.id,
                            operation_state: operation.state,
                            replayed: true,
                        });
                    }
                }
                Err(StoreError::ResourceNotFound) => {}
                Err(error) => return Err(ComputeError::Store(error)),
            }
        }
        // A name conflict is deterministic control-plane state. Reject it
        // before reserving Placement capacity; the second check below still
        // protects against a concurrent create racing this read.
        // CANONICAL INVARIANT: when CanonicalMutationContext is present the
        // deterministic server_id uniquely identifies the resource.  A
        // canonical replay carries the same server_id, so the name check must
        // be skipped to avoid rejecting an equivalent replay.
        let canonical_has_existing = canonical.is_some() && existing_res.is_ok();
        if !canonical_has_existing
            && self
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
        if !canonical_has_existing
            && servers
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
        // ASR-018 crash-window failpoint: the placement allocation is already
        // durable; the server/resource/create-operation intent is not yet
        // persisted. Killed here, restart startup reconciliation must release
        // the orphan allocation and the retried create must stay idempotent.
        test_fault_pause_ms(
            "after-placement-commit",
            "O3K_TEST_FAULT_PAUSE_AFTER_PLACEMENT_COMMIT_MS",
        );
        let request = match revived_from.as_ref() {
            Some(tombstone) => {
                // Revive the tombstoned row into a fresh lifecycle. The
                // operation identity and the provider idempotency key derive
                // deterministically from the tombstone's durable
                // observed_generation, so a retry before this persist
                // recomputes the same identities and a retry after a crash
                // between placement commit and persist converges through the
                // revive again (the tombstone is untouched until the atomic
                // persist below). The fresh idempotency key also keeps the
                // agent command journal from deduplicating the new create
                // against the completed lifecycle's terminal command.
                let revive_operation_id = Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!(
                        "o3k:operation:revive:{project_id}:{idempotency_key}:{}",
                        tombstone.observed_generation
                    )
                    .as_bytes(),
                );
                let revive_idempotency_key =
                    format!("{idempotency_key}:revive:{}", tombstone.observed_generation);
                let revive_request = CreateInstanceRequest {
                    operation_id: revive_operation_id,
                    idempotency_key: revive_idempotency_key,
                    ..request
                };
                let desired_state =
                    serde_json::to_string(&revive_request).map_err(|_| ComputeError::Conflict)?;
                match self
                    .store
                    .revive_resource_and_operation(
                        id,
                        tombstone.generation,
                        &desired_state,
                        server_state_to_storage(ServerState::Requested),
                        tombstone.observed_generation,
                        None,
                        &o3k_store::OperationRecord {
                            id: revive_operation_id,
                            resource_id: id,
                            kind: "create".to_owned(),
                            state: o3k_store::OperationState::Pending,
                            provider_operation_id: None,
                            error_category: None,
                            error_message: None,
                        },
                        placement
                            .as_ref()
                            .map(|decision| decision.allocation_id.as_str()),
                    )
                    .await
                {
                    Ok(_) => revive_request,
                    Err(StoreError::StaleGeneration) | Err(StoreError::ResourceAlreadyExists) => {
                        // A concurrent writer already advanced the row.
                        // Observe the durable row before deciding what to
                        // release: a decision owned by the live row backs
                        // that row and must not be released.
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
                        return Err(ComputeError::Conflict);
                    }
                    // The placement allocation referenced by this revive was
                    // reconciled away before the intent became durable
                    // (startup orphan reconciliation racing an in-flight
                    // create). Fail closed exactly like the fresh-create
                    // path: the caller retries with a fresh allocation.
                    Err(StoreError::PlacementAllocationNotFound) => {
                        return Err(ComputeError::Conflict);
                    }
                    Err(error) => return Err(ComputeError::Store(error)),
                }
            }
            None => {
                let acceptance =
                    match canonical {
                        Some(context) => {
                            self.journal
                                .begin_canonical_create(&project_id, &request, context)
                                .await
                        }
                        None => self.journal.begin_create(&project_id, &request).await.map(
                            |operation_id| o3k_store::CanonicalAcceptanceOutcome::Created {
                                operation_id,
                                resource_id: request.o3k_server_id,
                            },
                        ),
                    };
                match acceptance {
                    Ok(o3k_store::CanonicalAcceptanceOutcome::Conflict) => {
                        if let Some(decision) = placement.as_ref() {
                            self.release_placement_decision(decision).await?;
                        }
                        return Err(ComputeError::Conflict);
                    }
                    Ok(o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                        operation_id: existing_operation_id,
                        resource_id,
                    }) => {
                        let existing = self.store.get_resource(resource_id).await?;
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
                        let server = self
                            .show_server(&project_id, ServerId::from_uuid(resource_id))
                            .await?;
                        let operation = self.store.get_operation(existing_operation_id).await?;
                        return Ok(CreateMutationReceipt {
                            server,
                            operation_id: existing_operation_id,
                            operation_state: operation.state,
                            replayed: true,
                        });
                    }
                    Ok(_) => {}
                    Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {
                        // CANONICAL INVARIANT: a pre-existing legacy resource
                        // with no matching canonical idempotency reservation
                        // must fail closed.  The legacy path below preserves
                        // the existing OpenStack/TestLab recreate contract
                        // only for non-canonical mutations.
                        if canonical.is_some() {
                            if let Some(decision) = placement.as_ref() {
                                self.release_placement_decision(decision).await?;
                            }
                            return Err(ComputeError::Conflict);
                        }
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
                            let desired_state = serde_json::to_string(&request)
                                .map_err(|_| ComputeError::Conflict)?;
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
                        let server = self
                            .show_server(&project_id, ServerId::from_uuid(id))
                            .await?;
                        let operation = self
                            .store
                            .get_operation(existing_request.operation_id)
                            .await?;
                        return Ok(CreateMutationReceipt {
                            server,
                            operation_id: operation.id,
                            operation_state: operation.state,
                            replayed: true,
                        });
                    }
                    // The placement allocation referenced by this create was
                    // reconciled away before the consumer intent became durable
                    // (startup orphan reconciliation racing an in-flight
                    // create). Fail closed: no resource may outlive its
                    // allocation. The caller retries; the deterministic
                    // allocation identity keeps the retry idempotent.
                    Err(ReconcileError::Store(StoreError::PlacementAllocationNotFound)) => {
                        return Err(ComputeError::Conflict);
                    }
                    Err(error) => return Err(ComputeError::Reconcile(error)),
                }
                request
            }
        };
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
        let server = self
            .show_server(&project_id, ServerId::from_uuid(id))
            .await?;
        let _ = self.store.commit_reservation(&quota_res.id).await;
        let operation = self.store.get_operation(request.operation_id).await?;
        Ok(CreateMutationReceipt {
            server,
            operation_id: operation.id,
            operation_state: operation.state,
            replayed: false,
        })
    }

    pub async fn list_servers_for_auth(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<Server>, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ListServers").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ListServers".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        self.list_servers(auth.effective_scope().id().as_str())
            .await
    }

    /// Returns the durable control-plane generation for a server already
    /// authorized through the compute read path. The native API uses this
    /// bounded projection instead of fabricating generation metadata.
    pub async fn server_generation_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
    ) -> Result<i64, ComputeError> {
        let record = self
            .store
            .get_resource(id.as_uuid())
            .await
            .map_err(ComputeError::Store)?;
        if record.project_id != auth.effective_scope().id().as_str()
            || record.kind != "compute_instance"
        {
            return Err(ComputeError::NotFound);
        }
        Ok(record.generation)
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

    pub async fn show_server_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
    ) -> Result<Server, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ReadServer").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ReadServer".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.show_server(auth.effective_scope().id().as_str(), id)
            .await
    }

    /// Returns the durable network attachment intent even after the server
    /// has reached Deleted.  The Nova adapter uses this to retry cleanup after
    /// a process restart or a transient endpoint-store failure; the normal
    /// read projection intentionally hides deleted servers.
    pub async fn server_network_ids_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
    ) -> Result<Vec<String>, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ReadServer").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ReadServer".to_owned())
        });
        let target = ResourceTarget::instance(
            ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
            ResourceId::new(id.as_uuid().to_string()).map_err(|_| ComputeError::InvalidRequest)?,
            Some(auth.effective_scope().id().clone()),
        );
        let decision = self.authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: target,
        });
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.kind != "compute_instance"
            || resource.project_id != auth.effective_scope().id().as_str()
        {
            return Err(ComputeError::NotFound);
        }
        let request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        Ok(request.network_ids)
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

    /// Applies the bounded Nova in-place update supported by P13.2D.  The
    /// canonical resource ledger is the merge base; compatibility callers
    /// cannot update a stale projection or change server identity.
    pub async fn update_server_name_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
        name: String,
    ) -> Result<Server, ComputeError> {
        let action = ActionId::new("compute", "UpdateServer").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "UpdateServer".to_owned())
        });
        let target = ResourceTarget::instance(
            ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
            ResourceId::new(id.as_uuid().to_string()).map_err(|_| ComputeError::InvalidRequest)?,
            Some(auth.effective_scope().id().clone()),
        );
        if !self
            .authorizer
            .authorize(&AuthorizationRequest {
                auth_context: auth,
                action,
                resource_target: target,
            })
            .is_allowed()
        {
            return Err(ComputeError::NotFound);
        }
        if name.trim().is_empty() {
            return Err(ComputeError::InvalidRequest);
        }
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.kind != "compute_instance"
            || resource.project_id != auth.effective_scope().id().as_str()
        {
            return Err(ComputeError::NotFound);
        }
        let mut request: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        request.name = name;
        let desired = serde_json::to_string(&request).map_err(|_| ComputeError::Conflict)?;
        self.store
            .update_resource(
                resource.id,
                resource.generation,
                &desired,
                &resource.observed_state,
                resource.observed_generation,
                resource.provider_id.as_deref(),
            )
            .await
            .map_err(ComputeError::Store)?;
        self.show_server(auth.effective_scope().id().as_str(), id)
            .await
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
        tracing::debug!(resource_id = %resource.id, "create convergence drive entered");
        let Ok(request) = serde_json::from_str::<CreateInstanceRequest>(&resource.desired_state)
        else {
            return;
        };
        let Ok(operation) = self.store.get_operation(request.operation_id).await else {
            return;
        };
        // Issue #88 S5 rerun: a create whose transfer dispatch was rejected
        // mid-flight (agent killed during the handshake) is marked Retryable
        // by retry_or_fail; the scheduled retry must actually fire, or the
        // operation stays Retryable forever, the API delete 409s against it,
        // and every owned residue is held. Re-drive Retryable exactly like
        // Pending and UnknownOutcome — the retry budget in retry_or_fail
        // still bounds it (attempts >= max_attempts terminalizes Failed).
        let re_drive = matches!(
            operation.state,
            o3k_store::OperationState::Pending
                | o3k_store::OperationState::UnknownOutcome
                | o3k_store::OperationState::Retryable
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
                if let Ok(resource) = self.store.get_resource(resource.id).await
                    && resource.observed_state != "ACTIVE"
                    && let Err(error) = self
                        .store
                        .update_resource(
                            resource.id,
                            resource.generation,
                            &resource.desired_state,
                            "ACTIVE",
                            resource.generation,
                            resource.provider_id.as_deref(),
                        )
                        .await
                {
                    tracing::warn!(
                        operation_id = %request.operation_id,
                        resource_id = %resource.id,
                        error = %error,
                        "server create success projection to ACTIVE failed"
                    );
                }
            }
            _ => {}
        }
    }

    pub async fn attach_volume_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "AttachVolume").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "AttachVolume".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .attach_volume(
                auth.effective_scope().id().as_str(),
                server_id,
                volume_id,
                device,
                tag,
                delete_on_termination,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("volume", "volume_attachment").unwrap_or_else(|_| {
                            ResourceType::new_unchecked(
                                "volume".to_owned(),
                                "volume_attachment".to_owned(),
                            )
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
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

    pub async fn list_volume_attachments_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
    ) -> Result<Vec<VolumeAttachmentRecord>, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "ListVolumeAttachments").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "ListVolumeAttachments".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.list_volume_attachments(auth.effective_scope().id().as_str(), server_id)
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

    pub async fn get_volume_attachment_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "ReadVolumeAttachment").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "ReadVolumeAttachment".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.get_volume_attachment(
            auth.effective_scope().id().as_str(),
            server_id,
            attachment_id,
        )
        .await
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

    pub async fn detach_volume_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "DetachVolume").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "DetachVolume".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .detach_volume(
                auth.effective_scope().id().as_str(),
                server_id,
                attachment_id,
            )
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("volume", "volume_attachment").unwrap_or_else(|_| {
                            ResourceType::new_unchecked(
                                "volume".to_owned(),
                                "volume_attachment".to_owned(),
                            )
                        }),
                        ResourceId::new(attachment_id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
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

    pub async fn create_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: String,
        public_key: String,
    ) -> Result<Keypair, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ImportKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ImportKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        match self
            .create_keypair(
                auth.principal().id().as_str(),
                auth.effective_scope().id().as_str(),
                name,
                public_key,
            )
            .await
        {
            Ok(kp) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "keypair").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "keypair".to_owned())
                        }),
                        ResourceId::new(kp.name.clone()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(kp)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn list_keypairs_for_auth(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<Keypair>, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ListKeypairs").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ListKeypairs".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        self.list_keypairs(
            auth.principal().id().as_str(),
            auth.effective_scope().id().as_str(),
        )
        .await
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

    pub async fn show_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<Keypair, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ReadKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ReadKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(name.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.show_keypair(
            auth.principal().id().as_str(),
            auth.effective_scope().id().as_str(),
            name,
        )
        .await
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

    pub async fn delete_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "DeleteKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "DeleteKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(name.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .delete_keypair(
                auth.principal().id().as_str(),
                auth.effective_scope().id().as_str(),
                name,
            )
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "keypair").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "keypair".to_owned())
                        }),
                        ResourceId::new(name.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
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

    pub async fn delete_server_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "DeleteServer").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "DeleteServer".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .delete_server(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "server").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "server".to_owned())
                        }),
                        ResourceId::new(id.as_uuid().to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_server_for_auth_canonical(
        &self,
        auth: &AuthContext,
        id: ServerId,
        context: o3k_reconciler::CanonicalMutationContext,
    ) -> Result<MutationReceipt<ServerId>, ComputeError> {
        // CANONICAL INVARIANT: the canonical context's action must match the
        // expected mutation (InvalidRequest), while actor and owner_scope
        // must match the authenticated request (Unauthorized).
        if context.action.to_string() != "compute:DeleteServer" {
            return Err(ComputeError::InvalidRequest);
        }
        if context.actor != auth.principal().id().as_str()
            || context.owner_scope.id().as_str() != auth.effective_scope().id().as_str()
        {
            return Err(ComputeError::Unauthorized);
        }
        let action = context.action.clone();
        let target = ResourceTarget::instance(
            ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
            ResourceId::new(id.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
            Some(auth.effective_scope().id().clone()),
        );
        if !self
            .authorizer
            .authorize(&AuthorizationRequest {
                auth_context: auth,
                action,
                resource_target: target,
            })
            .is_allowed()
        {
            return Err(ComputeError::NotFound);
        }
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.project_id != auth.effective_scope().id().as_str() {
            return Err(ComputeError::NotFound);
        }
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:canonical-delete:{}:{}:{}",
                resource.project_id, id, context.idempotency_key
            )
            .as_bytes(),
        );
        let acceptance = self
            .journal
            .begin_canonical_lifecycle(
                id.as_uuid(),
                operation_id,
                LifecycleAction::Delete,
                &context,
            )
            .await?;
        let replayed = match acceptance {
            o3k_store::CanonicalAcceptanceOutcome::Conflict => return Err(ComputeError::Conflict),
            o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                operation_id: existing,
                resource_id,
            } => {
                let operation = self.store.get_operation(existing).await?;
                return Ok(MutationReceipt {
                    resource: ServerId::from_uuid(resource_id),
                    operation_id: existing,
                    operation_state: operation.state,
                    replayed: true,
                });
            }
            o3k_store::CanonicalAcceptanceOutcome::Created { .. } => false,
        };
        let operation_state = self
            .delete_server_with_accepted_operation(
                auth.effective_scope().id().as_str(),
                id,
                Some(operation_id),
            )
            .await?;
        Ok(MutationReceipt {
            resource: id,
            operation_id,
            operation_state,
            replayed,
        })
    }

    pub async fn delete_server(&self, project_id: &str, id: ServerId) -> Result<(), ComputeError> {
        let state = self
            .delete_server_with_accepted_operation(project_id, id, None)
            .await?;
        if state != o3k_store::OperationState::Succeeded {
            return Err(ComputeError::Conflict);
        }
        Ok(())
    }

    /// Returns the operation state after a bounded execution pass.
    ///
    /// LEGACY callers (no accepted_operation_id) reconcile in a polling loop
    /// until the operation reaches terminal, then run compensatory cleanup and
    /// return `Ok(OperationState::Succeeded)` if completed, or
    /// `Err(ComputeError::Conflict)` if not.
    ///
    /// CANONICAL callers (with accepted_operation_id) perform one
    /// reconciliation pass and return the resulting state directly without
    /// blocking.  The caller maps `Succeeded → 204` and
    /// `Pending/Running/Retryable/UnknownOutcome → 202` in the native API
    /// adapter; compensatory cleanup runs only after terminal success.
    async fn delete_server_with_accepted_operation(
        &self,
        project_id: &str,
        id: ServerId,
        accepted_operation_id: Option<Uuid>,
    ) -> Result<o3k_store::OperationState, ComputeError> {
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
            self.unbind_ports_from_intent(&intent).await;
            self.cleanup_config_drive_best_effort(&id.as_uuid().to_string());
            let _ = self
                .store
                .release_reservation_for_operation(&intent.operation_id.to_string())
                .await;
            if let Some(operation_id) = accepted_operation_id {
                self.store
                    .update_operation(
                        operation_id,
                        o3k_store::OperationState::Succeeded,
                        None,
                        None,
                        None,
                    )
                    .await?;
            }
            return Ok(o3k_store::OperationState::Succeeded);
        }
        if resource.provider_id.is_none() {
            // A server that never reached a provider cannot be deleted through
            // the provider path: there is no provider identity to address.
            // That is safe to complete locally only when the create is
            // terminally failed WITHOUT any provider acceptance (no provider
            // operation identity): the create dispatch provably never reached
            // an agent — e.g. the issue-87 empty-registry terminal — so no
            // provider side effect can exist, mirroring the reconciler's
            // "domain already absent" handling of provider NotFound on
            // delete. A create that WAS accepted (a provider operation
            // identity exists) is equally absent-proven when the durable
            // error_category is "not_found": the create provably never took
            // effect, so no provider side effect can exist either — either
            // converge_absent_create's presence-inspection evidence (issue-87
            // S3 rerun #4) or the agent's definitive pre-libvirt failure
            // evidence, where the failure provably happened before any
            // libvirt define (issue-87 C-1 qemu-img failure). Every other
            // shape — in-flight,
            // accepted without absence proof, or terminally failed for a
            // reason other than absence — still fails closed: the provider
            // may hold side effects that only the provider delete can
            // remove.
            let intent: CreateInstanceRequest = serde_json::from_str(&resource.desired_state)
                .map_err(|_| ComputeError::Conflict)?;
            let create = self
                .store
                .get_operation(intent.operation_id)
                .await
                .map_err(|error| match error {
                    StoreError::OperationNotFound => ComputeError::Conflict,
                    other => ComputeError::Store(other),
                })?;
            if !(matches!(create.state, o3k_store::OperationState::Failed)
                && (create.provider_operation_id.is_none()
                    || create.error_category.as_deref() == Some("not_found")))
            {
                return Err(ComputeError::Conflict);
            }
            let operation_id = accepted_operation_id.unwrap_or_else(|| {
                Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("o3k:delete:{project_id}:{id}:{}", resource.generation).as_bytes(),
                )
            });
            if accepted_operation_id.is_none() {
                match self
                    .journal
                    .begin_lifecycle(id.as_uuid(), operation_id, LifecycleAction::Delete)
                    .await
                {
                    Ok(_) | Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {}
                    Err(error) => return Err(ComputeError::Reconcile(error)),
                }
            }
            self.store
                .update_operation(
                    operation_id,
                    o3k_store::OperationState::Succeeded,
                    None,
                    None,
                    None,
                )
                .await?;
            self.store
                .update_resource(
                    id.as_uuid(),
                    resource.generation,
                    &resource.desired_state,
                    server_state_to_storage(ServerState::Deleted),
                    resource.generation,
                    None,
                )
                .await?;
            self.release_placement_allocation(id.as_uuid(), &intent)
                .await?;
            self.store.detach_server_keypair(id.as_uuid()).await?;
            self.project_terminal_binding_outcome(
                operation_id.to_string().as_str(),
                o3k_store::OperationState::Succeeded,
            )
            .await;
            let _ = self
                .store
                .release_reservation_for_operation(&intent.operation_id.to_string())
                .await;
            // Issue #88 S3 residue: the create may have been ACCEPTED by an
            // agent (the config-drive transfers commit before acceptance)
            // that crashed before any libvirt mutation. The accepted
            // create's ConfigDriveIso manifests and content survive on that
            // host with zero durable bindings, and nothing else ever tells
            // the agent to reap them — the local completion above proves
            // absence but does not remove the media. Dispatch a BEST-EFFORT
            // delete for the never-defined resource with the same
            // deterministic delete operation identity (the provider reuses
            // the durable command record idempotently on re-dispatch) and a
            // dedicated reap idempotency key. The agent's delete executor
            // reaps the config-drive media, network, and console residue
            // through its "domain already absent" arm — the single reaping
            // authority. A failed dispatch is logged and never changes the
            // already-terminal local delete: the residue verifier catches
            // leftovers separately. NotFound means the create never reached
            // any agent (e.g. the empty-registry terminal) — a clean no-op.
            if let Err(error) = self
                .provider
                .delete_instance(DeleteInstanceRequest {
                    operation_id,
                    provider_instance_id: id.as_uuid().to_string(),
                    idempotency_key: format!("o3k:delete-reap:{id}"),
                })
                .await
            {
                if matches!(error, ProviderError::NotFound) {
                    tracing::debug!(
                        resource_id = %id,
                        "reap dispatch found no accepted create; nothing to reap"
                    );
                } else {
                    tracing::warn!(
                        resource_id = %id,
                        error = ?error,
                        "best-effort reap dispatch failed; the local delete is unaffected"
                    );
                }
            }
            return Ok(o3k_store::OperationState::Succeeded);
        }
        let operation_id = accepted_operation_id.unwrap_or_else(|| {
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:delete:{project_id}:{id}:{}", resource.generation).as_bytes(),
            )
        });
        if accepted_operation_id.is_none() {
            match self
                .journal
                .begin_lifecycle(id.as_uuid(), operation_id, LifecycleAction::Delete)
                .await
            {
                Ok(_) | Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {}
                Err(error) => return Err(ComputeError::Reconcile(error)),
            }
        }
        // CANONICAL PATH: one reconciliation pass, return current state without
        // blocking.  The caller maps Succeeded→204 and non-terminal→202.  No
        // compensatory cleanup runs for an incomplete operation.
        //
        // LEGACY PATH: poll until terminal; the synchronous caller contract
        // requires the delete to complete before returning.
        let state = match accepted_operation_id {
            Some(_) => self
                .journal
                .reconcile_lifecycle_once(operation_id)
                .await
                .map_err(ComputeError::Reconcile)?,
            None => {
                self.reconcile_lifecycle_until_terminal(operation_id)
                    .await?
            }
        };
        if state != o3k_store::OperationState::Succeeded {
            return Ok(state);
        }
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        self.release_placement_allocation(id.as_uuid(), &intent)
            .await?;
        self.store.detach_server_keypair(id.as_uuid()).await?;
        self.project_terminal_binding_outcome(
            operation_id.to_string().as_str(),
            o3k_store::OperationState::Succeeded,
        )
        .await;
        let _ = self
            .store
            .release_reservation_for_operation(&intent.operation_id.to_string())
            .await;
        Ok(o3k_store::OperationState::Succeeded)
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

    pub async fn action_for_auth(
        &self,
        auth: &AuthContext,
        id: ServerId,
        action: InstanceAction,
    ) -> Result<Server, ComputeError> {
        let action_name = match action {
            InstanceAction::Start => "StartServer",
            InstanceAction::Stop => "StopServer",
            InstanceAction::Reboot => "RebootServer",
        };
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", action_name).unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), action_name.to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "server").map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(id.as_uuid().to_string())
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .action(auth.effective_scope().id().as_str(), id, action)
            .await
        {
            Ok(server) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "server").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "server".to_owned())
                        }),
                        ResourceId::new(server.id.as_uuid().to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(server)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
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
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Stateful in-memory agent registry used to test application scheduling
    /// and inventory behavior without wire types. The snapshots are
    /// application-level values, so the tests exercise exactly what the
    /// transport adapter would publish after its boundary conversion.
    #[derive(Clone, Default)]
    struct FakeAgentRegistry {
        nodes: Arc<tokio::sync::RwLock<BTreeMap<String, AgentNodeSnapshot>>>,
    }

    struct FakeAgentEpochLease {
        _nodes: tokio::sync::OwnedRwLockReadGuard<BTreeMap<String, AgentNodeSnapshot>>,
    }

    impl o3k_provider::AgentEpochLease for FakeAgentEpochLease {}

    #[async_trait]
    impl AgentNodeRegistry for FakeAgentRegistry {
        async fn all(&self) -> Vec<AgentNodeSnapshot> {
            self.nodes.read().await.values().cloned().collect()
        }

        async fn snapshot(&self, agent_id: &str) -> Option<AgentNodeSnapshot> {
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
                Some(Box::new(FakeAgentEpochLease { _nodes: nodes }))
            } else {
                None
            }
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file; the decorated create
        // intent references an allocation that must already be committed in
        // the same store (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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

    /// Issue #606: the inventory publisher must publish a registered agent's
    /// capacity immediately when the registration notify fires, without
    /// waiting for the next 5 s tick. The publisher starts with an empty
    /// registry and its immediate first tick is consumed before the agent is
    /// upserted, so the sub-second appearance below can only come from the
    /// notify, not from the periodic cadence.
    #[tokio::test]
    async fn registration_notify_publishes_inventory_before_the_next_tick()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = PathBuf::from(format!(
            "/tmp/o3k-placement-agent-registration-{}",
            Uuid::now_v7()
        ));
        let placement_store = o3k_store::testkit::open_memory().await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(placement_store);
        let placement = o3k_placement::PlacementLedger::open(&root, placement_repository).await?;
        let registry = FakeAgentRegistry::default();
        let registration = Arc::new(tokio::sync::Notify::new());
        let task = spawn_agent_inventory_publisher(
            Arc::new(registry.clone()),
            placement.clone(),
            registration.clone(),
        );
        // Let the publisher's immediate first tick (empty registry, nothing
        // to publish) complete before the agent exists; the next tick is
        // 5 s away, so only the registration notify can publish it within
        // the sub-second bound below.
        tokio::time::sleep(Duration::from_millis(100)).await;
        registry.upsert(agent_node("agent-a", 4, 4096, 20)).await;
        registration.notify_one();
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                if placement.provider("agent-a").await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let provider = placement.provider("agent-a").await?;
        assert_eq!(provider.state, o3k_placement::ProviderState::Enabled);
        assert_eq!(provider.inventories[o3k_placement::VCPU].total, 4);
        assert_eq!(provider.inventories[o3k_placement::MEMORY_MB].total, 4096);
        assert_eq!(provider.inventories[o3k_placement::DISK_GB].total, 20);
        task.abort();
        let _ = task.await;
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
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file; the decorated create
        // intent references an allocation that must already be committed in
        // the same store (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
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
        service
            .store
            .attach_server_keypair(request.o3k_server_id, keypair.id)
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file; the decorated create
        // intent references an allocation that must already be committed in
        // the same store (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .attach_server_keypair(request.o3k_server_id, keypair.id)
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
        // The real provider's `dispatch_recorded` persists the inspection's
        // durable command row (with the real payload) before sending it, and
        // the terminal agent update/observation are bound to that row. Model
        // that binding here exactly as the real adapter leaves it, so the
        // evidence below can be applied (the fake provider does not create
        // command rows itself).
        let inspect_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect-create:{}", request.operation_id).as_bytes(),
        );
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("o3k-inspect-command-{inspect_operation_id}"),
                idempotency_key: format!("o3k-inspect-create-{}", request.operation_id),
                operation_id: inspect_operation_id,
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
        // A repeated poll must reuse the in-flight inspection, not re-dispatch.
        let second = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(second.state, ServerState::Requested);
        assert_eq!(fake.inspect_dispatch_count(), 1);

        // The agent completes the inspection: terminal update + observation.
        let update = AgentOperationUpdate {
            agent_id: "node-a".to_owned(),
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
            agent_id: "node-a".to_owned(),
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
    /// record is inserted by the caller — no agent command row exists. The
    /// placement allocation the decorated request references is committed
    /// first, mirroring the scheduler-decorated create ordering (ASR-018).
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
        let raw_store = o3k_store::testkit::open_file(&path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_store: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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
        placement_store
            .register_provider(
                "node-a",
                &[o3k_store::PlacementInventoryRecord {
                    resource_class: "VCPU".to_owned(),
                    total: 8,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                }],
            )
            .await?;
        let generation = placement_store
            .get_provider("node-a")
            .await?
            .ok_or(StoreError::PlacementProviderNotFound)?
            .generation;
        placement_store
            .commit_allocation(
                "node-a",
                generation,
                &o3k_store::PlacementAllocationRecord {
                    id: "alloc-1".to_owned(),
                    provider_id: "node-a".to_owned(),
                    consumer_id: request.o3k_server_id.to_string(),
                    resources: vec![o3k_store::PlacementResourceRecord {
                        resource_class: "VCPU".to_owned(),
                        amount: 1,
                    }],
                },
            )
            .await?;
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
            let resource = store.get_resource(request.o3k_server_id).await?;
            if operation.state == o3k_store::OperationState::Succeeded
                && resource.observed_state == "ACTIVE"
            {
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

    /// Builds an artifact transfer row for the given operation, mirroring the
    /// rows the compute-agent persists during the artifact handshake.
    fn artifact_transfer(
        transfer_id: &str,
        operation_id: Uuid,
        resource_id: Uuid,
        state: o3k_store::ArtifactTransferState,
        contiguous_bytes: u64,
        next_chunk_index: u64,
    ) -> o3k_store::ArtifactTransferRecord {
        o3k_store::ArtifactTransferRecord {
            transfer_id: transfer_id.to_owned(),
            command_id: format!("command-{transfer_id}"),
            operation_id,
            resource_id,
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            artifact_id: "image-1".to_owned(),
            artifact_kind: "image_base".to_owned(),
            sha256: "a".repeat(64),
            size_bytes: 512 * 1024,
            expires_at_unix_ms: i64::MAX,
            format: "qcow2".to_owned(),
            chunk_size_bytes: 256 * 1024,
            chunk_count: 2,
            state,
            contiguous_bytes,
            next_chunk_index,
            retry_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// Issue #88: an operation that terminalizes through the reconciler sweep
    /// (the issue-88 live evidence: a create terminalized Failed after the
    /// agent crashed and the sweep re-dispatch timed out) leaves its
    /// non-terminal artifact transfers `expired` afterward — two rows stayed
    /// `offered` past their admission expiry while the operation was already
    /// terminal. Committed transfers of the same operation stay committed:
    /// they are durable cache/evidence the agent's committed manifests rely
    /// on. Transfers of a still non-terminal operation stay untouched.
    #[tokio::test]
    async fn create_convergence_sweep_expires_transfers_of_terminalized_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Terminal)?;
        let (service, store, request) =
            crash_before_dispatch_fixture("sweep-transfer-expiry", fake.clone()).await?;
        // A still-running operation keeps its own offer: the sweep must not
        // touch transfers whose operation is not terminal. The desired state
        // is intentionally unparseable so the drive loop skips this resource
        // (only the sweep cares about it).
        let running_resource = o3k_store::ResourceRecord {
            id: Uuid::now_v7(),
            kind: "compute_instance".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "unparseable-intent".to_owned(),
            observed_state: "BUILD".to_owned(),
            provider_id: None,
        };
        let running_operation = o3k_store::OperationRecord {
            id: Uuid::now_v7(),
            resource_id: running_resource.id,
            kind: "create".to_owned(),
            state: o3k_store::OperationState::Running,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        store
            .insert_resource_and_operation(&running_resource, &running_operation, None)
            .await?;
        for transfer in [
            artifact_transfer(
                "t-offered",
                request.operation_id,
                request.o3k_server_id,
                o3k_store::ArtifactTransferState::Offered,
                0,
                0,
            ),
            artifact_transfer(
                "t-receiving",
                request.operation_id,
                request.o3k_server_id,
                o3k_store::ArtifactTransferState::Receiving,
                256 * 1024,
                1,
            ),
            artifact_transfer(
                "t-committed",
                request.operation_id,
                request.o3k_server_id,
                o3k_store::ArtifactTransferState::Committed,
                512 * 1024,
                2,
            ),
            artifact_transfer(
                "t-running",
                running_operation.id,
                running_resource.id,
                o3k_store::ArtifactTransferState::Offered,
                0,
                0,
            ),
        ] {
            store.insert_artifact_transfer(&transfer).await?;
        }

        // One pass: the drive terminalizes the create through the reconciler
        // path and the per-pass sweep expires its abandoned handshake rows.
        service.drive_all_create_convergence().await?;

        assert_eq!(
            store.get_operation(request.operation_id).await?.state,
            o3k_store::OperationState::Failed
        );
        assert_eq!(
            store.get_artifact_transfer("t-offered").await?.state,
            o3k_store::ArtifactTransferState::Expired
        );
        assert_eq!(
            store.get_artifact_transfer("t-receiving").await?.state,
            o3k_store::ArtifactTransferState::Expired
        );
        assert_eq!(
            store.get_artifact_transfer("t-committed").await?.state,
            o3k_store::ArtifactTransferState::Committed
        );
        assert_eq!(
            store.get_artifact_transfer("t-running").await?.state,
            o3k_store::ArtifactTransferState::Offered
        );
        // The sweep is idempotent: a second pass expires nothing.
        service.drive_all_create_convergence().await?;
        assert_eq!(
            store.get_artifact_transfer("t-offered").await?.state,
            o3k_store::ArtifactTransferState::Expired
        );
        Ok(())
    }

    /// Issue #88 S5 rerun: an operation marked `Retryable` by `retry_or_fail`
    /// (the live shape: a create whose artifact-transfer dispatch was
    /// rejected mid-flight when the agent was killed during the handshake)
    /// must be re-driven by the create-convergence sweep. Before the fix the
    /// re-drive gate skipped Retryable, so the scheduled retry never fired:
    /// the operation stayed Retryable forever, the API delete 409'd against
    /// it, and every owned residue (op row, command, allocation, config-drive
    /// media, transfer part) was held. The retry budget in `retry_or_fail`
    /// still bounds the re-drive (attempts >= max_attempts terminalizes
    /// Failed).
    #[tokio::test]
    async fn create_convergence_re_drives_retryable_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("retryable-re-drive", fake).await?;
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Retryable,
                None,
                Some("retryable"),
                None,
            )
            .await?;
        service.drive_all_create_convergence().await?;
        let operation = store.get_operation(request.operation_id).await?;
        assert_ne!(
            operation.state,
            o3k_store::OperationState::Retryable,
            "a Retryable create must be re-driven by the convergence sweep"
        );
        assert!(
            matches!(
                operation.state,
                o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
            ),
            "the re-driven Retryable create must reach a terminal state, got {:?}",
            operation.state
        );
        Ok(())
    }

    /// Seeds the issue-88 B1 residue: a delete lifecycle operation durably
    /// `UnknownOutcome` with a provider operation identity (exactly what
    /// `handle_lifecycle_result` persists when the provider reports an
    /// unknown delete outcome) on a resource that still projects ACTIVE,
    /// while the provider-side domain is already gone — the undefine
    /// executed and the agent's outcome report was lost when libvirtd
    /// restarted mid-race. The provider answers the presence inspection
    /// (`get_instance`) with NotFound, which is what lets `observe_lifecycle`
    /// reach its adoption/terminalization arm for a delete.
    #[allow(clippy::type_complexity)]
    async fn lifecycle_unknown_outcome_fixture<P>(
        label: &str,
        provider: Arc<P>,
    ) -> Result<
        (ComputeService, Arc<dyn ComputeRepository>, Uuid, Uuid, Uuid),
        Box<dyn std::error::Error>,
    >
    where
        P: ComputeProvider + 'static,
    {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let service = ComputeService::new(store.clone(), provider);
        let resource_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        let provider_operation_id = Uuid::now_v7();
        let request = CreateInstanceRequest {
            operation_id,
            o3k_server_id: resource_id,
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
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: resource_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: serde_json::to_string(&request)?,
                observed_state: "ACTIVE".to_owned(),
                provider_id: Some("fake-instance-1".to_owned()),
            })
            .await?;
        service
            .journal
            .begin_lifecycle(resource_id, operation_id, LifecycleAction::Delete)
            .await
            .map_err(ComputeError::Reconcile)?;
        store
            .update_operation(
                operation_id,
                o3k_store::OperationState::UnknownOutcome,
                Some(&provider_operation_id.to_string()),
                Some("unknown_outcome"),
                None,
            )
            .await?;
        Ok((
            service,
            store,
            operation_id,
            resource_id,
            provider_operation_id,
        ))
    }

    /// Issue #88 B1: a delete lifecycle operation left in `UnknownOutcome`
    /// is never converged by anything after the API's synchronous 10s poll
    /// returns — the create-convergence sweep only drives create operations,
    /// the event stream rejects non-Succeeded observations
    /// (`apply_agent_observation`), and no periodic path calls
    /// `reconcile_lifecycle_once` again. The resource stays ACTIVE, the
    /// delete retry 409s, and every owned residue (op row, command row,
    /// allocation, config-drive media) is held. The periodic lifecycle sweep
    /// must re-drive the operation so `observe_lifecycle`'s presence
    /// inspection adopts the already-executed delete (provider domain absent
    /// → terminal DELETED). Before the fix the sweep does not exist: the
    /// operation stays UnknownOutcome forever.
    #[tokio::test]
    async fn lifecycle_convergence_sweep_terminalizes_unknown_outcome_delete()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, operation_id, resource_id, _provider_operation_id) =
            lifecycle_unknown_outcome_fixture("b1-sweep", provider.clone()).await?;

        service.drive_all_lifecycle_convergence().await?;

        assert_eq!(
            store.get_operation(operation_id).await?.state,
            o3k_store::OperationState::Succeeded,
            "the lifecycle sweep must converge the UnknownOutcome delete"
        );
        assert_eq!(
            store.get_resource(resource_id).await?.observed_state,
            "DELETED",
            "the resource must converge to DELETED"
        );
        assert_eq!(
            provider.delete_calls(),
            0,
            "an UnknownOutcome delete is observed (presence inspection), never re-dispatched"
        );
        Ok(())
    }

    /// The accepted-command invariant (#542) extends to lifecycle operations:
    /// a `Running` lifecycle operation WITH a provider operation identity was
    /// accepted by the provider and its terminal evidence arrives through the
    /// agent event stream. The lifecycle sweep must NOT re-drive it — a
    /// re-dispatch would race the event stream on the same operation records.
    #[tokio::test]
    async fn lifecycle_convergence_sweep_skips_running_operations_with_provider_operation()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, operation_id, resource_id, provider_operation_id) =
            lifecycle_unknown_outcome_fixture("running-skip", provider.clone()).await?;
        // The accepted window: durably Running with the provider operation
        // identity, exactly as the reconciler persists an accepted in-flight
        // provider operation.
        store
            .update_operation(
                operation_id,
                o3k_store::OperationState::Running,
                Some(&provider_operation_id.to_string()),
                None,
                None,
            )
            .await?;

        service.drive_all_lifecycle_convergence().await?;

        assert_eq!(
            store.get_operation(operation_id).await?.state,
            o3k_store::OperationState::Running,
            "an in-flight lifecycle operation must never be re-driven"
        );
        assert_eq!(
            store.get_resource(resource_id).await?.observed_state,
            "ACTIVE",
            "an in-flight lifecycle operation must never be re-driven"
        );
        assert_eq!(
            provider.delete_calls(),
            0,
            "no provider dispatch may happen for an in-flight lifecycle operation"
        );
        Ok(())
    }

    /// Issue #88 S5 rerun, lifecycle analogue: a lifecycle operation marked
    /// `Retryable` by `retry_or_fail` (a delete dispatch rejected mid-flight
    /// when the agent was killed) must be re-driven by the lifecycle sweep —
    /// before the fix no periodic path ever re-dispatches it, so the
    /// scheduled retry never fires and the operation stays Retryable
    /// forever. The retry budget in `retry_or_fail` still bounds the re-drive
    /// (attempts >= max_attempts terminalizes Failed), and the deterministic
    /// `o3k-operation-{id}` idempotency key makes the re-dispatch idempotent
    /// at the provider.
    #[tokio::test]
    async fn lifecycle_convergence_sweep_re_drives_retryable_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, operation_id, resource_id, _provider_operation_id) =
            lifecycle_unknown_outcome_fixture("retryable-re-drive", provider.clone()).await?;
        store
            .update_operation(
                operation_id,
                o3k_store::OperationState::Retryable,
                None,
                Some("retryable"),
                None,
            )
            .await?;

        service.drive_all_lifecycle_convergence().await?;

        let operation = store.get_operation(operation_id).await?;
        assert_ne!(
            operation.state,
            o3k_store::OperationState::Retryable,
            "a Retryable lifecycle operation must be re-driven by the sweep"
        );
        assert!(
            matches!(
                operation.state,
                o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
            ),
            "the re-driven Retryable lifecycle operation must reach a terminal state, got {:?}",
            operation.state
        );
        assert_eq!(
            provider.delete_calls(),
            1,
            "the Retryable delete must be re-dispatched exactly once"
        );
        assert_eq!(
            store.get_resource(resource_id).await?.observed_state,
            "DELETED",
            "the re-driven delete must converge the resource"
        );
        Ok(())
    }

    /// Terminal lifecycle operations are the reconciler's sticky terminal
    /// predicate: the sweep must never touch a Succeeded or Failed lifecycle
    /// operation.
    #[tokio::test]
    async fn lifecycle_convergence_sweep_leaves_terminal_operations_untouched()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, _operation_id, _resource_id, _provider_operation_id) =
            lifecycle_unknown_outcome_fixture("terminal-untouched", provider.clone()).await?;
        let mut terminal_operations = Vec::new();
        for (label, state) in [
            ("succeeded", o3k_store::OperationState::Succeeded),
            ("failed", o3k_store::OperationState::Failed),
        ] {
            let resource_id = Uuid::now_v7();
            let operation_id = Uuid::now_v7();
            store
                .insert_resource(&o3k_store::ResourceRecord {
                    id: resource_id,
                    kind: "compute_instance".to_owned(),
                    project_id: "project-a".to_owned(),
                    generation: 1,
                    observed_generation: 1,
                    desired_state: "{}".to_owned(),
                    observed_state: "ACTIVE".to_owned(),
                    provider_id: Some(format!("fake-{label}-instance")),
                })
                .await?;
            service
                .journal
                .begin_lifecycle(resource_id, operation_id, LifecycleAction::Delete)
                .await
                .map_err(ComputeError::Reconcile)?;
            store
                .update_operation(
                    operation_id,
                    state,
                    Some(&operation_id.to_string()),
                    None,
                    None,
                )
                .await?;
            terminal_operations.push((operation_id, state));
        }

        service.drive_all_lifecycle_convergence().await?;

        assert_eq!(
            provider.delete_calls(),
            0,
            "terminal lifecycle operations must never be re-driven"
        );
        for (operation_id, state) in terminal_operations {
            assert_eq!(
                store.get_operation(operation_id).await?.state,
                state,
                "a terminal lifecycle operation must be untouched"
            );
        }
        Ok(())
    }

    /// Wraps the stateful fake provider with the agent-registry lifecycle of
    /// the issue-87 empty-registry defect: while the agent is in reconnect
    /// backoff no node is registered, so the create dispatch reports NotFound
    /// (the command can provably never be delivered); `register()` simulates
    /// the agent re-registering on a later sweep tick.
    struct EmptyRegistryUntilRegisteredProvider {
        inner: FakeComputeProvider,
        registered: AtomicBool,
        create_attempts: AtomicUsize,
    }

    impl EmptyRegistryUntilRegisteredProvider {
        fn new() -> Self {
            Self {
                inner: FakeComputeProvider::new(),
                registered: AtomicBool::new(false),
                create_attempts: AtomicUsize::new(0),
            }
        }

        fn register(&self) {
            self.registered.store(true, Ordering::SeqCst);
        }

        fn create_attempts(&self) -> usize {
            self.create_attempts.load(Ordering::SeqCst)
        }

        fn instance_count(&self) -> usize {
            self.inner.instance_count()
        }
    }

    #[async_trait]
    impl ComputeProvider for EmptyRegistryUntilRegisteredProvider {
        async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<Operation, ProviderError> {
            self.create_attempts.fetch_add(1, Ordering::SeqCst);
            if !self.registered.load(Ordering::SeqCst) {
                // No agent is registered: `selected_agent` fails before any
                // dispatch, so the create command was never delivered.
                return Err(ProviderError::NotFound);
            }
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<Instance, ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: DeleteInstanceRequest,
        ) -> Result<Operation, ProviderError> {
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<Operation, ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<Operation, ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// The issue-87 rerun timeline with the real periodic sweep: the crash
    /// residue is re-driven at +5s while the preserved agent is still in
    /// reconnect backoff (registry empty), and the agent registers only at
    /// +13.6s. The empty-registry drive must NOT mark the create terminal
    /// Failed — the command was never delivered — and once the agent
    /// registers, a later sweep tick must re-dispatch the create and converge
    /// to ACTIVE.
    #[tokio::test]
    async fn create_convergence_sweep_survives_empty_registry_until_agent_registers()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-empty-registry-sweep-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&path).await?);
        let provider = Arc::new(EmptyRegistryUntilRegisteredProvider::new());
        let service = ComputeService::new(store.clone(), provider.clone());
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "empty-registry-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: Vec::new(),
            // No scheduler/placement in this fixture: the residue is the
            // crash-before-dispatch shape without a placement binding.
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "empty-registry-request".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        // The crash-before-dispatch residue: `Running` with no provider
        // operation identity — the exact shape the sweep re-drives.
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Running,
                None,
                None,
                None,
            )
            .await?;

        let task = service.spawn_create_convergence_reconciler(1);
        // The first drive(s) hit the empty registry; the operation must stay
        // re-drivable and never become terminal Failed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while provider.create_attempts() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "create convergence sweep never attempted the create dispatch"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_ne!(
            store.get_operation(request.operation_id).await?.state,
            o3k_store::OperationState::Failed,
            "an undelivered create must not become terminal while the registry is empty"
        );
        // The agent re-registers (reconnect backoff completed); a later sweep
        // tick re-dispatches the create and converges to ACTIVE.
        provider.register();
        loop {
            let operation = store.get_operation(request.operation_id).await?;
            if operation.state == o3k_store::OperationState::Succeeded {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "create convergence sweep did not converge after the agent registered"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(provider.instance_count(), 1);
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

    /// Counts and records provider delete dispatches so tests can prove a
    /// local delete completion reaches the provider boundary exactly once,
    /// with the best-effort reap shape (server id as the provider instance
    /// identity, the deterministic delete operation id, and the dedicated
    /// reap idempotency key).
    struct RecordingDeleteProvider {
        inner: FakeComputeProvider,
        delete_calls: AtomicUsize,
        delete_requests: std::sync::Mutex<Vec<DeleteInstanceRequest>>,
    }

    impl RecordingDeleteProvider {
        fn new() -> Self {
            Self {
                inner: FakeComputeProvider::new(),
                delete_calls: AtomicUsize::new(0),
                delete_requests: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn delete_calls(&self) -> usize {
            self.delete_calls.load(Ordering::SeqCst)
        }

        fn delete_requests(&self) -> Vec<DeleteInstanceRequest> {
            self.delete_requests
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl ComputeProvider for RecordingDeleteProvider {
        async fn capabilities(&self) -> Result<Capabilities, ProviderError> {
            self.inner.capabilities().await
        }

        async fn create_instance(
            &self,
            request: CreateInstanceRequest,
        ) -> Result<Operation, ProviderError> {
            self.inner.create_instance(request).await
        }

        async fn get_instance(
            &self,
            provider_instance_id: &str,
        ) -> Result<Instance, ProviderError> {
            self.inner.get_instance(provider_instance_id).await
        }

        async fn delete_instance(
            &self,
            request: DeleteInstanceRequest,
        ) -> Result<Operation, ProviderError> {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            self.delete_requests
                .lock()
                .map(|mut guard| guard.push(request.clone()))
                .unwrap_or_default();
            self.inner.delete_instance(request).await
        }

        async fn action_instance(
            &self,
            provider_instance_id: &str,
            action: InstanceAction,
            operation_id: Uuid,
            idempotency_key: &str,
        ) -> Result<Operation, ProviderError> {
            self.inner
                .action_instance(provider_instance_id, action, operation_id, idempotency_key)
                .await
        }

        async fn get_operation(
            &self,
            provider_operation_id: Uuid,
        ) -> Result<Operation, ProviderError> {
            self.inner.get_operation(provider_operation_id).await
        }
    }

    /// Seeds a stranded terminal create failure (issue #87): the create
    /// operation is durably Failed with no provider operation identity (the
    /// dispatch never reached an agent — the empty-registry terminal catch),
    /// the resource projects ERROR, no provider reference exists, and the
    /// keypair and placement allocation are still held.
    #[allow(clippy::type_complexity)]
    async fn stranded_failed_create_fixture(
        label: &str,
        provider: Arc<RecordingDeleteProvider>,
    ) -> Result<
        (
            ComputeService,
            Arc<dyn ComputeRepository>,
            o3k_placement::PlacementLedger,
            CreateInstanceRequest,
        ),
        Box<dyn std::error::Error>,
    > {
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file; the decorated create
        // intent references an allocation that must already be committed in
        // the same store (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .attach_server_keypair(request.o3k_server_id, keypair.id)
            .await?;
        // The stranded terminal shape: Failed create with no provider
        // operation identity (the dispatch never reached an agent), resource
        // projected ERROR, no provider reference attached.
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Failed,
                None,
                Some("terminal"),
                Some("no agent was registered to receive the create dispatch"),
            )
            .await?;
        let resource = store.get_resource(request.o3k_server_id).await?;
        store
            .update_resource(
                resource.id,
                resource.generation,
                &resource.desired_state,
                server_state_to_storage(ServerState::Error),
                resource.generation,
                None,
            )
            .await?;
        Ok((service, store, placement, request))
    }

    /// A stranded create failure with no provider operation identity — the
    /// create never reached any agent, so no provider side effect can exist —
    /// must be deletable: the delete completes without a provider call, the
    /// delete operation is terminal Succeeded, the resource ends DELETED, and
    /// the reverse-order compensation (placement allocation, keypair, ports)
    /// runs. Today the missing provider reference 409s the delete and leaves
    /// the server stranded in ERROR.
    #[tokio::test]
    async fn delete_stranded_failed_create_without_provider_reference_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, placement, request) =
            stranded_failed_create_fixture("delete-no-ref", provider.clone()).await?;
        let generation_at_delete = store.get_resource(request.o3k_server_id).await?.generation;

        service
            .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;

        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "DELETED");
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            None,
            "the delete must run reverse-order compensation for the keypair"
        );
        assert!(
            placement.provider("node-a").await?.allocations.is_empty(),
            "the delete must release the placement allocation"
        );
        assert_eq!(
            provider.delete_calls(),
            1,
            "a create that never reached a provider must still get one \
             best-effort reap dispatch (the empty-registry terminal is a \
             clean provider NotFound)"
        );
        let delete_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:delete:project-a:{}:{}",
                request.o3k_server_id, generation_at_delete
            )
            .as_bytes(),
        );
        assert_eq!(
            store.get_operation(delete_operation_id).await?.state,
            o3k_store::OperationState::Succeeded,
            "the local delete must record a terminal Succeeded delete operation"
        );
        Ok(())
    }

    /// Issue #88 S3 residue: a create that WAS accepted by an agent (the
    /// config-drive transfers committed before acceptance) which then crashed
    /// before any libvirt mutation leaves the ConfigDriveIso manifests and
    /// content on the agent host with zero durable bindings. The
    /// local-completion delete proves absence and succeeds without a provider
    /// call — but the agent still holds the media and nothing else tells it
    /// to reap them. The local delete must now dispatch a BEST-EFFORT
    /// provider delete with the same deterministic delete operation identity
    /// (idempotent re-dispatch via the durable command record) and a
    /// dedicated reap idempotency key; the agent's delete executor reaps the
    /// config-drive media through its "domain already absent" arm. The
    /// provider call never changes the already-terminal local outcome.
    #[tokio::test]
    async fn locally_completed_delete_dispatches_best_effort_reap()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, _placement, request) =
            stranded_failed_create_fixture("delete-reap", provider.clone()).await?;
        let generation_at_delete = store.get_resource(request.o3k_server_id).await?.generation;

        service
            .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;

        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "DELETED");
        let delete_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:delete:project-a:{}:{}",
                request.o3k_server_id, generation_at_delete
            )
            .as_bytes(),
        );
        assert_eq!(
            store.get_operation(delete_operation_id).await?.state,
            o3k_store::OperationState::Succeeded,
            "the local delete must record a terminal Succeeded delete operation"
        );
        let requests = provider.delete_requests();
        assert_eq!(
            requests.len(),
            1,
            "the local-completion delete must dispatch exactly one best-effort reap"
        );
        assert_eq!(
            requests[0].provider_instance_id,
            request.o3k_server_id.to_string(),
            "the reap must address the never-defined resource by its server id"
        );
        assert_eq!(
            requests[0].operation_id, delete_operation_id,
            "the reap must reuse the deterministic delete operation identity"
        );
        assert_eq!(
            requests[0].idempotency_key,
            format!("o3k:delete-reap:{}", request.o3k_server_id),
            "the reap must carry the dedicated idempotency key"
        );
        Ok(())
    }
    /// before the crash — the operation carries a provider operation identity
    /// — but the durable presence inspection provably found no instance, and
    /// `converge_absent_create` recorded a terminal Failed operation with
    /// error_category "not_found" and the resource projected ERROR with no
    /// provider reference. Absence is proven by inspection, so no provider
    /// side effect can exist; the delete must complete locally exactly like
    /// the never-dispatched #550 shape: no provider call, resource DELETED,
    /// delete operation terminal Succeeded, and the reverse-order
    /// compensation (placement allocation, keypair, ports) runs. Today the
    /// provider_operation_id makes the local-completion gate 409 the delete
    /// even though the durable not_found evidence proves absence.
    #[tokio::test]
    async fn delete_proven_absent_failed_create_with_provider_operation_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, placement, request) =
            stranded_failed_create_fixture("delete-proven-absent", provider.clone()).await?;
        // Seed exactly what converge_absent_create leaves: terminal Failed,
        // the pre-crash provider operation identity preserved, error_category
        // "not_found" recording the absent presence inspection.
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Failed,
                Some(&request.operation_id.to_string()),
                Some("not_found"),
                Some("presence inspection: create never took effect; instance is absent"),
            )
            .await?;
        let generation_at_delete = store.get_resource(request.o3k_server_id).await?.generation;

        service
            .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;

        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "DELETED");
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            None,
            "the delete must run reverse-order compensation for the keypair"
        );
        assert!(
            placement.provider("node-a").await?.allocations.is_empty(),
            "the delete must release the placement allocation"
        );
        assert_eq!(
            provider.delete_calls(),
            1,
            "an absence-proven create must get exactly one best-effort reap \
             dispatch so the accepting agent reaps its config-drive media"
        );
        let delete_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:delete:project-a:{}:{}",
                request.o3k_server_id, generation_at_delete
            )
            .as_bytes(),
        );
        assert_eq!(
            store.get_operation(delete_operation_id).await?.state,
            o3k_store::OperationState::Succeeded,
            "the local delete must record a terminal Succeeded delete operation"
        );
        Ok(())
    }

    /// The issue-87 C-1 qemu-img shape: the create was accepted before the
    /// failure — the operation carries a provider operation identity — and
    /// then failed definitively before libvirt could define the domain:
    /// image materialization (qemu-img) failed, so absence is proven BY
    /// CONSTRUCTION and the agent records the category it uses for
    /// definitive pre-libvirt failures. No provider side effect can exist;
    /// the delete must complete locally exactly like the never-dispatched
    /// (#550) and presence-inspected (#554) shapes: no provider call,
    /// resource DELETED, delete operation terminal Succeeded, and the
    /// reverse-order compensation (placement allocation, keypair, ports)
    /// runs. Before the fix, the category the agent recorded for this path
    /// ("terminal") made the local-completion gate 409 the delete even
    /// though absence is proven.
    #[tokio::test]
    async fn delete_definitive_pre_libvirt_failed_create_with_provider_operation_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, placement, request) =
            stranded_failed_create_fixture("delete-definitive", provider.clone()).await?;
        // Seed exactly what the definitive pre-libvirt failure path leaves:
        // terminal Failed, the accepted provider operation identity
        // preserved, the absence-proven category, and the redacted qemu-img
        // materialization reason.
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Failed,
                Some(&request.operation_id.to_string()),
                Some("not_found"),
                Some("instance image overlay could not be realized"),
            )
            .await?;
        let generation_at_delete = store.get_resource(request.o3k_server_id).await?.generation;

        service
            .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;

        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(resource.observed_state, "DELETED");
        assert_eq!(
            store.get_server_keypair_name(request.o3k_server_id).await?,
            None,
            "the delete must run reverse-order compensation for the keypair"
        );
        assert!(
            placement.provider("node-a").await?.allocations.is_empty(),
            "the delete must release the placement allocation"
        );
        assert_eq!(
            provider.delete_calls(),
            1,
            "an absence-proven create must get exactly one best-effort reap \
             dispatch so the accepting agent reaps its config-drive media"
        );
        let delete_operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:delete:project-a:{}:{}",
                request.o3k_server_id, generation_at_delete
            )
            .as_bytes(),
        );
        assert_eq!(
            store.get_operation(delete_operation_id).await?.state,
            o3k_store::OperationState::Succeeded,
            "the local delete must record a terminal Succeeded delete operation"
        );
        Ok(())
    }

    /// Invariant pin (#550 rationale): a terminal Failed create that carries
    /// a provider operation identity but whose durable error category is NOT
    /// "not_found" has no absence proof — the provider may hold side effects
    /// (a real dispatch failure, a created-then-errored instance) that only
    /// the provider delete can remove. The delete must still fail closed with
    /// a conflict; the proven-absence exception must not weaken the guard for
    /// every Failed shape.
    #[tokio::test]
    async fn failed_create_with_provider_operation_and_terminal_category_still_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(RecordingDeleteProvider::new());
        let (service, store, _placement, request) =
            stranded_failed_create_fixture("delete-terminal-guard", provider.clone()).await?;
        store
            .update_operation(
                request.operation_id,
                o3k_store::OperationState::Failed,
                Some(&request.operation_id.to_string()),
                Some("terminal"),
                Some("provider operation failed"),
            )
            .await?;

        assert!(matches!(
            service
                .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
                .await,
            Err(ComputeError::Conflict)
        ));
        assert_eq!(
            provider.delete_calls(),
            0,
            "the conflicted delete must not dispatch a provider delete"
        );
        Ok(())
    }

    /// The accepted-command invariant (#542/#549): a create with a provider
    /// operation identity was accepted and must never be re-driven by the
    /// convergence path, and its delete must still fail closed on the missing
    /// provider reference — the provider may hold side effects that only the
    /// provider delete can remove. Pins that the empty-registry and
    /// never-created delete fixes do not weaken either invariant.
    #[tokio::test]
    async fn accepted_create_is_never_redriven_and_delete_guard_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let fake = Arc::new(FakeComputeProvider::new());
        let (service, store, request) =
            crash_before_dispatch_fixture("accepted-guard", fake.clone()).await?;
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
        assert_eq!(
            fake.instance_count(),
            0,
            "an accepted create must never be re-dispatched"
        );
        assert!(matches!(
            service
                .delete_server("project-a", ServerId::from_uuid(request.o3k_server_id))
                .await,
            Err(ComputeError::Conflict)
        ));
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
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
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

        // Terminal states are sticky in both the operation and command
        // journals: conflicting later evidence fails closed and cannot
        // trigger another binding projection.
        let succeeded = AgentOperationUpdate {
            operation_sequence: 2,
            state: AgentOperationState::Succeeded,
            ..failed.clone()
        };
        assert!(matches!(
            service.apply_agent_update(&succeeded).await,
            Err(ComputeError::Reconcile(ReconcileError::InvalidIntent))
        ));
        assert_eq!(projector_calls(&projector).len(), 4);
        assert_eq!(
            store
                .get_agent_command_by_operation(request.operation_id)
                .await?
                .state,
            o3k_store::AgentCommandState::Failed
        );
        std::fs::remove_file(database_path)?;
        Ok(())
    }

    /// Issue #606: an agent-side capacity rejection (the create arm's
    /// disk-capacity backstop) must land in the durable ledger with the same
    /// `capacity` classification the placement gate produces, so the failed
    /// operation is indistinguishable from a placement rejection.
    #[tokio::test]
    async fn capacity_classified_agent_failure_persists_capacity_category()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-capacity-projection-{}.sqlite",
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
            name: "capacity-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 10,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: vec!["port-1".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "capacity-projection".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
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
        let failed = AgentOperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: request.operation_id,
            resource_id: request.o3k_server_id,
            state: AgentOperationState::Failed,
            error_category: Some(AgentErrorCategory::Capacity),
            redacted_message: Some(
                "create requires 10 GiB disk but the agent capacity is 1 GiB".to_owned(),
            ),
            provider_resource_id: None,
        };
        assert_eq!(
            service.apply_agent_update(&failed).await?,
            o3k_store::OperationState::Failed
        );
        let operation = store.get_operation(request.operation_id).await?;
        assert_eq!(operation.state, o3k_store::OperationState::Failed);
        assert_eq!(
            operation.error_category.as_deref(),
            Some("capacity"),
            "the durable operation must carry the placement-gate category"
        );
        assert!(
            operation
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("10 GiB"))
        );
        assert_eq!(
            store
                .get_agent_command_by_operation(request.operation_id)
                .await?
                .state,
            o3k_store::AgentCommandState::Failed
        );
        let resource = store.get_resource(request.o3k_server_id).await?;
        assert_eq!(
            server_state_from_storage(&resource.observed_state)?,
            ServerState::Error,
            "a terminally failed create must project the durable ERROR state"
        );
        assert_eq!(
            projector_calls(&projector),
            vec![ProjectorCall::CreateOutcome {
                project: "project-a".to_owned(),
                port: "port-1".to_owned(),
                succeeded: false,
            }]
        );
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

    fn config_drive_input(instance_id: &str) -> o3k_config_drive::ConfigDriveInput {
        o3k_config_drive::ConfigDriveInput {
            instance_id: instance_id.to_owned(),
            hostname: "cd-server".to_owned(),
            ssh_public_key: "ssh-ed25519 AAAA test@example".to_owned(),
            user_data: b"#cloud-config\nhostname: cd-server\n".to_vec(),
            metadata: BTreeMap::new(),
            network_data: BTreeMap::new(),
            vendor_data: None,
        }
    }

    /// Publishes an ISO pair that satisfies the config-drive ownership
    /// contract (managed_by `o3k-config-drive-iso`, schema_version 1,
    /// matching instance id and output name). Used to assert that terminal
    /// deletes reap the transfer-source media without invoking an ISO builder.
    fn publish_config_drive_iso_pair(
        root: &Path,
        instance_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let iso = root.join(format!("{instance_id}.iso"));
        std::fs::write(&iso, b"owned-iso-bytes")?;
        let manifest = serde_json::json!({
            "schema_version": 1,
            "managed_by": "o3k-config-drive-iso",
            "instance_id": instance_id,
            "source_fingerprint_sha256": "0".repeat(64),
            "artifact_fingerprint_sha256": "0".repeat(64),
            "output_name": format!("{instance_id}.iso"),
        });
        std::fs::write(
            root.join(format!("{instance_id}.iso.o3k-iso-ownership.json")),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_delete_reaps_owned_config_drive_media()
    -> Result<(), Box<dyn std::error::Error>> {
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-config-drive-reap-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&database_path).await?);
        let config_drive_root =
            std::env::temp_dir().join(format!("o3k-compute-config-drive-{}", Uuid::now_v7()));
        let config_drive = o3k_config_drive::ConfigDriveStore::open(&config_drive_root)?;
        let service = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()))
            .with_config_drive_cleaner(config_drive.clone());

        // The already-deleted shortcut: a server whose delete completed in a
        // previous run still owns config-drive media on this control plane.
        let shortcut_id = Uuid::now_v7();
        let shortcut_request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: shortcut_id,
            project_id: "project-a".to_owned(),
            name: "shortcut".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 0,
            image_id: Some("image-1".to_owned()),
            key_name: None,
            keypair_id: None,
            network_ids: Vec::new(),
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "shortcut-cd".to_owned(),
        };
        config_drive.generate(&config_drive_input(&shortcut_id.to_string()))?;
        publish_config_drive_iso_pair(&config_drive_root, &shortcut_id.to_string())?;
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: shortcut_id,
                kind: "compute_instance".to_owned(),
                project_id: "project-a".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: serde_json::to_string(&shortcut_request)?,
                observed_state: server_state_to_storage(ServerState::Deleted).to_owned(),
                provider_id: None,
            })
            .await?;
        service
            .delete_server("project-a", ServerId::from_uuid(shortcut_id))
            .await?;
        assert!(
            !config_drive_root.join(shortcut_id.to_string()).exists(),
            "the already-deleted shortcut must reap the config-drive directory"
        );
        assert!(
            !config_drive_root
                .join(format!("{shortcut_id}.iso"))
                .exists(),
            "the already-deleted shortcut must reap the config-drive ISO"
        );
        assert!(
            !config_drive_root
                .join(format!("{shortcut_id}.iso.o3k-iso-ownership.json"))
                .exists(),
            "the already-deleted shortcut must reap the config-drive ISO manifest"
        );

        // The terminal projection path (delete completed through the provider
        // and reconciler) reaps the media too, and a repeated delete takes
        // the already-deleted shortcut without disturbing the reaping.
        let server = service
            .create_server(
                "project-a",
                "cd-server".to_owned(),
                "image-1".to_owned(),
                service.flavors()[0].id,
                vec!["network-1".to_owned()],
                "terminal-cd".to_owned(),
            )
            .await?;
        let live_id = server.id.as_uuid();
        config_drive.generate(&config_drive_input(&live_id.to_string()))?;
        publish_config_drive_iso_pair(&config_drive_root, &live_id.to_string())?;
        service.delete_server("project-a", server.id).await?;
        assert!(!config_drive_root.join(live_id.to_string()).exists());
        assert!(!config_drive_root.join(format!("{live_id}.iso")).exists());
        assert!(
            !config_drive_root
                .join(format!("{live_id}.iso.o3k-iso-ownership.json"))
                .exists()
        );
        service.delete_server("project-a", server.id).await?;

        std::fs::remove_dir_all(&config_drive_root)?;
        std::fs::remove_file(&database_path)?;
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
        service
            .store
            .insert_agent_command(&o3k_store::AgentCommandRecord {
                command_id: format!("command-{}", request.operation_id),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id,
                resource_id: request.o3k_server_id,
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                payload_fingerprint_sha256: "0".repeat(64),
                payload: Vec::new(),
                state: o3k_store::AgentCommandState::Succeeded,
                accepted_sequence: 1,
                last_sequence: 1,
                provider_operation_id: None,
                provider_resource_id: None,
            })
            .await?;
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-scheduler-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(raw_store.clone());
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
        let service = ComputeService::new(
            Arc::new(raw_store) as Arc<dyn ComputeRepository>,
            Arc::new(FakeComputeProvider::new()),
        )
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-duplicate-name-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(raw_store.clone());
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
        let service = ComputeService::new(
            Arc::new(raw_store) as Arc<dyn ComputeRepository>,
            Arc::new(FakeComputeProvider::new()),
        )
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-create-race-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(raw_store.clone());
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
        let service = ComputeService::new(
            Arc::new(raw_store) as Arc<dyn ComputeRepository>,
            Arc::new(FakeComputeProvider::new()),
        )
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
        // Exactly one request wins; the loser either converges on the winner's
        // server (begin_create's ResourceAlreadyExists path is idempotent and
        // a show-path re-drive short-circuits on the first-writer terminal
        // outcome) or surfaces the name/placement collision as a Conflict.
        // No other error is legitimate: a stale provider generation cannot
        // produce NoValidHost here (both providers fit, and same-id commits
        // short-circuit in the store), so Scheduler errors must not be masked.
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

    /// One-line TestLab recreate contract (issue #613 blocker B): a create
    /// with the same name and idempotency key after a COMPLETED delete must
    /// converge — the prior lifecycle is terminal and the name is free —
    /// instead of conflicting. The deterministic server identity is reused
    /// and a fresh lifecycle operation is started; a differing network intent
    /// is part of the new lifecycle, not a conflicting retry of the old one.
    #[tokio::test]
    async fn create_after_completed_delete_reuses_identity_and_converges()
    -> Result<(), ComputeError> {
        let service = service("recreate-after-delete").await?;
        let flavor = service.flavors()[0].id;
        let first = service
            .create_server(
                "project-a",
                "recreate-vm".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "recreate-vm".to_owned(),
            )
            .await?;
        service.delete_server("project-a", first.id).await?;
        assert!(
            service.list_servers("project-a").await?.is_empty(),
            "the completed delete must leave no visible server"
        );
        // The recreation carries a new network intent; the completed delete
        // must make the deterministic identity free for a new lifecycle.
        let second = service
            .create_server(
                "project-a",
                "recreate-vm".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-2".to_owned()],
                "recreate-vm".to_owned(),
            )
            .await?;
        assert_eq!(
            second.id, first.id,
            "the deterministic server identity is reused across lifecycles"
        );
        assert_eq!(second.state, ServerState::Active);
        Ok(())
    }

    #[test]
    fn server_create_identity_is_stable_and_scope_bound() {
        let first = ComputeService::server_id_for_create("project-a", "request-1");
        assert_eq!(
            first,
            ComputeService::server_id_for_create("project-a", "request-1")
        );
        assert_ne!(
            first,
            ComputeService::server_id_for_create("project-b", "request-1")
        );
        assert_ne!(
            first,
            ComputeService::server_id_for_create("project-a", "request-2")
        );
    }

    /// A retry of the recreation (a crash between the revive persist and the
    /// provider dispatch, or a plain caller retry) must recompute the same
    /// fresh lifecycle identity from the tombstone fence and converge on the
    /// persisted row instead of conflicting or dispatching a second create.
    #[tokio::test]
    async fn recreated_server_retry_converges_on_the_persisted_revive() -> Result<(), ComputeError>
    {
        let fake = Arc::new(FakeComputeProvider::new());
        let path = PathBuf::from(format!(
            "/tmp/o3k-compute-recreate-retry-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store: Arc<dyn ComputeRepository> =
            Arc::new(o3k_store::testkit::open_file(&path).await?);
        let service = ComputeService::new(store, fake.clone());
        let flavor = service.flavors()[0].id;
        let first = service
            .create_server(
                "project-a",
                "recreate-vm".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-1".to_owned()],
                "recreate-vm".to_owned(),
            )
            .await?;
        service.delete_server("project-a", first.id).await?;
        let second = service
            .create_server(
                "project-a",
                "recreate-vm".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-2".to_owned()],
                "recreate-vm".to_owned(),
            )
            .await?;
        assert_eq!(second.id, first.id);
        assert_eq!(second.state, ServerState::Active);
        let retry = service
            .create_server(
                "project-a",
                "recreate-vm".to_owned(),
                "image-1".to_owned(),
                flavor,
                vec!["network-2".to_owned()],
                "recreate-vm".to_owned(),
            )
            .await?;
        assert_eq!(
            retry.id, first.id,
            "the retry must converge on the revived identity"
        );
        assert_eq!(retry.state, ServerState::Active);
        assert_eq!(
            fake.instance_count(),
            1,
            "the retry must not dispatch a second provider create"
        );
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-delete-release-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let fail_releases = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(FailingReleaseRepository {
                inner: raw_store.clone(),
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
        let service = ComputeService::new(
            Arc::new(raw_store) as Arc<dyn ComputeRepository>,
            Arc::new(FakeComputeProvider::new()),
        )
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let database_path = PathBuf::from(format!(
            "/tmp/o3k-compute-registry-gate-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(raw_store.clone());
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

        let service = ComputeService::new(
            Arc::new(raw_store) as Arc<dyn ComputeRepository>,
            Arc::new(FakeComputeProvider::new()),
        )
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let provider = Arc::new(FakeComputeProvider::new());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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
        // The production composition keeps the compute store and the
        // placement ledger on one durable SQLite file (ASR-018 ordering).
        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let provider = Arc::new(FakeComputeProvider::new());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
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

    fn test_compute_auth(project_id: &str, user_id: &str, role: &str) -> AuthContext {
        AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked(user_id),
                user_id,
                Some("default".to_string()),
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(project_id),
                Some(project_id.to_string()),
                Some("default".to_string()),
            ),
            vec![role.to_string()],
            1000,
            5000,
            uuid::Uuid::now_v7().to_string(),
            uuid::Uuid::now_v7().to_string(),
            None,
        )
    }

    #[tokio::test]
    async fn compute_service_authorization_enforcement() -> Result<(), Box<dyn std::error::Error>> {
        let service = service("compute-auth-test").await?;
        let member_auth = test_compute_auth("proj-a", "user-1", "member");
        let reader_auth = test_compute_auth("proj-a", "user-2", "reader");
        let other_auth = test_compute_auth("proj-b", "user-3", "member");

        // With standard authorizer, unauthorized action when empty authorizer is used
        let empty_service = service
            .clone()
            .with_authorizer(Arc::new(StaticAuthorizer::empty()));
        let empty_create = empty_service
            .create_flavor_for_auth(&reader_auth, "custom-flavor".to_owned(), 2, 2048, 10)
            .await;
        assert!(matches!(empty_create, Err(ComputeError::Unauthorized)));

        // Member can list flavors with standard authorizer
        let flavors = service.flavors_for_auth(&member_auth).await?;
        assert!(!flavors.is_empty());

        // Member can import a keypair
        let kp = service
            .create_keypair_for_auth(
                &member_auth,
                "my-key".to_owned(),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test".to_owned(),
            )
            .await?;
        assert_eq!(kp.name, "my-key");

        // Member can list keypairs
        let kps = service.list_keypairs_for_auth(&member_auth).await?;
        assert_eq!(kps.len(), 1);

        // Foreign project cannot read or delete the keypair
        let foreign_get = service.show_keypair_for_auth(&other_auth, "my-key").await;
        assert!(matches!(foreign_get, Err(ComputeError::NotFound)));

        let foreign_del = service.delete_keypair_for_auth(&other_auth, "my-key").await;
        assert!(matches!(foreign_del, Err(ComputeError::NotFound)));

        // Owner can read and delete keypair
        let owner_kp = service
            .show_keypair_for_auth(&member_auth, "my-key")
            .await?;
        assert_eq!(owner_kp.name, "my-key");
        service
            .delete_keypair_for_auth(&member_auth, "my-key")
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_create_replays_one_compute_operation()
    -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::DurableStore;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "native".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        let context = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"native","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let first = service
            .create_server_for_auth_canonical(&auth, input.clone(), context.clone())
            .await?;
        let replay = service
            .create_server_for_auth_canonical(&auth, input, context)
            .await?;
        assert_eq!(first.operation_id, replay.operation_id);
        assert_eq!(first.resource.id, replay.resource.id);
        assert_eq!(
            store
                .list_resources("project-a", "compute_instance")
                .await?
                .len(),
            1
        );
        let public = o3k_kernel::Operation::try_from(
            store.get_canonical_operation(first.operation_id).await?,
        )?;
        assert_eq!(public.action.to_string(), "compute:CreateServer");
        if matches!(
            public.state,
            o3k_kernel::OperationState::Succeeded | o3k_kernel::OperationState::Failed
        ) {
            assert!(public.finished_at.is_some());
        }
        assert_eq!(
            public.resource_id.as_ref().map(ToString::to_string),
            Some(first.resource.id.to_string())
        );
        assert_eq!(provider.instance_count(), 1);
        let delete_context = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":first.resource.id.to_string()}),
        )?;
        let deleted = service
            .delete_server_for_auth_canonical(&auth, first.resource.id, delete_context.clone())
            .await?;
        let delete_replay = service
            .delete_server_for_auth_canonical(&auth, first.resource.id, delete_context)
            .await?;
        assert_eq!(deleted.operation_id, delete_replay.operation_id);
        let public_delete = o3k_kernel::Operation::try_from(
            store.get_canonical_operation(deleted.operation_id).await?,
        )?;
        assert_eq!(public_delete.action.to_string(), "compute:DeleteServer");
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_create_delete_same_key_no_revive()
    -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::DurableStore;
        // CANONICAL INVARIANT: a deleted resource must NOT be revived into a
        // new lifecycle when the SAME canonical Idempotency-Key is reused.
        // The original key represents the original accepted mutation; the
        // caller must use a NEW key to create a subsequent lifecycle.
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "same-key".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        let create_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"same-key","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        // 1. Create the server
        let created = service
            .create_server_for_auth_canonical(&auth, input.clone(), create_ctx.clone())
            .await?;
        assert_eq!(provider.instance_count(), 1);
        // 2. Delete the server
        let delete_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":created.resource.id.to_string()}),
        )?;
        service
            .delete_server_for_auth_canonical(&auth, created.resource.id, delete_ctx)
            .await?;
        assert_eq!(provider.instance_count(), 0);
        // 3. Retry create with SAME key — must fail closed (the original key
        //    represents the original accepted mutation; recreating requires a
        //    new Idempotency-Key).
        let retry = service
            .create_server_for_auth_canonical(&auth, input, create_ctx)
            .await;
        assert!(
            matches!(
                retry,
                Err(ComputeError::Conflict) | Err(ComputeError::NotFound)
            ),
            "retry with same key after delete must fail closed, got {:?}",
            retry
        );
        // 4. No duplicate provider mutation occurred
        assert_eq!(provider.instance_count(), 0);
        // 5. No second resource was created
        let resources = store
            .list_resources("project-a", "compute_instance")
            .await?;
        assert_eq!(resources.len(), 1, "only the original tombstone must exist");
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_create_delete_new_key_creates()
    -> Result<(), Box<dyn std::error::Error>> {
        // CANONICAL INVARIANT: a NEW Idempotency-Key after delete creates the
        // next lifecycle correctly.
        use o3k_store::DurableStore;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input_a = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "first-srv".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        let ctx_a = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"first-srv","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        // 1. Create first server
        let first = service
            .create_server_for_auth_canonical(&auth, input_a, ctx_a)
            .await?;
        assert_eq!(provider.instance_count(), 1);
        // 2. Delete it
        let del_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":first.resource.id.to_string()}),
        )?;
        service
            .delete_server_for_auth_canonical(&auth, first.resource.id, del_ctx)
            .await?;
        assert_eq!(provider.instance_count(), 0);
        // 3. Create second server with NEW key and a different deterministic
        //    server_id (derived from the new idempotency key).
        let input_b = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "second-srv".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-B".into(),
        };
        let ctx_b = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-B".into(),
            serde_json::json!({"spec":{"name":"second-srv","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let second = service
            .create_server_for_auth_canonical(&auth, input_b, ctx_b)
            .await?;
        assert!(
            second.resource.id != first.resource.id,
            "new key must produce a different server_id"
        );
        assert_eq!(provider.instance_count(), 1);
        // 4. Verify two resource rows exist (one tombstone, one active)
        let resources = store
            .list_resources("project-a", "compute_instance")
            .await?;
        assert_eq!(resources.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_create_context_actor_mismatch_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        // CANONICAL INVARIANT: a context with mismatched actor is rejected.
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "mismatch".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        // Context with a DIFFERENT actor than the auth principal
        let bad_actor = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-evil".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"mismatch","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let result = service
            .create_server_for_auth_canonical(&auth, input.clone(), bad_actor)
            .await;
        assert!(
            matches!(result, Err(ComputeError::Unauthorized)),
            "mismatched actor must be Unauthorized, got {:?}",
            result
        );
        let bad_scope_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked("different-project"),
                None,
                None,
            ),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"mismatch","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let result2 = service
            .create_server_for_auth_canonical(&auth, input, bad_scope_ctx)
            .await;
        assert!(
            matches!(result2, Err(ComputeError::Unauthorized)),
            "mismatched scope must be Unauthorized, got {:?}",
            result2
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_delete_returns_state_not_conflict()
    -> Result<(), Box<dyn std::error::Error>> {
        // CANONICAL INVARIANT: a canonical delete returns the actual operation
        // state rather than unconditionally converting non-terminal to
        // Conflict, enabling the native API to produce a real 202.
        //
        // This test uses a fake provider that returns a non-terminal state
        // (Accepted), proving the first async delete request returns the
        // non-terminal state instead of ComputeError::Conflict.
        use o3k_provider::FailureInjection;
        use o3k_store::DurableStore;
        let fake = Arc::new(FakeComputeProvider::new());
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ComputeService::new(store.clone(), fake.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "async-delete".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        let create_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"async-delete","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        // 1. Create server normally (no failure injection)
        let created = service
            .create_server_for_auth_canonical(&auth, input, create_ctx)
            .await?;
        // 2. Set provider to Timeout so delete stays non-terminal on first pass
        fake.set_failure(FailureInjection::Timeout)?;
        let delete_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":created.resource.id.to_string()}),
        )?;
        let delete_receipt = service
            .delete_server_for_auth_canonical(&auth, created.resource.id, delete_ctx)
            .await?;
        // The delete must return a non-terminal state (UnknownOutcome — the
        // provider timed out) instead of erroring with Conflict.
        assert_eq!(
            delete_receipt.operation_state,
            o3k_store::OperationState::UnknownOutcome,
            "delete with timing-out provider must be UnknownOutcome"
        );
        // 4. GET /operations/O returns the canonical operation
        let public_op = o3k_kernel::Operation::try_from(
            store
                .get_canonical_operation(delete_receipt.operation_id)
                .await?,
        )?;
        assert_eq!(public_op.action.to_string(), "compute:DeleteServer");
        assert_eq!(
            public_op.resource_id.as_ref().map(ToString::to_string),
            Some(created.resource.id.to_string())
        );
        assert_eq!(
            public_op.state,
            o3k_kernel::OperationState::from(delete_receipt.operation_state)
        );
        // 5. Replay returns the SAME operation
        let delete_ctx2 = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":created.resource.id.to_string()}),
        )?;
        let replay = service
            .delete_server_for_auth_canonical(&auth, created.resource.id, delete_ctx2)
            .await?;
        assert_eq!(
            replay.operation_id, delete_receipt.operation_id,
            "replay must return same operation_id"
        );
        // 6. Clear failure and let delete converge
        fake.set_failure(FailureInjection::None)?;
        // Drive convergence
        let _ = service
            .journal
            .reconcile_lifecycle_once(delete_receipt.operation_id)
            .await;
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_create_action_mismatch_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        // CANONICAL INVARIANT: create_server_for_auth_canonical requires
        // context.action == compute:CreateServer; a different action returns
        // InvalidRequest.
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "action-test".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        // Context with DeleteServer action on a create call
        let bad_action = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "DeleteServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"action-test","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let result = service
            .create_server_for_auth_canonical(&auth, input, bad_action)
            .await;
        assert!(
            matches!(result, Err(ComputeError::InvalidRequest)),
            "action mismatch must be InvalidRequest, got {:?}",
            result
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_native_delete_action_mismatch_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        // CANONICAL INVARIANT: delete_server_for_auth_canonical requires
        // context.action == compute:DeleteServer; a different action returns
        // InvalidRequest.
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());
        let auth = test_compute_auth("project-a", "user-a", "member");
        // First create a server so we have something to pass to delete
        let input = ServerCreateInput {
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            name: "del-action".into(),
            image_id: "image-a".into(),
            flavor_id: Uuid::from_u128(1),
            network_ids: vec!["network-a".into()],
            key_name: None,
            config_drive: None,
            idempotency_key: "create-A".into(),
        };
        let create_ctx = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "create-A".into(),
            serde_json::json!({"spec":{"name":"del-action","image_id":"image-a","flavor_id":Uuid::from_u128(1),"network_ids":["network-a"]}}),
        )?;
        let created = service
            .create_server_for_auth_canonical(&auth, input, create_ctx)
            .await?;
        // Context with CreateServer action on a delete call
        let bad_action = o3k_reconciler::CanonicalMutationContext::new(
            ActionId::new("compute", "CreateServer")?,
            "user-a".into(),
            auth.effective_scope().clone(),
            None,
            "delete-A".into(),
            serde_json::json!({"resource_id":created.resource.id.to_string()}),
        )?;
        let result = service
            .delete_server_for_auth_canonical(&auth, created.resource.id, bad_action)
            .await;
        assert!(
            matches!(result, Err(ComputeError::InvalidRequest)),
            "action mismatch must be InvalidRequest, got {:?}",
            result
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_operation_attempt_synchronization() -> Result<(), Box<dyn std::error::Error>>
    {
        use o3k_store::DurableStore;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let resource_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        // Insert a resource first
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: resource_id,
                kind: "compute:server".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 0,
                desired_state: "active".into(),
                observed_state: "active".into(),
                provider_id: None,
            })
            .await?;
        // Create canonical operation via the store
        let operation = o3k_store::OperationRecord {
            id: operation_id,
            resource_id,
            kind: "lifecycle:create".into(),
            state: o3k_store::OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let canonical = o3k_store::CanonicalOperationRecord {
            id: operation_id,
            service: "compute".into(),
            action: "compute:CreateServer".into(),
            actor: "user-a".into(),
            owner_scope: "project-a".into(),
            resource_type: "compute:server".into(),
            resource_id: Some(resource_id.to_string()),
            state: o3k_store::OperationState::Pending,
            attempt: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            error: None,
            request_id: Some("req".into()),
        };
        let request = o3k_store::IdempotencyReservationRequest::from_semantics(
            "project-a",
            "compute:CreateServer",
            "attempt-sync-key",
            "compute:server",
            None,
            &serde_json::json!({"name":"sync"}),
            operation_id,
        )?;
        store
            .create_or_replay_canonical_idempotent_operation(&operation, &canonical, &request)
            .await?;

        // Verify initial attempt = 0
        let meta = store.get_canonical_operation(operation_id).await?;
        assert_eq!(meta.attempt, 0, "initial canonical attempt must be 0");

        // First increment
        let retry = store.increment_operation_retry(operation_id).await?;
        assert_eq!(retry, 1, "first retry count must be 1");
        let meta = store.get_canonical_operation(operation_id).await?;
        assert_eq!(
            meta.attempt, 1,
            "canonical attempt must be 1 after first increment"
        );

        // Second increment
        let retry2 = store.increment_operation_retry(operation_id).await?;
        assert_eq!(retry2, 2, "second retry count must be 2");
        let meta2 = store.get_canonical_operation(operation_id).await?;
        assert_eq!(
            meta2.attempt, 2,
            "canonical attempt must be 2 after second increment"
        );

        Ok(())
    }

    #[tokio::test]
    async fn legacy_operation_retry_no_canonical_metadata() -> Result<(), Box<dyn std::error::Error>>
    {
        use o3k_store::DurableStore;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let resource_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: resource_id,
                kind: "compute_instance".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 0,
                desired_state: "active".into(),
                observed_state: "active".into(),
                provider_id: None,
            })
            .await?;
        store
            .insert_operation(&o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:create".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            })
            .await?;
        // Legacy operation — no canonical_operation_metadata exists

        let retry = store.increment_operation_retry(operation_id).await?;
        assert_eq!(retry, 1, "legacy retry must still work");

        // No canonical metadata should exist
        assert!(
            matches!(
                store.get_canonical_operation(operation_id).await,
                Err(o3k_store::StoreError::OperationNotFound)
            ),
            "legacy operation must not have canonical metadata fabricated"
        );

        Ok(())
    }

    #[tokio::test]
    async fn compute_quota_enforcement_and_isolation() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::QuotaRepository;

        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::default());
        let service = ComputeService::new(store.clone(), provider.clone());

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-a"), None, None);

        // Limit proj-a to 1 server
        store
            .set_limit(
                &scope_a,
                &LimitKey::compute_servers(),
                LimitValue::Maximum(1),
            )
            .await?;

        let auth_a = test_compute_auth("proj-a", "user-1", "member");
        let auth_b = test_compute_auth("proj-b", "user-2", "member");

        let flavors = service.flavors_for_auth(&auth_a).await?;
        let flavor_id = flavors[0].id;

        // 1. First server for proj-a succeeds
        let server1 = service
            .create_server_for_auth(
                &auth_a,
                ServerCreateInput {
                    user_id: "user-1".to_owned(),
                    project_id: "proj-a".to_owned(),
                    name: "srv-1".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-1".to_owned(),
                },
            )
            .await?;
        assert_eq!(server1.name, "srv-1");

        // 2. Second server for proj-a fails with QuotaExceeded
        let res2 = service
            .create_server_for_auth(
                &auth_a,
                ServerCreateInput {
                    user_id: "user-1".to_owned(),
                    project_id: "proj-a".to_owned(),
                    name: "srv-2".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-2".to_owned(),
                },
            )
            .await;
        assert!(matches!(res2, Err(ComputeError::QuotaExceeded { .. })));

        // 3. Proj-b can create server because its quota is Unlimited (tenant isolation)
        let server_b = service
            .create_server_for_auth(
                &auth_b,
                ServerCreateInput {
                    user_id: "user-2".to_owned(),
                    project_id: "proj-b".to_owned(),
                    name: "srv-b1".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-b1".to_owned(),
                },
            )
            .await?;
        assert_eq!(server_b.name, "srv-b1");

        // 4. Deleting server1 frees quota for proj-a
        service.delete_server_for_auth(&auth_a, server1.id).await?;

        // Now srv-2 can be created
        let server2 = service
            .create_server_for_auth(
                &auth_a,
                ServerCreateInput {
                    user_id: "user-1".to_owned(),
                    project_id: "proj-a".to_owned(),
                    name: "srv-2".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-2".to_owned(),
                },
            )
            .await?;
        assert_eq!(server2.name, "srv-2");

        Ok(())
    }

    #[tokio::test]
    async fn compute_unknown_outcome_retains_quota_until_convergence()
    -> Result<(), Box<dyn std::error::Error>> {
        use o3k_provider::FailureInjection;

        let fake = Arc::new(FakeComputeProvider::new());
        fake.set_failure(FailureInjection::Timeout)?;
        let (service, store, _placement, request, provider_operation_id, _instance_id) =
            unknown_outcome_create_fixture("quota-unknown", fake.clone()).await?;

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("project-a"), None, None);
        let auth_a = test_compute_auth("project-a", "user-1", "member");

        // Limit project-a to 1 server
        store
            .set_limit(
                &scope_a,
                &LimitKey::compute_servers(),
                LimitValue::Maximum(1),
            )
            .await?;

        let flavors = service.flavors_for_auth(&auth_a).await?;
        let flavor_id = flavors[0].id;

        // 1. While server 1 is in UnknownOutcome, attempt to create server 2 is denied (QuotaExceeded)
        let create_2_res = service
            .create_server_for_auth(
                &auth_a,
                ServerCreateInput {
                    user_id: "user-1".to_owned(),
                    project_id: "project-a".to_owned(),
                    name: "srv-unk-2".to_owned(),
                    image_id: "image-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["port-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-unk-2".to_owned(),
                },
            )
            .await;
        assert!(matches!(
            create_2_res,
            Err(ComputeError::QuotaExceeded { .. })
        ));

        // 2. Provider clears failure, convergence drives server 1 to active
        fake.set_operation_provider_resource_id(provider_operation_id, Some(_instance_id))?;
        fake.set_failure(FailureInjection::None)?;

        let server = service
            .show_server("project-a", ServerId::from_uuid(request.o3k_server_id))
            .await?;
        assert_eq!(server.state, ServerState::Active);

        // 3. Delete server 1 releases quota
        service
            .delete_server_for_auth(&auth_a, ServerId::from_uuid(request.o3k_server_id))
            .await?;

        let usage = store
            .get_usage(&scope_a, &LimitKey::compute_servers())
            .await?;
        assert_eq!(
            usage.total_consumed(),
            0,
            "expected total consumed to be 0 after delete, got in_use={} reserved={}",
            usage.in_use,
            usage.reserved
        );

        // 4. Now server 2 creation succeeds
        let server2 = service
            .create_server_for_auth(
                &auth_a,
                ServerCreateInput {
                    user_id: "user-1".to_owned(),
                    project_id: "project-a".to_owned(),
                    name: "srv-unk-2".to_owned(),
                    image_id: "image-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["port-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-unk-2".to_owned(),
                },
            )
            .await?;
        assert_eq!(server2.name, "srv-unk-2");

        Ok(())
    }

    #[tokio::test]
    async fn compute_quota_denial_records_audit_event() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_kernel::audit::{AuditSink, MemoryAuditSink};
        use o3k_store::QuotaRepository;

        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let provider = Arc::new(FakeComputeProvider::new());
        let audit_sink = Arc::new(MemoryAuditSink::new());
        let service = ComputeService::new(store.clone(), provider.clone())
            .with_audit_sink(audit_sink.clone() as Arc<dyn AuditSink>);

        let scope = OwnershipScope::project(ScopeId::new_unchecked("proj-audit"), None, None);
        let auth = test_compute_auth("proj-audit", "user-audit", "member");

        // Set limit to 0 servers (deny all creates)
        store
            .set_limit(&scope, &LimitKey::compute_servers(), LimitValue::Maximum(0))
            .await?;

        let flavors = service.flavors_for_auth(&auth).await?;
        let flavor_id = flavors[0].id;

        let res = service
            .create_server_for_auth(
                &auth,
                ServerCreateInput {
                    user_id: "user-audit".to_owned(),
                    project_id: "proj-audit".to_owned(),
                    name: "srv-denied".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "idem-audit".to_owned(),
                },
            )
            .await;
        assert!(matches!(res, Err(ComputeError::QuotaExceeded { .. })));

        // Audit event was recorded with Failed outcome
        let events = audit_sink.events();
        assert!(!events.is_empty(), "expected audit event to be recorded");
        let last_event = events.last().unwrap_or_else(|| &events[0]);
        assert_eq!(last_event.outcome, o3k_kernel::audit::AuditOutcome::Failed);
        assert!(
            last_event
                .reason_category
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("quota exceeded"),
            "audit event reason should contain quota exceeded"
        );

        Ok(())
    }

    #[tokio::test]
    async fn real_finite_server_quota_full_scenario_acceptance()
    -> Result<(), Box<dyn std::error::Error>> {
        use o3k_kernel::audit::{AuditSink, MemoryAuditSink};
        use o3k_provider::InstanceAction;

        let database_path = PathBuf::from(format!(
            "/tmp/o3k-quota-acceptance-{}.sqlite",
            std::process::id()
        ));
        let placement_path = PathBuf::from(format!(
            "/tmp/o3k-quota-acceptance-placement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);

        let raw_store = o3k_store::testkit::open_file(&database_path).await?;
        let store: Arc<dyn ComputeRepository> = Arc::new(raw_store.clone());
        let placement_repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(raw_store);
        let placement =
            o3k_placement::PlacementLedger::open(&placement_path, placement_repository).await?;
        placement
            .register_provider(
                "node-quota",
                std::collections::BTreeMap::from([
                    (
                        o3k_placement::VCPU.to_owned(),
                        o3k_placement::Inventory {
                            total: 16,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::MEMORY_MB.to_owned(),
                        o3k_placement::Inventory {
                            total: 16384,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        o3k_placement::DISK_GB.to_owned(),
                        o3k_placement::Inventory {
                            total: 500,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .await?;

        let provider = Arc::new(FakeComputeProvider::new());
        let audit_sink = Arc::new(MemoryAuditSink::new());
        let service = ComputeService::new(store.clone(), provider.clone())
            .with_scheduler(Scheduler::new(placement.clone()))
            .with_audit_sink(audit_sink.clone() as Arc<dyn AuditSink>);

        let scope = OwnershipScope::project(ScopeId::new_unchecked("proj-finite"), None, None);
        let auth = test_compute_auth("proj-finite", "user-finite", "member");

        // 1. Configure finite server quota: compute:servers = 1
        store
            .set_limit(&scope, &LimitKey::compute_servers(), LimitValue::Maximum(1))
            .await?;

        let flavors = service.flavors_for_auth(&auth).await?;
        let flavor_id = flavors[0].id;

        // SCENARIO A: Create server-1 -> reaches ACTIVE
        let server1 = service
            .create_server_for_auth(
                &auth,
                ServerCreateInput {
                    user_id: "user-finite".to_owned(),
                    project_id: "proj-finite".to_owned(),
                    name: "server-1".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "create-srv-1".to_owned(),
                },
            )
            .await?;
        assert_eq!(server1.name, "server-1");
        assert_eq!(server1.state, ServerState::Active);
        assert_eq!(provider.instance_count(), 1);

        // SCENARIO B: Attempt server-2 -> must be denied by quota
        let server2_res = service
            .create_server_for_auth(
                &auth,
                ServerCreateInput {
                    user_id: "user-finite".to_owned(),
                    project_id: "proj-finite".to_owned(),
                    name: "server-2".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "create-srv-2".to_owned(),
                },
            )
            .await;
        assert!(matches!(
            server2_res,
            Err(ComputeError::QuotaExceeded { .. })
        ));

        // Independent proof after denial:
        // - exactly one server instance exists in provider
        assert_eq!(provider.instance_count(), 1);
        // - exactly one active server resource exists in store
        let active_servers = service.list_servers_for_auth(&auth).await?;
        assert_eq!(active_servers.len(), 1);
        assert_eq!(active_servers[0].id, server1.id);
        // - exactly one placement allocation exists
        let allocations = placement.provider("node-quota").await?.allocations;
        assert_eq!(allocations.len(), 1);
        // - canonical quota-denial AuditEvent exists
        let events = audit_sink.events();
        assert!(events.iter().any(|e| {
            e.outcome == o3k_kernel::audit::AuditOutcome::Failed
                && e.reason_category
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains("quota exceeded")
        }));

        // SCENARIO C: Verify server-1 remains healthy across actions
        let srv_show = service.show_server_for_auth(&auth, server1.id).await?;
        assert_eq!(srv_show.state, ServerState::Active);

        let srv_stopped = service
            .action_for_auth(&auth, server1.id, InstanceAction::Stop)
            .await?;
        assert_eq!(srv_stopped.state, ServerState::Stopped);

        let srv_started = service
            .action_for_auth(&auth, server1.id, InstanceAction::Start)
            .await?;
        assert_eq!(srv_started.state, ServerState::Active);

        let srv_rebooted = service
            .action_for_auth(&auth, server1.id, InstanceAction::Reboot)
            .await?;
        assert_eq!(srv_rebooted.state, ServerState::Active);

        // SCENARIO D: Delete server-1 and wait for terminal absence
        service.delete_server_for_auth(&auth, server1.id).await?;
        assert_eq!(provider.instance_count(), 0);

        let usage_after_del = store
            .get_usage(&scope, &LimitKey::compute_servers())
            .await?;
        assert_eq!(usage_after_del.total_consumed(), 0);

        // SCENARIO E: Create server-2 / replacement -> must now succeed
        let replacement = service
            .create_server_for_auth(
                &auth,
                ServerCreateInput {
                    user_id: "user-finite".to_owned(),
                    project_id: "proj-finite".to_owned(),
                    name: "server-2".to_owned(),
                    image_id: "img-1".to_owned(),
                    flavor_id,
                    network_ids: vec!["net-1".to_owned()],
                    key_name: None,
                    config_drive: None,
                    idempotency_key: "create-srv-2-replacement".to_owned(),
                },
            )
            .await?;
        assert_eq!(replacement.name, "server-2");
        assert_eq!(replacement.state, ServerState::Active);
        assert_eq!(provider.instance_count(), 1);

        // SCENARIO F: Delete replacement
        service
            .delete_server_for_auth(&auth, replacement.id)
            .await?;
        assert_eq!(provider.instance_count(), 0);

        // SCENARIO G: Independent leak & residue verification
        let final_usage = store
            .get_usage(&scope, &LimitKey::compute_servers())
            .await?;
        assert_eq!(
            final_usage.total_consumed(),
            0,
            "final quota consumed must be 0"
        );
        let final_servers = service.list_servers_for_auth(&auth).await?;
        assert_eq!(
            final_servers.len(),
            0,
            "no active servers must remain in store"
        );

        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_path);

        Ok(())
    }

    #[tokio::test]
    async fn multi_controller_reconciler_leases_mutating_work_and_skips_busy()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(o3k_store::O3kStore::connect_sqlite_memory().await?);
        let coord: Arc<dyn o3k_store::CoordinationRepository> = store.clone();

        let ctrl_a = o3k_store::ControllerId::new("ctrl-a");
        let epoch_a = o3k_store::ControllerEpoch::new("epoch-a");
        let ctrl_b = o3k_store::ControllerId::new("ctrl-b");
        let epoch_b = o3k_store::ControllerEpoch::new("epoch-b");

        let provider = Arc::new(FakeComputeProvider::new());
        let service = ComputeService::new(store.clone(), provider.clone()).with_coordination(
            coord.clone(),
            ctrl_a.clone(),
            epoch_a.clone(),
        );

        // 1. When Controller B holds the attachment reconciler lease, Controller A skips
        let busy_lease = coord
            .acquire_work_lease(
                "reconcile:volume_attachments",
                "volume_attachment_reconciler",
                &ctrl_b,
                &epoch_b,
                std::time::Duration::from_secs(30),
            )
            .await?;
        assert!(matches!(
            busy_lease,
            o3k_store::LeaseAcquireOutcome::Acquired { .. }
        ));

        // Controller A spawns reconciler task and skips leased work without panic
        let handle = service.spawn_attachment_reconciler(1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();

        // 2. When Controller B holds create convergence for an instance, Controller A skips
        let instance_id = Uuid::new_v4();
        let create_key = format!("convergence:create:{}", instance_id);
        let busy_create = coord
            .acquire_work_lease(
                &create_key,
                "create_convergence",
                &ctrl_b,
                &epoch_b,
                std::time::Duration::from_secs(30),
            )
            .await?;
        assert!(matches!(
            busy_create,
            o3k_store::LeaseAcquireOutcome::Acquired { .. }
        ));

        // Controller A runs create convergence sweep and skips without error
        let sweep_create_res = service.drive_all_create_convergence().await;
        assert!(
            sweep_create_res.is_ok(),
            "Controller A must skip leased create convergence"
        );

        // 3. When Controller B holds lifecycle convergence for an operation, Controller A skips
        let op_id = Uuid::new_v4();
        let op_key = format!("operation:{}", op_id);
        let busy_op = coord
            .acquire_work_lease(
                &op_key,
                "operation",
                &ctrl_b,
                &epoch_b,
                std::time::Duration::from_secs(30),
            )
            .await?;
        assert!(matches!(
            busy_op,
            o3k_store::LeaseAcquireOutcome::Acquired { .. }
        ));

        // Controller A runs lifecycle sweep and skips without error
        let sweep_op_res = service.drive_all_lifecycle_convergence().await;
        assert!(
            sweep_op_res.is_ok(),
            "Controller A must skip leased operation convergence"
        );

        Ok(())
    }
}
