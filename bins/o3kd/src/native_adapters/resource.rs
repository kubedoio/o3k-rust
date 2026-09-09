use std::{collections::BTreeMap, sync::Arc};

use sha2::{Digest, Sha256};

use o3k_domain::{
    AttachmentAccessMode, StorageExecutionScope, Volume, VolumeAttachment, VolumeAttachmentId,
    VolumeAttachmentState, VolumeId, VolumeState,
};
use o3k_kernel::Controller;
use o3k_native_api::{
    compute::ServerItem,
    network::AddressRealmItem,
    resource::{
        ActionRequest, MutationResult, ResourceApplication, ResourceApplicationError,
        ResourceDescriptor, ValidatedCreateRequest, VolumeAttachmentWorkflow,
    },
};
use o3k_store::{
    DurableStore, NetworkRepository, PublicAddressRepository, RelationshipRepository,
    storage::StorageRepository,
};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait PublicAddressWorkflow: Send + Sync {
    async fn remove(&self, project_id: &str, allocation_id: Uuid) -> Result<(), String>;
}

/// Application adapter for generic native resource reads and mutations.
pub struct GenericResourceApplication {
    pub compute: Arc<o3k_compute::ComputeService>,
    pub image: Option<Arc<o3k_image::ImageService>>,
    pub network_service: Arc<o3k_network::NetworkService>,
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub storage_provider: Option<Arc<dyn o3k_storage::StorageProvider>>,
    pub server: Arc<dyn o3k_native_api::compute::ServerReader>,
    pub network: Arc<dyn o3k_native_api::network::NetworkReader>,
    pub external_controllers: Arc<BTreeMap<String, Arc<o3k_service_sdk::GrpcControllerAdapter>>>,
    pub public_allocator: Option<Arc<o3k_network::PublicAddressAllocator>>,
    pub public_address_workflow: Option<Arc<dyn PublicAddressWorkflow>>,
    pub network_external_realm_id: Option<Uuid>,
    pub attachment_workflow: Option<Arc<dyn VolumeAttachmentWorkflow>>,
}

impl GenericResourceApplication {
    async fn fail_native_create(
        &self,
        operation_id: Uuid,
        resource_id: Uuid,
        message: &str,
    ) -> Result<(), ResourceApplicationError> {
        let now = chrono::Utc::now().to_rfc3339();
        let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::Failed,
            1,
            None,
            Some(now),
            Some(message.to_owned()),
        )
        .map_err(|_| ResourceApplicationError::Internal)?;
        self.store
            .update_canonical_operation_lifecycle(operation_id, &lifecycle)
            .await
            .map_err(|_| ResourceApplicationError::Internal)?;
        if let Ok(record) = self.store.get_resource(resource_id).await {
            let _ = self
                .store
                .update_resource(
                    resource_id,
                    record.generation,
                    &record.desired_state,
                    "ERROR",
                    record.generation,
                    None,
                )
                .await;
        }
        Ok(())
    }

    async fn mark_operation_unknown(
        &self,
        operation_id: Uuid,
        message: &str,
    ) -> Result<(), ResourceApplicationError> {
        let now = chrono::Utc::now().to_rfc3339();
        let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::UnknownOutcome,
            1,
            Some(now),
            None,
            Some(message.to_owned()),
        )
        .map_err(|_| ResourceApplicationError::Internal)?;
        self.store
            .update_canonical_operation_lifecycle(operation_id, &lifecycle)
            .await
            .map(|_| ())
            .map_err(|_| ResourceApplicationError::Internal)
    }

    /// Reserve the canonical operation and resource before a domain adapter
    /// performs its side effect.  Native compatibility projections must not
    /// manufacture an operation identifier after the mutation has happened.
    async fn accept_native_create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        resource_id: Uuid,
        request: &ValidatedCreateRequest,
        idempotency_key: &str,
    ) -> Result<(Uuid, bool), ResourceApplicationError> {
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Create)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:create:{}", descriptor.resource_type, resource_id).as_bytes(),
        );
        // The generic resource index uses the historical storage kind for
        // volumes (`volume`), while the public manifest uses the namespaced
        // resource type (`volume:volume`). Keep the index identity stable so
        // canonical reservation and compatibility projections converge on
        // one durable row instead of creating an unreachable duplicate.
        let resource_kind = if descriptor.resource_type.to_string() == "volume:volume" {
            "volume".to_owned()
        } else {
            descriptor.resource_type.to_string()
        };
        let resource = o3k_store::ResourceRecord {
            id: resource_id,
            kind: resource_kind,
            project_id: auth.effective_scope().id().as_str().to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: serde_json::to_string(&request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?,
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
            idempotency_key.to_owned(),
            &descriptor.resource_type.to_string(),
            Some(&resource_id.to_string()),
            &request.spec,
            operation_id,
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        match self
            .store
            .create_or_replay_canonical_resource_operation(
                &resource, &operation, &canonical, &identity, None,
            )
            .await
            .map_err(|error| match error {
                o3k_store::StoreError::ResourceAlreadyExists
                | o3k_store::StoreError::IdempotencyConflict => {
                    ResourceApplicationError::IdempotencyConflict
                }
                _ => ResourceApplicationError::Internal,
            })? {
            o3k_store::CanonicalAcceptanceOutcome::Created { operation_id, .. } => {
                Ok((operation_id, false))
            }
            o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent { operation_id, .. } => {
                Ok((operation_id, true))
            }
            o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                Err(ResourceApplicationError::IdempotencyConflict)
            }
        }
    }

    /// The process configuration historically names the external network
    /// selector `...REALM_ID`, while compute/network composition consumes it
    /// as a canonical external-network ID. Resolve the active realm at the
    /// authority boundary before gateway/address operations use it.
    async fn external_realm_id(&self, project_id: &str) -> Result<Uuid, ResourceApplicationError> {
        let network_id = self
            .network_external_realm_id
            .ok_or(ResourceApplicationError::NotReady)?;
        self.network_service
            .list_canonical_realms_for_project(project_id, network_id)
            .await
            .map_err(|_| ResourceApplicationError::Conflict)?
            .into_iter()
            .find(|realm| realm.state == "active")
            .map(|realm| realm.id)
            .ok_or(ResourceApplicationError::Conflict)
    }

    async fn annotate_server_migration_metadata(
        &self,
        resource_id: Uuid,
        migration_id: &Uuid,
        source_key: &str,
    ) -> Result<o3k_store::ResourceRecord, ResourceApplicationError> {
        for _ in 0..4 {
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            let mut desired = serde_json::from_str::<serde_json::Value>(&resource.desired_state)
                .map_err(|_| ResourceApplicationError::Internal)?;
            let object = desired
                .as_object_mut()
                .ok_or(ResourceApplicationError::Internal)?;
            object.insert(
                "migration_id".to_owned(),
                serde_json::Value::String(migration_id.to_string()),
            );
            object.insert(
                "source_key".to_owned(),
                serde_json::Value::String(source_key.to_owned()),
            );
            let desired_state =
                serde_json::to_string(&desired).map_err(|_| ResourceApplicationError::Internal)?;
            if desired_state == resource.desired_state {
                return Ok(resource);
            }
            match self
                .store
                .update_resource(
                    resource.id,
                    resource.generation,
                    &desired_state,
                    &resource.observed_state,
                    resource.observed_generation,
                    resource.provider_id.as_deref(),
                )
                .await
            {
                Ok(updated) => return Ok(updated),
                Err(o3k_store::StoreError::StaleGeneration) => continue,
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
        }
        Err(ResourceApplicationError::Conflict)
    }
}

fn compute_error(error: o3k_compute::ComputeError) -> ResourceApplicationError {
    match error {
        o3k_compute::ComputeError::Unauthorized => ResourceApplicationError::Forbidden,
        o3k_compute::ComputeError::NotFound => ResourceApplicationError::NotFound,
        o3k_compute::ComputeError::InvalidRequest => ResourceApplicationError::Validation,
        o3k_compute::ComputeError::Conflict => ResourceApplicationError::Conflict,
        // A provider timeout/transport failure is deliberately not a terminal
        // application failure.  The compute journal retains the canonical
        // operation and reconciliation must observe it before any retry.
        o3k_compute::ComputeError::Provider(provider) if provider.is_unknown_outcome() => {
            ResourceApplicationError::Retryable
        }
        o3k_compute::ComputeError::Reconcile(o3k_reconciler::ReconcileError::Provider(
            ref provider,
        )) if provider.is_unknown_outcome() => ResourceApplicationError::Retryable,
        _ => ResourceApplicationError::Internal,
    }
}

fn image_error(error: o3k_image::ImageError) -> ResourceApplicationError {
    match error {
        // Audit admission failure occurs after the image operation may have
        // crossed an external side-effect boundary. Preserve uncertainty so
        // callers retry/reconcile the canonical operation instead of seeing a
        // fabricated terminal failure.
        o3k_image::ImageError::AuditUnavailable => ResourceApplicationError::Retryable,
        o3k_image::ImageError::Unauthorized => ResourceApplicationError::Forbidden,
        o3k_image::ImageError::NotFound => ResourceApplicationError::NotFound,
        o3k_image::ImageError::Conflict => ResourceApplicationError::Conflict,
        o3k_image::ImageError::InvalidMetadata
        | o3k_image::ImageError::UnsupportedFormat
        | o3k_image::ImageError::ChecksumMismatch => ResourceApplicationError::Validation,
        _ => ResourceApplicationError::Internal,
    }
}

