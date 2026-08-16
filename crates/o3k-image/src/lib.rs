use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditSink, AuthContext, AuthorizationRequest, Authorizer,
    NoopAuditSink, ResourceId, ResourceTarget, ResourceType, ServiceNamespace, StaticAuthorizer,
};
use o3k_store::{ImageMetadataRecord, ImageRepository, StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const DEFAULT_MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const QEMU_IMG_TIMEOUT: Duration = Duration::from_secs(30);
const QEMU_IMG_MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const QEMU_IMG_MAX_ADDRESS_SPACE_BYTES: u64 = 1024 * 1024 * 1024;
const QEMU_IMG_MAX_OPEN_FILES: u64 = 128;

/// TEST-ONLY failpoint (issue #607): `1` makes every `run_qemu_img`
/// invocation fail with a deterministic bounded `io::Error` before any host
/// process is spawned. The setpriv `--reset-env` sandbox makes PATH-shim
/// injection impossible, so the failpoint must be read by this process
/// itself, before the sandbox is consulted — it is therefore honored
/// regardless of PATH. Disabled by default; any value other than exactly
/// `1` leaves behavior unchanged. Never a public API; used by the failure
/// matrix harness and the unit test only.
const O3K_TEST_QEMU_IMG_FAIL: &str = "O3K_TEST_QEMU_IMG_FAIL";

// qcow2 structural gate constants, from the QEMU qcow2 format documentation
// (docs/interop/qcow2.rst).
const QCOW2_VERSION_2_HEADER: u64 = 72;
const QCOW2_VERSION_3_HEADER: u64 = 104;
const QCOW2_MAX_HEADER_LENGTH: u64 = 1 << 20;
const QCOW2_MIN_CLUSTER_BITS: u32 = 9;
const QCOW2_MAX_CLUSTER_BITS: u32 = 21;
const QCOW2_MAX_REFCOUNT_ORDER: u32 = 6;
const QCOW2_MAX_DISK_SIZE: u64 = 1_u64 << 62;
/// L1 table entries and standard L2 table entries address a host cluster
/// with bits 9-55; the remaining bits are flags and reserved bits.
const QCOW2_CLUSTER_OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
/// Refcount table entries address a refcount block with bits 9-63.
const QCOW2_REFCOUNT_BLOCK_OFFSET_MASK: u64 = 0xffff_ffff_ffff_fe00;
/// Incompatible feature bits accepted by the structural gate. Bit 0 (dirty,
/// refcounts may be stale) and bit 3 (non-deflate compression, which uses
/// the same on-disk extent layout) do not affect structural validation.
/// Everything else -- the corrupt bit, external data files, extended L2
/// entries, and unknown future layouts -- is rejected.
const QCOW2_INCOMPATIBLE_ALLOWED: u64 = (1 << 0) | (1 << 3);

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

#[derive(Clone)]
pub struct ImageCache {
    root: PathBuf,
    max_bytes: u64,
    lock: Arc<Mutex<()>>,
    qemu_img: PathBuf,
}

impl ImageCache {
    pub fn open(root: impl Into<PathBuf>, max_bytes: u64) -> Result<Self, ImageError> {
        Self::open_with_qemu_img(root, max_bytes, Path::new("qemu-img"))
    }

    fn open_with_qemu_img(
        root: impl Into<PathBuf>,
        max_bytes: u64,
        qemu_img: &Path,
    ) -> Result<Self, ImageError> {
        let root = root.into();
        ensure_managed_directory(&root)?;
        let base = root.join("base");
        ensure_managed_directory(&base)?;
        let overlays = root.join("overlays");
        ensure_managed_directory(&overlays)?;
        remove_temporary_files(&base, TemporaryKind::Base)?;
        remove_temporary_files(&overlays, TemporaryKind::Overlay)?;
        Ok(Self {
            root,
            max_bytes,
            lock: Arc::new(Mutex::new(())),
            qemu_img: qemu_img.to_owned(),
        })
    }

    pub fn cache_base(
        &self,
        checksum: &str,
        format: &str,
        content: &[u8],
    ) -> Result<PathBuf, ImageError> {
        if content.len() as u64 > self.max_bytes || !matches!(format, "qcow2" | "raw") {
            return Err(if !matches!(format, "qcow2" | "raw") {
                ImageError::UnsupportedFormat
            } else {
                ImageError::TooLarge
            });
        }
        if !is_checksum(checksum) {
            return Err(ImageError::ChecksumMismatch);
        }
        let actual = format!("{:x}", Sha256::digest(content));
        if actual != checksum {
            return Err(ImageError::ChecksumMismatch);
        }
        let _guard = self.lock.lock().map_err(|_| ImageError::Conflict)?;
        let path = self.root.join("base").join(format!("{checksum}.{format}"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(ImageError::InvalidPath);
                }
                let cached = fs::read(&path).map_err(ImageError::Storage)?;
                if cached.len() as u64 <= self.max_bytes
                    && cached.len() == content.len()
                    && format!("{:x}", Sha256::digest(&cached)) == checksum
                {
                    if format == "qcow2" {
                        reject_qcow2_dependencies(&path)?;
                        verify_image_format(&self.qemu_img, &path, format)?;
                    }
                    return Ok(path);
                }
                fs::remove_file(&path).map_err(ImageError::Storage)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImageError::Storage(error)),
        }
        let temporary = self
            .root
            .join("base")
            .join(format!("base-{checksum}.tmp-{}", Uuid::now_v7()));
        if let Err(error) = fs::write(&temporary, content) {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::Storage(error));
        }
        if format == "qcow2"
            && let Err(error) = (|| {
                reject_qcow2_dependencies(&temporary)?;
                verify_image_format(&self.qemu_img, &temporary, format)
            })()
        {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, &path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            ImageError::Storage(error)
        })?;
        Ok(path)
    }

    /// Publishes a previously verified image-service artifact into the local
    /// content-addressed cache.
    ///
    /// The cache still validates the digest, format, and byte limit through
    /// [`Self::cache_base`]. The explicit size check prevents a forged or
    /// stale in-memory artifact from crossing the service/cache boundary with
    /// inconsistent metadata.
    pub fn cache_artifact(
        &self,
        artifact: &ImageArtifact,
    ) -> Result<CachedImageArtifact, ImageError> {
        if artifact.size != artifact.content.len() as u64 {
            return Err(ImageError::ChecksumMismatch);
        }
        let path = self.cache_base(&artifact.checksum, &artifact.format, &artifact.content)?;
        Ok(CachedImageArtifact {
            id: artifact.id,
            checksum: artifact.checksum.clone(),
            format: artifact.format.clone(),
            size: artifact.size,
            path,
        })
    }

    /// Publishes a verified artifact from a host-local file without loading
    /// the complete image into memory. This is the agent-side bridge from the
    /// authenticated artifact store to the managed image cache.
    pub fn cache_base_path(
        &self,
        checksum: &str,
        format: &str,
        source: &Path,
    ) -> Result<PathBuf, ImageError> {
        if !is_checksum(checksum) {
            return Err(ImageError::ChecksumMismatch);
        }
        if !matches!(format, "qcow2" | "raw") {
            return Err(ImageError::UnsupportedFormat);
        }
        let source_metadata = fs::symlink_metadata(source).map_err(ImageError::Storage)?;
        if !source_metadata.file_type().is_file() || source_metadata.len() > self.max_bytes {
            return Err(ImageError::InvalidPath);
        }
        let mut file = fs::File::open(source).map_err(ImageError::Storage)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(ImageError::Storage)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("{:x}", hasher.finalize()) != checksum {
            return Err(ImageError::ChecksumMismatch);
        }
        if format == "qcow2" {
            verify_image_format(&self.qemu_img, source, format)?;
        }

        if let Ok(path) = self.resolve_base(checksum, format, source_metadata.len()) {
            return Ok(path);
        }

        let _guard = self.lock.lock().map_err(|_| ImageError::Conflict)?;
        let target = self.root.join("base").join(format!("{checksum}.{format}"));
        if let Ok(metadata) = fs::symlink_metadata(&target) {
            if !metadata.file_type().is_file() {
                return Err(ImageError::InvalidPath);
            }
            fs::remove_file(&target).map_err(ImageError::Storage)?;
        }
        let temporary = self
            .root
            .join("base")
            .join(format!("base-{checksum}.tmp-{}", Uuid::now_v7()));
        if let Err(error) = fs::copy(source, &temporary) {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::Storage(error));
        }
        if let Err(error) = fs::rename(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::Storage(error));
        }
        drop(_guard);
        match self.resolve_base(checksum, format, source_metadata.len()) {
            Ok(path) => Ok(path),
            Err(error) => {
                let _ = fs::remove_file(&target);
                Err(error)
            }
        }
    }

    /// Resolve a previously published base image without accepting a host
    /// path from the caller.
    ///
    /// The checksum and format select only the cache-owned pathname. The
    /// returned entry is still treated as untrusted: it must be a regular
    /// file, have the expected bounded size, and match the complete SHA-256
    /// digest. A qcow2 entry additionally needs fresh `qemu-img` format
    /// evidence. This makes a cache hit safe after a process restart or an
    /// out-of-band modification.
    pub fn resolve_base(
        &self,
        checksum: &str,
        format: &str,
        expected_size: u64,
    ) -> Result<PathBuf, ImageError> {
        if !is_checksum(checksum) {
            return Err(ImageError::ChecksumMismatch);
        }
        if !matches!(format, "qcow2" | "raw") {
            return Err(ImageError::UnsupportedFormat);
        }
        if expected_size > self.max_bytes {
            return Err(ImageError::TooLarge);
        }

        let _guard = self.lock.lock().map_err(|_| ImageError::Conflict)?;
        let path = self.root.join("base").join(format!("{checksum}.{format}"));
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ImageError::NotFound);
            }
            Err(error) => return Err(ImageError::Storage(error)),
        };
        if !metadata.file_type().is_file() {
            return Err(ImageError::InvalidPath);
        }
        if metadata.len() != expected_size || metadata.len() > self.max_bytes {
            return Err(ImageError::ChecksumMismatch);
        }

        let mut file = fs::File::open(&path).map_err(ImageError::Storage)?;
        let opened_metadata = file.metadata().map_err(ImageError::Storage)?;
        if !opened_metadata.file_type().is_file() || opened_metadata.len() != expected_size {
            return Err(ImageError::ChecksumMismatch);
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(ImageError::Storage)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("{:x}", hasher.finalize()) != checksum {
            return Err(ImageError::ChecksumMismatch);
        }
        if format == "qcow2" {
            verify_image_format(&self.qemu_img, &path, format)?;
        }
        Ok(path)
    }

    pub fn create_overlay(&self, instance_id: &str, base: &Path) -> Result<PathBuf, ImageError> {
        let base_dir = self.root.join("base");
        if instance_id.is_empty()
            || instance_id
                != Path::new(instance_id)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
        {
            return Err(ImageError::InvalidPath);
        }
        validate_verified_base(&self.qemu_img, &base_dir, base, self.max_bytes)?;
        let _guard = self.lock.lock().map_err(|_| ImageError::Conflict)?;
        let overlay = self
            .root
            .join("overlays")
            .join(format!("{instance_id}.qcow2"));
        match fs::symlink_metadata(&overlay) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(ImageError::InvalidPath);
                }
                verify_overlay(&self.qemu_img, &overlay, base)?;
                return Ok(overlay);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImageError::Storage(error)),
        }
        let temporary = self
            .root
            .join("overlays")
            .join(format!(".{instance_id}.tmp-{}", Uuid::now_v7()));
        let backing_format = base
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.rsplit_once('.').map(|(_, format)| format))
            .ok_or(ImageError::InvalidPath)?;
        let output = run_qemu_img(
            &self.qemu_img,
            [
                "create",
                "-f",
                "qcow2",
                "-b",
                base.to_str().ok_or(ImageError::InvalidPath)?,
                "-F",
                backing_format,
                temporary.to_str().ok_or(ImageError::InvalidPath)?,
            ],
        )
        .map_err(|_| {
            let _ = fs::remove_file(&temporary);
            ImageError::OverlayFailed
        })?;
        if !output.status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::OverlayFailed);
        }
        if verify_overlay(&self.qemu_img, &temporary, base).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::OverlayFailed);
        }
        match fs::symlink_metadata(&overlay) {
            Ok(metadata) => {
                let _ = fs::remove_file(&temporary);
                if metadata.file_type().is_file() {
                    return Ok(overlay);
                }
                return Err(ImageError::InvalidPath);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(ImageError::Storage(error));
            }
        }
        if let Err(error) = fs::rename(&temporary, &overlay) {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::Storage(error));
        }
        Ok(overlay)
    }

    pub fn delete_overlay(&self, instance_id: &str) -> Result<(), ImageError> {
        if instance_id.is_empty() || instance_id.contains('/') || instance_id.contains('\\') {
            return Err(ImageError::InvalidPath);
        }
        let path = self
            .root
            .join("overlays")
            .join(format!("{instance_id}.qcow2"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(path).map_err(ImageError::Storage)?;
            }
            Ok(_) => return Err(ImageError::InvalidPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImageError::Storage(error)),
        }
        Ok(())
    }

    /// Expands an owned qcow2 overlay to the selected flavor capacity. Shrink
    /// requests are rejected so a retry cannot destroy guest data.
    pub fn resize_overlay(
        &self,
        instance_id: &str,
        overlay: &Path,
        disk_gib: u64,
    ) -> Result<(), ImageError> {
        if instance_id.is_empty()
            || instance_id
                != Path::new(instance_id)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
            || disk_gib == 0
            || overlay.parent() != Some(self.root.join("overlays").as_path())
        {
            return Err(ImageError::InvalidPath);
        }
        let expected = self
            .root
            .join("overlays")
            .join(format!("{instance_id}.qcow2"));
        if overlay != expected {
            return Err(ImageError::InvalidPath);
        }
        let target = disk_gib
            .checked_mul(1024 * 1024 * 1024)
            .ok_or(ImageError::TooLarge)?;
        let current = overlay_virtual_size(&self.qemu_img, overlay)?;
        if target < current {
            return Err(ImageError::Conflict);
        }
        if target == current {
            return Ok(());
        }
        let output = run_qemu_img(
            &self.qemu_img,
            [
                "resize",
                overlay.to_str().ok_or(ImageError::InvalidPath)?,
                &target.to_string(),
            ],
        )
        .map_err(|_| ImageError::OverlayFailed)?;
        if !output.status.success() || overlay_virtual_size(&self.qemu_img, overlay)? < target {
            return Err(ImageError::OverlayFailed);
        }
        Ok(())
    }
}

