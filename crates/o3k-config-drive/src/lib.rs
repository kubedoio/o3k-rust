//! Deterministic, filesystem-backed OpenStack config-drive content.
//!
//! ISO materialization uses the external `xorriso` executable. Protected
//! real-host execution must provision and capability-check `xorriso`; unit
//! tests inject a command runner and therefore do not require the executable.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_USER_DATA_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_NETWORK_DATA_BYTES: usize = 64 * 1024;
pub const MAX_VENDOR_DATA_BYTES: usize = 64 * 1024;
const MANIFEST_NAME: &str = "o3k-ownership.json";
const MANAGED_BY: &str = "o3k-config-drive";
const ISO_MANIFEST_SUFFIX: &str = ".o3k-iso-ownership.json";
const ISO_MANAGED_BY: &str = "o3k-config-drive-iso";
const ISO_VOLUME_ID: &str = "config-2";
const ISO_DATE: &str = "2020010100000000";
const ISO_PROGRAM: &str = "xorriso";

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

/// Injectable boundary for the external ISO builder.
pub trait IsoCommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<(), io::Error>;
}

struct SystemIsoCommandRunner;

impl IsoCommandRunner for SystemIsoCommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<(), io::Error> {
        let status = Command::new(program).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{ISO_PROGRAM} exited unsuccessfully"
            )))
        }
    }
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

#[derive(Debug, Deserialize, Serialize)]
struct OwnershipManifest {
    schema_version: u32,
    managed_by: String,
    instance_id: String,
    fingerprint_sha256: String,
}

#[derive(Debug, Clone)]
pub struct ConfigDriveStore {
    root: PathBuf,
}

impl ConfigDriveStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ConfigDriveError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(ConfigDriveError::Storage)?;
        Ok(Self { root })
    }

    pub fn generate(
        &self,
        input: &ConfigDriveInput,
    ) -> Result<ConfigDriveResult, ConfigDriveError> {
        generate_at(&self.root, input)
    }

    pub fn cleanup(&self, instance_id: &str) -> Result<(), ConfigDriveError> {
        if !valid_instance_id(instance_id) {
            return Err(ConfigDriveError::InvalidInput);
        }
        cleanup(&self.root.join(instance_id))
    }

    pub fn materialize_iso(
        &self,
        source_directory: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
    ) -> Result<ConfigDriveIsoResult, ConfigDriveError> {
        materialize_iso_with_runner(source_directory, output_path, &SystemIsoCommandRunner)
    }
}

pub fn generate(
    root: impl AsRef<Path>,
    input: &ConfigDriveInput,
) -> Result<ConfigDriveResult, ConfigDriveError> {
    ConfigDriveStore::open(root.as_ref())?.generate(input)
}

