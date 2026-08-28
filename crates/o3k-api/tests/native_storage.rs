use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use o3k_api::AppState;
use o3k_compute::ComputeService;
use o3k_domain::{
    StorageCapabilities, StorageExecutionScope, StorageProviderReference, Volume, VolumeId,
    VolumeState,
};
use o3k_identity::{BootstrapConfig, Secret, TokenService};
use o3k_provider::FakeComputeProvider;
use o3k_storage::{
    PreparedAttachment, StorageAttachmentObservation, StorageAttachmentRequest, StorageProvider,
    StorageProviderError, StorageSnapshotObservation, StorageSnapshotRequest,
    StorageVolumeObservation, StorageVolumeRequest,
};
use o3k_store::{DurableStore, StorageRepository, VolumeRecord, testkit::TestStore};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;

const PROJECT: &str = "eba29e2d-53de-461d-ae91-ede7402713cb";

#[derive(Default)]
struct RecoveryProvider {
    volumes: Mutex<BTreeMap<Uuid, StorageVolumeObservation>>,
}

#[async_trait]
impl StorageProvider for RecoveryProvider {
    async fn capabilities(&self) -> Result<StorageCapabilities, StorageProviderError> {
        Err(StorageProviderError::InvalidRequest)
    }

    async fn create_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<StorageVolumeObservation, StorageProviderError> {
        let observation = StorageVolumeObservation {
            provider_reference: StorageProviderReference {
                provider: "test".into(),
                resource_id: format!("volume-{}", request.volume_id),
            },
            size_bytes: request.size_bytes,
            owned: true,
            available: true,
        };
        self.volumes
            .lock()
            .map_err(|_| StorageProviderError::CommandFailed)?
            .insert(request.volume_id.as_uuid(), observation.clone());
        Ok(observation)
    }

    async fn inspect_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<StorageVolumeObservation, StorageProviderError> {
        self.volumes
            .lock()
            .map_err(|_| StorageProviderError::CommandFailed)?
            .get(&request.volume_id.as_uuid())
            .cloned()
            .ok_or(StorageProviderError::NotFound)
    }

    async fn delete_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<(), StorageProviderError> {
        self.volumes
            .lock()
            .map_err(|_| StorageProviderError::CommandFailed)?
            .remove(&request.volume_id.as_uuid())
            .map(|_| ())
            .ok_or(StorageProviderError::NotFound)
    }

    async fn prepare_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<PreparedAttachment, StorageProviderError> {
        PreparedAttachment::from_provider(
            StorageProviderReference {
                provider: "test".into(),
                resource_id: format!("volume-{}", request.volume_id),
            },
            "/dev/test".into(),
            request.attachment_id,
            request.volume_id,
        )
    }

    async fn inspect_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<StorageAttachmentObservation, StorageProviderError> {
        Ok(StorageAttachmentObservation {
            attachment_id: request.attachment_id,
            volume_id: request.volume_id,
            host_id: "test".into(),
            attached: false,
            provider_reference: StorageProviderReference {
                provider: "test".into(),
                resource_id: format!("volume-{}", request.volume_id),
            },
        })
    }

    async fn terminate_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<StorageAttachmentObservation, StorageProviderError> {
        self.inspect_attachment(request).await
    }

    async fn create_snapshot(
        &self,
        _request: &StorageSnapshotRequest,
    ) -> Result<StorageSnapshotObservation, StorageProviderError> {
        Err(StorageProviderError::InvalidRequest)
    }

    async fn delete_snapshot(
        &self,
        _request: &StorageSnapshotRequest,
    ) -> Result<(), StorageProviderError> {
        Err(StorageProviderError::InvalidRequest)
    }
}

async fn token(app: &axum::Router) -> Result<String, Box<dyn std::error::Error>> {
    let body = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    Ok(response
        .headers()
        .get("x-subject-token")
        .ok_or("missing token")?
        .to_str()?
        .to_owned())
}

async fn store() -> Result<Arc<TestStore>, Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    o3k_identity::seed_identity_defaults(
        store.as_ref(),
        &BootstrapConfig {
            catalog_endpoint: "http://127.0.0.1:18090".into(),
            bootstrap_password: Secret::new("password".into()),
            cinder_password: None,
            cinder_endpoint: None,
            pbkdf2_iterations: 1_000,
            extra_projects: vec![],
        },
    )
    .await?;
    Ok(store)
}