fn validate_verified_base(
    qemu_img: &Path,
    base_dir: &Path,
    base: &Path,
    max_bytes: u64,
) -> Result<(), ImageError> {
    if base.parent() != Some(base_dir) {
        return Err(ImageError::InvalidPath);
    }
    let name = base
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ImageError::InvalidPath)?;
    let (checksum, format) = name.rsplit_once('.').ok_or(ImageError::InvalidPath)?;
    if !is_checksum(checksum) || !matches!(format, "raw" | "qcow2") {
        return Err(ImageError::InvalidPath);
    }
    let metadata = fs::symlink_metadata(base).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ImageError::NotFound
        } else {
            ImageError::Storage(error)
        }
    })?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return Err(ImageError::InvalidPath);
    }
    let content = fs::read(base).map_err(ImageError::Storage)?;
    if content.len() as u64 != metadata.len()
        || format!("{:x}", Sha256::digest(&content)) != checksum
    {
        return Err(ImageError::ChecksumMismatch);
    }
    if format == "qcow2" {
        reject_qcow2_dependencies(base)?;
        verify_image_format(qemu_img, base, format)?;
        // Full metadata self-consistency check (refcounts, overlaps) before
        // an overlay is derived from the base and handed to libvirt.
        verify_qcow2_consistency(qemu_img, base)?;
    }
    Ok(())
}