fn generate_at(
    root: &Path,
    input: &ConfigDriveInput,
) -> Result<ConfigDriveResult, ConfigDriveError> {
    validate(input)?;
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
    let fingerprint_sha256 = fingerprint(
        &meta_data,
        &input.user_data,
        &network_data,
        input.vendor_data.as_deref(),
    );
    let directory = root.join(&input.instance_id);
    let temporary = root.join(format!(".{}-tmp-{}", input.instance_id, Uuid::now_v7()));
    fs::create_dir_all(&temporary).map_err(ConfigDriveError::Storage)?;
    let preparation = (|| {
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
        let manifest = OwnershipManifest {
            schema_version: 1,
            managed_by: MANAGED_BY.to_owned(),
            instance_id: input.instance_id.clone(),
            fingerprint_sha256: fingerprint_sha256.clone(),
        };
        let manifest =
            serde_json::to_vec_pretty(&manifest).map_err(ConfigDriveError::Serialization)?;
        write(&temporary.join(MANIFEST_NAME), &manifest)
    })();
    if let Err(error) = preparation {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    let backup = root.join(format!(".{}-old-{}", input.instance_id, Uuid::now_v7()));
    let had_previous = directory.exists() || directory.is_symlink();
    if had_previous {
        if let Err(error) = validate_owned_directory(&directory, &input.instance_id) {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
        fs::rename(&directory, &backup).map_err(|error| {
            let _ = fs::remove_dir_all(&temporary);
            ConfigDriveError::Storage(error)
        })?;
    }
    if let Err(error) = fs::rename(&temporary, &directory) {
        let _ = fs::remove_dir_all(&temporary);
        if had_previous {
            let _ = fs::rename(&backup, &directory);
        }
        return Err(ConfigDriveError::Storage(error));
    }
    if had_previous {
        fs::remove_dir_all(&backup).map_err(ConfigDriveError::Storage)?;
    }
    Ok(ConfigDriveResult {
        directory,
        fingerprint_sha256,
    })
}

pub fn cleanup(path: &Path) -> Result<(), ConfigDriveError> {
    let Some(instance_id) = path.file_name().and_then(|value| value.to_str()) else {
        return Err(ConfigDriveError::InvalidInput);
    };
    if instance_id.starts_with('.') || path.is_symlink() {
        return Err(ConfigDriveError::InvalidInput);
    }
    if !path.exists() {
        return Ok(());
    }
    validate_owned_directory(path, instance_id)?;
    fs::remove_dir_all(path).map_err(ConfigDriveError::Storage)?;
    Ok(())
}

/// Build a deterministic ISO from an O3K-owned config-drive directory.
///
/// The default runner invokes `xorriso`; callers running on the protected
/// real host must install that executable. The output and its ownership
/// manifest are published atomically, and an already verified matching pair
/// is returned without invoking the tool again.
pub fn materialize_iso(
    source_directory: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
) -> Result<ConfigDriveIsoResult, ConfigDriveError> {
    materialize_iso_with_runner(source_directory, output_path, &SystemIsoCommandRunner)
}

pub fn materialize_iso_with_runner(
    source_directory: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    runner: &dyn IsoCommandRunner,
) -> Result<ConfigDriveIsoResult, ConfigDriveError> {
    let source_directory = source_directory.as_ref();
    let output_path = output_path.as_ref();
    let instance_id = source_directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ConfigDriveError::InvalidInput)?;
    if !valid_instance_id(instance_id) || output_path.file_name().is_none() {
        return Err(ConfigDriveError::InvalidInput);
    }
    validate_owned_directory(source_directory, instance_id)?;
    let source_fingerprint = read_directory_fingerprint(source_directory)?;
    validate_output_location(source_directory, output_path)?;

    let manifest_path = iso_manifest_path(output_path)?;
    let output_name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ConfigDriveError::InvalidInput)?;
    let had_previous = output_path.exists() || output_path.is_symlink() || manifest_path.exists();
    if output_path.exists() || output_path.is_symlink() || manifest_path.exists() {
        if !output_path.exists() || manifest_path.is_symlink() {
            return Err(ConfigDriveError::UnownedPath);
        }
        let (manifest, fingerprint_sha256) =
            inspect_owned_iso(output_path, &manifest_path, instance_id, output_name)?;
        if manifest.source_fingerprint_sha256 == source_fingerprint {
            return Ok(ConfigDriveIsoResult {
                path: output_path.to_owned(),
                fingerprint_sha256,
                source_fingerprint_sha256: source_fingerprint,
            });
        }
    }

    let parent = output_path.parent().ok_or(ConfigDriveError::InvalidInput)?;
    let token = Uuid::now_v7();
    let temporary = parent.join(format!(".{}.iso-tmp-{}", instance_id, token));
    let temporary_manifest = parent.join(format!(".{}.iso-manifest-tmp-{}", instance_id, token));
    let preparation = (|| {
        let args = xorriso_args(&temporary, source_directory);
        runner
            .run(OsStr::new(ISO_PROGRAM), &args)
            .map_err(ConfigDriveError::ToolFailed)?;
        let artifact_fingerprint = verify_regular_file_digest(&temporary)?;
        let manifest = IsoOwnershipManifest {
            schema_version: 1,
            managed_by: ISO_MANAGED_BY.to_owned(),
            instance_id: instance_id.to_owned(),
            source_fingerprint_sha256: source_fingerprint.clone(),
            artifact_fingerprint_sha256: artifact_fingerprint.clone(),
            output_name: output_path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or(ConfigDriveError::InvalidInput)?
                .to_owned(),
        };
        let bytes =
            serde_json::to_vec_pretty(&manifest).map_err(ConfigDriveError::Serialization)?;
        fs::write(&temporary_manifest, bytes).map_err(ConfigDriveError::Storage)?;
        let verified = validate_owned_iso(
            &temporary,
            &temporary_manifest,
            instance_id,
            &source_fingerprint,
            output_name,
        )?;
        if verified.fingerprint_sha256 != artifact_fingerprint {
            return Err(ConfigDriveError::InvalidIsoOutput);
        }
        Ok(verified)
    })();
    if let Err(error) = preparation {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&temporary_manifest);
        return Err(error);
    }

    let backup_output = parent.join(format!(".{}.iso-old-{}", instance_id, token));
    let backup_manifest = parent.join(format!(".{}.iso-manifest-old-{}", instance_id, token));
    if had_previous {
        if let Err(error) = fs::rename(output_path, &backup_output) {
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&temporary_manifest);
            return Err(ConfigDriveError::Storage(error));
        }
        if let Err(error) = fs::rename(&manifest_path, &backup_manifest) {
            let _ = fs::rename(&backup_output, output_path);
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&temporary_manifest);
            return Err(ConfigDriveError::Storage(error));
        }
    }
    if let Err(error) = fs::rename(&temporary, output_path) {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&temporary_manifest);
        if had_previous {
            let _ = fs::rename(&backup_output, output_path);
            let _ = fs::rename(&backup_manifest, &manifest_path);
        }
        return Err(ConfigDriveError::Storage(error));
    }
    if let Err(error) = fs::rename(&temporary_manifest, &manifest_path) {
        let _ = fs::remove_file(output_path);
        let _ = fs::remove_file(&temporary_manifest);
        if had_previous {
            let _ = fs::rename(&backup_output, output_path);
            let _ = fs::rename(&backup_manifest, &manifest_path);
        }
        return Err(ConfigDriveError::Storage(error));
    }
    if had_previous {
        fs::remove_file(&backup_output).map_err(ConfigDriveError::Storage)?;
        fs::remove_file(&backup_manifest).map_err(ConfigDriveError::Storage)?;
    }
    validate_owned_iso(
        output_path,
        &manifest_path,
        instance_id,
        &source_fingerprint,
        output_name,
    )
}

