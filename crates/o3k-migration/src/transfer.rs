//! Resumable image and public-key transfer contracts.
//!
//! The traits are deliberate authority boundaries: the source is observed
//! through bounded reads and the destination is mutated only through its
//! canonical application API adapter supplied by the caller.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransferError {
    #[error("image metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("image exceeds the bounded transfer limit")]
    ImageTooLarge,
    #[error("image source returned an invalid range")]
    InvalidRange,
    #[error("image content digest does not match source metadata")]
    DigestMismatch,
    #[error("image content size does not match source metadata")]
    SizeMismatch,
    #[error("destination operation outcome is unknown")]
    UnknownOutcome,
    #[error("destination operation failed: {0}")]
    Destination(String),
    #[error("public key material is not an allowed public key")]
    PrivateKeyRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub source_id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub properties: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCheckpoint {
    pub source_id: String,
    pub destination_id: String,
    pub expected_size: u64,
    pub expected_sha256: String,
    pub transferred: u64,
    pub digest_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationImageState {
    pub destination_id: String,
    pub owned_by_migration: bool,
    pub transferred: u64,
    pub complete: bool,
}

#[async_trait]
pub trait ImageSource: Send + Sync {
    async fn metadata(&self, source_id: &str) -> Result<ImageMetadata, TransferError>;
    async fn read_range(
        &self,
        source_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, TransferError>;
}

#[async_trait]
pub trait ImageDestination: Send + Sync {
    async fn observe(
        &self,
        operation_id: &str,
    ) -> Result<Option<DestinationImageState>, TransferError>;
    async fn create(
        &self,
        operation_id: &str,
        metadata: &ImageMetadata,
    ) -> Result<String, TransferError>;
    async fn append(
        &self,
        operation_id: &str,
        destination_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), TransferError>;
    async fn complete(
        &self,
        operation_id: &str,
        destination_id: &str,
        metadata: &ImageMetadata,
    ) -> Result<(), TransferError>;
    async fn delete_owned(
        &self,
        operation_id: &str,
        destination_id: &str,
    ) -> Result<(), TransferError>;
}

pub async fn transfer_image<S: ImageSource, D: ImageDestination>(
    source: &S,
    destination: &D,
    operation_id: &str,
    source_id: &str,
) -> Result<TransferCheckpoint, TransferError> {
    if operation_id.trim().is_empty() || source_id.trim().is_empty() {
        return Err(TransferError::InvalidMetadata(
            "operation/source identity is empty".into(),
        ));
    }
    let metadata = source.metadata(source_id).await?;
    validate_metadata(&metadata)?;
    let existing = destination.observe(operation_id).await?;
    let (destination_id, mut offset) = match existing {
        Some(state) if state.owned_by_migration && state.complete => {
            if state.transferred != metadata.size {
                return Err(TransferError::SizeMismatch);
            }
            return Ok(checkpoint(
                &metadata,
                state.destination_id,
                state.transferred,
                &[],
            ));
        }
        Some(state) if state.owned_by_migration && state.transferred <= metadata.size => {
            (state.destination_id, state.transferred)
        }
        Some(_) => {
            return Err(TransferError::Destination(
                "foreign or inconsistent destination state".into(),
            ));
        }
        None => (destination.create(operation_id, &metadata).await?, 0),
    };
    let mut hasher = Sha256::new();
    let mut hashed = 0_u64;
    while hashed < offset {
        let request_size = (offset - hashed).min(MAX_CHUNK_BYTES as u64) as usize;
        let bytes = source.read_range(source_id, hashed, request_size).await?;
        if bytes.is_empty() || bytes.len() > request_size {
            return Err(TransferError::InvalidRange);
        }
        hasher.update(&bytes);
        hashed += bytes.len() as u64;
    }
    while offset < metadata.size {
        let request_size = (metadata.size - offset).min(MAX_CHUNK_BYTES as u64) as usize;
        let bytes = source.read_range(source_id, offset, request_size).await?;
        if bytes.is_empty() || bytes.len() > request_size {
            return Err(TransferError::InvalidRange);
        }
        hasher.update(&bytes);
        destination
            .append(operation_id, &destination_id, offset, &bytes)
            .await?;
        offset += bytes.len() as u64;
    }
    if offset != metadata.size || format!("{:x}", hasher.finalize()) != metadata.sha256 {
        return Err(TransferError::DigestMismatch);
    }
    destination
        .complete(operation_id, &destination_id, &metadata)
        .await?;
    Ok(checkpoint(&metadata, destination_id, offset, &[]))
}

pub fn validate_metadata(metadata: &ImageMetadata) -> Result<(), TransferError> {
    if metadata.source_id.trim().is_empty()
        || metadata.name.trim().is_empty()
        || metadata.sha256.len() != 64
        || !metadata.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(TransferError::InvalidMetadata(
            "missing or malformed image identity/digest".into(),
        ));
    }
    if metadata.size > MAX_IMAGE_BYTES {
        return Err(TransferError::ImageTooLarge);
    }
    if metadata
        .properties
        .iter()
        .any(|(key, value)| contains_secret(key) || contains_secret(value))
    {
        return Err(TransferError::InvalidMetadata(
            "secret-bearing image metadata".into(),
        ));
    }
    Ok(())
}

pub fn validate_public_key(name: &str, public_key: &str) -> Result<(), TransferError> {
    if name.trim().is_empty()
        || public_key.len() > 16 * 1024
        || public_key.contains("PRIVATE KEY")
        || public_key.contains("BEGIN RSA")
        || public_key.contains("BEGIN OPENSSH")
        || public_key.split_whitespace().count() < 2
    {
        return Err(TransferError::PrivateKeyRejected);
    }
    let algorithm = public_key.split_whitespace().next().unwrap_or_default();
    if !matches!(
        algorithm,
        "ssh-rsa"
            | "ssh-ed25519"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    ) {
        return Err(TransferError::PrivateKeyRejected);
    }
    Ok(())
}

fn checkpoint(
    metadata: &ImageMetadata,
    destination_id: String,
    transferred: u64,
    digest: &[u8],
) -> TransferCheckpoint {
    TransferCheckpoint {
        source_id: metadata.source_id.clone(),
        destination_id,
        expected_size: metadata.size,
        expected_sha256: metadata.sha256.clone(),
        transferred,
        digest_state: if digest.is_empty() {
            metadata.sha256.clone()
        } else {
            hex_digest(digest)
        },
    }
}
fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password",
        "token",
        "secret",
        "private",
        "credential",
        "cookie",
    ]
    .iter()
    .any(|part| lower.contains(part))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Source {
        content: Vec<u8>,
    }
    #[async_trait]
    impl ImageSource for Source {
        async fn metadata(&self, id: &str) -> Result<ImageMetadata, TransferError> {
            let mut hash = Sha256::new();
            hash.update(&self.content);
            Ok(ImageMetadata {
                source_id: id.into(),
                name: "image".into(),
                size: self.content.len() as u64,
                sha256: format!("{:x}", hash.finalize()),
                properties: vec![("format".into(), "raw".into())],
            })
        }
        async fn read_range(
            &self,
            _id: &str,
            offset: u64,
            limit: usize,
        ) -> Result<Vec<u8>, TransferError> {
            Ok(self
                .content
                .get(offset as usize..(offset as usize + limit).min(self.content.len()))
                .unwrap_or_default()
                .to_vec())
        }
    }
    #[derive(Clone)]
    struct Destination {
        bytes: Arc<Mutex<Vec<u8>>>,
    }
    #[async_trait]
    impl ImageDestination for Destination {
        async fn observe(
            &self,
            _operation_id: &str,
        ) -> Result<Option<DestinationImageState>, TransferError> {
            let bytes = self.bytes.lock().expect("lock").len();
            Ok((bytes > 0).then(|| DestinationImageState {
                destination_id: "canonical-image".into(),
                owned_by_migration: true,
                transferred: bytes as u64,
                complete: false,
            }))
        }
        async fn create(
            &self,
            _operation_id: &str,
            _metadata: &ImageMetadata,
        ) -> Result<String, TransferError> {
            Ok("canonical-image".into())
        }
        async fn append(
            &self,
            _operation_id: &str,
            _id: &str,
            offset: u64,
            bytes: &[u8],
        ) -> Result<(), TransferError> {
            let mut target = self.bytes.lock().expect("lock");
            assert_eq!(target.len() as u64, offset);
            target.extend_from_slice(bytes);
            Ok(())
        }
        async fn complete(
            &self,
            _operation_id: &str,
            _id: &str,
            _metadata: &ImageMetadata,
        ) -> Result<(), TransferError> {
            Ok(())
        }
        async fn delete_owned(&self, _operation_id: &str, _id: &str) -> Result<(), TransferError> {
            self.bytes.lock().expect("lock").clear();
            Ok(())
        }
    }
    #[tokio::test]
    async fn transfer_streams_and_replays_existing_owned_state() {
        let source = Source {
            content: b"bounded image content".to_vec(),
        };
        let destination = Destination {
            bytes: Arc::new(Mutex::new(Vec::new())),
        };
        let result = transfer_image(&source, &destination, "op-a", "source-image")
            .await
            .expect("transfer");
        assert_eq!(result.transferred, source.content.len() as u64);
        let replay = transfer_image(&source, &destination, "op-a", "source-image")
            .await
            .expect("resume");
        assert_eq!(replay.transferred, source.content.len() as u64);
    }
    #[test]
    fn public_key_validation_rejects_private_material_and_accepts_public() {
        assert!(validate_public_key("key", "ssh-ed25519 AAAA comment").is_ok());
        assert_eq!(
            validate_public_key("key", "-----BEGIN PRIVATE KEY-----"),
            Err(TransferError::PrivateKeyRejected)
        );
    }
    #[test]
    fn metadata_rejects_secret_properties() {
        let metadata = ImageMetadata {
            source_id: "image".into(),
            name: "image".into(),
            size: 0,
            sha256: "0".repeat(64),
            properties: vec![("password".into(), "redacted".into())],
        };
        assert!(matches!(
            validate_metadata(&metadata),
            Err(TransferError::InvalidMetadata(_))
        ));
    }
}
