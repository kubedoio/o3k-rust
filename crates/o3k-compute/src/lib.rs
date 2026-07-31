use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use o3k_provider::FakeComputeProvider;
use o3k_provider::{
    Capabilities, ComputeProvider, CreateInstanceRequest, DeleteInstanceRequest, Instance,
    InstanceAction, Operation, ProviderError,
};
use o3k_reconciler::{OperationJournal, ReconcileError};
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
}

#[derive(Clone)]
pub struct ComputeService {
    store: Arc<SqliteStore>,
    provider: Arc<ProviderBackend>,
    journal: OperationJournal<SqliteStore, ProviderBackend>,
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
        }
    }

    #[must_use]
    pub fn provider(&self) -> Arc<ProviderBackend> {
        self.provider.clone()
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
        if self
            .list_servers(project_id)
            .await?
            .iter()
            .any(|server| server.name == name && server.status != "DELETED")
        {
            return Err(ComputeError::Conflict);
        }
        let request = CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name: name.clone(),
            vcpus: flavor.vcpus,
            memory_mib: flavor.ram_mib,
            image_id: Some(image_id.clone()),
            network_ids,
            idempotency_key,
        };
        let id = request.o3k_server_id;
        self.journal.begin_create(project_id, &request).await?;
        let _ = self.journal.reconcile_once(request.operation_id).await?;
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
        let provider_id = resource
            .provider_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        let delete_result = self
            .provider
            .delete_instance(DeleteInstanceRequest {
                operation_id: Uuid::now_v7(),
                provider_instance_id: provider_id.to_owned(),
                idempotency_key: format!("delete-{id}"),
            })
            .await;
        if let Err(error) = delete_result {
            if !matches!(error, ProviderError::NotFound) {
                return Err(ComputeError::Provider(error));
            }
        }
        self.store
            .update_resource(
                id,
                resource.generation,
                &resource.desired_state,
                "DELETED",
                resource.observed_generation,
                Some(provider_id),
            )
            .await?;
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
        let provider_id = resource
            .provider_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        let target = match (action, resource.observed_state.as_str()) {
            (InstanceAction::Start, "stopped" | "STOPPED") => "ACTIVE",
            (InstanceAction::Stop, "active" | "ACTIVE") => "STOPPED",
            (InstanceAction::Reboot, "active" | "ACTIVE" | "stopped" | "STOPPED") => "ACTIVE",
            _ => return Err(ComputeError::Conflict),
        };
        self.provider
            .action_instance(
                provider_id,
                action,
                Uuid::now_v7(),
                &format!("action-{id}-{target}"),
            )
            .await?;
        let observed = self.provider.get_instance(provider_id).await?;
        let observed_state = match observed.state {
            o3k_provider::InstanceState::Running => "ACTIVE",
            o3k_provider::InstanceState::Stopped => "STOPPED",
            o3k_provider::InstanceState::Creating => "BUILD",
            o3k_provider::InstanceState::Deleting => "DELETING",
            o3k_provider::InstanceState::Deleted => "DELETED",
            o3k_provider::InstanceState::Error => "ERROR",
        };
        self.store
            .update_resource(
                id,
                resource.generation,
                &resource.desired_state,
                observed_state,
                resource.observed_generation,
                Some(provider_id),
            )
            .await?;
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
}