#[derive(Debug, Deserialize, Serialize)]
struct IsoOwnershipManifest {
    schema_version: u32,
    managed_by: String,
    instance_id: String,
    source_fingerprint_sha256: String,
    artifact_fingerprint_sha256: String,
    output_name: String,
}

fn xorriso_args(output: &Path, source: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-as"),
        OsString::from("mkisofs"),
        OsString::from("-o"),
        output.as_os_str().to_owned(),
        OsString::from("-V"),
        OsString::from(ISO_VOLUME_ID),
        OsString::from("-iso-level"),
        OsString::from("3"),
        OsString::from("-r"),
        OsString::from("-J"),
        OsString::from("-joliet-long"),
        OsString::from("-volume_date"),
        OsString::from(format!("all_file_dates={ISO_DATE}")),
        OsString::from("-volume_date"),
        OsString::from(format!("uuid={ISO_DATE}")),
        source.as_os_str().to_owned(),
    ]
}

fn validate_output_location(source: &Path, output: &Path) -> Result<(), ConfigDriveError> {
    let source_parent = source.parent().ok_or(ConfigDriveError::InvalidInput)?;
    let output_parent = output.parent().ok_or(ConfigDriveError::InvalidInput)?;
    let source_root = fs::canonicalize(source_parent).map_err(|_| ConfigDriveError::UnownedPath)?;
    let output_root = fs::canonicalize(output_parent).map_err(|_| ConfigDriveError::UnownedPath)?;
    if source_root != output_root || output.is_symlink() {
        return Err(ConfigDriveError::UnownedPath);
    }
    Ok(())
}

fn iso_manifest_path(output: &Path) -> Result<PathBuf, ConfigDriveError> {
    let name = output
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ConfigDriveError::InvalidInput)?;
    Ok(output.with_file_name(format!("{name}{ISO_MANIFEST_SUFFIX}")))
}

fn read_directory_fingerprint(path: &Path) -> Result<String, ConfigDriveError> {
    let manifest: OwnershipManifest =
        serde_json::from_slice(&read_owned_file(&path.join(MANIFEST_NAME))?)
            .map_err(|_| ConfigDriveError::UnownedPath)?;
    Ok(manifest.fingerprint_sha256)
}

fn verify_regular_file_digest(path: &Path) -> Result<String, ConfigDriveError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ConfigDriveError::InvalidIsoOutput)?;
    if !metadata.is_file() {
        return Err(ConfigDriveError::InvalidIsoOutput);
    }
    let bytes = fs::read(path).map_err(|_| ConfigDriveError::InvalidIsoOutput)?;
    Ok(digest_hex(&bytes))
}

fn digest_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(&mut result, "{byte:02x}");
    }
    result
}

