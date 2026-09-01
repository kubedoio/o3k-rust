use std::{collections::BTreeMap, sync::Arc};

use o3k_domain::{StorageExecutionScope, Volume, VolumeId, VolumeState};
use o3k_kernel::Controller;
use o3k_native_api::{
    compute::ServerItem,
    network::AddressRealmItem,
    resource::{
        CreateRequest, MutationResult, ResourceApplication, ResourceApplicationError,
        ResourceDescriptor,
    },
};
use o3k_store::{DurableStore, storage::StorageRepository};
use uuid::Uuid;

/// Application adapter for generic native resource reads and mutations.
pub struct GenericResourceApplication {
    pub compute: Arc<o3k_compute::ComputeService>,
    pub network_service: Arc<o3k_network::NetworkService>,
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub storage_provider: Option<Arc<dyn o3k_storage::StorageProvider>>,
    pub server: Arc<dyn o3k_native_api::compute::ServerReader>,
    pub network: Arc<dyn o3k_native_api::network::NetworkReader>,
    pub external_controllers: Arc<BTreeMap<String, Arc<o3k_service_sdk::GrpcControllerAdapter>>>,
}

fn compute_error(error: o3k_compute::ComputeError) -> ResourceApplicationError {
    match error {
        o3k_compute::ComputeError::Unauthorized => ResourceApplicationError::Forbidden,
        o3k_compute::ComputeError::NotFound => ResourceApplicationError::NotFound,
        o3k_compute::ComputeError::InvalidRequest => ResourceApplicationError::Validation,
        o3k_compute::ComputeError::Conflict => ResourceApplicationError::Conflict,
        _ => ResourceApplicationError::Internal,
    }
}

fn generic_read_error(error: o3k_native_api::error::NativeReadError) -> ResourceApplicationError {
    match error {
        o3k_native_api::error::NativeReadError::NotFound => ResourceApplicationError::NotFound,
        o3k_native_api::error::NativeReadError::Forbidden => ResourceApplicationError::Forbidden,
        o3k_native_api::error::NativeReadError::Internal => ResourceApplicationError::Internal,
    }
}