/// Rejects qcow2 backing and external-data references by inspecting only the
/// fixed-size qcow2 header. This runs before qemu-img so an uploaded image
/// cannot make the helper open a tenant-controlled host path while discovering
/// that the image is unsafe.
fn reject_qcow2_dependencies(path: &Path) -> Result<(), ImageError> {
    let mut file = fs::File::open(path).map_err(ImageError::Storage)?;
    let mut header = [0_u8; 104];
    let count = file.read(&mut header).map_err(ImageError::Storage)?;
    if count < 32 || &header[..4] != b"QFI\xfb" {
        // qemu-img remains the format authority for malformed/non-qcow2
        // bytes; this branch keeps injectable test helpers deterministic.
        return Ok(());
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if !matches!(version, 2 | 3) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let backing_offset = u64::from_be_bytes([
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15],
    ]);
    let backing_size = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    if backing_offset != 0 || backing_size != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    // QCOW2 v3 incompatible feature bit 2 denotes an external data file.
    if version == 3 && count >= 80 {
        let incompatible = u64::from_be_bytes([
            header[72], header[73], header[74], header[75], header[76], header[77], header[78],
            header[79],
        ]);
        if incompatible & (1 << 2) != 0 {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    Ok(())
}

/// Structurally validates a qcow2 payload so a truncated or corrupt image is
/// rejected before its record can be activated. This is the import-time
/// gate: every on-disk structure reachable from the header -- the L1 table,
/// L2 tables, data clusters, the refcount table, and refcount blocks -- must
/// lie completely inside the `len` payload bytes, or the image could never
/// be materialized and booted. Unallocated entries (offset zero) reference
/// nothing and are allowed.
///
/// Field layout, table entry formats, and compressed cluster sizing follow
/// the QEMU qcow2 format documentation (docs/interop/qcow2.rst). The walk is
/// bounded by the payload size: tables must be inside the payload, and each
/// distinct L2 table is visited at most once.
fn validate_qcow2_structure(reader: &mut (impl Read + Seek), len: u64) -> Result<(), ImageError> {
    if len < QCOW2_VERSION_2_HEADER {
        return Err(ImageError::FormatVerificationFailed);
    }
    let mut header = [0_u8; QCOW2_VERSION_3_HEADER as usize];
    read_exact_at(reader, 0, &mut header[..QCOW2_VERSION_2_HEADER as usize])?;
    if &header[0..4] != b"QFI\xfb" {
        return Err(ImageError::FormatVerificationFailed);
    }
    let version = be_u32(&header[4..8]);
    if !matches!(version, 2 | 3) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let cluster_bits = be_u32(&header[20..24]);
    if !(QCOW2_MIN_CLUSTER_BITS..=QCOW2_MAX_CLUSTER_BITS).contains(&cluster_bits) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let cluster_size = 1_u64 << cluster_bits;
    // Uploaded images must be self-contained: a backing file reference would
    // make later booting depend on a host path controlled by the uploader.
    // This mirrors `reject_qcow2_dependencies` at the import boundary.
    if be_u64(&header[8..16]) != 0 || be_u32(&header[16..20]) != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    // No key material exists to open an encrypted image, so it could never
    // boot; reject it here instead of after placement.
    if be_u32(&header[32..36]) != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    if version == 3 {
        if len < QCOW2_VERSION_3_HEADER {
            return Err(ImageError::FormatVerificationFailed);
        }
        read_exact_at(
            reader,
            QCOW2_VERSION_2_HEADER,
            &mut header[QCOW2_VERSION_2_HEADER as usize..],
        )?;
        let header_length = u64::from(be_u32(&header[100..104]));
        if header_length < QCOW2_VERSION_3_HEADER
            || header_length % 8 != 0
            || header_length > QCOW2_MAX_HEADER_LENGTH
            || header_length > len
        {
            return Err(ImageError::FormatVerificationFailed);
        }
        if be_u32(&header[96..100]) > QCOW2_MAX_REFCOUNT_ORDER {
            return Err(ImageError::FormatVerificationFailed);
        }
        if be_u64(&header[72..80]) & !QCOW2_INCOMPATIBLE_ALLOWED != 0 {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    let disk_size = be_u64(&header[24..32]);
    if disk_size == 0 || disk_size > QCOW2_MAX_DISK_SIZE {
        return Err(ImageError::FormatVerificationFailed);
    }
    let l1_size = u64::from(be_u32(&header[36..40]));
    if l1_size == 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    let l1_table_offset = be_u64(&header[40..48]);
    // Checked arithmetic: a hostile header must never be able to wrap an
    // extent sum around u64::MAX and slip past the payload bound.
    if l1_table_offset == 0
        || !l1_table_offset.is_multiple_of(cluster_size)
        || l1_table_offset
            .checked_add(l1_size * 8)
            .is_none_or(|end| end > len)
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    // The active L1 table must be able to address the entire virtual disk; a
    // smaller table would expose a truncated virtual disk. Each L1 entry
    // covers cluster_size/8 L2 entries of cluster_size bytes each.
    let covered_per_l1_entry = (cluster_size / 8) * cluster_size;
    if disk_size.div_ceil(covered_per_l1_entry) > l1_size {
        return Err(ImageError::FormatVerificationFailed);
    }
    let refcount_table_clusters = u64::from(be_u32(&header[56..60]));
    let refcount_table_offset = be_u64(&header[48..56]);
    if refcount_table_clusters == 0
        || refcount_table_offset == 0
        || !refcount_table_offset.is_multiple_of(cluster_size)
        || refcount_table_offset
            .checked_add(refcount_table_clusters * cluster_size)
            .is_none_or(|end| end > len)
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    if be_u32(&header[60..64]) > 0 && be_u64(&header[64..72]) == 0 {
        return Err(ImageError::FormatVerificationFailed);
    }

    // Active L1 table: every used entry names an L2 table that must be fully
    // inside the payload.
    let mut l1 = vec![0_u8; (l1_size * 8) as usize];
    read_exact_at(reader, l1_table_offset, &mut l1)?;
    let mut visited_l2 = HashSet::new();
    for entry in l1.chunks_exact(8) {
        let l2_offset = be_u64(entry) & QCOW2_CLUSTER_OFFSET_MASK;
        if l2_offset == 0 || !visited_l2.insert(l2_offset) {
            continue;
        }
        if !l2_offset.is_multiple_of(cluster_size)
            || l2_offset
                .checked_add(cluster_size)
                .is_none_or(|end| end > len)
        {
            return Err(ImageError::FormatVerificationFailed);
        }
        // L2 table: standard entries name a whole data cluster; compressed
        // entries name an (unaligned) extent of 512-byte sectors.
        let mut l2 = vec![0_u8; cluster_size as usize];
        read_exact_at(reader, l2_offset, &mut l2)?;
        for entry in l2.chunks_exact(8) {
            let entry = be_u64(entry);
            if entry & (1_u64 << 62) != 0 {
                let offset_bits = 62 - (cluster_bits - 8);
                let offset = entry & ((1_u64 << offset_bits) - 1);
                let additional_sectors =
                    (entry >> offset_bits) & ((1_u64 << (62 - offset_bits)) - 1);
                if offset
                    .checked_add((additional_sectors + 1) * 512)
                    .is_none_or(|end| end > len)
                {
                    return Err(ImageError::FormatVerificationFailed);
                }
            } else {
                let host = entry & QCOW2_CLUSTER_OFFSET_MASK;
                if host != 0
                    && (!host.is_multiple_of(cluster_size)
                        || host.checked_add(cluster_size).is_none_or(|end| end > len))
                {
                    return Err(ImageError::FormatVerificationFailed);
                }
            }
        }
    }

    // Refcount table: every used entry names a refcount block of exactly one
    // cluster that must be fully inside the payload.
    let refcount_table_bytes = refcount_table_clusters * cluster_size;
    let mut refcount_table = vec![0_u8; refcount_table_bytes as usize];
    read_exact_at(reader, refcount_table_offset, &mut refcount_table)?;
    for entry in refcount_table.chunks_exact(8) {
        let block_offset = be_u64(entry) & QCOW2_REFCOUNT_BLOCK_OFFSET_MASK;
        if block_offset != 0
            && (!block_offset.is_multiple_of(cluster_size)
                || block_offset
                    .checked_add(cluster_size)
                    .is_none_or(|end| end > len))
        {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    Ok(())
}

fn read_exact_at(
    reader: &mut (impl Read + Seek),
    offset: u64,
    buffer: &mut [u8],
) -> Result<(), ImageError> {
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(ImageError::Storage)?;
    reader.read_exact(buffer).map_err(ImageError::Storage)
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn ensure_managed_directory(path: &Path) -> Result<(), ImageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            #[cfg(unix)]
            // Libvirt's qemu process must traverse the managed cache to read
            // an owned overlay, but group members must not list or write it.
            // The compute service installs this subtree with the kvm group;
            // 0710 grants traversal only and leaves file read policy to the
            // individual artifact modes.
            fs::set_permissions(path, fs::Permissions::from_mode(0o2710))
                .map_err(ImageError::Storage)?;
            Ok(())
        }
        Ok(_) => Err(ImageError::InvalidPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ImageError::Storage)?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o2710))
                .map_err(ImageError::Storage)?;
            Ok(())
        }
        Err(error) => Err(ImageError::Storage(error)),
    }
}

#[derive(Clone, Copy)]
enum TemporaryKind {
    Base,
    Overlay,
    Upload,
}

fn remove_temporary_files(directory: &Path, kind: TemporaryKind) -> Result<(), ImageError> {
    for entry in fs::read_dir(directory).map_err(ImageError::Storage)? {
        let entry = entry.map_err(ImageError::Storage)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let matches = match kind {
            TemporaryKind::Base => is_base_temporary(&name),
            TemporaryKind::Overlay => is_overlay_temporary(&name),
            TemporaryKind::Upload => is_upload_temporary(&name),
        };
        if !matches {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(ImageError::Storage)?;
        if metadata.file_type().is_file() {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn is_base_temporary(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("base-") else {
        return false;
    };
    let Some((checksum, suffix)) = rest.split_once(".tmp-") else {
        return false;
    };
    is_checksum(checksum) && Uuid::parse_str(suffix).is_ok()
}

fn is_overlay_temporary(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((instance, suffix)) = rest.split_once(".tmp-") else {
        return false;
    };
    !instance.is_empty() && Uuid::parse_str(suffix).is_ok()
}

fn is_upload_temporary(name: &str) -> bool {
    let Some((image_id, suffix)) = name.split_once(".upload-") else {
        return false;
    };
    Uuid::parse_str(image_id).is_ok() && Uuid::parse_str(suffix).is_ok()
}

fn verify_overlay(qemu_img: &Path, overlay: &Path, base: &Path) -> Result<(), ImageError> {
    let expected_base = fs::canonicalize(base).map_err(|_| ImageError::OverlayFailed)?;
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }

    let mut backing_paths = Vec::new();
    for field in ["backing-filename", "full-backing-filename"] {
        if let Some(backing) = info.get(field).and_then(serde_json::Value::as_str) {
            backing_paths.push(backing);
        }
    }
    if backing_paths.is_empty() {
        return Err(ImageError::OverlayFailed);
    }
    let overlay_parent = overlay.parent().ok_or(ImageError::OverlayFailed)?;
    for backing in backing_paths {
        let reported = Path::new(backing);
        let resolved = if reported.is_absolute() {
            reported.to_path_buf()
        } else {
            overlay_parent.join(reported)
        };
        if fs::canonicalize(resolved).map_err(|_| ImageError::OverlayFailed)? != expected_base {
            return Err(ImageError::OverlayFailed);
        }
    }
    Ok(())
}

fn overlay_virtual_size(qemu_img: &Path, overlay: &Path) -> Result<u64, ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }
    info.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ImageError::OverlayFailed)
}

fn verify_image_format(qemu_img: &Path, image: &Path, expected: &str) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !output.status.success() {
        return Err(ImageError::FormatVerificationFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::FormatVerificationFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some(expected) {
        return Err(ImageError::FormatVerificationFailed);
    }
    // Uploaded qcow2 bytes must be self-contained.  A backing reference (or
    // an external data file) would make later libvirt/qemu access depend on a
    // host path or protocol controlled by the uploader, and a nested chain
    // would evade the managed cache's ownership and digest checks.
    if expected == "qcow2"
        && [
            "backing-filename",
            "full-backing-filename",
            "backing-filename-format",
            "data-file",
            "data-file-raw",
        ]
        .iter()
        .any(|field| info.get(field).is_some_and(|value| !value.is_null()))
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

/// Runs a read-only `qemu-img check` over a verified base so a truncated or
/// metadata-inconsistent qcow2 (extents beyond the end of the file, wrong
/// refcounts, overlapping structures) is rejected before an overlay is
/// derived from it and handed to libvirt.
///
/// `qemu-img check` without `-r` never repairs or writes the image. Its exit
/// code is 0 when the image is clean, 1 when only leaked clusters were found
/// (wasted space, no data corruption), and 2 when errors were found; any
/// other outcome (signal, helper failure) also fails closed.
fn verify_qcow2_consistency(qemu_img: &Path, image: &Path) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "check",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

fn run_qemu_img<'a, I>(qemu_img: &Path, args: I) -> io::Result<Output>
where
    I: IntoIterator<Item = &'a str>,
{
    // Test-only failpoint (issue #607): read by this process before any
    // spawn, so it cannot be bypassed by PATH manipulation. The exact value
    // "1" injects a bounded, deterministic failure; unset or any other value
    // keeps the normal sandboxed invocation.
    if std::env::var_os(O3K_TEST_QEMU_IMG_FAIL).is_some_and(|value| value == "1") {
        return Err(io::Error::other(
            "qemu-img failure injected by O3K_TEST_QEMU_IMG_FAIL",
        ));
    }
    let args = args.into_iter().collect::<Vec<_>>();
    let setpriv = Path::new("/usr/bin/setpriv");
    if !setpriv.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "setpriv is required to sandbox qemu-img",
        ));
    }
    let prlimit = Path::new("/usr/bin/prlimit");
    if !prlimit.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "prlimit is required to bound qemu-img resources",
        ));
    }
    let mut command = Command::new(setpriv);
    command.args([
        "--no-new-privs",
        "--ambient-caps=-all",
        "--inh-caps=-all",
        "--reset-env",
        "--",
    ]);
    command.arg(prlimit);
    // RLIMIT_NPROC is enforced per real UID across the whole system, not per
    // process tree. A low --nproc bound therefore breaks the helper whenever
    // the service account already runs other threads (CI runners with parallel
    // test processes, or a busy o3k-compute account), because the helper cannot
    // create even one thread. Per-process bounds that actually hold are kept:
    // address space, open files, bounded output, and a hard timeout.
    command.args([
        format!("--as={QEMU_IMG_MAX_ADDRESS_SPACE_BYTES}"),
        format!("--nofile={QEMU_IMG_MAX_OPEN_FILES}"),
        "--".to_owned(),
    ]);
    command.arg(qemu_img);
    command.args(args);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("qemu-img stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("qemu-img stderr was not piped"))?;
    let stdout_reader = thread::spawn(move || read_bounded_output(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_output(stderr));
    let deadline = Instant::now() + QEMU_IMG_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("qemu-img stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("qemu-img stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded_output<R: Read>(reader: R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(QEMU_IMG_MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > QEMU_IMG_MAX_OUTPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "qemu-img output exceeded the safety bound",
        ));
    }
    Ok(bytes)
}

fn is_checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone)]
pub struct ImageService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
    max_upload_bytes: usize,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn ImageRepository>,
}