fn validate_owned_iso(
    output: &Path,
    manifest_path: &Path,
    instance_id: &str,
    source_fingerprint: &str,
    expected_output_name: &str,
) -> Result<ConfigDriveIsoResult, ConfigDriveError> {
    let (manifest, fingerprint_sha256) =
        inspect_owned_iso(output, manifest_path, instance_id, expected_output_name)?;
    if manifest.source_fingerprint_sha256 != source_fingerprint {
        return Err(ConfigDriveError::UnownedPath);
    }
    Ok(ConfigDriveIsoResult {
        path: output.to_owned(),
        fingerprint_sha256,
        source_fingerprint_sha256: source_fingerprint.to_owned(),
    })
}

fn inspect_owned_iso(
    output: &Path,
    manifest_path: &Path,
    instance_id: &str,
    expected_output_name: &str,
) -> Result<(IsoOwnershipManifest, String), ConfigDriveError> {
    if output.is_symlink() || manifest_path.is_symlink() {
        return Err(ConfigDriveError::UnownedPath);
    }
    let manifest: IsoOwnershipManifest = serde_json::from_slice(&read_owned_file(manifest_path)?)
        .map_err(|_| ConfigDriveError::UnownedPath)?;
    if manifest.schema_version != 1
        || manifest.managed_by != ISO_MANAGED_BY
        || manifest.instance_id != instance_id
        || manifest.output_name != expected_output_name
    {
        return Err(ConfigDriveError::UnownedPath);
    }
    let fingerprint_sha256 = verify_regular_file_digest(output)?;
    if fingerprint_sha256 != manifest.artifact_fingerprint_sha256 {
        return Err(ConfigDriveError::InvalidIsoOutput);
    }
    Ok((manifest, fingerprint_sha256))
}

fn validate_owned_directory(path: &Path, instance_id: &str) -> Result<(), ConfigDriveError> {
    if path.is_symlink()
        || !fs::symlink_metadata(path)
            .map_err(|_| ConfigDriveError::UnownedPath)?
            .is_dir()
    {
        return Err(ConfigDriveError::UnownedPath);
    }
    let manifest_path = path.join(MANIFEST_NAME);
    let manifest: OwnershipManifest = serde_json::from_slice(&read_owned_file(&manifest_path)?)
        .map_err(|_| ConfigDriveError::UnownedPath)?;
    if manifest.schema_version != 1
        || manifest.managed_by != MANAGED_BY
        || manifest.instance_id != instance_id
    {
        return Err(ConfigDriveError::UnownedPath);
    }
    require_owned_directory(&path.join("openstack"))?;
    let latest = path.join("openstack/latest");
    require_owned_directory(&latest)?;
    let meta_data = read_owned_file(&latest.join("meta_data.json"))?;
    let network_data = read_owned_file(&latest.join("network_data.json"))?;
    let user_data = read_owned_file(&latest.join("user_data"))?;
    let vendor_path = latest.join("vendor_data.json");
    let vendor_data = if vendor_path.exists() {
        Some(read_owned_file(&vendor_path)?)
    } else {
        None
    };
    let actual_fingerprint = fingerprint(
        &meta_data,
        &user_data,
        &network_data,
        vendor_data.as_deref(),
    );
    if manifest.fingerprint_sha256 != actual_fingerprint {
        return Err(ConfigDriveError::UnownedPath);
    }
    reject_unexpected_entries(path, &[MANIFEST_NAME, "openstack"])?;
    reject_unexpected_entries(&path.join("openstack"), &["latest"])?;
    let mut expected = vec!["meta_data.json", "network_data.json", "user_data"];
    if vendor_data.is_some() {
        expected.push("vendor_data.json");
    }
    reject_unexpected_entries(&latest, &expected)?;
    Ok(())
}

fn read_owned_file(path: &Path) -> Result<Vec<u8>, ConfigDriveError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ConfigDriveError::UnownedPath)?;
    if !metadata.is_file() {
        return Err(ConfigDriveError::UnownedPath);
    }
    fs::read(path).map_err(|_| ConfigDriveError::UnownedPath)
}

fn require_owned_directory(path: &Path) -> Result<(), ConfigDriveError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ConfigDriveError::UnownedPath)?;
    if !metadata.is_dir() {
        return Err(ConfigDriveError::UnownedPath);
    }
    Ok(())
}

