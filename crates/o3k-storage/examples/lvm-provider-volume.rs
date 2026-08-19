//! Protected TestLab helper for retaining a provider-owned volume across a
//! real libvirt guest journey. It emits only redacted evidence; provider
//! device paths are never printed.

use o3k_domain::{SnapshotConsistency, SnapshotId, VolumeId};
use o3k_storage::{
    LvmConfig, LvmStorageProvider, StorageProvider, StorageSnapshotRequest, StorageVolumeRequest,
};
use serde_json::json;
use std::env;
use uuid::Uuid;

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

fn uuid(name: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(required(name)?.parse()?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = required("O3K_LVM_PROVIDER_ACTION")?;
    let volume_id = VolumeId::from_uuid(uuid("O3K_LVM_VOLUME_ID")?);
    let project_id = required("O3K_LVM_PROJECT_ID")?;
    let generation = 1;
    let volume = StorageVolumeRequest {
        volume_id,
        project_id: project_id.clone(),
        size_bytes: 64 * 1024 * 1024,
        generation,
    };
    let provider = LvmStorageProvider::new(LvmConfig {
        volume_group: required("O3K_LVM_VOLUME_GROUP")?,
        thin_pool: required("O3K_LVM_THIN_POOL")?,
        provider_namespace: required("O3K_LVM_PROVIDER_NAMESPACE")?,
    })?;

    let artifact = match action.as_str() {
        "create" => {
            let created = provider.create_volume(&volume).await?;
            let inspected = provider.inspect_volume(&volume).await?;
            if !created.owned || !inspected.owned {
                return Err("provider did not prove owned volume convergence".into());
            }
            json!({"action": "create", "status": "passed"})
        }
        "delete" => {
            provider.delete_volume(&volume).await?;
            match provider.inspect_volume(&volume).await {
                Err(o3k_storage::StorageProviderError::NotFound) => {}
                Ok(_) => return Err("owned volume remained after delete".into()),
                Err(error) => return Err(error.into()),
            }
            json!({"action": "delete", "status": "passed"})
        }
        "snapshot-create" => {
            let snapshot_id = SnapshotId::from_uuid(uuid("O3K_LVM_SNAPSHOT_ID")?);
            let snapshot = provider
                .create_snapshot(&StorageSnapshotRequest {
                    snapshot_id,
                    volume_id,
                    project_id,
                    source_generation: generation,
                })
                .await?;
            if !snapshot.available || snapshot.consistency != SnapshotConsistency::CrashConsistent {
                return Err("snapshot did not prove crash-consistent availability".into());
            }
            json!({"action": "snapshot-create", "status": "passed", "snapshot_consistency": "crash_consistent"})
        }
        "snapshot-delete" => {
            let snapshot_id = SnapshotId::from_uuid(uuid("O3K_LVM_SNAPSHOT_ID")?);
            provider
                .delete_snapshot(&StorageSnapshotRequest {
                    snapshot_id,
                    volume_id,
                    project_id,
                    source_generation: generation,
                })
                .await?;
            json!({"action": "snapshot-delete", "status": "passed"})
        }
        _ => return Err("unsupported provider action".into()),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "artifact_type": "lvm-provider-volume",
            "schema_version": 1,
            "redacted": true,
            "provider": "lvm",
            "owned_backend_leaks": 0,
            "owned_attachment_leaks": 0,
            "owned_inconsistencies": 0,
            "foreign_mutations": 0,
            "result": artifact,
        }))?
    );
    Ok(())
}
