//! Config drive domain types: input, result, artifact, errors.

use std::{collections::BTreeMap, io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigDriveError {
    #[error("config-drive input is invalid")]
    InvalidInput,
    #[error("config-drive user-data is too large")]
    UserDataTooLarge,
    #[error("config-drive metadata is too large")]
    MetadataTooLarge,
    #[error("config-drive network data is too large")]
    NetworkDataTooLarge,
    #[error("config-drive vendor data is too large")]
    VendorDataTooLarge,
    #[error("config-drive storage failed")]
    Storage(#[source] io::Error),
    #[error("config-drive serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("config-drive ownership manifest is corrupt")]
    CorruptManifest(#[source] serde_json::Error),
    #[error("config-drive path is not owned by o3k")]
    UnownedPath,
    #[error("config-drive ISO command failed")]
    ToolFailed(#[source] io::Error),
    #[error("config-drive ISO output is invalid or tampered")]
    InvalidIsoOutput,
}

#[derive(Debug, Clone)]
pub struct ConfigDriveInput {
    pub instance_id: String,
    pub hostname: String,
    pub ssh_public_key: String,
    pub user_data: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
    pub network_data: BTreeMap<String, String>,
    pub vendor_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDriveResult {
    pub directory: PathBuf,
    pub fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDriveIsoResult {
    pub path: PathBuf,
    pub fingerprint_sha256: String,
    pub source_fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDriveArtifact {
    pub format: String,
    pub sha256: String,
    pub size: u64,
    pub content: Vec<u8>,
}