fn reject_unexpected_entries(path: &Path, expected: &[&str]) -> Result<(), ConfigDriveError> {
    for entry in fs::read_dir(path).map_err(|_| ConfigDriveError::UnownedPath)? {
        let entry = entry.map_err(|_| ConfigDriveError::UnownedPath)?;
        let name = entry.file_name();
        if !expected.iter().any(|value| name == *value) {
            return Err(ConfigDriveError::UnownedPath);
        }
    }
    Ok(())
}

fn fingerprint(
    meta_data: &[u8],
    user_data: &[u8],
    network_data: &[u8],
    vendor_data: Option<&[u8]>,
) -> String {
    use std::fmt::Write as _;

    let mut digest = Sha256::new();
    digest.update(meta_data);
    digest.update(user_data);
    digest.update(network_data);
    if let Some(vendor) = vendor_data {
        digest.update(vendor);
    }
    let mut result = String::with_capacity(64);
    for byte in digest.finalize() {
        let _ = write!(&mut result, "{byte:02x}");
    }
    result
}

fn valid_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            == Path::new(value)
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
        && value.len() <= 128
        && !value.starts_with('.')
}

fn validate(input: &ConfigDriveInput) -> Result<(), ConfigDriveError> {
    if !valid_instance_id(&input.instance_id)
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
    let network_data =
        serde_json::to_vec(&input.network_data).map_err(ConfigDriveError::Serialization)?;
    if network_data.len() > MAX_NETWORK_DATA_BYTES {
        return Err(ConfigDriveError::NetworkDataTooLarge);
    }
    if input
        .vendor_data
        .as_ref()
        .is_some_and(|data| data.len() > MAX_VENDOR_DATA_BYTES)
    {
        return Err(ConfigDriveError::VendorDataTooLarge);
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
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeRunner {
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
        output: Vec<u8>,
        failure: bool,
    }

    impl FakeRunner {
        fn successful(output: &[u8]) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                output: output.to_vec(),
                failure: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                output: Vec::new(),
                failure: true,
            }
        }
    }

    impl IsoCommandRunner for FakeRunner {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<(), io::Error> {
            assert_eq!(program, OsStr::new(ISO_PROGRAM));
            self.calls
                .lock()
                .map_err(|_| io::Error::other("test mutex poisoned"))?
                .push(args.to_vec());
            if self.failure {
                return Err(io::Error::other("synthetic xorriso failure"));
            }
            let output = args
                .windows(2)
                .find(|pair| pair[0] == OsStr::new("-o"))
                .map(|pair| PathBuf::from(&pair[1]))
                .ok_or_else(|| io::Error::other("missing output argument"))?;
            fs::write(output, &self.output)
        }
    }

    impl FakeRunner {
        fn calls(&self) -> Vec<Vec<OsString>> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }
    }

    fn test_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::now_v7()))
    }

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
        let store = ConfigDriveStore::open(&root)?;
        let first = store.generate(&input())?;
        let bytes = fs::read(first.directory.join("openstack/latest/user_data"))
            .map_err(ConfigDriveError::Storage)?;
        assert_eq!(bytes, input().user_data);
        let second = store.generate(&input())?;
        assert_eq!(first.fingerprint_sha256, second.fingerprint_sha256);
        store.cleanup("instance-1")?;
        store.cleanup("instance-1")?;
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
        let mut value = input();
        value.network_data =
            BTreeMap::from([("payload".to_owned(), "x".repeat(MAX_NETWORK_DATA_BYTES))]);
        assert!(matches!(
            generate(std::env::temp_dir(), &value),
            Err(ConfigDriveError::NetworkDataTooLarge)
        ));
        let mut value = input();
        value.vendor_data = Some(vec![b'x'; MAX_VENDOR_DATA_BYTES + 1]);
        assert!(matches!(
            generate(std::env::temp_dir(), &value),
            Err(ConfigDriveError::VendorDataTooLarge)
        ));
    }

    #[test]
    fn cleanup_and_replacement_fail_closed_for_unowned_paths() -> Result<(), ConfigDriveError> {
        let root = std::env::temp_dir().join(format!("o3k-drive-owner-{}", Uuid::now_v7()));
        let unowned = root.join("instance-1");
        fs::create_dir_all(&unowned).map_err(ConfigDriveError::Storage)?;
        fs::write(unowned.join("keep"), b"do not remove").map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            cleanup(&unowned),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(unowned.exists());

        let store = ConfigDriveStore::open(&root)?;
        assert!(matches!(
            store.generate(&input()),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(unowned.join("keep").exists());
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn failed_generation_removes_unpublished_temporary_directory() -> Result<(), ConfigDriveError> {
        let root = std::env::temp_dir().join(format!("o3k-drive-temp-{}", Uuid::now_v7()));
        let unowned = root.join("instance-1");
        fs::create_dir_all(&unowned).map_err(ConfigDriveError::Storage)?;
        fs::write(unowned.join("keep"), b"do not remove").map_err(ConfigDriveError::Storage)?;

        let store = ConfigDriveStore::open(&root)?;
        assert!(matches!(
            store.generate(&input()),
            Err(ConfigDriveError::UnownedPath)
        ));
        let temporary_count = fs::read_dir(&root)
            .map_err(ConfigDriveError::Storage)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".instance-1-tmp-"))
            })
            .count();
        assert_eq!(temporary_count, 0);
        assert!(unowned.join("keep").exists());

        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn altered_or_extra_published_content_is_not_owned() -> Result<(), ConfigDriveError> {
        let root = std::env::temp_dir().join(format!("o3k-drive-integrity-{}", Uuid::now_v7()));
        let store = ConfigDriveStore::open(&root)?;
        let result = store.generate(&input())?;
        fs::write(
            result.directory.join("openstack/latest/user_data"),
            b"tampered",
        )
        .map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(result.directory.exists());
        fs::remove_file(result.directory.join("openstack/latest/user_data"))
            .map_err(ConfigDriveError::Storage)?;
        fs::write(
            result.directory.join("openstack/latest/user_data"),
            b"#cloud-config\nhostname: vm-1\n",
        )
        .map_err(ConfigDriveError::Storage)?;
        fs::write(result.directory.join("unexpected"), b"foreign")
            .map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(result.directory.exists());
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn iso_uses_fixed_arguments_and_is_restart_idempotent() -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-iso-args");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        let runner = FakeRunner::successful(b"deterministic-iso");
        let first = materialize_iso_with_runner(&source, &output, &runner)?;
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], OsStr::new("-as"));
        assert_eq!(calls[0][1], OsStr::new("mkisofs"));
        assert_eq!(calls[0][2], OsStr::new("-o"));
        assert_eq!(calls[0][4], OsStr::new("-V"));
        assert_eq!(calls[0][5], OsStr::new(ISO_VOLUME_ID));
        assert!(calls[0].windows(2).any(|pair| {
            pair[0] == OsStr::new("-volume_date")
                && pair[1] == OsString::from(format!("all_file_dates={ISO_DATE}"))
        }));
        assert!(calls[0].windows(2).any(|pair| {
            pair[0] == OsStr::new("-volume_date")
                && pair[1] == OsString::from(format!("uuid={ISO_DATE}"))
        }));
        drop(calls);

        let reopened = ConfigDriveStore::open(&root)?;
        let second = reopened.materialize_iso(&source, &output)?;
        assert_eq!(first, second);
        let third = materialize_iso_with_runner(&source, &output, &runner)?;
        assert_eq!(first, third);
        assert_eq!(runner.calls().len(), 1);
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn iso_tool_failure_removes_temporary_files() -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-iso-failure");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        let runner = FakeRunner::failing();
        assert!(matches!(
            materialize_iso_with_runner(&source, &output, &runner),
            Err(ConfigDriveError::ToolFailed(_))
        ));
        let leftovers = fs::read_dir(&root)
            .map_err(ConfigDriveError::Storage)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("iso-tmp"))
            .count();
        assert_eq!(leftovers, 0);
        assert!(!output.exists());
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn iso_tampering_and_unowned_output_fail_closed() -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-iso-owner");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        let runner = FakeRunner::successful(b"deterministic-iso");
        materialize_iso_with_runner(&source, &output, &runner)?;
        fs::write(&output, b"tampered").map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            materialize_iso_with_runner(&source, &output, &runner),
            Err(ConfigDriveError::InvalidIsoOutput)
        ));
        fs::write(&output, b"deterministic-iso").map_err(ConfigDriveError::Storage)?;
        fs::remove_file(iso_manifest_path(&output)?).map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            materialize_iso_with_runner(&source, &output, &runner),
            Err(ConfigDriveError::UnownedPath)
        ));

        fs::remove_file(&output).map_err(ConfigDriveError::Storage)?;
        fs::write(&output, b"foreign").map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            materialize_iso_with_runner(&source, &output, &runner),
            Err(ConfigDriveError::UnownedPath)
        ));
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }
}
