//! Deterministic, filesystem-backed OpenStack config-drive content.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_USER_DATA_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ConfigDriveError {
    #[error("config-drive input is invalid")]
    InvalidInput,
    #[error("config-drive user-data is too large")]
    UserDataTooLarge,
    #[error("config-drive metadata is too large")]
    MetadataTooLarge,
    #[error("config-drive storage failed")]
    Storage(#[source] io::Error),
    #[error("config-drive serialization failed")]
    Serialization(#[source] serde_json::Error),
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

#[derive(Serialize)]
struct MetaData<'a> {
    uuid: &'a str,
    name: &'a str,
    hostname: &'a str,
    #[serde(rename = "public_keys")]
    public_keys: BTreeMap<&'static str, &'a str>,
    meta: &'a BTreeMap<String, String>,
}

pub fn generate(
    root: impl AsRef<Path>,
    input: &ConfigDriveInput,
) -> Result<ConfigDriveResult, ConfigDriveError> {
    validate(input)?;
    let root = root.as_ref();
    fs::create_dir_all(root).map_err(ConfigDriveError::Storage)?;
    let metadata = MetaData {
        uuid: &input.instance_id,
        name: &input.instance_id,
        hostname: &input.hostname,
        public_keys: BTreeMap::from([("default", input.ssh_public_key.as_str())]),
        meta: &input.metadata,
    };
    let meta_data =
        serde_json::to_vec_pretty(&metadata).map_err(ConfigDriveError::Serialization)?;
    let network_data =
        serde_json::to_vec_pretty(&input.network_data).map_err(ConfigDriveError::Serialization)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(&meta_data);
    fingerprint.update(&input.user_data);
    fingerprint.update(&network_data);
    if let Some(vendor) = &input.vendor_data {
        fingerprint.update(vendor);
    }
    let digest = fingerprint.finalize();
    let mut fingerprint_sha256 = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut fingerprint_sha256, "{byte:02x}");
    }
    let directory = root.join(&input.instance_id);
    let temporary = root.join(format!(".{}-tmp-{}", input.instance_id, Uuid::now_v7()));
    fs::create_dir_all(&temporary).map_err(ConfigDriveError::Storage)?;
    write(
        &temporary.join("openstack/latest/meta_data.json"),
        &meta_data,
    )?;
    write(
        &temporary.join("openstack/latest/network_data.json"),
        &network_data,
    )?;
    write(
        &temporary.join("openstack/latest/user_data"),
        &input.user_data,
    )?;
    if let Some(vendor) = &input.vendor_data {
        write(&temporary.join("openstack/latest/vendor_data.json"), vendor)?;
    }
    if directory.exists() {
        fs::remove_dir_all(&directory).map_err(ConfigDriveError::Storage)?;
    }
    fs::rename(&temporary, &directory).map_err(|error| {
        let _ = fs::remove_dir_all(&temporary);
        ConfigDriveError::Storage(error)
    })?;
    Ok(ConfigDriveResult {
        directory,
        fingerprint_sha256,
    })
}

pub fn cleanup(path: &Path) -> Result<(), ConfigDriveError> {
    if path
        .file_name()
        .and_then(|value| value.to_str())
        .is_none_or(|name| name.starts_with('.'))
    {
        return Err(ConfigDriveError::InvalidInput);
    }
    if path.exists() {
        fs::remove_dir_all(path).map_err(ConfigDriveError::Storage)?;
    }
    Ok(())
}

fn validate(input: &ConfigDriveInput) -> Result<(), ConfigDriveError> {
    if input.instance_id.is_empty()
        || input.instance_id
            != Path::new(&input.instance_id)
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
        || input.instance_id.len() > 128
        || input.hostname.is_empty()
        || input.hostname.len() > 255
        || input
            .hostname
            .chars()
            .any(|c| c.is_whitespace() || c == '/' || c == '\\')
        || input.ssh_public_key.split_whitespace().count() < 2
        || (!input.ssh_public_key.starts_with("ssh-")
            && !input.ssh_public_key.starts_with("ecdsa-"))
    {
        return Err(ConfigDriveError::InvalidInput);
    }
    if input.user_data.len() > MAX_USER_DATA_BYTES {
        return Err(ConfigDriveError::UserDataTooLarge);
    }
    let encoded = serde_json::to_vec(&input.metadata).map_err(ConfigDriveError::Serialization)?;
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(ConfigDriveError::MetadataTooLarge);
    }
    Ok(())
}

fn write(path: &Path, content: &[u8]) -> Result<(), ConfigDriveError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigDriveError::Storage)?;
    }
    fs::write(path, content).map_err(ConfigDriveError::Storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> ConfigDriveInput {
        ConfigDriveInput {
            instance_id: "instance-1".to_owned(),
            hostname: "vm-1".to_owned(),
            ssh_public_key: "ssh-ed25519 AAAA test@example".to_owned(),
            user_data: b"#cloud-config\nhostname: vm-1\n".to_vec(),
            metadata: BTreeMap::from([("role".to_owned(), "worker".to_owned())]),
            network_data: BTreeMap::from([("version".to_owned(), "1".to_owned())]),
            vendor_data: None,
        }
    }
    #[test]
    fn generation_is_deterministic_and_layout_is_openstack_compatible()
    -> Result<(), ConfigDriveError> {
        let root = std::env::temp_dir().join(format!("o3k-drive-{}", Uuid::now_v7()));
        let first = generate(&root, &input())?;
        let bytes = fs::read(first.directory.join("openstack/latest/user_data"))
            .map_err(ConfigDriveError::Storage)?;
        assert_eq!(bytes, input().user_data);
        let second = generate(&root, &input())?;
        assert_eq!(first.fingerprint_sha256, second.fingerprint_sha256);
        cleanup(&second.directory)?;
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }
    #[test]
    fn unsafe_and_oversized_inputs_are_rejected() {
        let mut value = input();
        value.instance_id = "../escape".to_owned();
        assert!(matches!(
            generate(std::env::temp_dir(), &value),
            Err(ConfigDriveError::InvalidInput)
        ));
        let mut value = input();
        value.user_data = vec![b'x'; MAX_USER_DATA_BYTES + 1];
        assert!(matches!(
            generate(std::env::temp_dir(), &value),
            Err(ConfigDriveError::UserDataTooLarge)
        ));
    }
}
