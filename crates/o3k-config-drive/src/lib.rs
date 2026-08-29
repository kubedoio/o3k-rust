//! Deterministic, filesystem-backed OpenStack config-drive content.
//!
//! ISO materialization uses an external ISO builder. The runtime prefers
//! `xorriso` and falls back to the mkisofs-compatible `genisoimage` or
//! `mkisofs` executables available on the host.

pub mod types;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub use types::{
    ConfigDriveArtifact, ConfigDriveError, ConfigDriveInput, ConfigDriveIsoResult,
    ConfigDriveResult,
};

pub const MAX_USER_DATA_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_NETWORK_DATA_BYTES: usize = 64 * 1024;
pub const MAX_VENDOR_DATA_BYTES: usize = 64 * 1024;
pub const MAX_SSH_PUBLIC_KEY_BYTES: usize = 16 * 1024;
pub const MAX_INPUT_BYTES: usize = 128 * 1024;
pub const MAX_ISO_BYTES: usize = 64 * 1024 * 1024;
const MANIFEST_NAME: &str = "o3k-ownership.json";
const MANAGED_BY: &str = "o3k-config-drive";
const ISO_MANIFEST_SUFFIX: &str = ".o3k-iso-ownership.json";
const ISO_MANAGED_BY: &str = "o3k-config-drive-iso";
const ISO_VOLUME_ID: &str = "config-2";
const ISO_DATE: &str = "2020010100000000";
const ISO_PROGRAM: &str = "xorriso";
const ISO_FALLBACK_PROGRAMS: &[&str] = &["genisoimage", "mkisofs"];

/// Injectable boundary for the external ISO builder.
pub trait IsoCommandRunner {
    fn program(&self) -> OsString {
        OsString::from(ISO_PROGRAM)
    }

    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<(), io::Error>;
}

struct SystemIsoCommandRunner;

impl IsoCommandRunner for SystemIsoCommandRunner {
    fn program(&self) -> OsString {
        iso_program()
    }

    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<(), io::Error> {
        let status = Command::new(program).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} exited unsuccessfully",
                program.to_string_lossy()
            )))
        }
    }
}

