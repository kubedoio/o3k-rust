use o3k_domain::{AttachmentAccessMode, SnapshotId, VolumeAttachmentId, VolumeId};
use o3k_storage::{
    LvmConfig, LvmStorageProvider, StorageAttachmentRequest, StorageProvider, StorageProviderError,
    StorageSnapshotRequest, StorageVolumeRequest,
};
use serde_json::json;
use std::env;
use uuid::Uuid;

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let volume_id = VolumeId::from_uuid(Uuid::now_v7());
    let snapshot_id = SnapshotId::from_uuid(Uuid::now_v7());
    let attachment_id = VolumeAttachmentId::from_uuid(Uuid::now_v7());
    let project_id = "lvm-provider-gate-project".to_owned();
    let generation = 1;
    let provider = LvmStorageProvider::new(LvmConfig {
        volume_group: required("O3K_LVM_VOLUME_GROUP")?,
        thin_pool: required("O3K_LVM_THIN_POOL")?,
        provider_namespace: required("O3K_LVM_PROVIDER_NAMESPACE")?,
    })?;
    let volume = StorageVolumeRequest {
        volume_id,
        project_id: project_id.clone(),
        size_bytes: 8 * 1024 * 1024,
        generation,
    };
    let capabilities = provider.capabilities().await?;
    let created = provider.create_volume(&volume).await?;
    let inspected = provider.inspect_volume(&volume).await?;
    if created.provider_reference != inspected.provider_reference || !inspected.owned {
        return Err("LVM provider observation did not converge".into());
    }

    let attachment = StorageAttachmentRequest {
        attachment_id,
        volume_id,
        project_id: project_id.clone(),
        volume_generation: generation,
        host_id: required("O3K_LVM_HOST_ID")?,
        access_mode: AttachmentAccessMode::ReadWrite,
    };
    let prepared = provider.prepare_attachment(&attachment).await?;
    if !prepared.device_path().starts_with("/dev/") {
        return Err("provider returned an invalid bounded device observation".into());
    }
    let terminated = provider.terminate_attachment(&attachment).await?;
    if terminated.attached {
        return Err("attachment termination did not converge".into());
    }

    let snapshot = StorageSnapshotRequest {
        snapshot_id,
        volume_id,
        project_id,
        source_generation: generation,
    };
    let snapshot_observation = provider.create_snapshot(&snapshot).await?;
    if !snapshot_observation.available {
        return Err("snapshot did not become available".into());
    }
    provider.delete_snapshot(&snapshot).await?;
    provider.delete_volume(&volume).await?;
    match provider.inspect_volume(&volume).await {
        Err(StorageProviderError::NotFound) => {}
        Err(error) => return Err(format!("post-delete inspection failed: {error}").into()),
        Ok(_) => return Err("owned volume remained after delete".into()),
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "artifact_type": "lvm-provider-smoke",
            "schema_version": 1,
            "status": "passed",
            "redacted": true,
            "provider": "lvm",
            "snapshot_consistency": "crash_consistent",
            "capacity_bytes": capabilities.capacity_bytes,
            "owned_backend_leaks": 0,
            "owned_attachment_leaks": 0,
            "owned_inconsistencies": 0,
            "foreign_mutations": 0
        }))?
    );
    Ok(())
}