/// Operation records are durable and may be visible to operators.  Never
/// persist an image-service display/source error (which can contain database
/// or filesystem details); the public operation projection can only expose a
/// stable failure class.
fn image_operation_error_category(error: &o3k_image::ImageError) -> &'static str {
    match error {
        o3k_image::ImageError::AuditUnavailable => "audit_unavailable",
        o3k_image::ImageError::TooLarge => "payload_too_large",
        o3k_image::ImageError::QuotaExceeded { .. } => "quota_exceeded",
        o3k_image::ImageError::UnsupportedFormat => "unsupported_format",
        o3k_image::ImageError::ChecksumMismatch => "checksum_mismatch",
        o3k_image::ImageError::InvalidMetadata => "invalid_metadata",
        o3k_image::ImageError::Unauthorized => "unauthorized",
        o3k_image::ImageError::NotFound => "not_found",
        o3k_image::ImageError::Conflict => "conflict",
        o3k_image::ImageError::Storage(_)
        | o3k_image::ImageError::CorruptMetadata(_)
        | o3k_image::ImageError::Store(_)
        | o3k_image::ImageError::InvalidPath
        | o3k_image::ImageError::OverlayFailed
        | o3k_image::ImageError::FormatVerificationFailed => "image_operation_failed",
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

fn server_json_with_resource(
    item: ServerItem,
    resource: Option<&o3k_store::ResourceRecord>,
) -> serde_json::Value {
    let mut value = server_json(item);
    if let Some(resource) = resource
        && let Ok(spec) = serde_json::from_str::<serde_json::Value>(&resource.desired_state)
    {
        for key in ["migration_id", "source_key"] {
            if let Some(text) = spec.get(key).and_then(serde_json::Value::as_str) {
                value["metadata"][key] = serde_json::Value::String(text.to_owned());
            }
        }
    }
    value
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

fn flavor_json(item: &o3k_compute::Flavor, owner_scope: &str) -> serde_json::Value {
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "compute:flavor",
        "metadata": {"id": item.id, "owner_scope": owner_scope},
        "spec": {
            "name": item.name,
            "vcpus": item.vcpus,
            "ram_mib": item.ram_mib,
            "disk_gib": item.disk_gib
        },
        "status": {"state": "ACTIVE"}
    })
}

fn image_json(item: &o3k_image::ImageRecord) -> serde_json::Value {
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "image:image",
        "metadata": {"id": item.id, "owner_scope": item.project_id},
        "spec": {
            "name": item.name,
            "visibility": item.visibility,
            "container_format": item.container_format,
            "disk_format": item.disk_format
        },
        "status": {"state": item.status}
    })
}

fn image_json_with_resource(
    item: &o3k_image::ImageRecord,
    resource: Option<&o3k_store::ResourceRecord>,
) -> serde_json::Value {
    let mut value = image_json(item);
    if let Some(resource) = resource {
        value["metadata"]["owner_scope"] = serde_json::Value::String(resource.project_id.clone());
        if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&resource.desired_state) {
            if let Some(migration_id) = spec.get("migration_id").and_then(serde_json::Value::as_str)
            {
                value["metadata"]["migration_id"] =
                    serde_json::Value::String(migration_id.to_owned());
            }
            if let Some(source_key) = spec.get("source_key").and_then(serde_json::Value::as_str) {
                value["metadata"]["source_key"] = serde_json::Value::String(source_key.to_owned());
            }
        }
    }
    value
}

fn native_volume_json(record: &o3k_store::VolumeRecord) -> serde_json::Value {
    let public_metadata = serde_json::to_value(&record.volume.metadata)
        .map(sanitize_public_spec)
        .unwrap_or_else(|_| serde_json::json!({}));
    let mut metadata = serde_json::json!({
        "id": record.volume.id.to_string(),
        "owner_scope": record.volume.project_id,
        "generation": record.volume.generation,
        "created_at": record.created_at,
    });
    for key in ["migration_id", "source_key"] {
        if let Some(value) = record.volume.metadata.get(key) {
            metadata[key] = serde_json::Value::String(value.clone());
        }
    }
    serde_json::json!({
        "api_version":"o3k.io/v1",
        "kind":"volume:volume",
        "metadata":metadata,
        "spec":{"size_bytes":record.volume.size_bytes,"volume_type":record.volume.volume_type,"name":record.volume.name,"description":record.volume.description,"metadata":public_metadata,"availability_zone":record.volume.availability_zone},
        "status":{"state":record.volume.state}
    })
}

fn native_attachment_json(
    record: &o3k_store::storage::VolumeAttachmentRecordV1,
    resource: Option<&o3k_store::ResourceRecord>,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "id": record.attachment.id.to_string(),
        "owner_scope": record.attachment.project_id,
        "generation": record.attachment.generation,
    });
    if let Some(resource) = resource
        && let Ok(spec) = serde_json::from_str::<serde_json::Value>(&resource.desired_state)
    {
        for key in ["migration_id", "source_key"] {
            if let Some(value) = spec.get(key) {
                metadata[key] = value.clone();
            }
        }
    }
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "volume:volume_attachment",
        "metadata": metadata,
        "spec": {
            "server_id": record.attachment.server_id,
            "volume_id": record.attachment.volume_id,
            "delete_on_termination": record.attachment.delete_on_termination,
        },
        "status": {"state": record.attachment.state},
    })
}

fn floating_ip_json(
    binding: &o3k_network::PublicAddressBinding,
    realm_id: Option<Uuid>,
    owner: &str,
    migration_id: Option<&str>,
    source_key: Option<&str>,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "id": binding.allocation_id,
        "owner_scope": owner,
        "generation": binding.generation,
    });
    if let Some(value) = migration_id {
        metadata["migration_id"] = value.into();
    }
    if let Some(value) = source_key {
        metadata["source_key"] = value.into();
    }
    serde_json::json!({
        "api_version":"o3k.io/v1", "kind":"network:floating_ip", "metadata":metadata,
        "spec":{"floating_network_id":realm_id,"floating_ip_address":binding.public_address,"port_id":binding.endpoint_id},
        "status":{"state":"ACTIVE"}
    })
}

fn generic_external_json(resource: &o3k_store::ResourceRecord) -> serde_json::Value {
    let spec = serde_json::from_str::<serde_json::Value>(&resource.desired_state)
        .map(sanitize_public_spec)
        .unwrap_or(serde_json::Value::Null);
    let mut metadata = serde_json::json!({
        "id": resource.id,
        "owner_scope": resource.project_id,
        "generation": resource.generation
    });
    if let Some(migration_id) = spec.get("migration_id").and_then(serde_json::Value::as_str) {
        metadata["migration_id"] = serde_json::Value::String(migration_id.to_owned());
    }
    if let Some(source_key) = spec.get("source_key").and_then(serde_json::Value::as_str) {
        metadata["source_key"] = serde_json::Value::String(source_key.to_owned());
    }
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": resource.kind,
        "metadata": metadata,
        "spec": spec,
        "status": {"state": resource.observed_state}
    })
}

