use std::sync::Arc;

use async_trait::async_trait;
use o3k_compute_agent::NodeRegistry;
#[cfg(test)]
use o3k_provider::FakeComputeProvider;
use o3k_provider::{
    Capabilities, ComputeProvider, CreateInstanceRequest, DeleteInstanceRequest, Instance,
    InstanceAction, Operation, ProviderError,
};
use o3k_reconciler::{LifecycleAction, OperationJournal, ReconcileError};
use o3k_scheduler::{Flavor as SchedulerFlavor, Scheduler, SchedulerError};
use o3k_store::{DurableStore, SqliteStore, StoreError};
use serde::{Deserialize, Serialize};
use thiserror::Error;
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
}

#[derive(Clone)]
pub struct ProviderBackend(Arc<dyn ComputeProvider>);

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
        }
    }

    #[must_use]
    pub fn with_scheduler(mut self, scheduler: Scheduler) -> Self {
        self.scheduler = Some(scheduler);
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
        if name.trim().is_empty()
            || image_id.trim().is_empty()
            || network_ids.is_empty()
            || network_ids.iter().any(|id| id.trim().is_empty())
            || idempotency_key.trim().is_empty()
        {
            return Err(ComputeError::InvalidRequest);
        }
        let flavor = self.flavor(flavor_id)?;
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
            image_id: Some(image_id.clone()),
            network_ids: network_ids.clone(),
            placement_provider_id: None,
            placement_allocation_id: None,
            idempotency_key: idempotency_key.clone(),
        };
        let placement = self
            .scheduler
            .as_ref()
            .map(|scheduler| {
                scheduler.schedule(
                    &server_id.to_string(),
                    SchedulerFlavor {
                        vcpus: flavor.vcpus as u64,
                        memory_mb: flavor.ram_mib,
                        disk_gb: flavor.disk_gib,
                    },
                )
            })
            .transpose()?;
        let request = CreateInstanceRequest {
            placement_provider_id: placement
                .as_ref()
                .map(|decision| decision.provider_id.clone()),
            placement_allocation_id: placement
                .as_ref()
                .map(|decision| decision.allocation_id.clone()),
            ..request
        };
        match self.store.get_resource(server_id).await {
            Ok(existing) => {
                let existing_request: CreateInstanceRequest =
                    serde_json::from_str(&existing.desired_state)
                        .map_err(|_| ComputeError::Conflict)?;
                if existing_request == request {
                    return self.show_server(project_id, server_id).await;
                }
                return Err(ComputeError::Conflict);
            }
            Err(StoreError::ResourceNotFound) => {}
            Err(error) => return Err(ComputeError::Store(error)),
        }
        if self
            .list_servers(project_id)
            .await?
            .iter()
            .any(|server| server.name == name && server.status != "DELETED")
        {
            return Err(ComputeError::Conflict);
        }
        let request = CreateInstanceRequest {
            network_ids,
            ..request
        };
        let id = request.o3k_server_id;
        match self.journal.begin_create(project_id, &request).await {
            Ok(_) => {}
            Err(ReconcileError::Store(StoreError::ResourceAlreadyExists)) => {
                let existing = self.store.get_resource(id).await?;
                let existing_request: CreateInstanceRequest =
                    serde_json::from_str(&existing.desired_state)
                        .map_err(|_| ComputeError::Conflict)?;
                if existing_request != request {
                    return Err(ComputeError::Conflict);
                }
                return self.show_server(project_id, id).await;
            }
            Err(error) => return Err(ComputeError::Reconcile(error)),
        }
        let reconcile_state = self.journal.reconcile_once(request.operation_id).await?;
        if reconcile_state == o3k_store::OperationState::Failed {
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
        self.show_server(project_id, id).await
    }

    pub async fn list_servers(&self, project_id: &str) -> Result<Vec<Server>, ComputeError> {
        let resources = self
            .store
            .list_resources(project_id, "compute_instance")
            .await?;
        resources
            .into_iter()
            .filter_map(|resource| server_from_resource(resource, &self.flavors()).ok())
            .filter(|server| server.status != "DELETED")
            .collect::<Vec<_>>()
            .pipe(Ok)
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
        let server = server_from_resource(resource, &self.flavors())
            .map_err(|_| ComputeError::InvalidRequest)?;
        if server.status == "DELETED" {
            return Err(ComputeError::NotFound);
        }
        Ok(server)
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
        if self.journal.reconcile_lifecycle_once(operation_id).await?
            != o3k_store::OperationState::Succeeded
        {
            return Err(ComputeError::Conflict);
        }
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
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
                    consumer_id: id.to_string(),
                    resources: std::collections::BTreeMap::new(),
                },
            })?;
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
            (InstanceAction::Start, "stopped" | "STOPPED") => "ACTIVE",
            (InstanceAction::Stop, "active" | "ACTIVE") => "STOPPED",
            (InstanceAction::Reboot, "active" | "ACTIVE" | "stopped" | "STOPPED") => "ACTIVE",
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
        if self.journal.reconcile_lifecycle_once(operation_id).await?
            != o3k_store::OperationState::Succeeded
        {
            return Err(ComputeError::Conflict);
        }
        self.show_server(project_id, id).await
    }
}

fn server_from_resource(
    resource: o3k_store::ResourceRecord,
    flavors: &[Flavor],
) -> Result<Server, ()> {
    let request: CreateInstanceRequest =
        serde_json::from_str(&resource.desired_state).map_err(|_| ())?;
    let flavor = flavors
        .iter()
        .find(|flavor| flavor.vcpus == request.vcpus && flavor.ram_mib == request.memory_mib)
        .ok_or(())?;
    Ok(Server {
        id: resource.id,
        name: request.name,
        project_id: resource.project_id,
        flavor_id: flavor.id,
        image_id: request.image_id.unwrap_or_default(),
        status: resource.observed_state.to_ascii_uppercase(),
    })
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;
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
            "STOPPED"
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
    async fn agent_update_forwarding_uses_durable_journal() -> Result<(), ComputeError> {
        let service = service("agent-forwarding").await?;
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "agent-server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            image_id: Some("image-1".to_owned()),
            network_ids: vec!["network-1".to_owned()],
            placement_provider_id: None,
            placement_allocation_id: None,
            idempotency_key: "agent-forwarding".to_owned(),
        };
        service
            .journal
            .begin_create("project-a", &request)
            .await
            .map_err(ComputeError::Reconcile)?;
        let update = o3k_provider_contract::compute_proto::OperationUpdate {
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
            "active"
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
}