#[tokio::test]
async fn native_volume_and_attachment_projection_uses_canonical_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let store = store().await?;
    let server_id = Uuid::now_v7();
    store
        .insert_resource(&o3k_store::ResourceRecord {
            id: server_id,
            kind: "compute_instance".into(),
            project_id: PROJECT.into(),
            generation: 1,
            observed_generation: 1,
            desired_state: serde_json::to_string(&o3k_provider::CreateInstanceRequest {
                operation_id: Uuid::now_v7(),
                o3k_server_id: server_id,
                project_id: PROJECT.into(),
                name: "native-server".into(),
                vcpus: 1,
                memory_mib: 512,
                flavor_id: Uuid::from_u128(1).to_string(),
                disk_gib: 10,
                image_id: Some(Uuid::now_v7().to_string()),
                key_name: None,
                keypair_id: None,
                network_ids: Vec::new(),
                placement_provider_id: Some("compute-1".into()),
                placement_allocation_id: Some(Uuid::now_v7().to_string()),
                config_drive: None,
                idempotency_key: format!("create:{server_id}"),
            })?,
            observed_state: "ACTIVE".into(),
            provider_id: None,
        })
        .await?;
    let volume_id = VolumeId::new();
    store
        .insert_volume(&VolumeRecord {
            volume: Volume {
                id: volume_id,
                project_id: PROJECT.into(),
                name: "native".into(),
                description: String::new(),
                metadata: Default::default(),
                availability_zone: None,
                size_bytes: 1024 * 1024 * 1024,
                volume_type: "lvm".into(),
                backend_id: "local".into(),
                execution_scope: StorageExecutionScope::Host("local".into()),
                state: VolumeState::Available,
                generation: 1,
                operation_id: None,
                provider_reference: None,
            },
            created_at: "2026-08-28T00:00:00.000".into(),
        })
        .await?;
    let identity = TokenService::load(
        store.clone(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".into()),
        Duration::from_secs(3600),
    )
    .await?;
    let compute = ComputeService::new(store.clone(), Arc::new(FakeComputeProvider::new()));
    let storage_provider = Arc::new(RecoveryProvider::default());
    let state = AppState::new()
        .with_identity(identity)
        .with_compute(compute)
        .with_storage_store(store.clone())
        .with_storage_provider(storage_provider)
        .with_volume_attachments_enabled(true);
    state.set_ready(true);
    let app = o3k_api::router_with_state(state);
    let auth = token(&app).await?;
    let request = serde_json::json!({"volume_attachment":{"volume_id": volume_id.to_string(), "delete_on_termination": false}});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/v2.1/{PROJECT}/servers/{server_id}/os-volume_attachments"
                ))
                .header("x-auth-token", &auth)
                .header("content-type", "application/json")
                .body(Body::from(request.to_string()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 8192).await?)?;
    let attachment_id = body["volumeAttachment"]["id"]
        .as_str()
        .ok_or("missing attachment")?
        .to_owned();
    assert_eq!(
        store
            .get_volume(volume_id.as_uuid())
            .await?
            .ok_or("missing volume")?
            .volume
            .state,
        VolumeState::InUse
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!(
                    "/v2.1/{PROJECT}/servers/{server_id}/os-volume_attachments/{attachment_id}"
                ))
                .header("x-auth-token", &auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        store
            .get_volume(volume_id.as_uuid())
            .await?
            .ok_or("missing volume")?
            .volume
            .state,
        VolumeState::Available
    );
    Ok(())
}

#[tokio::test]
async fn native_volume_transitional_states_recover_from_provider_observation()
-> Result<(), Box<dyn std::error::Error>> {
    let store = store().await?;
    let volume_id = VolumeId::new();
    store
        .insert_volume(&VolumeRecord {
            volume: Volume {
                id: volume_id,
                project_id: PROJECT.into(),
                name: "recovering".into(),
                description: String::new(),
                metadata: Default::default(),
                availability_zone: None,
                size_bytes: 1024 * 1024 * 1024,
                volume_type: "lvm".into(),
                backend_id: "local".into(),
                execution_scope: StorageExecutionScope::Host("local".into()),
                state: VolumeState::Creating,
                generation: 2,
                operation_id: None,
                provider_reference: None,
            },
            created_at: "2026-08-28T00:00:00.000".into(),
        })
        .await?;
    let provider = Arc::new(RecoveryProvider::default());
    provider
        .create_volume(&StorageVolumeRequest {
            volume_id,
            project_id: PROJECT.into(),
            size_bytes: 1024 * 1024 * 1024,
            generation: 2,
        })
        .await?;
    let state = AppState::new()
        .with_storage_store(store.clone())
        .with_storage_provider(provider.clone());
    o3k_api::recover_native_volumes(&state).await;
    assert_eq!(
        store
            .get_volume(volume_id.as_uuid())
            .await?
            .ok_or("missing recovered volume")?
            .volume
            .state,
        VolumeState::Available
    );

    let deleting_id = VolumeId::new();
    let mut deleting = store
        .get_volume(volume_id.as_uuid())
        .await?
        .ok_or("missing recovered volume")?;
    deleting.volume.id = deleting_id;
    deleting.volume.state = VolumeState::Deleting;
    deleting.volume.generation += 1;
    store.insert_volume(&deleting).await?;
    o3k_api::recover_native_volumes(&state).await;
    assert!(store.get_volume(deleting_id.as_uuid()).await?.is_none());
    Ok(())
}
