//! Bounded persistent-volume transfer and attachment translation contracts.

use crate::{ResourceKind, manifest::MigrationManifest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_VOLUME_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VolumeError {
    #[error("volume metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("volume exceeds the bounded transfer limit")]
    TooLarge,
    #[error("volume format is unsupported")]
    UnsupportedFormat,
    #[error("encrypted volumes require an explicitly accepted capability")]
    EncryptionUnsupported,
    #[error("source volume is not quiesced")]
    NotQuiesced,
    #[error("source volume changed during transfer")]
    SourceDrift,
    #[error("volume content size does not match metadata")]
    SizeMismatch,
    #[error("volume content checksum does not match metadata")]
    ChecksumMismatch,
    #[error("volume source returned an invalid range")]
    InvalidRange,
    #[error("destination volume state is foreign or stale")]
    ForeignState,
    #[error("destination operation outcome is unknown")]
    UnknownOutcome,
    #[error("destination operation failed: {0}")]
    Destination(String),
    #[error("volume attachment translation is invalid: {0}")]
    InvalidAttachment(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMetadata {
    pub source_id: String,
    pub source_fingerprint: String,
    pub size: u64,
    pub format: String,
    pub sha256: String,
    pub encrypted: bool,
    pub quiesced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeCheckpoint {
    pub source_id: String,
    pub destination_id: String,
    pub source_fingerprint: String,
    pub expected_size: u64,
    pub expected_sha256: String,
    pub transferred: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationVolumeState {
    pub destination_id: String,
    pub migration_id: String,
    pub source_fingerprint: String,
    pub transferred: u64,
    pub complete: bool,
}

#[async_trait]
pub trait VolumeSource: Send + Sync {
    async fn metadata(&self, source_id: &str) -> Result<VolumeMetadata, VolumeError>;
    async fn read_range(
        &self,
        source_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, VolumeError>;
}

#[async_trait]
pub trait VolumeDestination: Send + Sync {
    async fn observe(
        &self,
        migration_id: &str,
        source_id: &str,
    ) -> Result<Option<DestinationVolumeState>, VolumeError>;
    async fn create(
        &self,
        migration_id: &str,
        metadata: &VolumeMetadata,
    ) -> Result<String, VolumeError>;
    async fn append(
        &self,
        migration_id: &str,
        destination_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), VolumeError>;
    async fn complete(
        &self,
        migration_id: &str,
        destination_id: &str,
        metadata: &VolumeMetadata,
    ) -> Result<(), VolumeError>;
    async fn delete_owned(
        &self,
        migration_id: &str,
        destination_id: &str,
    ) -> Result<(), VolumeError>;
}

pub async fn transfer_volume<S: VolumeSource, D: VolumeDestination>(
    source: &S,
    destination: &D,
    migration_id: &str,
    source_id: &str,
) -> Result<VolumeCheckpoint, VolumeError> {
    if migration_id.trim().is_empty() || source_id.trim().is_empty() {
        return Err(VolumeError::InvalidMetadata(
            "empty migration/source identity".into(),
        ));
    }
    let metadata = source.metadata(source_id).await?;
    validate_metadata(&metadata)?;
    let existing = destination.observe(migration_id, source_id).await?;
    let (destination_id, mut offset) = match existing {
        Some(state)
            if state.migration_id == migration_id
                && state.source_fingerprint == metadata.source_fingerprint
                && state.complete =>
        {
            if state.transferred != metadata.size {
                return Err(VolumeError::SizeMismatch);
            }
            return Ok(checkpoint(
                &metadata,
                state.destination_id,
                state.transferred,
            ));
        }
        Some(state)
            if state.migration_id == migration_id
                && state.source_fingerprint == metadata.source_fingerprint
                && state.transferred <= metadata.size =>
        {
            (state.destination_id, state.transferred)
        }
        Some(_) => return Err(VolumeError::ForeignState),
        None => (destination.create(migration_id, &metadata).await?, 0),
    };
    let mut hasher = Sha256::new();
    let mut hashed = 0_u64;
    while hashed < offset {
        let request_size = (offset - hashed).min(MAX_CHUNK_BYTES as u64) as usize;
        let bytes = source.read_range(source_id, hashed, request_size).await?;
        if bytes.is_empty() || bytes.len() > request_size {
            return Err(VolumeError::InvalidRange);
        }
        hasher.update(&bytes);
        hashed += bytes.len() as u64;
    }
    while offset < metadata.size {
        let request_size = (metadata.size - offset).min(MAX_CHUNK_BYTES as u64) as usize;
        let bytes = source.read_range(source_id, offset, request_size).await?;
        if bytes.is_empty() || bytes.len() > request_size {
            return Err(VolumeError::InvalidRange);
        }
        hasher.update(&bytes);
        destination
            .append(migration_id, &destination_id, offset, &bytes)
            .await?;
        offset += bytes.len() as u64;
    }
    if offset != metadata.size {
        return Err(VolumeError::SizeMismatch);
    }
    if format!("{:x}", hasher.finalize()) != metadata.sha256 {
        return Err(VolumeError::ChecksumMismatch);
    }
    destination
        .complete(migration_id, &destination_id, &metadata)
        .await?;
    Ok(checkpoint(&metadata, destination_id, offset))
}

pub fn validate_metadata(metadata: &VolumeMetadata) -> Result<(), VolumeError> {
    if metadata.source_id.trim().is_empty()
        || metadata.source_fingerprint.len() != 64
        || !metadata
            .source_fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || metadata.sha256.len() != 64
        || !metadata.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(VolumeError::InvalidMetadata(
            "missing identity or digest".into(),
        ));
    }
    if metadata.size > MAX_VOLUME_BYTES {
        return Err(VolumeError::TooLarge);
    }
    if !matches!(metadata.format.as_str(), "raw" | "qcow2") {
        return Err(VolumeError::UnsupportedFormat);
    }
    if metadata.encrypted {
        return Err(VolumeError::EncryptionUnsupported);
    }
    if !metadata.quiesced {
        return Err(VolumeError::NotQuiesced);
    }
    Ok(())
}

fn checkpoint(
    metadata: &VolumeMetadata,
    destination_id: String,
    transferred: u64,
) -> VolumeCheckpoint {
    VolumeCheckpoint {
        source_id: metadata.source_id.clone(),
        destination_id,
        source_fingerprint: metadata.source_fingerprint.clone(),
        expected_size: metadata.size,
        expected_sha256: metadata.sha256.clone(),
        transferred,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentTranslation {
    pub source_key: String,
    pub canonical_id: String,
    pub volume_id: String,
    pub server_id: String,
    pub owner_scope_id: String,
    pub ownership_marker: String,
}

pub fn translate_attachments(
    manifest: &MigrationManifest,
) -> Result<Vec<AttachmentTranslation>, VolumeError> {
    let mut result = Vec::new();
    for attachment in manifest
        .nodes
        .iter()
        .filter(|node| node.resource_type == ResourceKind::VolumeAttachment)
    {
        if !attachment.unsupported.is_empty() || attachment.destination_id.is_none() {
            return Err(VolumeError::InvalidAttachment(attachment.key.clone()));
        }
        let volume_key = attachment.depends_on.iter().find(|key| {
            manifest
                .nodes
                .iter()
                .any(|node| node.key == **key && node.resource_type == ResourceKind::Volume)
        });
        let server_key = attachment.depends_on.iter().find(|key| {
            manifest
                .nodes
                .iter()
                .any(|node| node.key == **key && node.resource_type == ResourceKind::Server)
        });
        let (Some(volume_key), Some(server_key)) = (volume_key, server_key) else {
            return Err(VolumeError::InvalidAttachment(
                "missing volume/server dependency".into(),
            ));
        };
        let volume = manifest.nodes.iter().find(|node| node.key == *volume_key);
        let server = manifest.nodes.iter().find(|node| node.key == *server_key);
        let (Some(volume), Some(server)) = (volume, server) else {
            return Err(VolumeError::InvalidAttachment(attachment.key.clone()));
        };
        let (Some(volume_id), Some(server_id), Some(canonical_id)) = (
            &volume.destination_id,
            &server.destination_id,
            &attachment.destination_id,
        ) else {
            return Err(VolumeError::InvalidAttachment(attachment.key.clone()));
        };
        result.push(AttachmentTranslation {
            source_key: attachment.key.clone(),
            canonical_id: canonical_id.clone(),
            volume_id: volume_id.clone(),
            server_id: server_id.clone(),
            owner_scope_id: attachment.owner_scope_id.clone(),
            ownership_marker: format!("o3k:migration:{}:{}", manifest.migration_id, canonical_id),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> VolumeMetadata {
        VolumeMetadata {
            source_id: "volume-a".into(),
            source_fingerprint: "a".repeat(64),
            size: 4,
            format: "raw".into(),
            sha256: format!("{:x}", Sha256::digest(b"data")),
            encrypted: false,
            quiesced: true,
        }
    }

    #[test]
    fn validation_rejects_unsafe_volume_capabilities() {
        let mut value = metadata();
        value.encrypted = true;
        assert_eq!(
            validate_metadata(&value),
            Err(VolumeError::EncryptionUnsupported)
        );
        value.encrypted = false;
        value.quiesced = false;
        assert_eq!(validate_metadata(&value), Err(VolumeError::NotQuiesced));
        value.quiesced = true;
        value.format = "vmdk".into();
        assert_eq!(
            validate_metadata(&value),
            Err(VolumeError::UnsupportedFormat)
        );
    }

    #[test]
    fn checkpoint_keeps_integrity_and_source_identity() {
        let value = metadata();
        let checkpoint = checkpoint(&value, "destination-a".into(), 4);
        assert_eq!(checkpoint.expected_sha256, value.sha256);
        assert_eq!(checkpoint.source_fingerprint, value.source_fingerprint);
        assert_eq!(checkpoint.transferred, value.size);
    }
}