/// External-controller desired state is not automatically a public contract.
/// Keep generic projections useful while structurally removing fields which
/// could carry credentials, user data, or provider-private topology.
pub(crate) fn sanitize_public_spec(value: serde_json::Value) -> serde_json::Value {
    // Normalize separators and case before matching.  Controller payloads
    // commonly use camelCase (for example `hostPath`) while compatibility
    // adapters use snake_case; a separator-sensitive deny list would leave a
    // simple spelling variant as a public secret/topology escape hatch.
    const FORBIDDEN: &[&str] = &[
        "password",
        "token",
        "secret",
        "credential",
        "privatekey",
        "userdata",
        "environment",
        "env",
        "connectionstring",
        "hostpath",
        "devicepath",
        "providerid",
        "providerreference",
        "providerresourceid",
        "provideroperationid",
        "backendid",
        "nodeid",
        "providerhost",
        "chap",
    ];
    match value {
        serde_json::Value::Object(mut object) => {
            object.retain(|key, _| {
                let normalized = key
                    .to_ascii_lowercase()
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .collect::<String>();
                !FORBIDDEN
                    .iter()
                    .any(|term| normalized == *term || normalized.contains(term))
            });
            serde_json::Value::Object(
                object
                    .into_iter()
                    .map(|(key, value)| (key, sanitize_public_spec(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sanitize_public_spec).collect())
        }
        other => other,
    }
}

fn public_spec_from_text(value: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(value)
        .map(sanitize_public_spec)
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod public_projection_tests {
    use super::{public_spec_from_text, sanitize_public_spec};

    #[test]
    fn generic_specs_cannot_expose_secret_or_provider_private_fields() {
        let value = sanitize_public_spec(serde_json::json!({
            "name": "safe",
            "nested": {"provider_password": "redacted", "disk": "vda"},
            "user-data": "do not expose",
            "provider_host": "/var/lib/private",
            "labels": [{"token": "redacted", "zone": "public"}],
        }));
        assert_eq!(value["name"], "safe");
        assert_eq!(value["nested"]["disk"], "vda");
        assert!(value["nested"].get("provider_password").is_none());
        assert!(value.get("user-data").is_none());
        assert!(value.get("provider_host").is_none());
        assert!(value["labels"][0].get("token").is_none());
        assert_eq!(value["labels"][0]["zone"], "public");
    }

    #[test]
    fn mutation_projection_parses_and_sanitizes_persisted_spec() {
        let value = public_spec_from_text(
            r#"{"name":"safe","provider_token":"hidden","nested":{"user_data":"hidden"}}"#,
        );
        assert_eq!(value["name"], "safe");
        assert!(value.get("provider_token").is_none());
        assert!(value["nested"].get("user_data").is_none());
    }

    #[test]
    fn sanitizer_rejects_camel_case_secret_and_topology_variants() {
        let value = sanitize_public_spec(serde_json::json!({
            "hostPath": "/srv/private",
            "device-path": "/dev/vda",
            "providerId": "backend-id",
            "providerReference": "private-reference",
            "providerResourceId": "private-resource",
            "providerOperationId": "private-operation",
            "backendId": "private-backend",
            "nodeId": "private-node",
            "connectionString": "postgres://private",
            "safeLabel": "ok",
        }));
        assert_eq!(value["safeLabel"], "ok");
        assert!(value.get("hostPath").is_none());
        assert!(value.get("device-path").is_none());
        assert!(value.get("providerId").is_none());
        assert!(value.get("providerReference").is_none());
        assert!(value.get("providerResourceId").is_none());
        assert!(value.get("providerOperationId").is_none());
        assert!(value.get("backendId").is_none());
        assert!(value.get("nodeId").is_none());
        assert!(value.get("connectionString").is_none());
    }
}

#[async_trait::async_trait]
impl ResourceApplication for GenericResourceApplication {
    async fn list_page(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        query: &o3k_native_api::resource::ListQuery,
    ) -> Result<o3k_native_api::resource::ResourcePage, ResourceApplicationError> {
        // `list` is retained as a compatibility-internal helper, but this
        // adapter already asks each durable repository for page_size + 1.
        // Convert that bounded probe into the explicit application boundary.
        let items = self.list(descriptor, auth, query).await?;
        let has_more = items.len() > query.page_size;
        Ok(o3k_native_api::resource::ResourcePage { items, has_more })
    }

    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        query: &o3k_native_api::resource::ListQuery,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "image:image" {
            let service = self
                .image
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let items = service
                .list_page_for_project(
                    auth.effective_scope().id().as_str(),
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map_err(image_error)?;
            let mut result = Vec::with_capacity(items.len());
            for item in items {
                let resource = self.store.get_resource(item.id).await.ok();
                result.push(image_json_with_resource(&item, resource.as_ref()));
            }
            return Ok(result);
        }
        if self
            .external_controllers
            .contains_key(&descriptor.owning_service)
        {
            return self
                .store
                .list_resources_page_by_observed_state(
                    auth.effective_scope().id().as_str(),
                    &descriptor.resource_type.to_string(),
                    query.observed_state.as_deref(),
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|resources| resources.iter().map(generic_external_json).collect())
                .map_err(|_| ResourceApplicationError::Internal);
        }
        if matches!(
            descriptor.resource_type.to_string().as_str(),
            "network:network"
                | "network:subnet"
                | "network:port"
                | "network:security_group"
                | "network:security_group_rule"
                | "network:router"
                | "network:router_interface"
                | "network:floating_ip"
        ) {
            return self
                .store
                .list_resources_page_by_observed_state(
                    auth.effective_scope().id().as_str(),
                    &descriptor.resource_type.to_string(),
                    query.observed_state.as_deref(),
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|resources| resources.iter().map(generic_external_json).collect())
                .map_err(|_| ResourceApplicationError::Internal);
        }
        match descriptor.resource_type.to_string().as_str() {
            "compute:flavor" => self
                .compute
                .flavors_for_project_page(
                    auth.effective_scope().id().as_str(),
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|items| {
                    items
                        .iter()
                        .map(|item| flavor_json(item, auth.effective_scope().id().as_str()))
                        .collect()
                })
                .map_err(compute_error),
            "compute:server" => self
                .server
                .list_servers_page(
                    auth,
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|items| items.into_iter().map(server_json).collect())
                .map_err(generic_read_error),
            "network:address_realm" => self
                .network
                .list_address_realms_page(
                    auth,
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|items| items.into_iter().map(realm_json).collect())
                .map_err(generic_read_error),
            "network:network" => self
                .network_service
                .list_canonical_networks_page(
                    auth,
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|items| items.iter().map(network_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            "network:floating_ip" => {
                // The file-backed allocator loads its complete state vector
                // and is execution state, not a bounded canonical resource
                // repository. Do not expose it as native tenant truth until
                // a durable, scope-enforced paged authority is available.
                Err(ResourceApplicationError::NotReady)
            }
            "volume:volume" => self
                .store
                .list_volumes_page(
                    auth.effective_scope().id().as_str(),
                    query.continuation_id.as_deref(),
                    query.page_size.saturating_add(1),
                )
                .await
                .map(|items| items.iter().map(native_volume_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            "volume:volume_attachment" => {
                let items = self
                    .store
                    .list_volume_attachments_v1_page(
                        auth.effective_scope().id().as_str(),
                        query.continuation_id.as_deref(),
                        query.page_size.saturating_add(1),
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let mut result = Vec::new();
                for item in items {
                    if item.attachment.state == VolumeAttachmentState::Attached {
                        let resource = self
                            .store
                            .get_resource(item.attachment.id.as_uuid())
                            .await
                            .ok();
                        result.push(native_attachment_json(&item, resource.as_ref()));
                    }
                }
                Ok(result)
            }
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn validate_list_cursor(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        cursor_id: &str,
    ) -> Result<bool, ResourceApplicationError> {
        let id = Uuid::parse_str(cursor_id).map_err(|_| ResourceApplicationError::Validation)?;
        let Ok(resource) = self.store.get_resource(id).await else {
            return Ok(false);
        };
        let _ = descriptor;
        Ok(resource.project_id == auth.effective_scope().id().as_str()
            && resource.observed_state != "DELETED")
    }

    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "image:image" {
            let service = self
                .image
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let item = service.get(auth, id).await.map_err(image_error)?;
            let resource = self.store.get_resource(id).await.ok();
            return Ok(image_json_with_resource(&item, resource.as_ref()));
        }
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
        // Flavor resources created through the generic canonical path carry
        // migration ownership in their durable record.  Preserve that
        // metadata on read so the migration runner can fence and verify the
        // exact resource it created; seeded compatibility flavors continue
        // through the concrete compute read below.
        if descriptor.resource_type.to_string() == "compute:flavor"
            && let Ok(resource) = self.store.get_resource(id).await
            && resource.kind == "compute_flavor"
            && resource.project_id == auth.effective_scope().id().as_str()
        {
            return Ok(generic_external_json(&resource));
        }
        if descriptor.resource_type.to_string() == "network:network"
            && let Ok(resource) = self.store.get_resource(id).await
            && resource.kind == "network:network"
            && resource.project_id == auth.effective_scope().id().as_str()
        {
            return Ok(generic_external_json(&resource));
        }
        if matches!(
            descriptor.resource_type.to_string().as_str(),
            "network:subnet"
                | "network:port"
                | "network:security_group"
                | "network:security_group_rule"
                | "network:router"
                | "network:router_interface"
                | "network:floating_ip"
        ) && let Ok(resource) = self.store.get_resource(id).await
            && resource.kind == descriptor.resource_type.to_string()
            && resource.project_id == auth.effective_scope().id().as_str()
        {
            return Ok(generic_external_json(&resource));
        }
        match descriptor.resource_type.to_string().as_str() {
            "compute:flavor" => self
                .compute
                .flavor_for_auth(auth, id)
                .await
                .map(|item| flavor_json(&item, auth.effective_scope().id().as_str()))
                .map_err(compute_error),
            "compute:server" => {
                let item = self
                    .server
                    .show_server(auth, id)
                    .await
                    .map_err(generic_read_error)?;
                let resource = self
                    .store
                    .get_resource(
                        item.id
                            .parse::<Uuid>()
                            .map_err(|_| ResourceApplicationError::Internal)?,
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                Ok(server_json_with_resource(item, Some(&resource)))
            }
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
                .map(|item| {
                    // Native migration records are an ownership/evidence
                    // projection over the canonical network authority.
                    network_json(&item)
                })
                .map_err(|_| ResourceApplicationError::NotFound),
            "network:floating_ip" => {
                let allocator = self
                    .public_allocator
                    .as_ref()
                    .ok_or(ResourceApplicationError::NotReady)?;
                let item = allocator
                    .get(auth.effective_scope().id().as_str(), id)
                    .map_err(|_| ResourceApplicationError::NotFound)?;
                let record = self.store.get_resource(id).await.ok();
                let spec = record
                    .as_ref()
                    .and_then(|r| serde_json::from_str::<serde_json::Value>(&r.desired_state).ok());
                Ok(floating_ip_json(
                    &item,
                    Some(
                        self.external_realm_id(auth.effective_scope().id().as_str())
                            .await?,
                    ),
                    auth.effective_scope().id().as_str(),
                    spec.as_ref()
                        .and_then(|v| v.get("migration_id"))
                        .and_then(serde_json::Value::as_str),
                    spec.as_ref()
                        .and_then(|v| v.get("source_key"))
                        .and_then(serde_json::Value::as_str),
                ))
            }
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
            "volume:volume_attachment" => {
                let record = self
                    .store
                    .get_volume_attachment_v1(id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?
                    .filter(|item| {
                        item.attachment.project_id == auth.effective_scope().id().as_str()
                            && item.attachment.state == VolumeAttachmentState::Attached
                    })
                    .ok_or(ResourceApplicationError::NotFound)?;
                let resource = self.store.get_resource(id).await.ok();
                Ok(native_attachment_json(&record, resource.as_ref()))
            }
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        request: ValidatedCreateRequest,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "image:image" {
            let service = self
                .image
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let source = request
                .spec
                .get("source")
                .filter(|value| value.is_object())
                .unwrap_or(&request.spec);
            let source = source
                .get("image")
                .filter(|value| value.is_object())
                .unwrap_or(source);
            let name = source
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(ResourceApplicationError::Validation)?
                .to_owned();
            let visibility = source
                .get("visibility")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("private")
                .to_owned();
            let container_format = source
                .get("container_format")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("bare")
                .to_owned();
            let disk_format = source
                .get("disk_format")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("qcow2")
                .to_owned();
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?;
            let canonical_id = canonical_id.unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let image = service.get(auth, canonical_id).await.map_err(image_error)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY"
                        || existing.observed_state == "active",
                    resource: Some(image_json_with_resource(&image, Some(&existing))),
                });
            }
            let image = match service
                .create_with_id_and_operation(
                    auth,
                    canonical_id,
                    operation_id,
                    name,
                    visibility,
                    container_format,
                    disk_format,
                )
                .await
            {
                Ok(image) => image,
                Err(error) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical image creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(image_error(error));
                }
            };
            let now = chrono::Utc::now().to_rfc3339();
            self.store
                .update_resource(
                    canonical_id,
                    1,
                    &serde_json::to_string(&request.spec)
                        .map_err(|_| ResourceApplicationError::Internal)?,
                    "READY",
                    1,
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
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
                resource_id: Some(image.id.to_string()),
                complete: true,
                resource: Some(image_json_with_resource(
                    &image,
                    Some(
                        &self
                            .store
                            .get_resource(image.id)
                            .await
                            .map_err(|_| ResourceApplicationError::Internal)?,
                    ),
                )),
            });
        }
        if descriptor.resource_type.to_string() == "compute:flavor" {
            let source = request
                .spec
                .get("source")
                .filter(|value| value.is_object())
                .unwrap_or(&request.spec);
            let source = source
                .get("flavor")
                .filter(|value| value.is_object())
                .unwrap_or(source);
            let name = source
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(ResourceApplicationError::Validation)?
                .to_owned();
            let vcpus = source
                .get("vcpus")
                .and_then(serde_json::Value::as_u64)
                .ok_or(ResourceApplicationError::Validation)?;
            let ram_mib = source
                .get("ram_mib")
                .or_else(|| source.get("ram"))
                .or_else(|| source.get("memory_mib"))
                .and_then(serde_json::Value::as_u64)
                .ok_or(ResourceApplicationError::Validation)?;
            let disk_gib = source
                .get("disk_gib")
                .or_else(|| source.get("disk"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .unwrap_or_else(|| {
                    Uuid::new_v5(
                        &Uuid::NAMESPACE_OID,
                        format!("{}:compute:flavor:{}", auth.effective_scope().id(), key)
                            .as_bytes(),
                    )
                });
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Create)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:create:{resource_id}", descriptor.resource_type).as_bytes(),
            );
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
            match self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id, ..
                } => {
                    let existing = self
                        .store
                        .get_resource(resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(resource_id.to_string()),
                        complete: existing.observed_state == "ACTIVE",
                        resource: Some(serde_json::json!({
                            "api_version": "o3k.io/v1",
                            "kind": "compute:flavor",
                            "metadata": {"id": resource_id, "generation": existing.generation},
                            "spec": public_spec_from_text(&existing.desired_state),
                            "status": {"state": existing.observed_state}
                        })),
                    });
                }
                o3k_store::CanonicalAcceptanceOutcome::Created { .. } => {}
            }
            let flavor = match self
                .compute
                .create_flavor_for_auth_with_id(
                    auth,
                    resource_id,
                    name,
                    u32::try_from(vcpus).map_err(|_| ResourceApplicationError::Validation)?,
                    ram_mib,
                    disk_gib,
                )
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical flavor creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(compute_error(error));
                }
            };
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
            // The concrete compute service owns the flavor fields, while the
            // generic migration envelope owns run correlation.  Retain the
            // latter in the durable desired state so later reads can prove
            // migration ownership without trusting an in-memory response.
            if request.spec.get("migration_id").is_some()
                || request.spec.get("source_key").is_some()
            {
                let mut desired = serde_json::to_value(&flavor)
                    .map_err(|_| ResourceApplicationError::Internal)?;
                if let Some(object) = desired.as_object_mut() {
                    for key in ["migration_id", "source_key"] {
                        if let Some(value) = request.spec.get(key) {
                            object.insert(key.to_owned(), value.clone());
                        }
                    }
                }
                let record = self
                    .store
                    .get_resource(flavor.id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let desired = serde_json::to_string(&desired)
                    .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_resource(
                        flavor.id,
                        record.generation,
                        &desired,
                        &record.observed_state,
                        record.observed_generation,
                        record.provider_id.as_deref(),
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
            }
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(flavor.id.to_string()),
                complete: true,
                resource: Some(flavor_json(&flavor, auth.effective_scope().id().as_str())),
            });
        }
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
                        desired_spec: request.spec.into_value(),
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
                    "spec": public_spec_from_text(&resource.desired_state),
                    "status": {"state": if complete {"READY"} else {"PROVISIONING"}}
                })),
            });
        }
        if descriptor.resource_type.to_string() == "volume:volume_attachment" {
            let workflow = self
                .attachment_workflow
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let server_id = source
                .get("server_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let volume_id = source
                .get("volume_id")
                .or_else(|| source.get("id"))
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            self.server
                .show_server(auth, server_id)
                .await
                .map_err(generic_read_error)?;
            let volume = self
                .store
                .get_volume(volume_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                .filter(|record| {
                    record.volume.project_id == auth.effective_scope().id().as_str()
                        && record.volume.state == VolumeState::Available
                })
                .ok_or(ResourceApplicationError::Conflict)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let attachment_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .unwrap_or_else(|| {
                    Uuid::new_v5(
                        &Uuid::NAMESPACE_OID,
                        format!("{}:{}", auth.effective_scope().id(), key).as_bytes(),
                    )
                });
            let record = o3k_store::storage::VolumeAttachmentRecordV1 {
                attachment: VolumeAttachment {
                    id: VolumeAttachmentId::from_uuid(attachment_id),
                    project_id: auth.effective_scope().id().as_str().to_owned(),
                    volume_id: volume.volume.id,
                    server_id,
                    execution_scope: StorageExecutionScope::Host("local".to_owned()),
                    access_mode: AttachmentAccessMode::ReadWrite,
                    delete_on_termination: request
                        .spec
                        .get("delete_on_termination")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    state: VolumeAttachmentState::Reserved,
                    generation: 1,
                    operation_id: None,
                },
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, attachment_id, &request, &key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_volume_attachment_v1(attachment_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?
                    .ok_or(ResourceApplicationError::NotFound)?;
                let resource = self
                    .store
                    .get_resource(attachment_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let operation = self
                    .store
                    .get_canonical_operation(operation_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(attachment_id.to_string()),
                    complete: operation.state == o3k_store::OperationState::Succeeded,
                    resource: Some(native_attachment_json(&existing, Some(&resource))),
                });
            }
            if self
                .store
                .insert_volume_attachment_v1(&record)
                .await
                .is_err()
            {
                // The canonical reservation precedes the domain insert.  Never
                // leave a reserved operation pending when the second durable
                // write rejects (including a concurrent attachment conflict).
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Failed,
                    1,
                    Some(now.clone()),
                    Some(now),
                    Some("volume attachment reservation failed".to_owned()),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_resource(attachment_id, 1, "{}", "ERROR", 0, None)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(ResourceApplicationError::Conflict);
            }
            let resource = self
                .store
                .get_resource(attachment_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            if let Err(error) = workflow.attach(attachment_id).await {
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Retryable,
                    1,
                    Some(now),
                    None,
                    Some(error),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(ResourceApplicationError::Retryable);
            }
            let attached = self
                .store
                .get_volume_attachment_v1(attachment_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                .ok_or(ResourceApplicationError::Internal)?;
            let now = chrono::Utc::now().to_rfc3339();
            let complete = attached.attachment.state == VolumeAttachmentState::Attached;
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                if complete {
                    o3k_kernel::OperationState::Succeeded
                } else {
                    o3k_kernel::OperationState::Running
                },
                1,
                Some(now.clone()),
                complete.then_some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(attachment_id.to_string()),
                complete,
                resource: Some(native_attachment_json(&attached, Some(&resource))),
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
                #[serde(default, rename = "canonical_id")]
                _canonical_id: Option<Uuid>,
                #[serde(default)]
                migration_id: Option<Uuid>,
                #[serde(default)]
                source_key: Option<String>,
            }
            let spec: VolumeSpec = serde_json::from_value(request.spec.clone().into_value())
                .map_err(|_| ResourceApplicationError::Validation)?;
            if spec.size_bytes == 0 || spec.volume_type.trim().is_empty() {
                return Err(ResourceApplicationError::Validation);
            }
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_id = spec._canonical_id.unwrap_or_else(|| {
                Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    format!("{}:{}", auth.effective_scope().id(), key).as_bytes(),
                )
            });
            // Reserve the canonical resource/operation/idempotency tuple
            // before inserting the provider-facing volume row.  This makes
            // retries and process recovery observable through Operations and
            // prevents a provider side effect without a durable operation.
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, resource_id, &request, &key)
                .await?;
            if replayed {
                let operation = self
                    .store
                    .get_canonical_operation(operation_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let existing = self
                    .store
                    .get_volume(resource_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(resource_id.to_string()),
                    complete: matches!(operation.state, o3k_store::OperationState::Succeeded),
                    resource: existing.as_ref().map(native_volume_json),
                });
            }
            let mut metadata = spec.metadata.unwrap_or_default();
            if let Some(migration_id) = spec.migration_id {
                metadata.insert("migration_id".to_owned(), migration_id.to_string());
            }
            if let Some(source_key) = spec.source_key {
                metadata.insert("source_key".to_owned(), source_key);
            }
            let volume = Volume {
                id: VolumeId::from_uuid(resource_id),
                project_id: auth.effective_scope().id().as_str().to_owned(),
                name: spec.name.unwrap_or_else(|| resource_id.to_string()),
                description: spec.description.unwrap_or_default(),
                metadata,
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
            let compatibility_generation = record.volume.generation;
            let Some(provider) = self.storage_provider.clone() else {
                self.fail_native_create(
                    operation_id,
                    resource_id,
                    "volume storage provider is not configured",
                )
                .await?;
                return Err(ResourceApplicationError::NotReady);
            };
            match self.store.insert_volume(&record).await {
                Ok(()) => {}
                Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                    self.fail_native_create(
                        operation_id,
                        resource_id,
                        "volume reservation conflict",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Conflict);
                }
                Err(_) => {
                    self.fail_native_create(
                        operation_id,
                        resource_id,
                        "volume durable insert failed",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Internal);
                }
            }
            if let Err(error) =
                o3k_api::realize_native_volume_create(self.store.clone(), provider, record).await
            {
                // Provider unavailability/uncertainty is deliberately kept
                // unknown; callers must observe/reconcile before retrying.
                // A timeout can happen both during the provider mutation and
                // during the mandatory post-mutation observation.  In either
                // case the control plane cannot safely assert that the
                // volume was not created.  Keep the canonical operation
                // unknown until reconciliation observes the provider.  Do
                // not classify this from the HTTP status: provider adapters
                // intentionally return a bounded, human-readable error.
                let error_class = error.to_ascii_lowercase();
                if error_class.contains("unknown")
                    || error_class.contains("unavailable")
                    || error_class.contains("timeout")
                    || error_class.contains("timed out")
                    || error_class.contains("deadline")
                {
                    // Never persist provider exception text in the durable
                    // operation record: adapters may accidentally include
                    // private paths, connection details, or credentials.
                    self.mark_operation_unknown(operation_id, "provider_outcome_unknown")
                        .await?;
                    // The public mutation envelope currently represents this
                    // durable state as an accepted/retryable response; the
                    // canonical Operation retains the unknown-outcome class.
                    return Err(ResourceApplicationError::Retryable);
                }
                // Keep durable failures categorical; the raw provider error
                // is an internal diagnostic and must not become tenant truth.
                self.fail_native_create(operation_id, resource_id, "provider_operation_failed")
                    .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            // The legacy generic-resource index is a compatibility projection
            // used by relationship tests and older native callers.  The
            // canonical volume above remains the sole authority.
            match self
                .store
                .insert_resource(&o3k_store::ResourceRecord {
                    id: resource_id,
                    kind: "volume".to_owned(),
                    project_id: auth.effective_scope().id().as_str().to_owned(),
                    // The native volume row is authoritative and has already
                    // advanced through provider realization.  Keep the
                    // compatibility projection at the same generation so
                    // later lifecycle updates cannot be rejected as stale.
                    generation: compatibility_generation as i64,
                    observed_generation: compatibility_generation as i64,
                    desired_state: "available".to_owned(),
                    observed_state: "available".to_owned(),
                    provider_id: None,
                })
                .await
            {
                Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
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
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("network").unwrap_or(source);
            let name = source
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(ResourceApplicationError::Validation)?
                .to_owned();
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            // Reserve the canonical operation before touching the network
            // authority.  The migration envelope used to manufacture an
            // operation UUID after the side effect, which made retries and
            // failures invisible to the Operations API.
            let key = idempotency_key
                .map(str::to_owned)
                .or_else(|| {
                    request
                        .spec
                        .get("migration_id")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| format!("migration:{value}"))
                })
                .unwrap_or_else(|| format!("native:create:{canonical_id}"));
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, &key)
                .await?;
            if replayed {
                let resource = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let operation = self
                    .store
                    .get_canonical_operation(operation_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: operation.state == o3k_store::OperationState::Succeeded,
                    resource: Some(generic_external_json(&resource)),
                });
            }
            let network = match self
                .network_service
                .create_network_for_project_with_id(
                    auth.effective_scope().id().as_str(),
                    canonical_id,
                    name,
                )
                .await
            {
                Ok(network) => network,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical network creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let canonical = self
                .network_service
                .get_canonical_network(auth, network.id)
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
                resource_id: Some(network.id.to_string()),
                complete: true,
                resource: Some(if request.spec.get("migration_id").is_some() {
                    generic_external_json(
                        &self
                            .store
                            .get_resource(network.id)
                            .await
                            .map_err(|_| ResourceApplicationError::Internal)?,
                    )
                } else {
                    network_json(&canonical)
                }),
            });
        }
        if descriptor.resource_type.to_string() == "network:subnet" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("subnet").unwrap_or(source);
            let network_id = source
                .get("network_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|v| v.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let cidr = source
                .get("cidr")
                .and_then(serde_json::Value::as_str)
                .ok_or(ResourceApplicationError::Validation)?
                .to_owned();
            let name = source
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("migrated-subnet")
                .to_owned();
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let subnet = match self
                .network_service
                .create_subnet_for_project_with_id(
                    auth.effective_scope().id().as_str(),
                    canonical_id,
                    network_id,
                    name,
                    cidr,
                    None,
                    None,
                    None,
                )
                .await
            {
                Ok(subnet) => subnet,
                Err(_) => {
                    self.store
                        .update_resource(
                            canonical_id,
                            1,
                            &serde_json::to_string(&request.spec)
                                .map_err(|_| ResourceApplicationError::Internal)?,
                            "ERROR",
                            1,
                            None,
                        )
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical subnet creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let id = subnet.id;
            let record = self
                .store
                .get_resource(id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(id, 1, &record.desired_state, "READY", 1, None)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(
                    operation_id,
                    &o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Succeeded,
                        1,
                        Some(chrono::Utc::now().to_rfc3339()),
                        Some(chrono::Utc::now().to_rfc3339()),
                        None,
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:port" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("port").unwrap_or(source);
            let network_id = source
                .get("network_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|v| v.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let name = source
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("migrated-port")
                .to_owned();
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let requested_fixed_ip = source
                .get("fixed_ips")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| {
                    Some((
                        item.get("subnet_id")?.as_str()?.parse().ok()?,
                        item.get("ip_address")?.as_str()?.parse().ok(),
                    ))
                });
            let port = match self
                .network_service
                .create_port_for_project_with_id_and_fixed_ip(
                    auth.effective_scope().id().as_str(),
                    canonical_id,
                    network_id,
                    name,
                    requested_fixed_ip,
                )
                .await
            {
                Ok(port) => port,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical port creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let id = port.id;
            let record = self
                .store
                .get_resource(id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(id, 1, &record.desired_state, "READY", 1, None)
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
                resource_id: Some(id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:security_group" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("security_group").unwrap_or(source);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let group = match self
                .network_service
                .create_security_group_for_project_with_id(
                    auth.effective_scope().id().as_str(),
                    canonical_id,
                    source
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .ok_or(ResourceApplicationError::Validation)?
                        .to_owned(),
                    source
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
                .await
            {
                Ok(group) => group,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical security group creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    let desired = serde_json::to_string(&request.spec)
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_resource(canonical_id, 1, &desired, "ERROR", 1, None)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let record = self
                .store
                .get_resource(group.id)
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
                .update_resource(group.id, 1, &record.desired_state, "READY", 1, None)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(group.id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:security_group_rule" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("security_group_rule").unwrap_or(source);
            let group_id = source
                .get("security_group_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let rule = match self
                .network_service
                .create_security_group_rule_for_project_with_id(
                    auth.effective_scope().id().as_str(),
                    canonical_id,
                    group_id,
                    source
                        .get("direction")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("ingress")
                        .to_owned(),
                    source
                        .get("protocol")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tcp")
                        .to_owned(),
                    source
                        .get("port_range_min")
                        .and_then(serde_json::Value::as_u64)
                        .map(|value| {
                            u16::try_from(value).map_err(|_| ResourceApplicationError::Validation)
                        })
                        .transpose()?,
                    source
                        .get("port_range_max")
                        .and_then(serde_json::Value::as_u64)
                        .map(|value| {
                            u16::try_from(value).map_err(|_| ResourceApplicationError::Validation)
                        })
                        .transpose()?,
                    source
                        .get("remote_ip_prefix")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                )
                .await
            {
                Ok(rule) => rule,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical security group rule creation failed".to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    let desired = serde_json::to_string(&request.spec)
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_resource(canonical_id, 1, &desired, "ERROR", 1, None)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let record = self
                .store
                .get_resource(rule.id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(rule.id, 1, &record.desired_state, "READY", 1, None)
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
                resource_id: Some(rule.id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:router" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("router").unwrap_or(source);
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let gateway = match self
                .network_service
                .create_l3_gateway_for_project_with_id(
                    canonical_id,
                    auth.effective_scope().id().as_str(),
                    source
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .ok_or(ResourceApplicationError::Validation)?
                        .to_owned(),
                    Some(
                        self.external_realm_id(auth.effective_scope().id().as_str())
                            .await?,
                    ),
                    source
                        .get("enable_snat")
                        .and_then(serde_json::Value::as_bool)
                        .or_else(|| {
                            source
                                .get("external_gateway_info")
                                .and_then(|value| value.get("enable_snat"))
                                .and_then(serde_json::Value::as_bool)
                        })
                        .unwrap_or(true),
                )
                .await
            {
                Ok(value) => value,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical router creation failed".into()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    let desired = serde_json::to_string(&request.spec)
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_resource(canonical_id, 1, &desired, "ERROR", 1, None)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let record = self
                .store
                .get_resource(gateway.id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(
                    gateway.id,
                    record.generation,
                    &record.desired_state,
                    "READY",
                    record.generation,
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
                resource_id: Some(gateway.id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:router_interface" {
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("router_interface").unwrap_or(source);
            let router_id = source
                .get("router_id")
                .or_else(|| source.get("device_id"))
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let subnet_id = source
                .get("subnet_id")
                .or_else(|| {
                    source
                        .get("fixed_ips")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|items| items.first())
                        .and_then(|item| item.get("subnet_id"))
                })
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or(ResourceApplicationError::Validation)?;
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?
                .unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_id, replayed) = self
                .accept_native_create(descriptor, auth, canonical_id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(canonical_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(canonical_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let attachment = match self
                .network_service
                .attach_l3_gateway_realm_with_id(
                    canonical_id,
                    auth.effective_scope().id().as_str(),
                    &router_id,
                    &subnet_id,
                )
                .await
            {
                Ok(value) => value,
                Err(_) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Failed,
                        1,
                        None,
                        Some(now),
                        Some("canonical router interface creation failed".into()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    let desired = serde_json::to_string(&request.spec)
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_resource(canonical_id, 1, &desired, "ERROR", 1, None)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            let record = self
                .store
                .get_resource(attachment.id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(
                    attachment.id,
                    record.generation,
                    &record.desired_state,
                    "READY",
                    record.generation,
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
                resource_id: Some(attachment.id.to_string()),
                complete: true,
                resource: Some(generic_external_json(&record)),
            });
        }
        if descriptor.resource_type.to_string() == "network:floating_ip" {
            let allocator = self
                .public_allocator
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let realm_id = self
                .external_realm_id(auth.effective_scope().id().as_str())
                .await?;
            let source = request.spec.get("source").unwrap_or(&request.spec);
            let source = source.get("floatingip").unwrap_or(source);
            // The source cloud's external-network UUID is not a destination
            // identity and is therefore intentionally not rewritten when the
            // source project cannot read the shared public network.  The
            // destination's configured external realm is the authority for
            // this bounded public-address pool; require the source request to
            // carry an external-network reference, but never treat its UUID
            // as an O3K resource identity.
            if source
                .get("floating_network_id")
                .and_then(serde_json::Value::as_str)
                .is_none()
            {
                return Err(ResourceApplicationError::Validation);
            }
            let port_id = source
                .get("port_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok());
            if let Some(port_id) = port_id {
                self.network_service
                    .get_port(auth, port_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Validation)?;
            }
            let canonical_id = request
                .spec
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<Uuid>)
                .transpose()
                .map_err(|_| ResourceApplicationError::Validation)?;
            let id = canonical_id.unwrap_or_else(Uuid::now_v7);
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let (operation_uuid, replayed) = self
                .accept_native_create(descriptor, auth, id, &request, key)
                .await?;
            if replayed {
                let existing = self
                    .store
                    .get_resource(id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                let binding = self
                    .store
                    .get_public_address(auth.effective_scope().id().as_str(), id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?
                    .ok_or(ResourceApplicationError::Internal)?;
                let binding = o3k_network::PublicAddressBinding {
                    allocation_id: binding.allocation_id,
                    operation_id: binding.operation_id,
                    project_id: binding.project_id,
                    public_address: binding.public_address,
                    endpoint_id: binding.endpoint_id,
                    generation: binding.generation,
                };
                return Ok(MutationResult {
                    operation_id: operation_uuid.to_string(),
                    resource_id: Some(id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(floating_ip_json(
                        &binding,
                        Some(realm_id),
                        auth.effective_scope().id().as_str(),
                        request
                            .spec
                            .get("migration_id")
                            .and_then(serde_json::Value::as_str),
                        request
                            .spec
                            .get("source_key")
                            .and_then(serde_json::Value::as_str),
                    )),
                });
            }
            let (first, last) = allocator.pool_bounds();
            let mut binding = match self
                .store
                .allocate_public_address(
                    auth.effective_scope().id().as_str(),
                    &operation_uuid.to_string(),
                    id,
                    first,
                    last,
                )
                .await
            {
                Ok(binding) => binding,
                Err(_error) => {
                    // Provider errors may contain private connection
                    // details; keep logs categorical and correlate via
                    // the durable operation id.
                    tracing::warn!(operation_id = %operation_uuid, "floating address allocation rejected");
                    self.fail_native_create(
                        operation_uuid,
                        id,
                        "floating address allocation failed",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Conflict);
                }
            };
            if let Some(port_id) = port_id {
                binding = match self
                    .store
                    .associate_public_address(
                        auth.effective_scope().id().as_str(),
                        binding.allocation_id,
                        port_id,
                    )
                    .await
                {
                    Ok(binding) => binding,
                    Err(_error) => {
                        tracing::warn!(allocation_id = %binding.allocation_id, port_id = %port_id, "floating address association rejected");
                        let _ = self
                            .store
                            .release_public_address(
                                auth.effective_scope().id().as_str(),
                                binding.allocation_id,
                            )
                            .await;
                        self.fail_native_create(
                            operation_uuid,
                            id,
                            "floating address association failed",
                        )
                        .await?;
                        return Err(ResourceApplicationError::Conflict);
                    }
                };
            }
            let record = self
                .store
                .get_resource(id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            if self
                .store
                .update_resource(
                    id,
                    record.generation,
                    &record.desired_state,
                    "READY",
                    binding.generation as i64,
                    None,
                )
                .await
                .is_err()
            {
                self.mark_operation_unknown(
                    operation_uuid,
                    "floating IP resource projection failed",
                )
                .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                o3k_kernel::OperationState::Succeeded,
                1,
                Some(now.clone()),
                Some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            if self
                .store
                .update_canonical_operation_lifecycle(operation_uuid, &lifecycle)
                .await
                .is_err()
            {
                self.mark_operation_unknown(
                    operation_uuid,
                    "floating IP operation projection failed",
                )
                .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            let binding = o3k_network::PublicAddressBinding {
                allocation_id: binding.allocation_id,
                operation_id: binding.operation_id,
                project_id: binding.project_id,
                public_address: binding.public_address,
                endpoint_id: binding.endpoint_id,
                generation: binding.generation,
            };
            return Ok(MutationResult {
                operation_id: operation_uuid.to_string(),
                resource_id: Some(id.to_string()),
                complete: true,
                resource: Some(floating_ip_json(
                    &binding,
                    Some(realm_id),
                    auth.effective_scope().id().as_str(),
                    request
                        .spec
                        .get("migration_id")
                        .and_then(serde_json::Value::as_str),
                    request
                        .spec
                        .get("source_key")
                        .and_then(serde_json::Value::as_str),
                )),
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct ComputeSpec {
            name: String,
            image_id: String,
            flavor_id: Uuid,
            network_ids: Vec<String>,
            #[serde(default)]
            key_name: Option<String>,
            #[serde(default)]
            ssh_public_key: Option<String>,
            // MigrationRunner carries source identity alongside the
            // destination compute intent. Keep the semantic payload strict
            // while accepting that bounded execution metadata at this
            // compatibility edge.
            #[serde(default, rename = "canonical_id")]
            _canonical_id: Option<Uuid>,
            #[serde(default)]
            migration_id: Option<Uuid>,
            #[serde(default)]
            source_key: Option<String>,
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
        let key = key.replace('/', "_");
        let canonical_id = spec._canonical_id;
        let compute_key = canonical_id.map_or_else(|| key.clone(), |id| format!("canonical:{id}"));
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
            compute_key.clone(),
            semantic_request,
        )
        .map_err(|error| {
            tracing::warn!(error = ?error, "canonical native server context rejected");
            ResourceApplicationError::Validation
        })?;
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
                    config_drive: spec.ssh_public_key.map(|ssh_public_key| {
                        o3k_provider::ConfigDriveRequest {
                            user_data: Vec::new(),
                            vendor_data: None,
                            ssh_public_key,
                        }
                    }),
                    // Keep provider command identity scoped even when the
                    // client reuses the same canonical key in another tenant.
                    idempotency_key: format!("{}:{compute_key}", auth.effective_scope().id()),
                },
                context,
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = ?error, "canonical native server create failed");
                compute_error(error)
            })?;
        let server = receipt.resource;
        let resource = if let (Some(migration_id), Some(source_key)) =
            (spec.migration_id.as_ref(), spec.source_key.as_deref())
        {
            self.annotate_server_migration_metadata(server.id.as_uuid(), migration_id, source_key)
                .await?
        } else {
            self.store
                .get_resource(server.id.as_uuid())
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
        };
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(server.id.as_uuid().to_string()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded
            ),
            resource: Some(server_json_with_resource(
                ServerItem {
                    id: server.id.as_uuid().to_string(),
                    project_id: server.project_id,
                    name: server.name,
                    flavor_id: server.flavor_id.to_string(),
                    image_id: server.image_id,
                    state: format!("{:?}", server.state),
                    generation: resource.generation,
                    created_at: None,
                    migration_id: resource
                        .desired_state
                        .as_str()
                        .parse::<serde_json::Value>()
                        .ok()
                        .and_then(|value| {
                            value
                                .get("migration_id")
                                .and_then(serde_json::Value::as_str)
                                .map(ToOwned::to_owned)
                        }),
                    source_key: resource
                        .desired_state
                        .as_str()
                        .parse::<serde_json::Value>()
                        .ok()
                        .and_then(|value| {
                            value
                                .get("source_key")
                                .and_then(serde_json::Value::as_str)
                                .map(ToOwned::to_owned)
                        }),
                },
                Some(&resource),
            )),
        })
    }

    async fn relationships(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        limit: usize,
    ) -> Result<Vec<o3k_native_api::resource::RelationshipView>, ResourceApplicationError> {
        self.relationships_page(descriptor, auth, id, None, limit)
            .await
    }

    async fn relationships_page(
        &self,
        _descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        after_slot: Option<&str>,
        limit: usize,
    ) -> Result<Vec<o3k_native_api::resource::RelationshipView>, ResourceApplicationError> {
        let parent = Uuid::parse_str(id).map_err(|_| ResourceApplicationError::NotFound)?;
        let record = self
            .store
            .get_resource(parent)
            .await
            .map_err(|error| match error {
                o3k_store::StoreError::ResourceNotFound => ResourceApplicationError::NotFound,
                _ => ResourceApplicationError::Internal,
            })?;
        // Tenant callers are constrained to their durable owner scope.  A
        // system-scoped operator may inspect relationships after the native
        // route has separately authorized the operator action; using the
        // literal `system` scope as a project id would incorrectly conceal
        // every relationship while never weakening tenant isolation.
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System
            && record.project_id != auth.effective_scope().id().as_str()
        {
            return Err(ResourceApplicationError::NotFound);
        }
        let bounded = u32::try_from(limit.saturating_add(1))
            .map_err(|_| ResourceApplicationError::Internal)?;
        let records = self
            .store
            .list_relationships_page(parent, after_slot, bounded)
            .await
            .map_err(|_| ResourceApplicationError::Internal)?;
        // The parent resource check above establishes the caller's scope, but
        // the relationship ledger also carries an owner scope.  Treat a
        // disagreement as corruption rather than projecting a relationship
        // whose authority belongs to another tenant (or to no tenant).  This
        // keeps the relationship projection fail-closed even if a stale or
        // malformed ledger row survives a migration.
        if records
            .iter()
            .any(|relationship| relationship.owner_scope != record.project_id)
        {
            return Err(ResourceApplicationError::Internal);
        }
        records
            .into_iter()
            .map(|record| {
                Ok(o3k_native_api::resource::RelationshipView {
                    slot: record.slot,
                    resource_type: record.expected_child_resource_type,
                    resource_id: record.child_resource_id.map(|id| id.to_string()),
                    ownership: record.ownership,
                    state: record.state,
                    parent_operation_id: record.parent_operation_id.to_string(),
                    child_operation_id: record.child_operation_id.map(|id| id.to_string()),
                })
            })
            .collect()
    }

    async fn update(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        request: o3k_native_api::resource::ValidatedUpdateRequest,
        idempotency_key: Option<&str>,
        expected_generation: i64,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        let resource_id = Uuid::parse_str(id).map_err(|_| ResourceApplicationError::NotFound)?;
        let existing = self
            .store
            .get_resource(resource_id)
            .await
            .map_err(|error| match error {
                o3k_store::StoreError::ResourceNotFound => ResourceApplicationError::NotFound,
                _ => ResourceApplicationError::Internal,
            })?;
        if existing.kind != "compute_instance"
            || existing.project_id != auth.effective_scope().id().as_str()
        {
            return Err(ResourceApplicationError::NotFound);
        }
        let name = request
            .spec
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(ResourceApplicationError::Validation)?;
        let mut desired: serde_json::Value = serde_json::from_str(&existing.desired_state)
            .map_err(|_| ResourceApplicationError::Conflict)?;
        desired["name"] = serde_json::Value::String(name.to_owned());
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Update)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}:{}:{}", descriptor.resource_type, id, action, key).as_bytes(),
        );
        let operation = o3k_store::OperationRecord {
            id: operation_id,
            resource_id,
            kind: "lifecycle:update".into(),
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
            key.to_owned(),
            &descriptor.resource_type.to_string(),
            Some(id),
            &serde_json::json!({"spec": request.spec, "generation": expected_generation}),
            operation_id,
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        match self
            .store
            .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
            .await
            .map_err(|_| ResourceApplicationError::Internal)?
        {
            o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                return Err(ResourceApplicationError::IdempotencyConflict);
            }
            o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent { operation_id, .. } => {
                // Replays must reflect the durable operation outcome.  A
                // previous failed/unknown update is not synchronous success;
                // reporting `complete=true` would fabricate convergence and
                // prevent the caller from observing the required recovery
                // state.
                let existing_operation = self
                    .store
                    .get_canonical_operation(operation_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(id.to_owned()),
                    complete: matches!(
                        existing_operation.state,
                        o3k_store::OperationState::Succeeded
                    ),
                    resource: None,
                });
            }
            o3k_store::CanonicalAcceptanceOutcome::Created { .. } => {}
        }
        if existing.generation != expected_generation {
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                o3k_kernel::OperationState::Failed,
                0,
                None,
                Some(now),
                Some("stale_generation".to_owned()),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Err(ResourceApplicationError::PreconditionConflict);
        }
        let desired =
            serde_json::to_string(&desired).map_err(|_| ResourceApplicationError::Internal)?;
        self.store
            .update_resource(
                resource_id,
                expected_generation,
                &desired,
                &existing.observed_state,
                existing.observed_generation,
                existing.provider_id.as_deref(),
            )
            .await
            .map_err(|error| match error {
                o3k_store::StoreError::StaleGeneration => {
                    ResourceApplicationError::PreconditionConflict
                }
                _ => ResourceApplicationError::Internal,
            })?;
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
        Ok(MutationResult {
            operation_id: operation_id.to_string(),
            resource_id: Some(id.to_owned()),
            complete: true,
            resource: None,
        })
    }

    async fn action(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        action: o3k_kernel::ActionId,
        request: ActionRequest,
        idempotency_key: &str,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "image:image" && action.action() == "UploadImage"
        {
            let service = self
                .image
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let content = request
                .payload
                .as_ref()
                .ok_or(ResourceApplicationError::Validation)?;
            if !request.input.is_object() {
                return Err(ResourceApplicationError::Validation);
            }
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != "image:image"
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "image:upload:{}:{id}:{idempotency_key}",
                    auth.effective_scope().id()
                )
                .as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "action:upload".into(),
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
                idempotency_key,
                &descriptor.resource_type.to_string(),
                Some(id),
                &serde_json::json!({
                    "resource_id": id,
                    "content_sha256": format!("{:x}", Sha256::digest(content)),
                    "content_length": content.len(),
                }),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_scoped_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
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
            let result = service
                .upload_with_operation(auth, resource_id, content, Some(operation_id))
                .await;
            let image = match result {
                Ok(image) => image,
                Err(error) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let state = if matches!(error, o3k_image::ImageError::AuditUnavailable) {
                        o3k_kernel::OperationState::UnknownOutcome
                    } else {
                        o3k_kernel::OperationState::Failed
                    };
                    let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        state,
                        1,
                        None,
                        Some(now),
                        Some(image_operation_error_category(&error).to_owned()),
                    )
                    .map_err(|_| ResourceApplicationError::Internal)?;
                    self.store
                        .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Err(image_error(error));
                }
            };
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
                resource: Some(image_json_with_resource(&image, Some(&resource))),
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        let action_kind = match action.action() {
            "StartServer" => o3k_provider::InstanceAction::Start,
            "StopServer" => o3k_provider::InstanceAction::Stop,
            "RebootServer" => o3k_provider::InstanceAction::Reboot,
            _ => return Err(ResourceApplicationError::UnsupportedOperation),
        };
        if !request.input.is_object() {
            return Err(ResourceApplicationError::Validation);
        }
        let server_id = id
            .parse::<Uuid>()
            .map(o3k_compute::ServerId::from_uuid)
            .map_err(|_| ResourceApplicationError::NotFound)?;
        let context = o3k_reconciler::CanonicalMutationContext::new(
            action,
            auth.principal().id().to_string(),
            auth.effective_scope().clone(),
            Some(auth.request_id().to_owned()),
            idempotency_key.to_owned(),
            request.input,
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        let receipt = self
            .compute
            .action_for_auth_canonical(auth, server_id, action_kind, context)
            .await
            .map_err(compute_error)?;
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(receipt.resource.to_string()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
            ),
            resource: None,
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
        if descriptor.resource_type.to_string() == "volume:volume_attachment" {
            let workflow = self
                .attachment_workflow
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            let attachment_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            // Deletion removes the attachment row after the provider side
            // effect succeeds. Resolve an equivalent retry before looking up
            // that row, otherwise a successful delete becomes indistinguish-
            // able from a cross-project/not-found request.
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:volume-attachment-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "volume:volume_attachment:delete:{}:{id}:{}",
                    auth.effective_scope().id(),
                    key
                )
                .as_bytes(),
            );
            match self.store.get_canonical_operation(operation_id).await {
                Ok(existing) => {
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(id.to_owned()),
                        complete: existing.state == o3k_store::OperationState::Succeeded,
                        resource: None,
                    });
                }
                Err(o3k_store::StoreError::OperationNotFound) => {}
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
            let record = self
                .store
                .get_volume_attachment_v1(attachment_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                .filter(|record| {
                    record.attachment.project_id == auth.effective_scope().id().as_str()
                })
                .ok_or(ResourceApplicationError::NotFound)?;
            if expected_generation
                .is_some_and(|expected| expected != record.attachment.generation as i64)
            {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id: attachment_id,
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
                &serde_json::json!({
                    "resource_id": id,
                    "expected_generation": expected_generation
                }),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_scoped_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
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
            if let Err(_error) = workflow.detach(attachment_id).await {
                // A transport timeout/unavailable response does not establish
                // the provider-side state.  This compatibility workflow only
                // exposes a redacted String, so classify every failure as
                // UnknownOutcome rather than guessing that a mutation did not
                // reach the provider.  Recovery must observe before retrying.
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::UnknownOutcome,
                    1,
                    None,
                    None,
                    Some("attachment provider outcome unknown; observe before retry".to_owned()),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(ResourceApplicationError::Retryable);
            }
            let resource = self
                .store
                .get_resource(attachment_id)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            if expected_generation.is_some_and(|expected| expected != resource.generation) {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            self.store
                .update_resource(
                    attachment_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation.saturating_add(1),
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .delete_volume_attachment_v1(
                    auth.effective_scope().id().as_str(),
                    record.attachment.id.as_uuid(),
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
        if descriptor.resource_type.to_string() == "compute:flavor" {
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
                .unwrap_or_else(|| format!("native:compute-flavor-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "{}:{}:{}:{}:generation={}",
                    auth.effective_scope().id(),
                    descriptor.resource_type,
                    id,
                    key,
                    expected_generation.unwrap_or_default()
                )
                .as_bytes(),
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
                &serde_json::json!({"resource_id": id, "expected_generation": expected_generation}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id, ..
                } => {
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
                o3k_store::CanonicalAcceptanceOutcome::Created { .. } => {}
            }
            if let Err(error) = self.compute.delete_flavor_for_auth(auth, resource_id).await {
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Failed,
                    1,
                    None,
                    Some(now),
                    Some(error.to_string()),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(compute_error(error));
            }
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() == "image:image" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != "image:image"
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
                .unwrap_or_else(|| format!("native:image-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "image:delete:{}:{id}:{key}:generation={}",
                    auth.effective_scope().id(),
                    resource.generation
                )
                .as_bytes(),
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
                &serde_json::json!({
                    "resource_id": id,
                    "expected_generation": expected_generation
                }),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id, ..
                } => {
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
                o3k_store::CanonicalAcceptanceOutcome::Created { .. } => {}
            }
            let service = self
                .image
                .as_ref()
                .ok_or(ResourceApplicationError::NotReady)?;
            if let Err(error) = service
                .delete_with_operation(auth, resource_id, Some(operation_id))
                .await
            {
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    match error {
                        o3k_image::ImageError::AuditUnavailable => {
                            o3k_kernel::OperationState::UnknownOutcome
                        }
                        _ => o3k_kernel::OperationState::Failed,
                    },
                    1,
                    None,
                    Some(now),
                    Some(error.to_string()),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(image_error(error));
            }
            if self
                .store
                .update_resource(
                    resource_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation.saturating_add(1),
                    None,
                )
                .await
                .is_err()
            {
                self.mark_operation_unknown(operation_id, "floating IP resource projection failed")
                    .await?;
                return Err(ResourceApplicationError::Retryable);
            }
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
        // Migration-owned compatibility projections are backed by canonical
        // network operations.  Do not route their deletion through the
        // generic execution controller: these records have no provider
        // lifecycle session of their own, and that path correctly rejects a
        // missing provider operation with 501.  The canonical network service
        // is the authority and the sidecar is marked deleted only after the
        // operation succeeds.
        let network_kind = descriptor.resource_type.to_string();
        if matches!(
            network_kind.as_str(),
            "network:network"
                | "network:subnet"
                | "network:port"
                | "network:security_group"
                | "network:security_group_rule"
                | "network:router"
                | "network:router_interface"
        ) {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != network_kind
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
            let key = idempotency_key.ok_or(ResourceApplicationError::Validation)?;
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "{}:{}:{}:{}:generation={}",
                    auth.effective_scope().id(),
                    descriptor.resource_type,
                    id,
                    key,
                    resource.generation
                )
                .as_bytes(),
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
                &serde_json::json!({"resource_id": id, "generation": resource.generation}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id, ..
                } => {
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
                o3k_store::CanonicalAcceptanceOutcome::Created { .. } => {}
            }
            let project_id = auth.effective_scope().id().as_str();
            let mutation_result: Result<(), ResourceApplicationError> = async {
                match network_kind.as_str() {
                    "network:network" => self
                        .network_service
                        .delete_network_for_project(project_id, resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Conflict)?,
                    "network:subnet" => self
                        .network_service
                        .delete_subnet_for_project(project_id, resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Conflict)?,
                    "network:port" => self
                        .network_service
                        .delete_port_for_project(project_id, resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Conflict)?,
                    "network:security_group" => self
                        .network_service
                        .delete_security_group_for_project(project_id, resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Conflict)?,
                    "network:security_group_rule" => self
                        .network_service
                        .delete_security_group_rule_for_project(project_id, resource_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Conflict)?,
                    "network:router" => {
                        let gateway = self
                            .network_service
                            .delete_l3_gateway_for_project(
                                project_id,
                                &resource_id,
                                resource.generation as u64,
                            )
                            .await
                            .map_err(|_| ResourceApplicationError::Conflict)?;
                        self.network_service
                            .finalize_l3_gateway_deletion_for_project(
                                project_id,
                                &resource_id,
                                gateway.generation,
                            )
                            .await
                            .map_err(|_| ResourceApplicationError::Conflict)?;
                    }
                    "network:router_interface" => {
                        let attachment = self
                            .network_service
                            .detach_l3_gateway_realm(
                                project_id,
                                &resource_id,
                                resource.generation as u64,
                            )
                            .await
                            .map_err(|_| ResourceApplicationError::Conflict)?;
                        self.network_service
                            .finalize_l3_gateway_realm_detachment_for_project(
                                project_id,
                                &resource_id,
                                attachment.generation,
                            )
                            .await
                            .map_err(|_| ResourceApplicationError::Conflict)?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }
            .await;
            if let Err(error) = mutation_result {
                let now = chrono::Utc::now().to_rfc3339();
                let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Failed,
                    1,
                    None,
                    Some(now),
                    Some("network deletion failed".to_owned()),
                )
                .map_err(|_| ResourceApplicationError::Internal)?;
                self.store
                    .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                return Err(error);
            }
            if self
                .store
                .update_resource(
                    resource_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation.saturating_add(1),
                    None,
                )
                .await
                .is_err()
            {
                self.mark_operation_unknown(operation_id, "network resource projection failed")
                    .await?;
                return Err(ResourceApplicationError::Retryable);
            }
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
        if network_kind == "network:floating_ip" {
            let project_id = auth.effective_scope().id().to_string();
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != network_kind
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            if expected_generation.is_some_and(|expected| expected != resource.generation) {
                return Err(ResourceApplicationError::PreconditionConflict);
            }
            // Reserve the canonical lifecycle identity before touching the
            // file-backed allocator.  The allocator is an execution
            // boundary, not an authority for operation identity; retries
            // must therefore converge through the durable operation store.
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:floating-ip-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("network:floating_ip:delete:{}:{id}:{key}", project_id).as_bytes(),
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
                project_id,
                action.to_string(),
                key,
                &descriptor.resource_type.to_string(),
                Some(id),
                &serde_json::json!({
                    "resource_id": id,
                    "expected_generation": expected_generation
                }),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            match self
                .store
                .create_or_replay_canonical_scoped_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
            {
                o3k_store::IdempotencyReservation::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
                o3k_store::IdempotencyReservation::ExistingEquivalent(existing_id) => {
                    let existing = self
                        .store
                        .get_canonical_operation(existing_id)
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                    return Ok(MutationResult {
                        operation_id: existing_id.to_string(),
                        resource_id: Some(id.to_owned()),
                        complete: existing.state == o3k_store::OperationState::Succeeded,
                        resource: None,
                    });
                }
                o3k_store::IdempotencyReservation::Created(_) => {}
            }
            let project_id = auth.effective_scope().id().as_str();
            let binding = match self.store.get_public_address(project_id, resource_id).await {
                Ok(Some(binding)) => binding,
                Ok(None) => {
                    self.mark_operation_unknown(
                        operation_id,
                        "floating IP binding observation failed",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Retryable);
                }
                Err(_) => {
                    self.mark_operation_unknown(
                        operation_id,
                        "floating IP binding observation failed",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Retryable);
                }
            };
            if binding.endpoint_id.is_some() {
                if let Some(workflow) = self.public_address_workflow.as_ref()
                    && workflow.remove(project_id, resource_id).await.is_err()
                {
                    self.mark_operation_unknown(
                        operation_id,
                        "floating IP disassociation outcome unknown",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Retryable);
                }
                if self
                    .store
                    .disassociate_public_address(project_id, resource_id)
                    .await
                    .is_err()
                {
                    self.mark_operation_unknown(
                        operation_id,
                        "floating IP disassociation outcome unknown",
                    )
                    .await?;
                    return Err(ResourceApplicationError::Retryable);
                }
            }
            if self
                .store
                .release_public_address(project_id, resource_id)
                .await
                .is_err()
            {
                // A file-backed allocator cannot prove whether release
                // reached durable state. Never convert that uncertainty
                // into synchronous success.
                self.mark_operation_unknown(operation_id, "floating IP release outcome unknown")
                    .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            if self
                .store
                .update_resource(
                    resource_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation.saturating_add(1),
                    None,
                )
                .await
                .is_err()
            {
                self.mark_operation_unknown(operation_id, "floating IP resource projection failed")
                    .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                o3k_kernel::OperationState::Succeeded,
                1,
                Some(now.clone()),
                Some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            if self
                .store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .is_err()
            {
                self.mark_operation_unknown(
                    operation_id,
                    "floating IP operation projection failed",
                )
                .await?;
                return Err(ResourceApplicationError::Retryable);
            }
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        // Native volumes are owned by the in-process storage boundary even
        // when a compatibility controller with the same service identity is
        // registered.  The provider-backed native lifecycle must run first;
        // the generic controller path can otherwise report success without
        // removing the LVM realization.
        if descriptor.resource_type.to_string() != "volume:volume"
            && let Some(controller) = self.external_controllers.get(&descriptor.owning_service)
        {
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
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:volume-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!(
                    "volume:delete:{}:{resource_id}:{key}:generation={}",
                    auth.effective_scope().id(),
                    expected_generation.map_or(-1, |generation| generation)
                )
                .as_bytes(),
            );
            // A successful delete removes the native volume row. Resolve a
            // deterministic replay before looking up that row so an
            // equivalent retry returns the original terminal operation.
            match self.store.get_canonical_operation(operation_id).await {
                Ok(existing) => {
                    if let Some(expected) = expected_generation {
                        let bookkeeping = self
                            .store
                            .get_resource(resource_id)
                            .await
                            .map_err(|_| ResourceApplicationError::NotFound)?;
                        if bookkeeping.project_id != auth.effective_scope().id().as_str()
                            || bookkeeping.kind != "volume"
                            || bookkeeping.generation != expected.saturating_add(1)
                        {
                            return Err(ResourceApplicationError::PreconditionConflict);
                        }
                    }
                    if existing.state != o3k_store::OperationState::Succeeded {
                        // A retry can arrive after provider deletion but before
                        // the first request persisted its terminal operation.
                        let Some(provider) = self.storage_provider.clone() else {
                            return Err(ResourceApplicationError::NotReady);
                        };
                        if let Some(record) = self
                            .store
                            .get_volume(resource_id)
                            .await
                            .map_err(|_| ResourceApplicationError::Internal)?
                        {
                            if record.volume.project_id != auth.effective_scope().id().as_str() {
                                return Err(ResourceApplicationError::NotFound);
                            }
                            o3k_api::remove_native_volume(
                    self.store.clone(),
                    provider,
                    auth.effective_scope().id().as_str(),
                    resource_id,
                    Some(operation_id),
                )
                .await
                .map_err(|error| {
                    tracing::error!(volume_id = %resource_id, %error, "native volume delete failed");
                    ResourceApplicationError::Retryable
                })?;
                            let bookkeeping = self
                                .store
                                .get_resource(resource_id)
                                .await
                                .map_err(|_| ResourceApplicationError::Internal)?;
                            if bookkeeping.project_id != auth.effective_scope().id().as_str()
                                || bookkeeping.kind != "volume"
                            {
                                return Err(ResourceApplicationError::NotFound);
                            }
                            if bookkeeping.observed_state != "DELETED"
                                || bookkeeping.desired_state != "DELETED"
                            {
                                self.store
                                    .update_resource(
                                        resource_id,
                                        bookkeeping.generation,
                                        "DELETED",
                                        "DELETED",
                                        bookkeeping.generation.saturating_add(1),
                                        None,
                                    )
                                    .await
                                    .map_err(|_| ResourceApplicationError::Internal)?;
                            }
                            let now = chrono::Utc::now().to_rfc3339();
                            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                                o3k_kernel::OperationState::Succeeded,
                                existing.attempt.saturating_add(1),
                                Some(now.clone()),
                                Some(now),
                                None,
                            )
                            .map_err(|_| ResourceApplicationError::Internal)?;
                            self.store
                                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                                .await
                                .map_err(|_| ResourceApplicationError::Internal)?;
                            self.store
                                .delete_volume(auth.effective_scope().id().as_str(), resource_id)
                                .await
                                .map_err(|_| ResourceApplicationError::Internal)?;
                            return Ok(MutationResult {
                                operation_id: operation_id.to_string(),
                                resource_id: Some(id.to_owned()),
                                complete: true,
                                resource: None,
                            });
                        }
                    }
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(id.to_owned()),
                        complete: existing.state == o3k_store::OperationState::Succeeded,
                        resource: None,
                    });
                }
                Err(o3k_store::StoreError::OperationNotFound) => {}
                Err(_) => return Err(ResourceApplicationError::Internal),
            }
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
                let Some(provider) = self.storage_provider.clone() else {
                    return Err(ResourceApplicationError::NotReady);
                };
                // The native volume table is the lifecycle authority, while
                // the shared operation journal enforces its resource FK
                // through `resources`.  Compatibility-created volumes
                // predate that projection, so materialize the bookkeeping
                // row before reserving the native delete operation.
                if let Err(error) = self.store.get_resource(resource_id).await {
                    if !matches!(error, o3k_store::StoreError::ResourceNotFound) {
                        return Err(ResourceApplicationError::Internal);
                    }
                    self.store
                        .insert_resource(&o3k_store::ResourceRecord {
                            id: resource_id,
                            kind: "volume".to_owned(),
                            project_id: record.volume.project_id.clone(),
                            generation: record.volume.generation as i64,
                            observed_generation: record.volume.generation as i64,
                            desired_state: "AVAILABLE".to_owned(),
                            observed_state: "AVAILABLE".to_owned(),
                            provider_id: record
                                .volume
                                .provider_reference
                                .as_ref()
                                .map(|reference| reference.resource_id.clone()),
                        })
                        .await
                        .map_err(|_| ResourceApplicationError::Internal)?;
                }
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
                        &serde_json::json!({"resource_id": id, "expected_generation": expected_generation}),
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
                    return Ok(MutationResult {
                        operation_id: operation_id.to_string(),
                        resource_id: Some(id.to_owned()),
                        complete: existing.state == o3k_store::OperationState::Succeeded,
                        resource: None,
                    });
                }
                o3k_api::remove_native_volume(
                    self.store.clone(),
                    provider,
                    auth.effective_scope().id().as_str(),
                    resource_id,
                    Some(operation_id),
                )
                .await
                .map_err(|error| {
                    tracing::error!(volume_id = %resource_id, %error, "native volume delete failed");
                    ResourceApplicationError::Retryable
                })?;
                let bookkeeping = self
                    .store
                    .get_resource(resource_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                if bookkeeping.project_id != auth.effective_scope().id().as_str()
                    || bookkeeping.kind != "volume"
                {
                    return Err(ResourceApplicationError::NotFound);
                }
                self.store
                    .update_resource(
                        resource_id,
                        bookkeeping.generation,
                        "DELETED",
                        "DELETED",
                        bookkeeping.generation.saturating_add(1),
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
                // Keep the Deleting row until the canonical operation is
                // terminal. If cleanup fails, restart recovery still owns the
                // durable provider inventory and can finish the deletion.
                self.store
                    .delete_volume(auth.effective_scope().id().as_str(), resource_id)
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
                .map_err(|_| ResourceApplicationError::Internal)?;
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
            let audit = o3k_store::AuditEventRecord {
                event_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("o3k:network-delete-audit:{operation_id}").as_bytes(),
                )
                .to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                request_id: auth.request_id().to_owned(),
                audit_id: operation_id.to_string(),
                principal_id: auth.principal().id().to_string(),
                effective_scope: auth.effective_scope().id().to_string(),
                service_namespace: "network".to_owned(),
                action: action.to_string(),
                resource_type: Some("network:network".to_owned()),
                resource_id: Some(id.to_owned()),
                owner_scope: Some(auth.effective_scope().id().to_string()),
                operation_id: Some(operation_id),
                outcome: "succeeded".to_owned(),
                reason_category: None,
                event_json: serde_json::json!({"event_id": operation_id, "outcome": "succeeded"})
                    .to_string(),
            };
            self.store
                .delete_canonical_network_with_audit(
                    auth.effective_scope().id().as_str(),
                    &resource_id,
                    operation_id,
                    &audit,
                )
                .await
                .map_err(|_| ResourceApplicationError::Retryable)?;
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