fn iso_program() -> OsString {
    let candidates = std::iter::once(ISO_PROGRAM).chain(ISO_FALLBACK_PROGRAMS.iter().copied());
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .flat_map(|directory| {
            candidates
                .clone()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|path| path.is_file())
        .and_then(|path| path.file_name().map(OsStr::to_owned))
        .unwrap_or_else(|| OsString::from(ISO_PROGRAM))
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
        #[cfg(unix)]
        let root_created;
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                #[cfg(unix)]
                {
                    root_created = false;
                }
            }
            Ok(_) => return Err(ConfigDriveError::UnownedPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&root).map_err(ConfigDriveError::Storage)?;
                #[cfg(unix)]
                {
                    root_created = true;
                }
            }
            Err(error) => return Err(ConfigDriveError::Storage(error)),
        }
        // Restrict the root only when O3K created it. A pre-existing root
        // (for example /tmp in tests, or a state root already provisioned by
        // the installer) may be a shared system directory or owned by another
        // account; chmod'ing it would either fail with EPERM or change
        // foreign state. The generated per-instance content below is still
        // restricted to 0600/0700 regardless of the root.
        #[cfg(unix)]
        if root_created {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(ConfigDriveError::Storage)?;
        }
        reap_abandoned_publication_artifacts(&root)?;
        Ok(Self { root })
    }

    pub fn generate(
        &self,
        input: &ConfigDriveInput,
    ) -> Result<ConfigDriveResult, ConfigDriveError> {
        generate_at(&self.root, input)
    }

    /// Removes the per-instance ownership unit: the generated directory
    /// (`<instance_id>/`) and the published ISO transfer-source pair
    /// (`<instance_id>.iso` plus its ownership manifest), each validated as
    /// O3K-owned before anything is removed. Idempotent: an already absent
    /// unit returns `Ok(())`, and any unowned or tampered part fails closed
    /// without deleting anything.
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

    /// Reads an ISO only after revalidating its O3K ownership manifest and
    /// whole-file digest. This is the byte-oriented boundary used when an ISO
    /// must cross into an authenticated compute agent.
    pub fn read_verified_iso(
        &self,
        result: &ConfigDriveIsoResult,
    ) -> Result<Vec<u8>, ConfigDriveError> {
        self.read_verified_iso_for_instance(result, None)
    }

    fn read_verified_iso_for_instance(
        &self,
        result: &ConfigDriveIsoResult,
        expected_instance_id: Option<&str>,
    ) -> Result<Vec<u8>, ConfigDriveError> {
        let root = fs::canonicalize(&self.root).map_err(|_| ConfigDriveError::UnownedPath)?;
        let manifest_path = iso_manifest_path(&result.path)?;
        let output_metadata =
            fs::symlink_metadata(&result.path).map_err(|_| ConfigDriveError::UnownedPath)?;
        if !output_metadata.file_type().is_file() || output_metadata.len() > MAX_ISO_BYTES as u64 {
            return Err(ConfigDriveError::InvalidIsoOutput);
        }
        let canonical_output =
            fs::canonicalize(&result.path).map_err(|_| ConfigDriveError::UnownedPath)?;
        if !canonical_output.starts_with(&root) {
            return Err(ConfigDriveError::UnownedPath);
        }
        let manifest: IsoOwnershipManifest =
            serde_json::from_slice(&read_owned_file(&manifest_path)?)
                .map_err(ConfigDriveError::CorruptManifest)?;
        let output_name = result
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(ConfigDriveError::InvalidInput)?;
        if manifest.schema_version != 1
            || manifest.managed_by != ISO_MANAGED_BY
            || manifest.output_name != output_name
            || expected_instance_id.is_some_and(|id| manifest.instance_id != id)
            || manifest.source_fingerprint_sha256 != result.source_fingerprint_sha256
        {
            return Err(ConfigDriveError::UnownedPath);
        }
        let content = read_bounded_regular_file(&result.path, output_metadata.len())?;
        if sha256(&content) != manifest.artifact_fingerprint_sha256
            || sha256(&content) != result.fingerprint_sha256
        {
            return Err(ConfigDriveError::InvalidIsoOutput);
        }
        Ok(content)
    }

    /// Reads a verified ISO only when its output and ownership manifest are
    /// beneath this store's root and the bounded artifact contract holds.
    pub fn read_verified_artifact(
        &self,
        result: &ConfigDriveIsoResult,
        expected_instance_id: &str,
    ) -> Result<ConfigDriveArtifact, ConfigDriveError> {
        if !valid_instance_id(expected_instance_id) {
            return Err(ConfigDriveError::InvalidInput);
        }
        let content = self.read_verified_iso_for_instance(result, Some(expected_instance_id))?;
        Ok(ConfigDriveArtifact {
            format: "iso".to_owned(),
            sha256: result.fingerprint_sha256.clone(),
            size: content.len() as u64,
            content,
        })
    }
}

