use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
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
            && let Err(error) = verify_image_format(&self.qemu_img, &temporary, format)
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
        verify_image_format(qemu_img, base, format)?;
    }
    Ok(())
}

fn ensure_managed_directory(path: &Path) -> Result<(), ImageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(ImageError::Storage)?;
            Ok(())
        }
        Ok(_) => Err(ImageError::InvalidPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ImageError::Storage)?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
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

fn run_qemu_img<'a, I>(qemu_img: &Path, args: I) -> io::Result<Output>
where
    I: IntoIterator<Item = &'a str>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let setpriv = Path::new("/usr/bin/setpriv");
    if !setpriv.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "setpriv is required to sandbox qemu-img",
        ));
    }
    let mut command = Command::new(setpriv);
    command.args([
        "--no-new-privs",
        "--ambient-caps=-all",
        "--inh-caps=-all",
        "--bounding-set=-all",
        "--reset-env",
        "--",
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
        })
    }

    pub async fn create(
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

    pub async fn list(&self, project_id: &str) -> Result<Vec<ImageRecord>, ImageError> {
        self.inner
            .repository
            .list_images(project_id)
            .await
            .map_err(Self::map_store_error)?
            .into_iter()
            .map(image_from_store)
            .collect()
    }

    pub async fn get(&self, project_id: &str, id: Uuid) -> Result<ImageRecord, ImageError> {
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
        project_id: &str,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        if content.len() > self.max_upload_bytes {
            return Err(ImageError::TooLarge);
        }
        // Upload and delete are serialized by this lock, preserving the
        // check-then-act atomicity of the previous in-memory implementation:
        // a concurrent upload of the same image must not write bytes after
        // another upload already activated the record, because the losing
        // writer would then have to remove the winner's published content
        // file. The store's conditional activate remains the authoritative
        // guard; this lock only reproduces the single-process serialization.
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
                // Roll back the published content file when the activation
                // fails; the record stays queued and remains re-uploadable.
                let _ = fs::remove_file(&content_path);
                Err(Self::map_store_error(error))
            }
        }
    }

    pub async fn delete(&self, project_id: &str, id: Uuid) -> Result<(), ImageError> {
        // Same serialization as upload so a delete cannot interleave with an
        // upload of the same image.
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "qcow2".to_owned(),
            )
            .await?;
        let uploaded = service
            .upload("project-a", image.id, b"image-bytes")
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
        assert_eq!(reopened.get("project-a", image.id).await?, uploaded);
        let artifact = reopened.resolve_artifact("project-a", image.id).await?;
        assert_eq!(artifact.id, image.id);
        assert_eq!(artifact.format, "qcow2");
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id).await,
            Err(ImageError::NotFound)
        ));
        service
            .upload("project-a", image.id, b"image-bytes")
            .await?;
        assert!(matches!(
            service.resolve_artifact("project-b", image.id).await,
            Err(ImageError::NotFound)
        ));
        fs::write(path.join("content").join(image.id.to_string()), b"tampered")?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id).await,
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service.upload("project-a", image.id, b"abc").await?;
        fs::write(
            path.join("content").join(image.id.to_string()),
            b"too-large",
        )?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id).await,
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service
            .upload("project-a", image.id, b"image-bytes")
            .await?;
        let artifact = service.resolve_artifact("project-a", image.id).await?;
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
                "project-a",
                "../outside".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        assert!(matches!(
            service.upload("project-a", image.id, b"four").await,
            Err(ImageError::TooLarge)
        ));
        assert!(matches!(
            service.get("project-b", image.id).await,
            Err(ImageError::NotFound)
        ));
        service.delete("project-a", image.id).await?;
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
                    "project-a",
                    "test".to_owned(),
                    "private".to_owned(),
                    "bare".to_owned(),
                    "raw".to_owned(),
                )
                .await?;
            let uploaded = service.upload("project-a", image.id, content).await?;
            (image.id, uploaded)
        };
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(std::path::Path::new(&sqlite_path)).await?);
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES, reopened_store).await?;
        assert_eq!(service.list("project-a").await?, vec![uploaded.clone()]);
        assert_eq!(service.get("project-a", image_id).await?, uploaded);
        let artifact = service.resolve_artifact("project-a", image_id).await?;
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service.upload("project-a", image.id, &payload).await?;
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
        let artifact = service.resolve_artifact("project-a", image.id).await?;
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        let bytes_a = vec![0x41u8; 4096];
        let bytes_b = vec![0x42u8; 4096];
        let (first, second) = tokio::join!(
            service.upload("project-a", image.id, &bytes_a),
            service.upload("project-a", image.id, &bytes_b),
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
        let artifact = service.resolve_artifact("project-a", image.id).await?;
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        let uploaded = service
            .upload("project-a", image.id, b"image-bytes")
            .await?;
        fs::remove_file(path.join("content").join(image.id.to_string()))?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id).await,
            Err(ImageError::NotFound)
        ));
        let record = service.get("project-a", image.id).await?;
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
                "project-a",
                "test".to_owned(),
                "private".to_owned(),
                "bare".to_owned(),
                "raw".to_owned(),
            )
            .await?;
        service
            .upload("project-a", image.id, b"image-bytes")
            .await?;
        fs::write(
            path.join("content").join(image.id.to_string()),
            b"tampered!",
        )?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id).await,
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
}
