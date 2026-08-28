//! Image domain types: status, record, artifact, error.

use std::{io, path::PathBuf};

use o3k_kernel::{LimitKey, LimitValue};
use o3k_store::StoreError;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub(crate) const QCOW2_VERSION_2_HEADER: u64 = 72;
pub(crate) const QCOW2_VERSION_3_HEADER: u64 = 104;
pub(crate) const QCOW2_MAX_HEADER_LENGTH: u64 = 1 << 20;
pub(crate) const QCOW2_MIN_CLUSTER_BITS: u32 = 9;
pub(crate) const QCOW2_MAX_CLUSTER_BITS: u32 = 21;
pub(crate) const QCOW2_MAX_REFCOUNT_ORDER: u32 = 6;
pub(crate) const QCOW2_MAX_DISK_SIZE: u64 = 1_u64 << 62;
pub(crate) const QCOW2_CLUSTER_OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
pub(crate) const QCOW2_REFCOUNT_BLOCK_OFFSET_MASK: u64 = 0xffff_ffff_ffff_fe00;
pub(crate) const QCOW2_INCOMPATIBLE_ALLOWED: u64 = (1 << 0) | (1 << 3);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageStatus {
    Queued,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRecord {
    pub id: Uuid,
    pub name: String,
    pub project_id: String,
    pub status: ImageStatus,
    pub visibility: String,
    pub container_format: String,
    pub disk_format: String,
    pub size: Option<u64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageArtifact {
    pub id: Uuid,
    pub checksum: String,
    pub format: String,
    pub size: u64,
    pub content: Vec<u8>,
}

/// A verified image artifact published into this process's managed cache.
///
/// The path is intentionally local to the image-cache boundary. It must not
/// be sent through the public API or compute-agent protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedImageArtifact {
    pub id: Uuid,
    pub checksum: String,
    pub format: String,
    pub size: u64,
    pub path: PathBuf,
}

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("image not found")]
    NotFound,
    #[error("image operation is not allowed")]
    Conflict,
    #[error("image name and formats must be non-empty")]
    InvalidMetadata,
    #[error("image upload exceeds the configured limit")]
    TooLarge,
    #[error("quota exceeded for {key}: limit {limit}, used {used}, requested {requested}")]
    QuotaExceeded {
        key: LimitKey,
        limit: LimitValue,
        used: u64,
        requested: u64,
    },
    #[error("image storage error")]
    Storage(#[source] io::Error),
    #[error("image metadata is corrupt")]
    CorruptMetadata(#[source] serde_json::Error),
    #[error("image store error")]
    Store(#[source] StoreError),
    #[error("image format is not supported")]
    UnsupportedFormat,
    #[error("image checksum does not match")]
    ChecksumMismatch,
    #[error("image path is invalid")]
    InvalidPath,
    #[error("image overlay tool is unavailable or failed")]
    OverlayFailed,
    #[error("image format verification failed")]
    FormatVerificationFailed,
}
