use super::{
    AuthContext, ComputeError, ComputeService, Flavor, ResourceId, ResourceTarget, ResourceType,
    ServerState, StoreError, Uuid,
};

use o3k_kernel::{ActionId, AuditEvent, AuditOutcome, AuthorizationRequest, ServiceNamespace};
use o3k_store::server_state_from_storage;

impl ComputeService {
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
}