fn read_bounded_regular_file(path: &Path, expected_size: u64) -> Result<Vec<u8>, ConfigDriveError> {
    let file = fs::File::open(path).map_err(ConfigDriveError::Storage)?;
    let opened_size = file.metadata().map_err(ConfigDriveError::Storage)?.len();
    if opened_size != expected_size || opened_size > MAX_ISO_BYTES as u64 {
        return Err(ConfigDriveError::InvalidIsoOutput);
    }
    let mut content = Vec::with_capacity(opened_size as usize);
    file.take(MAX_ISO_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .map_err(ConfigDriveError::Storage)?;
    if content.len() as u64 != expected_size {
        return Err(ConfigDriveError::InvalidIsoOutput);
    }
    Ok(content)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn reap_abandoned_publication_artifacts(root: &Path) -> Result<(), ConfigDriveError> {
    let entries = fs::read_dir(root).map_err(ConfigDriveError::Storage)?;
    for entry in entries {
        let entry = entry.map_err(ConfigDriveError::Storage)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_publication_temporary_name(name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(ConfigDriveError::Storage)?;
        if metadata.file_type().is_symlink() {
            // A matching symlink is not O3K-owned residue. Preserve it and
            // never follow it during restart cleanup.
            continue;
        }
        if metadata.is_dir() {
            fs::remove_dir_all(path).map_err(ConfigDriveError::Storage)?;
        } else if metadata.is_file() {
            fs::remove_file(path).map_err(ConfigDriveError::Storage)?;
        }
    }
    Ok(())
}

fn is_publication_temporary_name(name: &str) -> bool {
    let Some(value) = name.strip_prefix('.') else {
        return false;
    };
    let Some((prefix, suffix)) = value
        .split_once("-tmp-")
        .or_else(|| value.split_once("-old-"))
    else {
        return false;
    };
    let instance = prefix
        .strip_suffix(".iso-manifest")
        .or_else(|| prefix.strip_suffix(".iso"))
        .unwrap_or(prefix);
    Uuid::parse_str(instance).is_ok() && Uuid::parse_str(suffix).is_ok()
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
    #[cfg(unix)]
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))
        .map_err(ConfigDriveError::Storage)?;
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

/// Removes the per-instance ownership unit at `path` (a directory named by
/// the instance id): the generated directory plus the published ISO
/// transfer-source pair (`<instance_id>.iso` and its ownership manifest) in
/// the same parent. Every part is validated as O3K-owned before anything is
/// removed, so a symlink anywhere, an unowned or corrupt ISO ownership
/// manifest, an ISO present without its manifest, an output-name mismatch, or
/// a non-regular oversized ISO fails closed and deletes nothing. Idempotent:
/// an already absent unit returns `Ok(())`, and partial residue (for example
/// a lone manifest whose ISO is already gone) is removed only after it
/// validates as owned.
pub fn cleanup(path: &Path) -> Result<(), ConfigDriveError> {
    let Some(instance_id) = path.file_name().and_then(|value| value.to_str()) else {
        return Err(ConfigDriveError::InvalidInput);
    };
    if instance_id.starts_with('.') || path.is_symlink() {
        return Err(ConfigDriveError::InvalidInput);
    }
    let directory_present = path.exists();
    if directory_present {
        validate_owned_directory(path, instance_id)?;
    }
    let iso = path.with_file_name(format!("{instance_id}.iso"));
    let manifest_path = iso_manifest_path(&iso)?;
    let iso_present = iso.exists() || iso.is_symlink();
    let manifest_present = manifest_path.exists() || manifest_path.is_symlink();
    if iso_present {
        validate_owned_iso_pair(&iso, &manifest_path, instance_id)?;
    } else if manifest_present {
        validate_owned_iso_manifest(&manifest_path, instance_id)?;
    }
    if directory_present {
        fs::remove_dir_all(path).map_err(ConfigDriveError::Storage)?;
    }
    if iso_present {
        fs::remove_file(&iso).map_err(ConfigDriveError::Storage)?;
        fs::remove_file(&manifest_path).map_err(ConfigDriveError::Storage)?;
    } else if manifest_present {
        fs::remove_file(&manifest_path).map_err(ConfigDriveError::Storage)?;
    }
    Ok(())
}

/// Build a deterministic ISO from an O3K-owned config-drive directory.
///
/// The default runner selects an available ISO builder. The output and its
/// ownership manifest are published atomically, and an already verified
/// matching pair is returned without invoking the tool again.
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
        let program = runner.program();
        let args = iso_args(&program, &temporary, source_directory);
        runner
            .run(&program, &args)
            .map_err(ConfigDriveError::ToolFailed)?;
        #[cfg(unix)]
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(ConfigDriveError::Storage)?;
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
        #[cfg(unix)]
        fs::set_permissions(&temporary_manifest, fs::Permissions::from_mode(0o600))
            .map_err(ConfigDriveError::Storage)?;
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

fn iso_args(program: &OsStr, output: &Path, source: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-o"),
        output.as_os_str().to_owned(),
        OsString::from("-V"),
        OsString::from(ISO_VOLUME_ID),
        OsString::from("-iso-level"),
        OsString::from("3"),
        OsString::from("-r"),
        OsString::from("-J"),
        OsString::from("-joliet-long"),
    ];
    if program == OsStr::new(ISO_PROGRAM) {
        args.splice(0..0, [OsString::from("-as"), OsString::from("mkisofs")]);
        // xorriso's mkisofs emulation accepts these deterministic date
        // options. Native `-volume_date` is rejected in this mode.
        args.extend([
            OsString::from(format!("--modification-date={ISO_DATE}")),
            OsString::from("--set_all_file_dates"),
            OsString::from(ISO_DATE),
        ]);
    }
    args.push(source.as_os_str().to_owned());
    args
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

/// Validates the ISO ownership manifest before the sibling ISO (or a lone
/// manifest) may be removed. Fail-closed: a symlink, a missing or corrupt
/// manifest, or a manifest that does not name this instance and the canonical
/// `<instance_id>.iso` output yields `UnownedPath`.
fn validate_owned_iso_manifest(
    manifest_path: &Path,
    instance_id: &str,
) -> Result<(), ConfigDriveError> {
    if manifest_path.is_symlink() {
        return Err(ConfigDriveError::UnownedPath);
    }
    let expected_output_name = format!("{instance_id}.iso");
    let manifest: IsoOwnershipManifest = serde_json::from_slice(&read_owned_file(manifest_path)?)
        .map_err(|_| ConfigDriveError::UnownedPath)?;
    if manifest.schema_version != 1
        || manifest.managed_by != ISO_MANAGED_BY
        || manifest.instance_id != instance_id
        || manifest.output_name != expected_output_name
    {
        return Err(ConfigDriveError::UnownedPath);
    }
    Ok(())
}

/// Validates the full ISO ownership pair before removal: the ownership
/// manifest must be present, regular, and name this instance and this exact
/// output, and the ISO itself must be a regular file within the bounded
/// maximum size. Fail-closed: nothing is removed on any violation.
fn validate_owned_iso_pair(
    iso: &Path,
    manifest_path: &Path,
    instance_id: &str,
) -> Result<(), ConfigDriveError> {
    if iso.is_symlink() || manifest_path.is_symlink() {
        return Err(ConfigDriveError::UnownedPath);
    }
    let expected_output_name = iso
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ConfigDriveError::InvalidInput)?;
    let manifest: IsoOwnershipManifest = serde_json::from_slice(&read_owned_file(manifest_path)?)
        .map_err(|_| ConfigDriveError::UnownedPath)?;
    if manifest.schema_version != 1
        || manifest.managed_by != ISO_MANAGED_BY
        || manifest.instance_id != instance_id
        || manifest.output_name != expected_output_name
    {
        return Err(ConfigDriveError::UnownedPath);
    }
    let metadata = fs::symlink_metadata(iso).map_err(|_| ConfigDriveError::UnownedPath)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_ISO_BYTES as u64 {
        return Err(ConfigDriveError::InvalidIsoOutput);
    }
    Ok(())
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

pub fn validate(input: &ConfigDriveInput) -> Result<(), ConfigDriveError> {
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
    validate_input_bounds(
        &input.ssh_public_key,
        &input.user_data,
        input.vendor_data.as_deref(),
    )?;
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

/// Validates the secret-bearing request fields before a compute operation is
/// persisted. This is intentionally independent of generated metadata and
/// network data so the API can reject oversized input at admission time.
pub fn validate_input_bounds(
    ssh_public_key: &str,
    user_data: &[u8],
    vendor_data: Option<&[u8]>,
) -> Result<(), ConfigDriveError> {
    if user_data.len() > MAX_USER_DATA_BYTES {
        return Err(ConfigDriveError::UserDataTooLarge);
    }
    if ssh_public_key.len() > MAX_SSH_PUBLIC_KEY_BYTES {
        return Err(ConfigDriveError::InvalidInput);
    }
    if vendor_data.is_some_and(|data| data.len() > MAX_VENDOR_DATA_BYTES) {
        return Err(ConfigDriveError::VendorDataTooLarge);
    }
    let total = user_data
        .len()
        .saturating_add(vendor_data.map_or(0, <[u8]>::len))
        .saturating_add(ssh_public_key.len());
    if total > MAX_INPUT_BYTES {
        return Err(ConfigDriveError::InvalidInput);
    }
    Ok(())
}

fn write(path: &Path, content: &[u8]) -> Result<(), ConfigDriveError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigDriveError::Storage)?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(ConfigDriveError::Storage)?;
    }
    fs::write(path, content).map_err(ConfigDriveError::Storage)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(ConfigDriveError::Storage)?;
    Ok(())
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
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(first.directory.join("openstack/latest/user_data"))
                    .map_err(ConfigDriveError::Storage)?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&first.directory)
                    .map_err(ConfigDriveError::Storage)?
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
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
        assert!(matches!(
            validate_input_bounds(&"k".repeat(MAX_SSH_PUBLIC_KEY_BYTES + 1), &[], None),
            Err(ConfigDriveError::InvalidInput)
        ));
        assert!(matches!(
            validate_input_bounds(
                "ssh-ed25519 key",
                &vec![b'x'; MAX_USER_DATA_BYTES],
                Some(&vec![b'y'; MAX_VENDOR_DATA_BYTES])
            ),
            Err(ConfigDriveError::InvalidInput)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn open_rejects_symlinked_root() -> Result<(), ConfigDriveError> {
        use std::os::unix::fs::symlink;
        let parent = test_root("symlink-root");
        fs::create_dir_all(&parent).map_err(ConfigDriveError::Storage)?;
        let target = parent.join("target");
        fs::create_dir_all(&target).map_err(ConfigDriveError::Storage)?;
        let root = parent.join("root");
        symlink(&target, &root).map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            ConfigDriveStore::open(&root),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(
            target
                .read_dir()
                .map_err(ConfigDriveError::Storage)?
                .next()
                .is_none()
        );
        fs::remove_dir_all(parent).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn open_restricts_only_created_root_directories() -> Result<(), ConfigDriveError> {
        use std::os::unix::fs::PermissionsExt;

        let parent = test_root("root-restrict");
        let root = parent.join("config-drive");
        let store = ConfigDriveStore::open(&root)?;
        assert_eq!(
            fs::symlink_metadata(&root)
                .map_err(ConfigDriveError::Storage)?
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "a config-drive root created by open must be restricted to 0700"
        );
        drop(store);
        // Restrict the parent back to a shared-system-like mode so the second
        // open exercises the pre-existing-root branch.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o1777))
            .map_err(ConfigDriveError::Storage)?;
        let reopened = ConfigDriveStore::open(&root)?;
        drop(reopened);
        assert_eq!(
            fs::symlink_metadata(&root)
                .map_err(ConfigDriveError::Storage)?
                .permissions()
                .mode()
                & 0o1777,
            0o1777,
            "open must not chmod a pre-existing root directory"
        );
        fs::remove_dir_all(parent).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn restart_reaps_only_fenced_publication_residue() -> Result<(), ConfigDriveError> {
        use std::os::unix::fs::symlink;
        let parent = test_root("publication-restart-reap");
        fs::create_dir_all(&parent).map_err(ConfigDriveError::Storage)?;
        let instance = Uuid::now_v7();
        let stale_dir = parent.join(format!(".{instance}-tmp-{}", Uuid::now_v7()));
        fs::create_dir_all(&stale_dir).map_err(ConfigDriveError::Storage)?;
        fs::write(stale_dir.join("user_data"), b"secret residue")
            .map_err(ConfigDriveError::Storage)?;
        let stale_iso = parent.join(format!(".{instance}.iso-old-{}", Uuid::now_v7()));
        fs::write(&stale_iso, b"iso residue").map_err(ConfigDriveError::Storage)?;
        let foreign = parent.join(".foreign.tmp-user");
        fs::write(&foreign, b"keep").map_err(ConfigDriveError::Storage)?;
        let symlink_target = parent.join("foreign-target");
        fs::create_dir_all(&symlink_target).map_err(ConfigDriveError::Storage)?;
        let symlinked = parent.join(format!(".{instance}-tmp-{}", Uuid::now_v7()));
        symlink(&symlink_target, &symlinked).map_err(ConfigDriveError::Storage)?;

        let _store = ConfigDriveStore::open(&parent)?;
        assert!(!stale_dir.exists());
        assert!(!stale_iso.exists());
        assert!(foreign.exists());
        assert!(symlinked.is_symlink());
        assert!(symlink_target.exists());
        fs::remove_dir_all(parent).map_err(ConfigDriveError::Storage)?;
        Ok(())
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
        // An ISO present without its ownership manifest is foreign state even
        // when the instance directory itself is O3K-owned: cleanup must fail
        // closed and remove nothing.
        let mut iso_input = input();
        iso_input.instance_id = "instance-2".to_owned();
        let generated = store.generate(&iso_input)?;
        let foreign_iso = root.join("instance-2.iso");
        fs::write(&foreign_iso, b"foreign").map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            store.cleanup("instance-2"),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(generated.directory.exists());
        assert!(foreign_iso.exists());
        fs::remove_file(&foreign_iso).map_err(ConfigDriveError::Storage)?;

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
        assert_eq!(store.read_verified_iso(&first)?, b"deterministic-iso");
        let artifact = store.read_verified_artifact(&first, "instance-1")?;
        assert_eq!(artifact.format, "iso");
        assert_eq!(artifact.size, b"deterministic-iso".len() as u64);
        assert_eq!(artifact.content, b"deterministic-iso");
        assert_eq!(artifact.sha256, first.fingerprint_sha256);
        assert!(matches!(
            store.read_verified_artifact(&first, "instance-2"),
            Err(ConfigDriveError::UnownedPath)
        ));
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], OsStr::new("-as"));
        assert_eq!(calls[0][1], OsStr::new("mkisofs"));
        assert_eq!(calls[0][2], OsStr::new("-o"));
        assert_eq!(calls[0][4], OsStr::new("-V"));
        assert_eq!(calls[0][5], OsStr::new(ISO_VOLUME_ID));
        assert!(
            calls
                .iter()
                .flatten()
                .any(|arg| { arg == &OsString::from(format!("--modification-date={ISO_DATE}")) })
        );
        assert!(calls[0].windows(2).any(|pair| {
            pair[0] == OsStr::new("--set_all_file_dates") && pair[1] == OsStr::new(ISO_DATE)
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

    #[test]
    fn cleanup_removes_directory_and_published_iso_pair_for_owned_instance()
    -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-cleanup-iso");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        materialize_iso_with_runner(&source, &output, &FakeRunner::successful(b"iso-bytes"))?;
        let manifest_path = iso_manifest_path(&output)?;
        assert!(output.exists() && manifest_path.exists());

        store.cleanup("instance-1")?;
        assert!(!source.exists(), "the owned directory must be removed");
        assert!(!output.exists(), "the owned ISO must be removed");
        assert!(
            !manifest_path.exists(),
            "the owned ISO manifest must be removed"
        );

        // Idempotent when the whole ownership unit is already absent.
        store.cleanup("instance-1")?;
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn cleanup_rejects_iso_whose_ownership_manifest_does_not_match_the_instance()
    -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-cleanup-iso-foreign");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        materialize_iso_with_runner(&source, &output, &FakeRunner::successful(b"iso-bytes"))?;
        let manifest_path = iso_manifest_path(&output)?;
        let mut manifest: IsoOwnershipManifest =
            serde_json::from_slice(&fs::read(&manifest_path).map_err(ConfigDriveError::Storage)?)
                .map_err(ConfigDriveError::CorruptManifest)?;
        manifest.instance_id = "instance-2".to_owned();
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).map_err(ConfigDriveError::Serialization)?,
        )
        .map_err(ConfigDriveError::Storage)?;

        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));
        // Nothing of the ownership unit may be removed while any part of it
        // is unverified: neither the directory nor the ISO may be deleted.
        assert!(source.exists());
        assert!(output.exists());
        assert!(manifest_path.exists());
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn cleanup_fails_closed_for_iso_without_manifest_and_oversized_iso()
    -> Result<(), ConfigDriveError> {
        let root = test_root("o3k-drive-cleanup-iso-unowned");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let output = root.join("instance-1.iso");
        let manifest_path = iso_manifest_path(&output)?;

        // An ISO present without its ownership manifest is foreign state.
        fs::write(&output, b"foreign").map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(source.exists() && output.exists() && !manifest_path.exists());
        fs::remove_file(&output).map_err(ConfigDriveError::Storage)?;

        // An ISO larger than the bounded maximum is never removed even when
        // its ownership manifest matches the instance.
        materialize_iso_with_runner(&source, &output, &FakeRunner::successful(b"iso-bytes"))?;
        fs::write(&output, vec![b'x'; MAX_ISO_BYTES + 1]).map_err(ConfigDriveError::Storage)?;
        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::InvalidIsoOutput)
        ));
        assert!(source.exists() && output.exists() && manifest_path.exists());

        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_fails_closed_for_symlinked_iso() -> Result<(), ConfigDriveError> {
        use std::os::unix::fs::symlink;

        let root = test_root("o3k-drive-cleanup-iso-symlink");
        let store = ConfigDriveStore::open(&root)?;
        let source = store.generate(&input())?.directory;
        let outside = root.with_extension("outside");
        fs::write(&outside, b"outside").map_err(ConfigDriveError::Storage)?;
        let output = root.join("instance-1.iso");
        symlink(&outside, &output).map_err(ConfigDriveError::Storage)?;

        assert!(matches!(
            store.cleanup("instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));
        assert!(source.exists() && output.is_symlink());
        fs::remove_file(&output).map_err(ConfigDriveError::Storage)?;
        fs::remove_file(&outside).map_err(ConfigDriveError::Storage)?;
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }

    #[test]
    fn verified_artifact_rejects_foreign_root_and_oversized_output() -> Result<(), ConfigDriveError>
    {
        let root = test_root("o3k-drive-iso-artifact");
        let foreign_root = test_root("o3k-drive-iso-foreign");
        let store = ConfigDriveStore::open(&root)?;
        let foreign_store = ConfigDriveStore::open(&foreign_root)?;
        let source = foreign_store.generate(&input())?.directory;
        let output = foreign_root.join("instance-1.iso");
        let runner = FakeRunner::successful(b"foreign-iso");
        let foreign = materialize_iso_with_runner(&source, &output, &runner)?;
        assert!(matches!(
            store.read_verified_artifact(&foreign, "instance-1"),
            Err(ConfigDriveError::UnownedPath)
        ));

        let oversized_root = test_root("o3k-drive-iso-large");
        let oversized_store = ConfigDriveStore::open(&oversized_root)?;
        let oversized_source = oversized_store.generate(&input())?.directory;
        let oversized_output = oversized_root.join("instance-1.iso");
        let oversized = vec![b'x'; MAX_ISO_BYTES + 1];
        let oversized_result = materialize_iso_with_runner(
            &oversized_source,
            &oversized_output,
            &FakeRunner::successful(&oversized),
        )?;
        assert!(matches!(
            oversized_store.read_verified_artifact(&oversized_result, "instance-1"),
            Err(ConfigDriveError::InvalidIsoOutput)
        ));
        fs::remove_dir_all(root).map_err(ConfigDriveError::Storage)?;
        fs::remove_dir_all(foreign_root).map_err(ConfigDriveError::Storage)?;
        fs::remove_dir_all(oversized_root).map_err(ConfigDriveError::Storage)?;
        Ok(())
    }
}