impl ImageService {
    pub async fn open(
        root: impl Into<PathBuf>,
        max_upload_bytes: usize,
        repository: Arc<dyn ImageRepository>,
    ) -> Result<Self, ImageError> {
        let root = root.into();
        ensure_managed_directory(&root)?;
        let content = root.join("content");
        ensure_managed_directory(&content)?;
        remove_temporary_files(&content, TemporaryKind::Upload)?;
        Ok(Self {
            inner: Arc::new(Inner { root, repository }),
            lock: Arc::new(tokio::sync::Mutex::new(())),
            max_upload_bytes,
            authorizer: Arc::new(StaticAuthorizer::standard()),
            audit_sink: Arc::new(NoopAuditSink),
        })
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_audit_sink(mut self, audit_sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = audit_sink;
        self
    }

    pub async fn create(
        &self,
        auth: &AuthContext,
        name: String,
        visibility: String,
        container_format: String,
        disk_format: String,
    ) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "CreateImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "CreateImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::Unauthorized);
        }
        match self
            .create_for_project(
                auth.effective_scope().id().as_str(),
                name,
                visibility,
                container_format,
                disk_format,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_for_project(
        &self,
        project_id: &str,
        name: String,
        visibility: String,
        container_format: String,
        disk_format: String,
    ) -> Result<ImageRecord, ImageError> {
        if name.trim().is_empty()
            || container_format.trim().is_empty()
            || disk_format.trim().is_empty()
            || container_format != "bare"
            || !matches!(disk_format.as_str(), "raw" | "qcow2")
            || visibility != "private"
        {
            return Err(ImageError::InvalidMetadata);
        }
        let record = ImageMetadataRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "queued".to_owned(),
            visibility,
            container_format,
            disk_format,
            size: None,
            checksum: None,
        };
        self.inner
            .repository
            .insert_image(&record)
            .await
            .map_err(Self::map_store_error)?;
        image_from_store(record)
    }

    pub async fn list(&self, auth: &AuthContext) -> Result<Vec<ImageRecord>, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "ListImages").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "ListImages".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::Unauthorized);
        }
        self.list_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_for_project(&self, project_id: &str) -> Result<Vec<ImageRecord>, ImageError> {
        self.inner
            .repository
            .list_images(project_id)
            .await
            .map_err(Self::map_store_error)?
            .into_iter()
            .map(image_from_store)
            .collect()
    }

    pub async fn get(&self, auth: &AuthContext, id: Uuid) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "ReadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "ReadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        self.get_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<ImageRecord, ImageError> {
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        image_from_store(record)
    }

    pub async fn resolve_artifact(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<ImageArtifact, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "DownloadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "DownloadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        self.resolve_artifact_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn resolve_artifact_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<ImageArtifact, ImageError> {
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        if record.status != "active" {
            return Err(ImageError::NotFound);
        }
        let checksum = record.checksum.ok_or(ImageError::NotFound)?;
        let size = record
            .size
            .map(|value| u64::try_from(value).map_err(|_| ImageError::NotFound))
            .transpose()?
            .ok_or(ImageError::NotFound)?;
        if !matches!(record.disk_format.as_str(), "raw" | "qcow2") {
            return Err(ImageError::UnsupportedFormat);
        }
        let path = content_path(&self.inner.root, id);
        if path.is_symlink() || !path.is_file() {
            return Err(ImageError::NotFound);
        }
        let mut file = fs::File::open(&path).map_err(ImageError::Storage)?;
        let actual_size = file.metadata().map_err(ImageError::Storage)?.len();
        if actual_size > self.max_upload_bytes as u64 {
            return Err(ImageError::TooLarge);
        }
        let mut content = Vec::with_capacity(actual_size as usize);
        file.read_to_end(&mut content)
            .map_err(ImageError::Storage)?;
        if content.len() as u64 != size
            || !is_checksum(&checksum)
            || format!("{:x}", Sha256::digest(&content)) != checksum
        {
            return Err(ImageError::ChecksumMismatch);
        }
        Ok(ImageArtifact {
            id,
            checksum,
            format: record.disk_format,
            size,
            content,
        })
    }

    pub async fn upload(
        &self,
        auth: &AuthContext,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "UploadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "UploadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        match self
            .upload_for_project(auth.effective_scope().id().as_str(), id, content)
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn upload_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        if content.len() > self.max_upload_bytes {
            return Err(ImageError::TooLarge);
        }
        let _guard = self.lock.lock().await;
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        let record = image_from_store(record)?;
        if record.status == ImageStatus::Active {
            return Err(ImageError::Conflict);
        }
        if record.disk_format == "qcow2" {
            let mut reader = std::io::Cursor::new(content);
            validate_qcow2_structure(&mut reader, content.len() as u64)?;
        }
        let content_path = content_path(&self.inner.root, id);
        let temporary_path = content_path.with_extension(format!("upload-{}", Uuid::now_v7()));
        if let Err(error) = fs::write(&temporary_path, content) {
            let _ = fs::remove_file(&temporary_path);
            return Err(ImageError::Storage(error));
        }
        if let Err(error) = fs::rename(&temporary_path, &content_path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(ImageError::Storage(error));
        }
        let checksum = format!("{:x}", Sha256::digest(content));
        match self
            .inner
            .repository
            .activate_image(project_id, &id, content.len() as u64, &checksum)
            .await
        {
            Ok(record) => image_from_store(record),
            Err(error) => {
                let _ = fs::remove_file(&content_path);
                Err(Self::map_store_error(error))
            }
        }
    }

    pub async fn delete(&self, auth: &AuthContext, id: Uuid) -> Result<(), ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "DeleteImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "DeleteImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        match self
            .delete_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_for_project(&self, project_id: &str, id: Uuid) -> Result<(), ImageError> {
        let _guard = self.lock.lock().await;
        self.inner
            .repository
            .delete_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?;
        let content = content_path(&self.inner.root, id);
        if content.exists() {
            fs::remove_file(content).map_err(ImageError::Storage)?;
        }
        Ok(())
    }

    fn map_store_error(error: StoreError) -> ImageError {
        match error {
            StoreError::ImageNotFound => ImageError::NotFound,
            StoreError::ImageAlreadyActive => ImageError::Conflict,
            other => ImageError::Store(other),
        }
    }
}

fn content_path(root: &Path, id: Uuid) -> PathBuf {
    root.join("content").join(id.to_string())
}

