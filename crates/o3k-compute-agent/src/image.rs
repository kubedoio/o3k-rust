use std::{
    fs,
    path::{Path, PathBuf},
};

use o3k_image::{ImageCache, ImageError};
use o3k_provider_contract::compute_proto as proto;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::artifact::CommittedImageQuery;
use crate::{ArtifactStore, ArtifactStoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMaterializationRequest {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: String,
    pub resource_id: String,
    pub agent_id: String,
    pub artifact_id: String,
    pub sha256: String,
    pub format: String,
    pub size_bytes: u64,
    pub instance_id: String,
    pub disk_gib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMaterialization {
    pub base_path: PathBuf,
    pub overlay_path: PathBuf,
    pub checksum: String,
    pub format: String,
    pub size_bytes: u64,
}

#[derive(Debug, Error)]
pub enum ImageMaterializerError {
    #[error("committed image artifact is invalid or unavailable")]
    Artifact(#[from] ArtifactStoreError),
    #[error("image cache operation failed")]
    Image(#[from] ImageError),
    #[error("image ownership state is invalid")]
    Ownership,
    #[error("image ownership state is unavailable")]
    Storage(#[source] std::io::Error),
    #[error("image ownership manifest is corrupt")]
    CorruptManifest(#[source] serde_json::Error),
}

/// Converts the authenticated create payload into the exact image lookup
/// identity used by the agent-local materializer. This performs no host I/O;
/// callers may therefore validate the command before touching the cache.
pub fn image_materialization_request(
    command: &proto::Command,
) -> Result<ImageMaterializationRequest, ImageMaterializerError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(ImageMaterializerError::Ownership);
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(ImageMaterializerError::Ownership);
    };
    let Some(reference) = resolved.image_transfer.as_ref() else {
        return Err(ImageMaterializerError::Ownership);
    };
    let expected_transfer = crate::deterministic_artifact_transfer_id(
        &command.command_id,
        proto::ArtifactKind::ImageBase,
        &resolved.image_artifact_id,
    );
    if reference.transfer_id != expected_transfer
        || reference.expires_at_unix_ms <= crate::unix_ms()
    {
        return Err(ImageMaterializerError::Ownership);
    }
    Ok(ImageMaterializationRequest {
        transfer_id: reference.transfer_id.clone(),
        command_id: command.command_id.clone(),
        operation_id: command.operation_id.clone(),
        resource_id: command.resource_id.clone(),
        agent_id: command.agent_id.clone(),
        artifact_id: resolved.image_artifact_id.clone(),
        sha256: resolved.image_sha256.clone(),
        format: resolved.image_format.clone(),
        size_bytes: reference.size_bytes,
        instance_id: command.resource_id.clone(),
        disk_gib: resolved.disk_gib,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OwnershipManifest {
    instance_id: String,
    transfer_id: String,
    command_id: String,
    operation_id: String,
    resource_id: String,
    agent_id: String,
    artifact_id: String,
    sha256: String,
    format: String,
    size_bytes: u64,
    overlay_file: String,
}

/// Agent-local bridge from a committed authenticated artifact to the managed
/// image cache. Host paths never cross the control-plane or protobuf boundary.
#[derive(Clone)]
pub struct ImageMaterializer {
    artifacts: ArtifactStore,
    cache: ImageCache,
    ownership_root: PathBuf,
}

impl ImageMaterializer {
    pub fn open(
        artifacts: ArtifactStore,
        cache_root: impl Into<PathBuf>,
        max_cache_bytes: u64,
    ) -> Result<Self, ImageMaterializerError> {
        let cache_root = cache_root.into();
        let ownership_root = cache_root.join("ownership");
        fs::create_dir_all(&ownership_root).map_err(ImageMaterializerError::Storage)?;
        Ok(Self {
            artifacts,
            cache: ImageCache::open(cache_root, max_cache_bytes)?,
            ownership_root,
        })
    }

    pub fn materialize(
        &self,
        request: &ImageMaterializationRequest,
    ) -> Result<ImageMaterialization, ImageMaterializerError> {
        validate_request(request)?;
        let ownership_path = self.ownership_path(&request.instance_id)?;
        if ownership_path.exists() || ownership_path.is_symlink() {
            let manifest = self.read_manifest(&ownership_path)?;
            let mut effective = request.clone();
            if effective.size_bytes == 0 {
                effective.size_bytes = manifest.size_bytes;
            }
            validate_manifest(&manifest, &effective)?;
            let overlay_path = self.cache.create_overlay(
                &effective.instance_id,
                &self.cache.resolve_base(
                    &effective.sha256,
                    &effective.format,
                    effective.size_bytes,
                )?,
            )?;
            return Ok(ImageMaterialization {
                base_path: self.cache.resolve_base(
                    &effective.sha256,
                    &effective.format,
                    effective.size_bytes,
                )?,
                overlay_path,
                checksum: effective.sha256.clone(),
                format: effective.format.clone(),
                size_bytes: effective.size_bytes,
            });
        }

        let source = self
            .artifacts
            .resolve_committed_image_query(&CommittedImageQuery {
                transfer_id: request.transfer_id.clone(),
                command_id: request.command_id.clone(),
                operation_id: request.operation_id.clone(),
                resource_id: request.resource_id.clone(),
                agent_id: request.agent_id.clone(),
                artifact_id: request.artifact_id.clone(),
                sha256: request.sha256.clone(),
                format: request.format.clone(),
                size_bytes: 0,
            })?;
        let mut effective = request.clone();
        if effective.size_bytes == 0 {
            effective.size_bytes = fs::metadata(&source)
                .map_err(ImageMaterializerError::Storage)?
                .len();
        }
        validate_request(&effective)?;
        let base_path =
            self.cache
                .cache_base_path(&effective.sha256, &effective.format, &source)?;
        let overlay_path = match self
            .cache
            .create_overlay(&effective.instance_id, &base_path)
            .and_then(|overlay| {
                if effective.disk_gib == 0 {
                    Ok(overlay)
                } else {
                    self.cache
                        .resize_overlay(&effective.instance_id, &overlay, effective.disk_gib)
                        .map(|()| overlay)
                }
            }) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.cache.delete_overlay(&effective.instance_id);
                return Err(error.into());
            }
        };
        let manifest = OwnershipManifest {
            instance_id: effective.instance_id.clone(),
            transfer_id: effective.transfer_id.clone(),
            command_id: effective.command_id.clone(),
            operation_id: effective.operation_id.clone(),
            resource_id: effective.resource_id.clone(),
            agent_id: effective.agent_id.clone(),
            artifact_id: effective.artifact_id.clone(),
            sha256: effective.sha256.clone(),
            format: effective.format.clone(),
            size_bytes: effective.size_bytes,
            overlay_file: overlay_path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or(ImageMaterializerError::Ownership)?
                .to_owned(),
        };
        if let Err(error) = self.write_manifest(&ownership_path, &manifest) {
            let _ = self.cache.delete_overlay(&effective.instance_id);
            return Err(error);
        }
        Ok(ImageMaterialization {
            base_path,
            overlay_path,
            checksum: effective.sha256.clone(),
            format: effective.format.clone(),
            size_bytes: effective.size_bytes,
        })
    }

    pub fn delete(
        &self,
        request: &ImageMaterializationRequest,
    ) -> Result<(), ImageMaterializerError> {
        validate_request(request)?;
        let path = self.ownership_path(&request.instance_id)?;
        let manifest = self.read_manifest(&path)?;
        validate_manifest(&manifest, request)?;
        self.cache.delete_overlay(&request.instance_id)?;
        fs::remove_file(path).map_err(ImageMaterializerError::Storage)
    }

    fn ownership_path(&self, instance_id: &str) -> Result<PathBuf, ImageMaterializerError> {
        if instance_id.is_empty()
            || Path::new(instance_id).file_name().and_then(|v| v.to_str()) != Some(instance_id)
        {
            return Err(ImageMaterializerError::Ownership);
        }
        Ok(self.ownership_root.join(format!("{instance_id}.json")))
    }

    fn read_manifest(&self, path: &Path) -> Result<OwnershipManifest, ImageMaterializerError> {
        let metadata = fs::symlink_metadata(path).map_err(ImageMaterializerError::Storage)?;
        if !metadata.file_type().is_file() {
            return Err(ImageMaterializerError::Ownership);
        }
        serde_json::from_slice(&fs::read(path).map_err(ImageMaterializerError::Storage)?)
            .map_err(ImageMaterializerError::CorruptManifest)
    }

    fn write_manifest(
        &self,
        path: &Path,
        manifest: &OwnershipManifest,
    ) -> Result<(), ImageMaterializerError> {
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
        fs::write(
            &temporary,
            serde_json::to_vec(manifest).map_err(|_| ImageMaterializerError::Ownership)?,
        )
        .map_err(ImageMaterializerError::Storage)?;
        fs::rename(temporary, path).map_err(ImageMaterializerError::Storage)
    }
}

fn validate_request(request: &ImageMaterializationRequest) -> Result<(), ImageMaterializerError> {
    if request.transfer_id.is_empty()
        || request.command_id.is_empty()
        || request.operation_id.is_empty()
        || request.resource_id.is_empty()
        || request.agent_id.is_empty()
        || request.artifact_id.is_empty()
        || request.instance_id.is_empty()
        || request.sha256.len() != 64
        || !request.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !matches!(request.format.as_str(), "raw" | "qcow2")
        || request.size_bytes > 64 * 1024 * 1024
    {
        return Err(ImageMaterializerError::Ownership);
    }
    Ok(())
}

fn validate_manifest(
    manifest: &OwnershipManifest,
    request: &ImageMaterializationRequest,
) -> Result<(), ImageMaterializerError> {
    (manifest.instance_id == request.instance_id
        && manifest.transfer_id == request.transfer_id
        && manifest.command_id == request.command_id
        && manifest.operation_id == request.operation_id
        && manifest.resource_id == request.resource_id
        && manifest.agent_id == request.agent_id
        && manifest.artifact_id == request.artifact_id
        && manifest.sha256 == request.sha256
        && manifest.format == request.format
        && manifest.size_bytes == request.size_bytes)
        .then_some(())
        .ok_or(ImageMaterializerError::Ownership)
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_provider_contract::compute_proto as proto;
    use sha2::{Digest, Sha256};

    fn request(
        content: &[u8],
    ) -> Result<(ArtifactStore, ImageMaterializationRequest), Box<dyn std::error::Error>> {
        let artifact_root =
            std::env::temp_dir().join(format!("o3k-image-artifact-{}", uuid::Uuid::now_v7()));
        let store = ArtifactStore::open(&artifact_root, "agent-1")?;
        let checksum = format!("{:x}", Sha256::digest(content));
        let offer = proto::ArtifactOffer {
            transfer_id: "transfer-1".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id: "operation-1".to_owned(),
            resource_id: "resource-1".to_owned(),
            agent_id: "agent-1".to_owned(),
            artifact_id: "image-1".to_owned(),
            kind: proto::ArtifactKind::ImageBase as i32,
            sha256: checksum.clone(),
            size_bytes: content.len() as u64,
            format: "raw".to_owned(),
            chunk_size_bytes: 256,
            chunk_count: content.len().div_ceil(256) as u32,
            ..Default::default()
        };
        store.begin(&offer)?;
        for (index, chunk) in content.chunks(256).enumerate() {
            store.accept_chunk(
                &offer,
                &proto::ArtifactChunk {
                    transfer_id: offer.transfer_id.clone(),
                    chunk_index: index as u32,
                    offset_bytes: index as u64 * 256,
                    data: chunk.to_vec(),
                    chunk_sha256: format!("{:x}", Sha256::digest(chunk)),
                },
            )?;
        }
        store.finish(
            &offer,
            &proto::ArtifactEnd {
                transfer_id: offer.transfer_id.clone(),
                sha256: checksum.clone(),
                size_bytes: content.len() as u64,
            },
        )?;
        let request = ImageMaterializationRequest {
            transfer_id: offer.transfer_id,
            command_id: offer.command_id,
            operation_id: offer.operation_id,
            resource_id: offer.resource_id,
            agent_id: offer.agent_id,
            artifact_id: offer.artifact_id,
            sha256: checksum,
            format: offer.format,
            size_bytes: offer.size_bytes,
            instance_id: "instance-1".to_owned(),
            disk_gib: 1,
        };
        Ok((store, request))
    }

    #[test]
    fn committed_raw_image_becomes_owned_overlay_and_deletes_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let content = vec![0_u8; 1024 * 1024];
        let (store, request) = request(&content)?;
        let root = std::env::temp_dir().join(format!("o3k-image-cache-{}", uuid::Uuid::now_v7()));
        let materializer = ImageMaterializer::open(store, &root, 2 * 1024 * 1024 * 1024)?;
        let first = materializer.materialize(&request)?;
        assert!(first.base_path.is_file());
        assert!(first.overlay_path.is_file());
        let repeated = materializer.materialize(&request)?;
        assert_eq!(first, repeated);
        materializer.delete(&request)?;
        assert!(!first.overlay_path.exists());
        assert!(materializer.delete(&request).is_err());
        std::fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn image_request_requires_command_bound_transfer_identity() -> Result<(), ImageMaterializerError>
    {
        let command_id = "command-1";
        let artifact_id = "image-1";
        let command = proto::Command {
            command_id: command_id.to_owned(),
            operation_id: "operation-1".to_owned(),
            agent_id: "agent-1".to_owned(),
            resource_id: "resource-1".to_owned(),
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                image_id: "image-1".to_owned(),
                flavor_id: "flavor-1".to_owned(),
                resolved: Some(proto::ResolvedCreateInputs {
                    image_artifact_id: artifact_id.to_owned(),
                    image_sha256: "a".repeat(64),
                    image_format: "raw".to_owned(),
                    disk_gib: 1,
                    image_transfer: Some(proto::ArtifactReference {
                        transfer_id: crate::deterministic_artifact_transfer_id(
                            command_id,
                            proto::ArtifactKind::ImageBase,
                            artifact_id,
                        ),
                        expires_at_unix_ms: crate::unix_ms().saturating_add(10_000),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        let request = image_materialization_request(&command)?;
        assert_eq!(
            request.transfer_id,
            crate::deterministic_artifact_transfer_id(
                command_id,
                proto::ArtifactKind::ImageBase,
                artifact_id,
            )
        );
        assert_eq!(request.command_id, command_id);
        Ok(())
    }
}
