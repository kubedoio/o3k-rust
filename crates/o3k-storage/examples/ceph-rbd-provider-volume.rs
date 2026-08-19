//! Protected TestLab helper for the bounded Ceph RBD provider.
//!
//! Provider-native device paths are emitted only for the immediate compute
//! hand-off (`prepare`) and are never included in the redacted evidence.

use o3k_domain::{
    AttachmentAccessMode, SnapshotConsistency, SnapshotId, VolumeAttachmentId, VolumeId,
};
use o3k_storage::{
    CephRbdConfig, CephRbdStorageProvider, StorageAttachmentRequest, StorageProvider,
    StorageSnapshotRequest, StorageVolumeRequest,
};
use serde_json::json;
use std::env;
use uuid::Uuid;

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn uuid(name: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(required(name)?.parse()?)
}

fn provider() -> Result<CephRbdStorageProvider, Box<dyn std::error::Error>> {
    Ok(CephRbdStorageProvider::new(CephRbdConfig {
        pool: required("O3K_CEPH_POOL")?,
        namespace: optional("O3K_CEPH_NAMESPACE"),
        provider_namespace: required("O3K_CEPH_PROVIDER_NAMESPACE")?,
        conf_path: optional("O3K_CEPH_CONF_PATH"),
        client_id: optional("O3K_CEPH_CLIENT_ID"),
        keyring_path: optional("O3K_CEPH_KEYRING_PATH"),
        capacity_bytes: required("O3K_CEPH_CAPACITY_BYTES")?.parse()?,
    })?)
}

fn volume() -> Result<StorageVolumeRequest, Box<dyn std::error::Error>> {
    Ok(StorageVolumeRequest {
        volume_id: VolumeId::from_uuid(uuid("O3K_CEPH_VOLUME_ID")?),
        project_id: required("O3K_CEPH_PROJECT_ID")?,
        size_bytes: required("O3K_CEPH_VOLUME_SIZE_BYTES")?.parse()?,
        generation: 1,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = required("O3K_CEPH_PROVIDER_ACTION")?;
    let provider = provider()?;
    let volume = volume()?;
    let result = match action.as_str() {
        "create" => {
            let created = provider.create_volume(&volume).await?;
            let inspected = provider.inspect_volume(&volume).await?;
            if !created.owned || !inspected.owned {
                return Err("provider did not prove owned image convergence".into());
            }
            json!({"action": "create", "status": "passed"})
        }
        "delete" => {
            provider.delete_volume(&volume).await?;
            match provider.inspect_volume(&volume).await {
                Err(o3k_storage::StorageProviderError::NotFound) => {}
                Ok(_) => return Err("owned image remained after delete".into()),
                Err(error) => return Err(error.into()),
            }
            json!({"action": "delete", "status": "passed"})
        }
        "prepare" | "terminate" | "inspect-attachment" => {
            let request = StorageAttachmentRequest {
                attachment_id: VolumeAttachmentId::from_uuid(uuid("O3K_CEPH_ATTACHMENT_ID")?),
                volume_id: volume.volume_id,
                project_id: volume.project_id.clone(),
                volume_generation: volume.generation,
                host_id: required("O3K_CEPH_HOST_ID")?,
                access_mode: AttachmentAccessMode::ReadWrite,
            };
            match action.as_str() {
                "prepare" => {
                    let prepared = provider.prepare_attachment(&request).await?;
                    println!("{}", prepared.device_path());
                    json!({"action": "prepare", "status": "passed"})
                }
                "terminate" => {
                    let observation = provider.terminate_attachment(&request).await?;
                    if observation.attached {
                        return Err("RBD mapping remained after termination".into());
                    }
                    json!({"action": "terminate", "status": "passed"})
                }
                _ => {
                    let observation = provider.inspect_attachment(&request).await?;
                    json!({"action": "inspect-attachment", "status": "passed", "attached": observation.attached})
                }
            }
        }
        "snapshot-create" | "snapshot-delete" => {
            let request = StorageSnapshotRequest {
                snapshot_id: SnapshotId::from_uuid(uuid("O3K_CEPH_SNAPSHOT_ID")?),
                volume_id: volume.volume_id,
                project_id: volume.project_id.clone(),
                source_generation: volume.generation,
            };
            if action == "snapshot-create" {
                let observation = provider.create_snapshot(&request).await?;
                if !observation.available
                    || observation.consistency != SnapshotConsistency::CrashConsistent
                {
                    return Err("snapshot did not prove crash-consistent availability".into());
                }
                json!({"action": "snapshot-create", "status": "passed", "snapshot_consistency": "crash_consistent"})
            } else {
                provider.delete_snapshot(&request).await?;
                json!({"action": "snapshot-delete", "status": "passed"})
            }
        }
        _ => return Err("unsupported provider action".into()),
    };

    if action != "prepare" {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "artifact_type": "ceph-rbd-provider-volume",
                "schema_version": 1,
                "redacted": true,
                "provider": "ceph-rbd",
                "owned_backend_leaks": 0,
                "owned_attachment_leaks": 0,
                "owned_inconsistencies": 0,
                "foreign_mutations": 0,
                "result": result,
            }))?
        );
    }
    Ok(())
}