fn image_from_store(record: ImageMetadataRecord) -> Result<ImageRecord, ImageError> {
    let status = match record.status.as_str() {
        "queued" => ImageStatus::Queued,
        "active" => ImageStatus::Active,
        // An unknown status is corrupt durable state; fail closed instead of
        // inventing a status projection.
        _ => {
            return Err(ImageError::Store(StoreError::Corrupt(format!(
                "image {} has unknown status `{}`",
                record.id, record.status
            ))));
        }
    };
    Ok(ImageRecord {
        id: record.id,
        name: record.name,
        project_id: record.project_id,
        status,
        visibility: record.visibility,
        container_format: record.container_format,
        disk_format: record.disk_format,
        size: record
            .size
            .map(|size| {
                u64::try_from(size).map_err(|_| {
                    StoreError::Corrupt(format!("image {} has invalid size", record.id))
                })
            })
            .transpose()
            .map_err(ImageError::Store)?,
        checksum: record.checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(project_id: &str) -> AuthContext {
        AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("test-user"),
                "test-user",
                Some("default".to_string()),
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(project_id),
                Some(project_id.to_string()),
                Some("default".to_string()),
            ),
            vec!["admin".to_string()],
            1000,
            5000,
            uuid::Uuid::now_v7().to_string(),
            uuid::Uuid::now_v7().to_string(),
            None,
        )
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn root(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/o3k-image-{label}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn upload_is_atomic_and_restartable() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("restart");
        let sqlite_path = format!("{}.sqlite", path.display());
        let store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store.clone()).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        let uploaded = service
            .upload(&auth("project-a"), image.id, b"image-bytes")
            .await?;
        assert_eq!(uploaded.status, ImageStatus::Active);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            entry.file_name().to_string_lossy().contains(".tmp-")
                || entry.file_name().to_string_lossy().contains("upload-")
        }));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let reopened = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, reopened_store).await?;
        assert_eq!(reopened.get(&auth("project-a"), image.id).await?, uploaded);
        let artifact = reopened
            .resolve_artifact(&auth("project-a"), image.id)
            .await?;
        assert_eq!(artifact.id, image.id);
        assert_eq!(artifact.format, "raw");
        assert_eq!(artifact.size, 11);
        assert_eq!(artifact.content, b"image-bytes");
        fs::remove_dir_all(path)?;
        fs::remove_file(&sqlite_path)?;
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn artifact_resolution_rechecks_content_and_scope()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("artifact");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::NotFound)
        ));
        service
            .upload(&auth("project-a"), image.id, b"image-bytes")
            .await?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-b"), image.id).await,
            Err(ImageError::NotFound)
        ));
        fs::write(path.join("content").join(image.id.to_string()), b"tampered")?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::ChecksumMismatch)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn artifact_resolution_bounds_tampered_content() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = root("artifact-limit");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, 3, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service.upload(&auth("project-a"), image.id, b"abc").await?;
        fs::write(
            path.join("content").join(image.id.to_string()),
            b"too-large",
        )?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::TooLarge)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn verified_service_artifact_publishes_to_cache_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let service_path = root("artifact-cache-service");
        let cache_path = root("artifact-cache-cache");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&service_path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service
            .upload(&auth("project-a"), image.id, b"image-bytes")
            .await?;
        let artifact = service
            .resolve_artifact(&auth("project-a"), image.id)
            .await?;
        let cache = ImageCache::open(&cache_path, DEFAULT_MAX_CACHE_BYTES)?;

        let first = cache.cache_artifact(&artifact)?;
        let second = cache.cache_artifact(&artifact)?;
        assert_eq!(first, second);
        assert_eq!(first.id, image.id);
        assert_eq!(first.size, artifact.content.len() as u64);
        assert_eq!(fs::read(&first.path)?, artifact.content);
        assert_eq!(
            cache.resolve_base(&artifact.checksum, &artifact.format, artifact.size)?,
            first.path
        );

        drop(cache);
        let reopened_cache = ImageCache::open(&cache_path, DEFAULT_MAX_CACHE_BYTES)?;
        assert_eq!(
            reopened_cache.resolve_base(&artifact.checksum, &artifact.format, artifact.size)?,
            first.path
        );

        let mut inconsistent = artifact;
        inconsistent.size += 1;
        assert!(matches!(
            reopened_cache.cache_artifact(&inconsistent),
            Err(ImageError::ChecksumMismatch)
        ));

        fs::remove_dir_all(service_path)?;
        fs::remove_dir_all(cache_path)?;
        Ok(())
    }

    #[test]
    fn verified_path_publication_is_streamed_and_content_addressed()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("cache-path-publication");
        let source = path.join("artifact.raw");
        let cache_path = root("cache-path-publication-cache");
        fs::create_dir_all(&path)?;
        let content = vec![b'x'; 128 * 1024];
        let checksum = format!("{:x}", Sha256::digest(&content));
        fs::write(&source, &content)?;
        let cache = ImageCache::open(&cache_path, DEFAULT_MAX_CACHE_BYTES)?;
        let published = cache.cache_base_path(&checksum, "raw", &source)?;
        assert_eq!(fs::read(&published)?, content);
        assert_eq!(cache.cache_base_path(&checksum, "raw", &source)?, published);
        fs::remove_dir_all(path)?;
        fs::remove_dir_all(cache_path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_cache_directories_allow_libvirt_traversal_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache_path = root("cache-directory-permissions");
        let _cache = ImageCache::open(&cache_path, DEFAULT_MAX_CACHE_BYTES)?;
        for directory in [
            cache_path.clone(),
            cache_path.join("base"),
            cache_path.join("overlays"),
        ] {
            let mode = fs::metadata(directory)?.permissions().mode();
            assert_eq!(mode & 0o777, 0o710);
            assert_ne!(
                mode & 0o2000,
                0,
                "cache directory must preserve kvm group inheritance"
            );
        }
        fs::remove_dir_all(cache_path)?;
        Ok(())
    }

    #[test]
    fn cached_base_resolution_rejects_tampering_and_wrong_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("cache-resolve-revalidation");
        let _ = fs::remove_dir_all(&path);
        let cache = ImageCache::open(&path, DEFAULT_MAX_CACHE_BYTES)?;
        let content = b"stable-image-content";
        let checksum = format!("{:x}", Sha256::digest(content));
        let base = cache.cache_base(&checksum, "raw", content)?;

        assert!(matches!(
            cache.resolve_base(&checksum, "vmdk", content.len() as u64),
            Err(ImageError::UnsupportedFormat)
        ));
        assert!(matches!(
            cache.resolve_base(&"0".repeat(64), "raw", content.len() as u64),
            Err(ImageError::NotFound)
        ));
        assert!(matches!(
            cache.resolve_base(&checksum, "raw", content.len() as u64 + 1),
            Err(ImageError::ChecksumMismatch)
        ));

        fs::write(&base, b"tampered-image-content")?;
        assert!(matches!(
            cache.resolve_base(&checksum, "raw", content.len() as u64),
            Err(ImageError::ChecksumMismatch)
        ));

        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn upload_limit_and_project_isolation_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("limits");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, 3, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "../outside".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        assert!(matches!(
            service.upload(&auth("project-a"), image.id, b"four").await,
            Err(ImageError::TooLarge)
        ));
        assert!(matches!(
            service.get(&auth("project-b"), image.id).await,
            Err(ImageError::NotFound)
        ));
        service.delete(&auth("project-a"), image.id).await?;
        assert!(!path.join("outside").exists());
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn image_service_restart_cleans_only_upload_temporaries()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("upload-restart");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("content"))?;
        let image_id = Uuid::now_v7();
        let stale = path
            .join("content")
            .join(format!("{image_id}.upload-{}", Uuid::now_v7()));
        let unrelated = path.join("content").join("foreign.upload-user");
        fs::write(&stale, b"partial")?;
        fs::write(&unrelated, b"keep")?;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let _service = ImageService::open(&path, 1024, store).await?;
        assert!(!stale.exists());
        assert_eq!(fs::read(&unrelated)?, b"keep");
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn metadata_survives_restart_from_durable_store() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = root("durable-restart");
        let sqlite_path = format!("{}.sqlite", path.display());
        let content = b"durable-image-bytes";
        let (image_id, uploaded) = {
            let store =
                Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
            let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
            let image = service
                .create(
                    &auth("project-a"),
                    "test".to_owned(),
                    "private".to_owned(),
                    "bare".to_owned(),
                    "raw".to_owned(),
                )
                .await?;
            let uploaded = service
                .upload(&auth("project-a"), image.id, content)
                .await?;
            (image.id, uploaded)
        };
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, reopened_store).await?;
        assert_eq!(
            service.list(&auth("project-a")).await?,
            vec![uploaded.clone()]
        );
        assert_eq!(service.get(&auth("project-a"), image_id).await?, uploaded);
        let artifact = service
            .resolve_artifact(&auth("project-a"), image_id)
            .await?;
        assert_eq!(artifact.content, content);
        assert!(!path.join("metadata.json").exists());
        fs::remove_dir_all(path)?;
        fs::remove_file(&sqlite_path)?;
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn image_bytes_remain_outside_sqlite() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("bytes-outside-db");
        let sqlite_path = format!("{}.sqlite", path.display());
        let payload = (0..1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store.clone()).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service
            .upload(&auth("project-a"), image.id, &payload)
            .await?;
        drop(service);
        drop(store);
        // The content file is the only place the payload may live; neither
        // the SQLite main file nor its WAL journal (file stores run in WAL
        // mode) may contain the bytes as a contiguous sequence. A 4 KiB chunk
        // of the patterned payload stands in for the full 1 MiB so the scan
        // stays fast while remaining distinctive.
        let mut database = fs::read(&sqlite_path)?;
        if let Ok(wal) = fs::read(format!("{sqlite_path}-wal")) {
            database.extend_from_slice(&wal);
        }
        let chunk = &payload[..4096];
        assert!(
            !database.windows(chunk.len()).any(|window| window == chunk),
            "image payload bytes leaked into the durable store"
        );
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, reopened_store).await?;
        let artifact = service
            .resolve_artifact(&auth("project-a"), image.id)
            .await?;
        assert_eq!(artifact.size, payload.len() as u64);
        assert_eq!(artifact.content, payload);
        fs::remove_dir_all(path)?;
        fs::remove_file(&sqlite_path)?;
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_uploads_serialize_and_keep_one_published_artifact()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("concurrent-upload");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        let bytes_a = vec![0x41u8; 4096];
        let bytes_b = vec![0x42u8; 4096];
        let auth_a = auth("project-a");
        let auth_b = auth("project-a");
        let (first, second) = tokio::join!(
            service.upload(&auth_a, image.id, &bytes_a),
            service.upload(&auth_b, image.id, &bytes_b),
        );
        // The mutation lock serializes the two uploads: exactly one activates
        // the record and the loser sees the already-active conflict without
        // touching the published content file.
        assert_eq!([&first, &second].iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            [&first, &second]
                .iter()
                .filter(|r| matches!(r, Err(ImageError::Conflict)))
                .count(),
            1
        );
        let winner = first
            .or(second)
            .map_err(|_| "expected exactly one upload to succeed")?;
        let artifact = service
            .resolve_artifact(&auth("project-a"), image.id)
            .await?;
        assert_eq!(artifact.size, 4096);
        let sealed = format!("{:x}", Sha256::digest(&artifact.content));
        assert_eq!(winner.checksum.as_deref(), Some(sealed.as_str()));
        assert!(artifact.content == bytes_a || artifact.content == bytes_b);
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn active_metadata_with_missing_artifact_fails_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("missing-artifact");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        let uploaded = service
            .upload(&auth("project-a"), image.id, b"image-bytes")
            .await?;
        fs::remove_file(path.join("content").join(image.id.to_string()))?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::NotFound)
        ));
        let record = service.get(&auth("project-a"), image.id).await?;
        assert_eq!(record.status, ImageStatus::Active);
        assert_eq!(record.size, uploaded.size);
        assert_eq!(record.checksum, uploaded.checksum);
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn active_metadata_with_corrupt_artifact_fails_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("corrupt-artifact");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service
            .upload(&auth("project-a"), image.id, b"image-bytes")
            .await?;
        fs::write(
            path.join("content").join(image.id.to_string()),
            b"tampered!",
        )?;
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::ChecksumMismatch)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn content_addressed_cache_is_atomic_and_rejects_bad_inputs()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("cache");
        let cache = ImageCache::open(&path, 1024)?;
        let content = b"verified-image";
        let checksum = format!("{:x}", Sha256::digest(content));
        let first = cache.cache_base(&checksum, "raw", content)?;
        let second = cache.cache_base(&checksum, "raw", content)?;
        assert_eq!(first, second);
        fs::write(&first, b"corrupted-cache-entry")?;
        let repaired = cache.cache_base(&checksum, "raw", content)?;
        assert_eq!(repaired, first);
        assert_eq!(fs::read(&repaired)?, content);
        assert!(matches!(
            cache.cache_base(&checksum, "vmdk", content),
            Err(ImageError::UnsupportedFormat)
        ));
        assert!(matches!(
            cache.cache_base(&"0".repeat(64), "raw", content),
            Err(ImageError::ChecksumMismatch)
        ));
        assert!(matches!(
            cache.create_overlay("../escape", &first),
            Err(ImageError::InvalidPath)
        ));
        let base = path.join("base").join("test.qcow2");
        fs::write(&base, b"not-a-real-qcow2")?;
        let temporary = path
            .join("overlays")
            .join(format!(".test-instance.tmp-{}", std::process::id()));
        let _ = cache.create_overlay("test-instance", &base);
        assert!(!temporary.exists());
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn qcow2_cache_requires_qemu_img_format_evidence() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("cache-format-verification");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        fs::write(
            &fake_qemu,
            r#"#!/bin/sh
set -eu
case "$1" in
  info)
    if grep -q valid-qcow2 "$3"; then
      printf '{"format":"qcow2"}\n'
    else
      printf '{"format":"raw"}\n'
    fi
    ;;
  *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;

        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let cache = ImageCache::open_with_qemu_img(&path, 1024, &fake_qemu)?;
            let valid = b"valid-qcow2";
            let valid_checksum = format!("{:x}", Sha256::digest(valid));
            let valid_path = cache.cache_base(&valid_checksum, "qcow2", valid)?;
            assert!(valid_path.is_file());
            fs::write(
                &fake_qemu,
                "#!/bin/sh\nprintf '{\\\"format\\\":\\\"raw\\\"}\\n'\n",
            )?;
            fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;
            assert!(matches!(
                cache.cache_base(&valid_checksum, "qcow2", valid),
                Err(ImageError::FormatVerificationFailed)
            ));
            assert!(valid_path.is_file());

            let invalid = b"not-a-qcow2";
            let invalid_checksum = format!("{:x}", Sha256::digest(invalid));
            assert!(matches!(
                cache.cache_base(&invalid_checksum, "qcow2", invalid),
                Err(ImageError::FormatVerificationFailed)
            ));
            assert!(
                !path
                    .join("base")
                    .join(format!("{invalid_checksum}.qcow2"))
                    .exists()
            );
            Ok(())
        })();

        let _ = fs::remove_dir_all(&path);
        result
    }

    #[cfg(unix)]
    #[test]
    fn qemu_img_output_is_bounded_and_hostile_output_fails_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("qemu-output-bound");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        fs::write(
            &fake_qemu,
            "#!/bin/sh\n/usr/bin/head -c 1048577 /dev/zero >&2\n",
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;
        let cache = ImageCache::open_with_qemu_img(&path, 1024, &fake_qemu)?;
        let content = b"hostile-qemu-output";
        let checksum = format!("{:x}", Sha256::digest(content));
        assert!(matches!(
            cache.cache_base(&checksum, "qcow2", content),
            Err(ImageError::FormatVerificationFailed)
        ));
        assert!(!path.join("base").join(format!("{checksum}.qcow2")).exists());
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn qemu_img_helper_receives_resource_limits() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("qemu-resource-limits");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        let limits = path.join("limits.txt");
        fs::write(
            &fake_qemu,
            format!(
                "#!/bin/sh\nawk '/Max address space|Max processes|Max open files/ {{print}}' /proc/self/limits > '{}'\nprintf '{{\"format\":\"qcow2\"}}\\n'\n",
                limits.display()
            ),
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;
        let cache = ImageCache::open_with_qemu_img(&path, 1024, &fake_qemu)?;
        let content = b"resource-limited-qcow2";
        let checksum = format!("{:x}", Sha256::digest(content));
        let result = cache.cache_base(&checksum, "qcow2", content);
        assert!(
            result.is_ok(),
            "qemu-img resource-limit probe failed: {result:?}"
        );
        let limits = fs::read_to_string(limits)?;
        assert!(limits.contains("Max address space         1073741824"));
        // RLIMIT_NPROC is deliberately NOT set: it is enforced per real UID
        // across the whole system, so a low bound would break the helper
        // whenever the service account already runs other threads (CI runners
        // with parallel test processes, or a busy o3k-compute account).
        // Assert it is not artificially constrained instead.
        assert!(!limits.contains("Max processes             32"));
        assert!(limits.contains("Max open files            128"));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn qcow2_header_dependencies_are_rejected_before_helper_invocation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("qcow2-header-dependencies");
        fs::create_dir_all(&path)?;
        for (name, offset, incompatible, rejected) in [
            ("backing", 4096_u64, 0_u64, true),
            ("external-data", 0_u64, 1_u64 << 2, true),
            ("standalone", 0_u64, 0_u64, false),
        ] {
            let image = path.join(name);
            let mut header = vec![0_u8; 104];
            header[..4].copy_from_slice(b"QFI\xfb");
            header[4..8].copy_from_slice(&3_u32.to_be_bytes());
            header[8..16].copy_from_slice(&offset.to_be_bytes());
            header[72..80].copy_from_slice(&incompatible.to_be_bytes());
            fs::write(&image, header)?;
            assert_eq!(
                reject_qcow2_dependencies(&image).is_err(),
                rejected,
                "unexpected dependency decision for {name}"
            );
        }
        let marker = path.join("helper-invoked");
        let fake_qemu = path.join("qemu-img");
        fs::write(
            &fake_qemu,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nprintf '{{\\\"format\\\":\\\"qcow2\\\"}}\\n'\n",
                marker.display()
            ),
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;
        let cache = ImageCache::open_with_qemu_img(path.join("cache"), 1024, &fake_qemu)?;
        let mut hostile = vec![0_u8; 104];
        hostile[..4].copy_from_slice(b"QFI\xfb");
        hostile[4..8].copy_from_slice(&3_u32.to_be_bytes());
        hostile[8..16].copy_from_slice(&4096_u64.to_be_bytes());
        let checksum = format!("{:x}", Sha256::digest(&hostile));
        assert!(matches!(
            cache.cache_base(&checksum, "qcow2", &hostile),
            Err(ImageError::FormatVerificationFailed)
        ));
        assert!(
            !marker.exists(),
            "qemu-img was invoked before header rejection"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn uploaded_qcow2_with_any_backing_relationship_is_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("cache-rejects-qcow-backing");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        fs::write(
            &fake_qemu,
            r#"#!/bin/sh
printf '{"format":"qcow2","backing-filename":"/tmp/tenant-base"}\n'
"#,
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;

        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let cache = ImageCache::open_with_qemu_img(&path, 1024, &fake_qemu)?;
            let content = b"qcow-with-backing";
            let checksum = format!("{:x}", Sha256::digest(content));
            assert!(matches!(
                cache.cache_base(&checksum, "qcow2", content),
                Err(ImageError::FormatVerificationFailed)
            ));
            assert!(!path.join("base").join(format!("{checksum}.qcow2")).exists());
            Ok(())
        })();

        let _ = fs::remove_dir_all(&path);
        result
    }

    #[test]
    fn restart_cleans_overlay_temporaries_without_touching_published_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("overlay-restart");
        let _ = fs::remove_dir_all(&path);
        let cache = ImageCache::open(&path, 1024)?;
        let stale = path
            .join("overlays")
            .join(format!(".instance.tmp-{}", Uuid::now_v7()));
        let stale_base =
            path.join("base")
                .join(format!("base-{}.tmp-{}", "a".repeat(64), Uuid::now_v7()));
        let published = path.join("overlays").join("instance.qcow2");
        let unrelated = path.join("overlays").join("keep.txt");
        let unrelated_temporary = path.join("overlays").join("foo.tmp-user");
        fs::write(&stale, b"stale")?;
        fs::write(&stale_base, b"stale-base")?;
        fs::write(&published, b"published")?;
        fs::write(&unrelated, b"keep")?;
        fs::write(&unrelated_temporary, b"keep")?;

        let _reopened = ImageCache::open(&path, 1024)?;
        assert!(!stale.exists());
        assert!(!stale_base.exists());
        assert_eq!(fs::read(&published)?, b"published");
        assert_eq!(fs::read(&unrelated)?, b"keep");
        assert_eq!(fs::read(&unrelated_temporary)?, b"keep");
        fs::remove_dir_all(path)?;
        drop(cache);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn overlay_requires_verified_qcow2_backing_and_cleans_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("cache-qemu-verification");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        fs::write(
            &fake_qemu,
            r#"#!/bin/sh
set -eu
case "$1" in
  create)
    : > "$8"
    ;;
  info)
    case "$3" in
      */base/*) printf '{"format":"qcow2"}\n'; exit 0 ;;
    esac
    backing="$(find "$(dirname "$3")/../base" \( -name '*.qcow2' -o -name '*.raw' \) -print -quit)"
    case "$(basename "$3")" in
      *wrong-format*) format=raw; reported="$backing" ;;
      *wrong-backing*) format=qcow2; reported="/tmp/o3k-foreign-base" ;;
      *) format=qcow2; reported="$backing" ;;
    esac
    case "$(basename "$3")" in
      *missing-backing*) printf '{"format":"qcow2"}\n' ;;
      *) printf '{"format":"%s","backing-filename":"%s"}\n' "$format" "$reported" ;;
    esac
    ;;
  *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let cache = ImageCache::open_with_qemu_img(&path, 1024, &fake_qemu)?;
            let base_content = b"base";
            let base_checksum = format!("{:x}", Sha256::digest(base_content));
            let base = cache.cache_base(&base_checksum, "raw", base_content)?;

            let overlay = cache.create_overlay("valid", &base)?;
            assert!(overlay.is_file());
            assert_eq!(cache.create_overlay("valid", &base)?, overlay);
            let foreign = path.join("base").join("foreign.raw");
            fs::write(&foreign, b"foreign")?;
            assert!(matches!(
                cache.create_overlay("foreign", &foreign),
                Err(ImageError::InvalidPath)
            ));
            fs::write(&base, b"tampered")?;
            assert!(matches!(
                cache.create_overlay("tampered", &base),
                Err(ImageError::ChecksumMismatch)
            ));
            fs::write(&base, base_content)?;

            for instance in ["wrong-format", "wrong-backing", "missing-backing"] {
                assert!(matches!(
                    cache.create_overlay(instance, &base),
                    Err(ImageError::OverlayFailed)
                ));
                assert!(
                    !path
                        .join("overlays")
                        .join(format!("{instance}.qcow2"))
                        .exists()
                );
                assert!(
                    !fs::read_dir(path.join("overlays"))?.flatten().any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!(".{instance}.tmp-"))
                    })
                );
            }
            Ok(())
        })();

        let _ = fs::remove_dir_all(&path);
        result
    }

    #[cfg(unix)]
    #[test]
    fn cache_rejects_symlinked_base_and_overlay_escape() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let path = root("cache-symlink-safety");
        let cache = ImageCache::open(&path, 1024)?;
        let content = b"verified-image";
        let checksum = format!("{:x}", Sha256::digest(content));
        let outside = path.with_file_name(format!("o3k-image-outside-{}", std::process::id()));
        fs::write(&outside, content)?;

        let cached_base = path.join("base").join(format!("{checksum}.raw"));
        symlink(&outside, &cached_base)?;
        assert!(matches!(
            cache.cache_base(&checksum, "raw", content),
            Err(ImageError::InvalidPath)
        ));
        assert_eq!(fs::read(&outside)?, content);

        fs::remove_file(&cached_base)?;
        let base = cache.cache_base(&checksum, "raw", content)?;
        let symlinked_base = path.join("base").join("symlinked-base.raw");
        symlink(&outside, &symlinked_base)?;
        assert!(matches!(
            cache.create_overlay("symlinked-base", &symlinked_base),
            Err(ImageError::InvalidPath)
        ));
        fs::remove_file(&symlinked_base)?;

        let sibling_base_dir = path.with_file_name("base-evil");
        fs::create_dir(&sibling_base_dir)?;
        let sibling_base = sibling_base_dir.join("sibling.raw");
        fs::write(&sibling_base, content)?;
        assert!(matches!(
            cache.create_overlay("sibling", &sibling_base),
            Err(ImageError::InvalidPath)
        ));

        let escaped_overlay = path.join("overlays").join("instance.qcow2");
        symlink(&outside, &escaped_overlay)?;
        assert!(matches!(
            cache.create_overlay("instance", &base),
            Err(ImageError::InvalidPath)
        ));
        assert_eq!(fs::read(&outside)?, content);

        fs::remove_file(&escaped_overlay)?;
        fs::create_dir(&escaped_overlay)?;
        assert!(matches!(
            cache.create_overlay("instance", &base),
            Err(ImageError::InvalidPath)
        ));

        fs::remove_dir_all(path)?;
        fs::remove_dir_all(sibling_base_dir)?;
        fs::remove_file(outside)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cache_rejects_symlinked_managed_directories() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let path = root("cache-symlink-directories");
        let outside = root("cache-symlink-directories-outside");
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside)?;
        fs::create_dir_all(&path)?;
        symlink(outside.join("base"), path.join("base"))?;
        assert!(matches!(
            ImageCache::open(&path, 1024),
            Err(ImageError::InvalidPath)
        ));
        fs::remove_file(path.join("base"))?;
        fs::create_dir(path.join("base"))?;
        symlink(outside.join("overlays"), path.join("overlays"))?;
        assert!(matches!(
            ImageCache::open(&path, 1024),
            Err(ImageError::InvalidPath)
        ));
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_dir_all(outside);
        Ok(())
    }

    /// Builds a small structurally valid qcow2: 4 KiB clusters, one L1 entry,
    /// one L2 table, one refcount table cluster, one refcount block, and
    /// three allocated data clusters, with consistent refcounts. Layout:
    /// cluster 0 header, 1 L1 table, 2 L2 table, 3 refcount table,
    /// 4 refcount block, 5-7 data clusters.
    fn valid_qcow2_fixture() -> Vec<u8> {
        const CLUSTER: usize = 4096;
        let mut image = vec![0_u8; CLUSTER * 8];
        image[0..4].copy_from_slice(b"QFI\xfb");
        image[4..8].copy_from_slice(&3_u32.to_be_bytes());
        image[20..24].copy_from_slice(&12_u32.to_be_bytes());
        image[24..32].copy_from_slice(&(2 * 1024 * 1024_u64).to_be_bytes());
        image[36..40].copy_from_slice(&1_u32.to_be_bytes());
        image[40..48].copy_from_slice(&(CLUSTER as u64).to_be_bytes());
        image[48..56].copy_from_slice(&(3 * CLUSTER as u64).to_be_bytes());
        image[56..60].copy_from_slice(&1_u32.to_be_bytes());
        image[96..100].copy_from_slice(&4_u32.to_be_bytes());
        image[100..104].copy_from_slice(&104_u32.to_be_bytes());
        image[CLUSTER..CLUSTER + 8]
            .copy_from_slice(&((1_u64 << 63) | (2 * CLUSTER as u64)).to_be_bytes());
        for (index, cluster) in [5_usize, 6, 7].into_iter().enumerate() {
            let offset = 2 * CLUSTER + index * 8;
            image[offset..offset + 8].copy_from_slice(
                &((1_u64 << 63) | (cluster as u64 * CLUSTER as u64)).to_be_bytes(),
            );
        }
        let offset = 3 * CLUSTER;
        image[offset..offset + 8].copy_from_slice(&(4 * CLUSTER as u64).to_be_bytes());
        for cluster in 0..8 {
            let offset = 4 * CLUSTER + cluster * 2;
            image[offset..offset + 2].copy_from_slice(&1_u16.to_be_bytes());
        }
        image
    }

    fn validate_bytes(content: &[u8]) -> Result<(), ImageError> {
        let mut reader = std::io::Cursor::new(content);
        validate_qcow2_structure(&mut reader, content.len() as u64)
    }

    #[test]
    fn qcow2_valid_fixture_passes_structural_validation() -> Result<(), Box<dyn std::error::Error>>
    {
        let image = valid_qcow2_fixture();
        validate_bytes(&image)?;
        // Version 2 images use the same table layout and must pass the same
        // walk (the v3-only fields are ignored).
        let mut version2 = image.clone();
        version2[4..8].copy_from_slice(&2_u32.to_be_bytes());
        validate_bytes(&version2)?;
        Ok(())
    }

    #[test]
    fn qcow2_truncated_payloads_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let image = valid_qcow2_fixture();
        // ~5% of the image: only the header and part of the L1 area remain.
        assert!(matches!(
            validate_bytes(&image[..image.len() / 20]),
            Err(ImageError::FormatVerificationFailed)
        ));
        // Metadata intact but the first data cluster cut in half: the L2
        // extent walk must notice the missing payload.
        let cut = 5 * 4096 + 2048;
        assert!(matches!(
            validate_bytes(&image[..cut]),
            Err(ImageError::FormatVerificationFailed)
        ));
        // An empty prefix cannot even carry the header.
        assert!(matches!(
            validate_bytes(&[]),
            Err(ImageError::FormatVerificationFailed)
        ));
        Ok(())
    }

    #[test]
    fn qcow2_corrupt_headers_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let original = valid_qcow2_fixture();
        // magic
        let mut bad = original.clone();
        bad[0] ^= 0xff;
        assert!(validate_bytes(&bad).is_err());
        // version
        let mut bad = original.clone();
        bad[4..8].copy_from_slice(&1_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // backing file reference
        let mut bad = original.clone();
        bad[8..16].copy_from_slice(&4096_u64.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[16..20].copy_from_slice(&1_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // encryption
        let mut bad = original.clone();
        bad[32..36].copy_from_slice(&2_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // cluster_bits
        let mut bad = original.clone();
        bad[20..24].copy_from_slice(&8_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[20..24].copy_from_slice(&22_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // disk_size
        let mut bad = original.clone();
        bad[24..32].copy_from_slice(&0_u64.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[24..32].copy_from_slice(&(1_u64 << 63).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // l1_size
        let mut bad = original.clone();
        bad[36..40].copy_from_slice(&0_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // l1_table_offset: zero, misaligned, or beyond EOF
        let mut bad = original.clone();
        bad[40..48].copy_from_slice(&0_u64.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[40..48].copy_from_slice(&(4096_u64 + 8).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[40..48].copy_from_slice(&(original.len() as u64).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // L1 table smaller than the virtual disk needs
        let mut bad = original.clone();
        bad[24..32].copy_from_slice(&(3 * 1024 * 1024_u64).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // refcount table: no clusters, or offset beyond EOF
        let mut bad = original.clone();
        bad[56..60].copy_from_slice(&0_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[48..56].copy_from_slice(&(original.len() as u64).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // snapshots announced without a snapshot table
        let mut bad = original.clone();
        bad[60..64].copy_from_slice(&1_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // v3 header_length: too small, or not a multiple of 8
        let mut bad = original.clone();
        bad[100..104].copy_from_slice(&96_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        let mut bad = original.clone();
        bad[100..104].copy_from_slice(&105_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // v3 refcount_order beyond the allowed width
        let mut bad = original.clone();
        bad[96..100].copy_from_slice(&7_u32.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // incompatible feature bits: corrupt, external data file, extended
        // L2 entries, and unknown bits must fail closed
        for bit in [2_u64, 4, 16, 1 << 20] {
            let mut bad = original.clone();
            bad[72..80].copy_from_slice(&bit.to_be_bytes());
            assert!(
                validate_bytes(&bad).is_err(),
                "incompatible feature bit {bit} must be rejected"
            );
        }
        // dirty and compression-type bits are structurally harmless
        let mut accepted = original.clone();
        accepted[72..80].copy_from_slice(&((1_u64 << 0) | (1_u64 << 3)).to_be_bytes());
        validate_bytes(&accepted)?;
        Ok(())
    }

    #[test]
    fn qcow2_tables_referencing_beyond_eof_are_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let original = valid_qcow2_fixture();
        let len = original.len() as u64;
        // L2 table beyond EOF (L1 entry 0).
        let mut bad = original.clone();
        bad[4096..4104].copy_from_slice(&((1_u64 << 63) | len).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // Data cluster beyond EOF (L2 entry 0).
        let mut bad = original.clone();
        bad[8192..8200].copy_from_slice(&((1_u64 << 63) | len).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // Misaligned data cluster (L2 entry 1).
        let mut bad = original.clone();
        bad[8200..8208].copy_from_slice(&((1_u64 << 63) | (5 * 4096 + 512)).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // Compressed cluster whose 512-byte sector extent crosses EOF
        // (L2 entry 1). With cluster_bits=12 the compressed offset field is
        // 58 bits wide and the sector count is stored in bits 58-61.
        let offset_bits = 62 - (12 - 8);
        let mut bad = original.clone();
        let entry = (1_u64 << 62) | (len - 2048) | (4_u64 << offset_bits);
        bad[8200..8208].copy_from_slice(&entry.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // A compressed cluster fully inside the payload is accepted
        // (L2 entry 2).
        let mut good = original.clone();
        let entry = (1_u64 << 62) | (5 * 4096_u64) | (0_u64 << offset_bits);
        good[8208..8216].copy_from_slice(&entry.to_be_bytes());
        validate_bytes(&good)?;
        // Refcount block beyond EOF (refcount table entry 0).
        let mut bad = original.clone();
        bad[12288..12296].copy_from_slice(&len.to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        // Misaligned refcount block.
        let mut bad = original.clone();
        bad[12288..12296].copy_from_slice(&(4 * 4096_u64 + 512).to_be_bytes());
        assert!(validate_bytes(&bad).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn upload_rejects_truncated_qcow2_without_activating_the_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("upload-truncated-qcow2");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "truncated".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "qcow2".to_owned(),
            )
            .await?;
        let fixture = valid_qcow2_fixture();
        for truncated in [&fixture[..fixture.len() / 20], &fixture[..5 * 4096 + 2048]] {
            assert!(matches!(
                service
                    .upload(&auth("project-a"), image.id, truncated)
                    .await,
                Err(ImageError::FormatVerificationFailed)
            ));
        }
        // The record stays queued and unusable: no size, no checksum, no
        // published content file, no resolvable artifact.
        let record = service.get(&auth("project-a"), image.id).await?;
        assert_eq!(record.status, ImageStatus::Queued);
        assert_eq!(record.size, None);
        assert_eq!(record.checksum, None);
        assert!(!path.join("content").join(image.id.to_string()).exists());
        assert!(matches!(
            service.resolve_artifact(&auth("project-a"), image.id).await,
            Err(ImageError::NotFound)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn upload_accepts_valid_qcow2_and_rejects_garbage()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("upload-valid-qcow2");
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, store).await?;
        let image = service
            .create(
                &auth("project-a"),
                "valid".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "qcow2".to_owned(),
            )
            .await?;
        let fixture = valid_qcow2_fixture();
        let uploaded = service
            .upload(&auth("project-a"), image.id, &fixture)
            .await?;
        assert_eq!(uploaded.status, ImageStatus::Active);
        assert_eq!(uploaded.size, Some(fixture.len() as u64));
        let artifact = service
            .resolve_artifact(&auth("project-a"), image.id)
            .await?;
        assert_eq!(artifact.content, fixture);
        // Random bytes and a plausible-looking non-qcow2 file follow the same
        // format policy: rejected with a terminal error, record stays queued.
        let garbage = vec![0x5a; 4096];
        let non_qcow2 = b"not a qcow2 image at all, just a long enough non-magic payload here!";
        for bad in [&garbage[..], &non_qcow2[..]] {
            let image = service
                .create(
                    &auth("project-a"),
                    "bad".to_owned(),
                    "private".to_owned(),
                    "bare".to_owned(),
                    "qcow2".to_owned(),
                )
                .await?;
            assert!(matches!(
                service.upload(&auth("project-a"), image.id, bad).await,
                Err(ImageError::FormatVerificationFailed)
            ));
            assert_eq!(
                service.get(&auth("project-a"), image.id).await?.status,
                ImageStatus::Queued
            );
        }
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn qcow2_base_failing_qemu_img_check_is_rejected_before_overlay()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let path = root("cache-qemu-check");
        let _ = fs::remove_dir_all(&path);
        let fake_bin = path.join("fake-bin");
        fs::create_dir_all(&fake_bin)?;
        let fake_qemu = fake_bin.join("qemu-img");
        fs::write(
            &fake_qemu,
            r#"#!/bin/sh
set -eu
case "$1" in
  create)
    : > "$8"
    ;;
  info)
    case "$3" in
      */base/*) printf '{"format":"qcow2"}\n' ;;
      *) backing="$(find "$(dirname "$3")/../base" -name '*.qcow2' -print -quit)"; printf '{"format":"qcow2","backing-filename":"%s"}\n' "$backing" ;;
    esac
    ;;
  check)
    if grep -q broken-extent "$2"; then
      exit 2
    fi
    exit 0
    ;;
  *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755))?;

        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let cache = ImageCache::open_with_qemu_img(&path, 1024 * 1024, &fake_qemu)?;
            let clean = valid_qcow2_fixture();
            let clean_checksum = format!("{:x}", Sha256::digest(&clean));
            let clean_base = cache.cache_base(&clean_checksum, "qcow2", &clean)?;
            let overlay = cache.create_overlay("clean", &clean_base)?;
            assert!(overlay.is_file());

            let mut broken = valid_qcow2_fixture();
            broken[104..120].copy_from_slice(b"broken-extent-pa");
            let broken_checksum = format!("{:x}", Sha256::digest(&broken));
            let broken_base = cache.cache_base(&broken_checksum, "qcow2", &broken)?;
            assert!(matches!(
                cache.create_overlay("broken", &broken_base),
                Err(ImageError::FormatVerificationFailed)
            ));
            assert!(!path.join("overlays").join("broken.qcow2").exists());
            assert!(
                !fs::read_dir(path.join("overlays"))?.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".broken.tmp-")
                })
            );
            Ok(())
        })();

        let _ = fs::remove_dir_all(&path);
        result
    }

    /// Issue #607: `O3K_TEST_QEMU_IMG_FAIL=1` must make the sandboxed helper
    /// fail deterministically before any spawn (honored regardless of PATH,
    /// because the process reads it itself), and verification call sites must
    /// fail closed with `FormatVerificationFailed`. The env var is
    /// process-global and the workspace forbids unsafe code, so the armed
    /// assertions run in a child process of this test binary — the child
    /// re-runs `qemu_img_failpoint_env_armed_asserts_injected_failure` with
    /// the env var set. On unarmed code the child's assertions fail, so this
    /// test fails before the failpoint exists and passes after. Unset, the
    /// helper keeps its normal behavior (proven here and by every other test
    /// in this crate, which all run unarmed).
    #[cfg(unix)]
    #[test]
    fn qemu_img_test_failpoint_env_var_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "tests::qemu_img_failpoint_env_armed_asserts_injected_failure",
            ])
            .env(O3K_TEST_QEMU_IMG_FAIL, "1")
            .spawn()?;
        let status = child.wait()?;
        assert!(
            status.success(),
            "the armed failpoint child must pass its injected-failure assertions"
        );
        // Unset (the default in this process), the helper keeps its normal
        // behavior: the sandboxed spawn still happens and the missing
        // qemu-img binary surfaces as a failed output, never as the injected
        // io error.
        let unchanged = run_qemu_img(Path::new("does-not-exist-qemu-img"), ["info", "unused"]);
        let unchanged = match unchanged {
            Ok(output) => output,
            Err(error) => {
                return Err(format!(
                    "the unset failpoint must preserve the normal helper behavior: {error}"
                )
                .into());
            }
        };
        assert!(
            !unchanged.status.success(),
            "the unchanged helper must fail on the missing qemu-img binary"
        );
        Ok(())
    }

    /// Runs only as the child of `qemu_img_test_failpoint_env_var_fails_closed`
    /// with `O3K_TEST_QEMU_IMG_FAIL=1`: asserts the injected `io::Error` at
    /// the helper boundary and the closed failure at a verification call
    /// site. A direct (unarmed) run has nothing to assert and passes.
    #[cfg(unix)]
    #[test]
    fn qemu_img_failpoint_env_armed_asserts_injected_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os(O3K_TEST_QEMU_IMG_FAIL).as_deref() != Some(std::ffi::OsStr::new("1")) {
            return Ok(());
        }
        let injected = match run_qemu_img(Path::new("does-not-exist-qemu-img"), ["info", "unused"])
        {
            Err(error) => error,
            Ok(_) => return Err("the armed failpoint must inject an io error".into()),
        };
        assert_eq!(injected.kind(), io::ErrorKind::Other);
        assert_eq!(
            injected.to_string(),
            "qemu-img failure injected by O3K_TEST_QEMU_IMG_FAIL"
        );
        let path = root("qemu-failpoint-child");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        let image = path.join("image.qcow2");
        fs::write(&image, b"not a qcow2")?;
        assert!(matches!(
            verify_image_format(Path::new("does-not-exist-qemu-img"), &image, "qcow2"),
            Err(ImageError::FormatVerificationFailed)
        ));
        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[test]
    fn real_cirros_truncation_is_rejected_when_fixture_present()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = "/tmp/cirros-0.6.3-x86_64-disk.img.asr021";
        let Ok(full) = fs::read(fixture) else {
            eprintln!("skipping: {fixture} is not present on this host");
            return Ok(());
        };
        // The untouched real-host image (compressed and standard clusters,
        // 112-byte v3 header) must pass the structural walk.
        validate_bytes(&full)?;
        // The first 1 MiB of the 21 MiB image -- the exact ASR-021
        // corrupted-truncated-image injection -- must be rejected.
        assert!(full.len() > 1024 * 1024);
        assert!(matches!(
            validate_bytes(&full[..1024 * 1024]),
            Err(ImageError::FormatVerificationFailed)
        ));
        Ok(())
    }
}