fn server_json(item: ServerItem) -> serde_json::Value {
    serde_json::json!({"api_version":"o3k.io/v1","kind":"compute:server","metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation,"created_at":item.created_at},"spec":{"name":item.name,"flavor_id":item.flavor_id,"image_id":item.image_id},"status":{"state":item.state}})
}

fn realm_json(item: AddressRealmItem) -> serde_json::Value {
    serde_json::json!({"api_version":"o3k.io/v1","kind":"network:address_realm","metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation,"created_at":item.created_at},"spec":{"prefix":item.prefix,"overlapping_prefixes":item.overlapping_prefixes},"status":{"state":item.state}})
}

fn network_json(item: &o3k_store::CanonicalNetworkRecord) -> serde_json::Value {
    serde_json::json!({
        "api_version":"o3k.io/v1",
        "kind":"network:network",
        "metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation},
        "spec":{"name":item.name},
        "status":{"state":item.state}
    })
}

fn native_volume_json(record: &o3k_store::VolumeRecord) -> serde_json::Value {
    serde_json::json!({
        "api_version":"o3k.io/v1",
        "kind":"volume:volume",
        "metadata":{"id":record.volume.id.to_string(),"owner_scope":record.volume.project_id,"generation":record.volume.generation,"created_at":record.created_at},
        "spec":{"size_bytes":record.volume.size_bytes,"volume_type":record.volume.volume_type,"name":record.volume.name,"description":record.volume.description,"metadata":record.volume.metadata,"availability_zone":record.volume.availability_zone},
        "status":{"state":record.volume.state}
    })
}

fn generic_external_json(resource: &o3k_store::ResourceRecord) -> serde_json::Value {
    let spec = serde_json::from_str(&resource.desired_state).unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": resource.kind,
        "metadata": {
            "id": resource.id,
            "owner_scope": resource.project_id,
            "generation": resource.generation
        },
        "spec": spec,
        "status": {"state": resource.observed_state}
    })
}

#[async_trait::async_trait]
impl ResourceApplication for GenericResourceApplication {
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError> {
        if self
            .external_controllers
            .contains_key(&descriptor.owning_service)
        {
            return self
                .store
                .list_resources(
                    auth.effective_scope().id().as_str(),
                    &descriptor.resource_type.to_string(),
                )
                .await
                .map(|resources| resources.iter().map(generic_external_json).collect())
                .map_err(|_| ResourceApplicationError::Internal);
        }
        match descriptor.resource_type.to_string().as_str() {
            "compute:server" => self
                .server
                .list_servers(auth)
                .await
                .map(|items| items.into_iter().map(server_json).collect())
                .map_err(generic_read_error),
            "network:address_realm" => self
                .network
                .list_address_realms(auth)
                .await
                .map(|items| items.into_iter().map(realm_json).collect())
                .map_err(generic_read_error),
            "network:network" => self
                .network_service
                .list_canonical_networks(auth)
                .await
                .map(|items| items.iter().map(network_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            "volume:volume" => self
                .store
                .list_volumes(auth.effective_scope().id().as_str())
                .await
                .map(|items| items.iter().map(native_volume_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError> {
        if self
            .external_controllers
            .contains_key(&descriptor.owning_service)
        {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != descriptor.resource_type.to_string()
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            return Ok(generic_external_json(&resource));
        }
        let id = id
            .parse::<Uuid>()
            .map_err(|_| ResourceApplicationError::NotFound)?;
        match descriptor.resource_type.to_string().as_str() {
            "compute:server" => self
                .server
                .show_server(auth, id)
                .await
                .map(server_json)
                .map_err(generic_read_error),
            "network:address_realm" => self
                .network
                .show_address_realm(auth, id)
                .await
                .map(realm_json)
                .map_err(generic_read_error),
            "network:network" => self
                .network_service
                .get_canonical_network(auth, id)
                .await
                .map(|item| network_json(&item))
                .map_err(|_| ResourceApplicationError::NotFound),
            "volume:volume" => self
                .store
                .get_volume(id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)
                .and_then(|record| match record {
                    Some(record)
                        if record.volume.project_id == auth.effective_scope().id().as_str() =>
                    {
                        Ok(native_volume_json(&record))
                    }
                    _ => Err(ResourceApplicationError::NotFound),
                }),
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        request: CreateRequest,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if let Some(controller) = self.external_controllers.get(&descriptor.owning_service) {
            if !controller.health().await.healthy {
                return Err(ResourceApplicationError::NotReady);
            }
            // The descriptor is derived at startup and cannot reflect a later
            // controller outage.  Re-check readiness at the mutation boundary
            // so a Ready -> NotReady transition cannot accept new work.
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Create)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_identity = format!(
                "{}:{}:{}:{}",
                auth.effective_scope().id(),
                descriptor.resource_type,
                action,
                key
            );
            let resource_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, resource_identity.as_bytes());
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:create:{resource_id}", descriptor.resource_type).as_bytes(),
            );
            let desired_state = serde_json::to_string(&request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?;
            let resource = o3k_store::ResourceRecord {
                id: resource_id,
                kind: descriptor.resource_type.to_string(),
                project_id: auth.effective_scope().id().as_str().to_owned(),
                generation: 1,
                observed_generation: 0,
                desired_state,
                observed_state: "PROVISIONING".to_owned(),
                provider_id: None,
            };
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:create".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    descriptor.owning_service.clone(),
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    descriptor.resource_type.clone(),
                    Some(o3k_kernel::ResourceId::new_unchecked(
                        resource_id.to_string(),
                    )),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                &descriptor.resource_type.to_string(),
                Some(&resource_id.to_string()),
                &request.spec,
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            let acceptance = self
                .store
                .create_or_replay_canonical_resource_operation(
                    &resource, &operation, &canonical, &identity, None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            let (operation_id, resource_id, replayed) = match acceptance {
                o3k_store::CanonicalAcceptanceOutcome::Created {
                    operation_id,
                    resource_id,
                } => (operation_id, resource_id, false),
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id,
                    resource_id,
                } => (operation_id, resource_id, true),
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
            };
            if replayed {
                let existing = self
                    .store
                    .get_resource(resource_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                // An equivalent replay must not redrive an external mutation
                // while its canonical operation is still converging.  The
                // durable reconciler owns retry/recovery; this API call only
                // returns the existing canonical result.
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(resource_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let session = controller.session();
            let context = o3k_kernel::OperationContext {
                request_id: auth
                    .request_id()
                    .parse()
                    .map_err(|_| ResourceApplicationError::Internal)?,
                operation_id,
                action,
                service_id: descriptor.owning_service.clone(),
                owner_scope: auth.effective_scope().clone(),
                session_id: session.session_id,
                session_generation: session.session_generation,
                deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
                replay_identity: format!("parent:{operation_id}"),
                audit_correlation: format!("parent:{operation_id}"),
            };
            let parent_reference = o3k_kernel::ResourceReference {
                resource_type: descriptor.resource_type.clone(),
                resource_id: o3k_kernel::ResourceId::new_unchecked(resource_id.to_string()),
                generation: 1,
            };
            let delegation = controller
                .issue_parent_delegation(
                    &context,
                    auth.principal().id().to_string(),
                    &parent_reference,
                )
                .map_err(|_| ResourceApplicationError::Unauthorized)?;
            let outcome = controller
                .reconcile(o3k_kernel::ReconcileRequest {
                    context,
                    resource: o3k_kernel::ResourceSnapshot {
                        reference: parent_reference,
                        desired_spec: request.spec,
                        known_status: None,
                        owner_scope: auth.effective_scope().clone(),
                    },
                    delegation: Some(delegation),
                })
                .await;
            let complete = matches!(outcome, o3k_kernel::ReconcileOutcome::Succeeded { .. });
            let observed_state = match &outcome {
                o3k_kernel::ReconcileOutcome::Succeeded { .. } => "READY",
                o3k_kernel::ReconcileOutcome::Unknown { .. } => "UNKNOWN",
                o3k_kernel::ReconcileOutcome::Failed { .. }
                | o3k_kernel::ReconcileOutcome::Retryable { .. } => "ERROR",
                o3k_kernel::ReconcileOutcome::Accepted { .. } => "PROVISIONING",
            };
            let lifecycle_state = match &outcome {
                o3k_kernel::ReconcileOutcome::Succeeded { .. } => {
                    o3k_kernel::OperationState::Succeeded
                }
                o3k_kernel::ReconcileOutcome::Unknown { .. } => {
                    o3k_kernel::OperationState::UnknownOutcome
                }
                o3k_kernel::ReconcileOutcome::Retryable { .. } => {
                    o3k_kernel::OperationState::Retryable
                }
                o3k_kernel::ReconcileOutcome::Failed { .. } => o3k_kernel::OperationState::Failed,
                o3k_kernel::ReconcileOutcome::Accepted { .. } => {
                    o3k_kernel::OperationState::Running
                }
            };
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                lifecycle_state,
                1,
                Some(now.clone()),
                matches!(
                    lifecycle_state,
                    o3k_kernel::OperationState::Succeeded | o3k_kernel::OperationState::Failed
                )
                .then_some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(
                    resource_id,
                    1,
                    &resource.desired_state,
                    observed_state,
                    if complete { 1 } else { 0 },
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(resource_id.to_string()),
                complete,
                resource: Some(serde_json::json!({
                    "api_version": "o3k.io/v1",
                    "kind": descriptor.resource_type.to_string(),
                    "metadata": {"id": resource_id, "generation": 1},
                    "spec": resource.desired_state,
                    "status": {"state": if complete {"READY"} else {"PROVISIONING"}}
                })),
            });
        }
        if descriptor.resource_type.to_string() == "volume:volume" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct VolumeSpec {
                size_bytes: u64,
                volume_type: String,
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                description: Option<String>,
                #[serde(default)]
                metadata: Option<std::collections::BTreeMap<String, String>>,
                #[serde(default)]
                availability_zone: Option<String>,
            }
            let spec: VolumeSpec = serde_json::from_value(request.spec.clone())
                .map_err(|_| ResourceApplicationError::Validation)?;
            if spec.size_bytes == 0 || spec.volume_type.trim().is_empty() {
                return Err(ResourceApplicationError::Validation);
            }
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{}:{}", auth.effective_scope().id(), key).as_bytes(),
            );
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("volume:create:{resource_id}").as_bytes(),
            );
            let volume = Volume {
                id: VolumeId::from_uuid(resource_id),
                project_id: auth.effective_scope().id().as_str().to_owned(),
                name: spec.name.unwrap_or_else(|| resource_id.to_string()),
                description: spec.description.unwrap_or_default(),
                metadata: spec.metadata.unwrap_or_default(),
                availability_zone: spec.availability_zone,
                size_bytes: spec.size_bytes,
                volume_type: spec.volume_type,
                backend_id: "local".to_owned(),
                execution_scope: StorageExecutionScope::Host("local".to_owned()),
                state: VolumeState::Requested,
                generation: 1,
                operation_id: Some(operation_id),
                provider_reference: None,
            };
            let record = o3k_store::VolumeRecord {
                volume,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let Some(provider) = self.storage_provider.clone() else {
                return Err(ResourceApplicationError::NotReady);
            };
            match self.store.insert_volume(&record).await {
                Ok(()) => {}
                Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                    let existing = self
                        .store
                        .get_volume(resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?
                        .ok_or(ResourceApplicationError::Internal)?;
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(resource_id.to_string()),
                        complete: existing.volume.state == VolumeState::Available,
                        resource: Some(native_volume_json(&existing)),
                    });
                }
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
            o3k_api::realize_native_volume_create(self.store.clone(), provider, record)
                .await
                .map_err(|_| ResourceApplicationError::Retryable)?;
            // The legacy generic-resource index is a compatibility projection
            // used by relationship tests and older native callers.  The
            // canonical volume above remains the sole authority.
            match self
                .store
                .insert_resource(&o3k_store::ResourceRecord {
                    id: resource_id,
                    kind: "volume".to_owned(),
                    project_id: auth.effective_scope().id().as_str().to_owned(),
                    generation: 1,
                    observed_generation: 1,
                    desired_state: "available".to_owned(),
                    observed_state: "available".to_owned(),
                    provider_id: None,
                })
                .await
            {
                Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
            match self
                .store
                .insert_operation(&o3k_store::OperationRecord {
                    id: operation_id,
                    resource_id,
                    kind: "lifecycle:create".to_owned(),
                    state: o3k_store::OperationState::Succeeded,
                    provider_operation_id: None,
                    error_category: None,
                    error_message: None,
                })
                .await
            {
                Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
            let record = self
                .store
                .get_volume(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                .ok_or(ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(resource_id.to_string()),
                complete: true,
                resource: Some(native_volume_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:network" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct NetworkSpec {
                name: String,
            }
            let spec: NetworkSpec = serde_json::from_value(request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?;
            let network = self
                .network_service
                .create_network(auth, spec.name)
                .await
                .map_err(|_| ResourceApplicationError::Conflict)?;
            let canonical = self
                .network_service
                .get_canonical_network(auth, network.id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("network:create:{}", network.id).as_bytes(),
                )
                .to_string(),
                resource_id: Some(network.id.to_string()),
                complete: true,
                resource: Some(network_json(&canonical)),
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ComputeSpec {
            name: String,
            image_id: String,
            flavor_id: Uuid,
            network_ids: Vec<String>,
            #[serde(default)]
            key_name: Option<String>,
        }
        let semantic_request = serde_json::json!({"spec": request.spec});
        let spec: ComputeSpec = serde_json::from_value(semantic_request["spec"].clone())
            .map_err(|_| ResourceApplicationError::Validation)?;
        for network_id in &spec.network_ids {
            // Durable port references cross the Network authority boundary;
            // the provider's legacy opaque test references remain outside it.
            if let Ok(port_id) = network_id.parse::<Uuid>() {
                match self.network_service.get_port(auth, port_id).await {
                    Ok(_) => {}
                    Err(o3k_network::NetworkError::Unauthorized) => {
                        return Err(ResourceApplicationError::Forbidden);
                    }
                    // P12.6 generic composition also carries UUID child
                    // slots which are not NetworkService ports. Preserve
                    // that contract; resolvable ports remain owner-checked.
                    Err(o3k_network::NetworkError::NotFound) => {
                        if let Some(port) = self
                            .network_service
                            .find_port_by_id(port_id)
                            .await
                            .map_err(|_| ResourceApplicationError::Conflict)?
                            && port.project_id != auth.effective_scope().id().as_str()
                        {
                            return Err(ResourceApplicationError::Forbidden);
                        }
                    }
                    Err(_) => return Err(ResourceApplicationError::Conflict),
                }
            }
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Create)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let context = o3k_reconciler::CanonicalMutationContext::new(
            action,
            auth.principal().id().to_string(),
            auth.effective_scope().clone(),
            None,
            key.clone(),
            semantic_request,
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        let receipt = self
            .compute
            .create_server_for_auth_canonical(
                auth,
                o3k_compute::ServerCreateInput {
                    user_id: auth.principal().id().to_string(),
                    project_id: auth.effective_scope().id().as_str().to_owned(),
                    name: spec.name,
                    image_id: spec.image_id,
                    flavor_id: spec.flavor_id,
                    network_ids: spec.network_ids,
                    key_name: spec.key_name,
                    config_drive: None,
                    // Keep provider command identity scoped even when the
                    // client reuses the same canonical key in another tenant.
                    idempotency_key: format!("{}:{key}", auth.effective_scope().id()),
                },
                context,
            )
            .await
            .map_err(compute_error)?;
        let server = receipt.resource;
        let resource = self
            .store
            .get_resource(server.id.as_uuid())
            .await
            .map_err(|_| ResourceApplicationError::Internal)?;
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(server.id.as_uuid().to_string()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded
            ),
            resource: Some(server_json(ServerItem {
                id: server.id.as_uuid().to_string(),
                project_id: server.project_id,
                name: server.name,
                flavor_id: server.flavor_id.to_string(),
                image_id: server.image_id,
                state: format!("{:?}", server.state),
                generation: resource.generation,
                created_at: None,
            })),
        })
    }

    async fn delete(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        idempotency_key: Option<&str>,
        expected_generation: Option<i64>,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if let Some(controller) = self.external_controllers.get(&descriptor.owning_service) {
            if !controller.health().await.healthy {
                return Err(ResourceApplicationError::NotReady);
            }
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != descriptor.resource_type.to_string()
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            if expected_generation.is_some_and(|expected| expected != resource.generation) {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:delete:{id}:{key}", descriptor.resource_type).as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:delete".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    descriptor.owning_service.clone(),
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    descriptor.resource_type.clone(),
                    Some(o3k_kernel::ResourceId::new_unchecked(id)),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                &descriptor.resource_type.to_string(),
                Some(id),
                &serde_json::json!({"resource_id": id}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            if self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                == o3k_store::CanonicalAcceptanceOutcome::Conflict
            {
                return Err(ResourceApplicationError::IdempotencyConflict);
            }
            let session = controller.session();
            let context = o3k_kernel::OperationContext {
                request_id: auth
                    .request_id()
                    .parse()
                    .map_err(|_| ResourceApplicationError::Internal)?,
                operation_id,
                action,
                service_id: descriptor.owning_service.clone(),
                owner_scope: auth.effective_scope().clone(),
                session_id: session.session_id,
                session_generation: session.session_generation,
                deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
                replay_identity: format!("delete:{operation_id}"),
                audit_correlation: format!("delete:{operation_id}"),
            };
            let parent_reference = o3k_kernel::ResourceReference {
                resource_type: descriptor.resource_type.clone(),
                resource_id: o3k_kernel::ResourceId::new_unchecked(id),
                generation: resource.generation,
            };
            let delegation = controller
                .issue_parent_delegation(
                    &context,
                    auth.principal().id().to_string(),
                    &parent_reference,
                )
                .map_err(|_| ResourceApplicationError::Unauthorized)?;
            let outcome = controller
                .delete(o3k_kernel::DeleteRequest {
                    context,
                    resource: parent_reference,
                    owner_scope: auth.effective_scope().clone(),
                    delegation: Some(delegation),
                })
                .await;
            let complete = matches!(outcome, o3k_kernel::ReconcileOutcome::Succeeded { .. });
            if complete {
                self.store
                    .update_resource(
                        resource_id,
                        resource.generation,
                        "DELETED",
                        "DELETED",
                        resource.generation.saturating_add(1),
                        None,
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
            }
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() == "volume:volume" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if let Some(record) = self
                .store
                .get_volume(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                if record.volume.project_id != auth.effective_scope().id().as_str() {
                    return Err(ResourceApplicationError::NotFound);
                }
                if expected_generation
                    .is_some_and(|expected| expected != record.volume.generation as i64)
                {
                    return Err(ResourceApplicationError::PreconditionConflict);
                }
                let action = descriptor
                    .lifecycle_actions
                    .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                    .cloned()
                    .ok_or(ResourceApplicationError::UnsupportedOperation)?;
                let key = idempotency_key
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("native:volume-delete:{id}"));
                let operation_id = Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!(
                        "volume:delete:{}:{resource_id}:{key}",
                        auth.effective_scope().id()
                    )
                    .as_bytes(),
                );
                let Some(provider) = self.storage_provider.clone() else {
                    return Err(ResourceApplicationError::NotReady);
                };
                let operation = o3k_store::OperationRecord {
                    id: operation_id,
                    resource_id,
                    kind: "lifecycle:delete".into(),
                    state: o3k_store::OperationState::Pending,
                    provider_operation_id: None,
                    error_category: None,
                    error_message: None,
                };
                let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                    &o3k_kernel::Operation::new(
                        operation_id,
                        "volume",
                        action.clone(),
                        auth.principal().id().to_string(),
                        auth.effective_scope().clone(),
                        o3k_kernel::ResourceType::new_unchecked("volume", "volume"),
                        Some(o3k_kernel::ResourceId::new_unchecked(id)),
                        Some(auth.request_id().to_owned()),
                    ),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                    auth.effective_scope().id().as_str(),
                    action.to_string(),
                    key,
                    "volume:volume",
                    Some(id),
                    &serde_json::json!({"resource_id": id}),
                    operation_id,
                )
                .map_err(|_| ResourceApplicationError::Validation)?;
                let acceptance = self
                    .store
                    .create_or_replay_canonical_lifecycle_operation(
                        &operation, &canonical, &identity,
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                if let o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id,
                    ..
                } = acceptance
                {
                    let existing = self
                        .store
                        .get_canonical_operation(operation_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    if existing.state == o3k_store::OperationState::Succeeded {
                        return Ok(MutationResult {
                            operation_id: operation_id.to_string(),
                            resource_id: Some(id.to_owned()),
                            complete: true,
                            resource: None,
                        });
                    }
                }
                o3k_api::remove_native_volume(
                    self.store.clone(),
                    provider,
                    auth.effective_scope().id().as_str(),
                    resource_id,
                )
                .await
                .map_err(|_| ResourceApplicationError::Retryable)?;
                self.store
                    .update_resource(
                        resource_id,
                        record.volume.generation as i64,
                        "DELETED",
                        "DELETED",
                        record.volume.generation.saturating_add(1) as i64,
                        None,
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Succeeded,
                    1,
                    Some(now.clone()),
                    Some(now),
                    None,
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(id.to_owned()),
                    complete: true,
                    resource: None,
                });
            }
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != "volume"
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            if expected_generation.is_some_and(|expected| expected != resource.generation) {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:volume-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("volume:delete:{}:{id}:{key}", auth.effective_scope().id()).as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:delete".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    "volume",
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    o3k_kernel::ResourceType::new_unchecked("volume", "volume"),
                    Some(o3k_kernel::ResourceId::new_unchecked(id)),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let request_identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                "volume:volume",
                Some(id),
                &serde_json::json!({"resource_id": id}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            if self
                .store
                .create_or_replay_canonical_lifecycle_operation(
                    &operation,
                    &canonical,
                    &request_identity,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                == o3k_store::CanonicalAcceptanceOutcome::Conflict
            {
                return Err(ResourceApplicationError::IdempotencyConflict);
            }
            self.store
                .update_resource(
                    resource_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation.saturating_add(1),
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() == "network:network" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:network-delete:{id}"));
            let network = self
                .network_service
                .get_canonical_network(auth, resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if expected_generation.is_some_and(|expected| {
                expected != i64::try_from(network.generation).unwrap_or(i64::MAX)
            }) {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("network:delete:{}:{id}:{key}", auth.effective_scope().id()).as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:delete".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    "network",
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    o3k_kernel::ResourceType::new_unchecked("network", "network"),
                    Some(o3k_kernel::ResourceId::new_unchecked(id)),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                "network:network",
                Some(id),
                &serde_json::json!({"resource_id": id}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            let acceptance = self
                .store
                .create_or_replay_canonical_scoped_operation(&operation, &canonical, &identity)
                .await
                .map_err(|error| match error {
                    o3k_store::StoreError::ResourceNotFound
                    | o3k_store::StoreError::NetworkNotFound => ResourceApplicationError::NotFound,
                    o3k_store::StoreError::IdempotencyConflict => {
                        ResourceApplicationError::IdempotencyConflict
                    }
                    _ => ResourceApplicationError::Internal,
                })?;
            match acceptance {
                o3k_store::IdempotencyReservation::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::IdempotencyReservation::ExistingEquivalent(operation_id) => {
                    let existing = self
                        .store
                        .get_canonical_operation(operation_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(id.to_owned()),
                        complete: existing.state == o3k_store::OperationState::Succeeded,
                        resource: None,
                    });
                }
                o3k_store::IdempotencyReservation::Created(_) => {}
            }
            self.network_service
                .delete_canonical_network(auth, resource_id)
                .await
                .map_err(|_| ResourceApplicationError::Retryable)?;
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                o3k_kernel::OperationState::Succeeded,
                1,
                Some(now.clone()),
                Some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
        let resource_id = id
            .parse::<Uuid>()
            .map_err(|_| ResourceApplicationError::NotFound)?;
        let existing = self
            .store
            .get_resource(resource_id)
            .await
            .map_err(|_| ResourceApplicationError::NotFound)?;
        if existing.project_id != auth.effective_scope().id().as_str() {
            return Err(ResourceApplicationError::NotFound);
        }
        if expected_generation.is_some_and(|expected| expected != existing.generation) {
            return Err(ResourceApplicationError::PreconditionConflict);
        }
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Delete)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let context = o3k_reconciler::CanonicalMutationContext::new(
            action,
            auth.principal().id().to_string(),
            auth.effective_scope().clone(),
            None,
            key,
            serde_json::json!({"resource_id": id}),
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        let receipt = self
            .compute
            .delete_server_for_auth_canonical(
                auth,
                o3k_domain::ServerId::from_uuid(resource_id),
                context,
            )
            .await
            .map_err(compute_error)?;
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(id.to_owned()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded
            ),
            resource: None,
        })
    }
}
