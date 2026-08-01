use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const DEFAULT_MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageStatus {
    Queued,
    Active,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
            .join(format!("base-{checksum}.tmp-{}", Uuid::now_v7()));
        if let Err(error) = fs::write(&temporary, content) {
            let _ = fs::remove_file(&temporary);
            return Err(ImageError::Storage(error));
        }
        if format == "qcow2" {
            if let Err(error) = verify_image_format(&self.qemu_img, &temporary, format) {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
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

    pub fn create_overlay(&self, instance_id: &str, base: &Path) -> Result<PathBuf, ImageError> {
        let base_dir = self.root.join("base");
        let base_is_owned = base.parent() == Some(base_dir.as_path());
        let base_is_regular = match fs::symlink_metadata(base) {
            Ok(metadata) => metadata.file_type().is_file(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(ImageError::Storage(error)),
        };
        if instance_id.is_empty()
            || instance_id
                != Path::new(instance_id)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
            || !base_is_owned
            || !base_is_regular
        {
            return Err(ImageError::InvalidPath);
        }
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
        let status = std::process::Command::new(&self.qemu_img)
            .args(["create", "-f", "qcow2", "-b"])
            .arg(base)
            .arg(&temporary)
            .status()
            .map_err(|_| {
                let _ = fs::remove_file(&temporary);
                ImageError::OverlayFailed
            })?;
        if !status.success() {
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
}

fn ensure_managed_directory(path: &Path) -> Result<(), ImageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(ImageError::InvalidPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ImageError::Storage)
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
    let output = std::process::Command::new(qemu_img)
        .args(["info", "--output=json"])
        .arg(overlay)
        .output()
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

fn verify_image_format(qemu_img: &Path, image: &Path, expected: &str) -> Result<(), ImageError> {
    let output = std::process::Command::new(qemu_img)
        .args(["info", "--output=json"])
        .arg(image)
        .output()
        .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !output.status.success() {
        return Err(ImageError::FormatVerificationFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::FormatVerificationFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some(expected) {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

fn is_checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone)]
pub struct ImageService {
    inner: Arc<Mutex<Inner>>,
    max_upload_bytes: usize,
}

struct Inner {
    root: PathBuf,
    images: Vec<ImageRecord>,
}

impl ImageService {
    pub fn open(root: impl Into<PathBuf>, max_upload_bytes: usize) -> Result<Self, ImageError> {
        let root = root.into();
        ensure_managed_directory(&root)?;
        let content = root.join("content");
        ensure_managed_directory(&content)?;
        remove_temporary_files(&content, TemporaryKind::Upload)?;
        let metadata_path = root.join("metadata.json");
        let images = if metadata_path.exists() {
            let data = fs::read(&metadata_path).map_err(ImageError::Storage)?;
            serde_json::from_slice(&data).map_err(ImageError::CorruptMetadata)?
        } else {
            Vec::new()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner { root, images })),
            max_upload_bytes,
        })
    }

    pub fn create(
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
        let mut inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
        let image = ImageRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: ImageStatus::Queued,
            visibility,
            container_format,
            disk_format,
            size: None,
            checksum: None,
        };
        inner.images.push(image.clone());
        persist(&inner)?;
        Ok(image)
    }

    pub fn list(&self, project_id: &str) -> Result<Vec<ImageRecord>, ImageError> {
        let inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
        Ok(inner
            .images
            .iter()
            .filter(|image| image.project_id == project_id)
            .cloned()
            .collect())
    }

    pub fn get(&self, project_id: &str, id: Uuid) -> Result<ImageRecord, ImageError> {
        let inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
        inner
            .images
            .iter()
            .find(|image| image.id == id && image.project_id == project_id)
            .cloned()
            .ok_or(ImageError::NotFound)
    }

    pub fn resolve_artifact(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<ImageArtifact, ImageError> {
        let (format, checksum, size, path) = {
            let inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
            let image = inner
                .images
                .iter()
                .find(|image| image.id == id && image.project_id == project_id)
                .ok_or(ImageError::NotFound)?;
            if image.status != ImageStatus::Active {
                return Err(ImageError::NotFound);
            }
            let checksum = image.checksum.clone().ok_or(ImageError::NotFound)?;
            let size = image.size.ok_or(ImageError::NotFound)?;
            if !matches!(image.disk_format.as_str(), "raw" | "qcow2") {
                return Err(ImageError::UnsupportedFormat);
            }
            (
                image.disk_format.clone(),
                checksum,
                size,
                content_path(&inner.root, id),
            )
        };

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
            format,
            size,
            content,
        })
    }

    pub fn upload(
        &self,
        project_id: &str,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        if content.len() > self.max_upload_bytes {
            return Err(ImageError::TooLarge);
        }
        let mut inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
        let position = inner
            .images
            .iter()
            .position(|image| image.id == id && image.project_id == project_id)
            .ok_or(ImageError::NotFound)?;
        if inner.images[position].status == ImageStatus::Active {
            return Err(ImageError::Conflict);
        }
        let content_path = content_path(&inner.root, id);
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
        inner.images[position].status = ImageStatus::Active;
        inner.images[position].size = Some(content.len() as u64);
        inner.images[position].checksum = Some(checksum);
        if let Err(error) = persist(&inner) {
            let _ = fs::remove_file(&content_path);
            return Err(error);
        }
        Ok(inner.images[position].clone())
    }

    pub fn delete(&self, project_id: &str, id: Uuid) -> Result<(), ImageError> {
        let mut inner = self.inner.lock().map_err(|_| ImageError::Conflict)?;
        let position = inner
            .images
            .iter()
            .position(|image| image.id == id && image.project_id == project_id)
            .ok_or(ImageError::NotFound)?;
        let content = content_path(&inner.root, id);
        inner.images.remove(position);
        persist(&inner)?;
        if content.exists() {
            fs::remove_file(content).map_err(ImageError::Storage)?;
        }
        Ok(())
    }
}

fn content_path(root: &Path, id: Uuid) -> PathBuf {
    root.join("content").join(id.to_string())
}

fn persist(inner: &Inner) -> Result<(), ImageError> {
    let metadata_path = inner.root.join("metadata.json");
    let temporary_path = inner
        .root
        .join(format!("metadata.json.tmp-{}", Uuid::now_v7()));
    let encoded = serde_json::to_vec_pretty(&inner.images).map_err(ImageError::CorruptMetadata)?;
    if let Err(error) = fs::write(&temporary_path, encoded) {
        let _ = fs::remove_file(&temporary_path);
        return Err(ImageError::Storage(error));
    }
    if let Err(error) = fs::rename(&temporary_path, &metadata_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(ImageError::Storage(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn root(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/o3k-image-{label}-{}", std::process::id()))
    }

    #[test]
    fn upload_is_atomic_and_restartable() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("restart");
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES)?;
        let image = service.create(
            "project-a",
            "test".to_owned(),
            "private".to_owned(),
            "bare".to_owned(),
            "qcow2".to_owned(),
        )?;
        let uploaded = service.upload("project-a", image.id, b"image-bytes")?;
        assert_eq!(uploaded.status, ImageStatus::Active);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            entry.file_name().to_string_lossy().contains(".tmp-")
                || entry.file_name().to_string_lossy().contains("upload-")
        }));
        let reopened = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES)?;
        assert_eq!(reopened.get("project-a", image.id)?, uploaded);
        let artifact = reopened.resolve_artifact("project-a", image.id)?;
        assert_eq!(artifact.id, image.id);
        assert_eq!(artifact.format, "qcow2");
        assert_eq!(artifact.size, 11);
        assert_eq!(artifact.content, b"image-bytes");
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn artifact_resolution_rechecks_content_and_scope() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("artifact");
        let service = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES)?;
        let image = service.create(
            "project-a",
            "test".to_owned(),
            "private".to_owned(),
            "bare".to_owned(),
            "raw".to_owned(),
        )?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id),
            Err(ImageError::NotFound)
        ));
        service.upload("project-a", image.id, b"image-bytes")?;
        assert!(matches!(
            service.resolve_artifact("project-b", image.id),
            Err(ImageError::NotFound)
        ));
        fs::write(path.join("content").join(image.id.to_string()), b"tampered")?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id),
            Err(ImageError::ChecksumMismatch)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn artifact_resolution_bounds_tampered_content() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("artifact-limit");
        let service = ImageService::open(&path, 3)?;
        let image = service.create(
            "project-a",
            "test".to_owned(),
            "private".to_owned(),
            "bare".to_owned(),
            "raw".to_owned(),
        )?;
        service.upload("project-a", image.id, b"abc")?;
        fs::write(
            path.join("content").join(image.id.to_string()),
            b"too-large",
        )?;
        assert!(matches!(
            service.resolve_artifact("project-a", image.id),
            Err(ImageError::TooLarge)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn verified_service_artifact_publishes_to_cache_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let service_path = root("artifact-cache-service");
        let cache_path = root("artifact-cache-cache");
        let service = ImageService::open(&service_path, DEFAULT_MAX_UPLOAD_BYTES)?;
        let image = service.create(
            "project-a",
            "test".to_owned(),
            "private".to_owned(),
            "bare".to_owned(),
            "raw".to_owned(),
        )?;
        service.upload("project-a", image.id, b"image-bytes")?;
        let artifact = service.resolve_artifact("project-a", image.id)?;
        let cache = ImageCache::open(&cache_path, DEFAULT_MAX_CACHE_BYTES)?;

        let first = cache.cache_artifact(&artifact)?;
        let second = cache.cache_artifact(&artifact)?;
        assert_eq!(first, second);
        assert_eq!(first.id, image.id);
        assert_eq!(first.size, artifact.content.len() as u64);
        assert_eq!(fs::read(&first.path)?, artifact.content);

        let mut inconsistent = artifact;
        inconsistent.size += 1;
        assert!(matches!(
            cache.cache_artifact(&inconsistent),
            Err(ImageError::ChecksumMismatch)
        ));

        fs::remove_dir_all(service_path)?;
        fs::remove_dir_all(cache_path)?;
        Ok(())
    }

    #[test]
    fn upload_limit_and_project_isolation_are_enforced() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("limits");
        let service = ImageService::open(&path, 3)?;
        let image = service.create(
            "project-a",
            "../outside".to_owned(),
            "private".to_owned(),
            "bare".to_owned(),
            "raw".to_owned(),
        )?;
        assert!(matches!(
            service.upload("project-a", image.id, b"four"),
            Err(ImageError::TooLarge)
        ));
        assert!(matches!(
            service.get("project-b", image.id),
            Err(ImageError::NotFound)
        ));
        service.delete("project-a", image.id)?;
        assert!(!path.join("outside").exists());
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn image_service_restart_cleans_only_upload_temporaries()
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
        let _service = ImageService::open(&path, 1024)?;
        assert!(!stale.exists());
        assert_eq!(fs::read(&unrelated)?, b"keep");
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

    #[test]
    fn restart_cleans_overlay_temporaries_without_touching_published_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("overlay-restart");
        let _ = fs::remove_dir_all(&path);
        let cache = ImageCache::open(&path, 1024)?;
        let stale = path
            .join("overlays")
            .join(format!(".instance.tmp-{}", Uuid::now_v7()));
        let published = path.join("overlays").join("instance.qcow2");
        let unrelated = path.join("overlays").join("keep.txt");
        let unrelated_temporary = path.join("overlays").join("foo.tmp-user");
        fs::write(&stale, b"stale")?;
        fs::write(&published, b"published")?;
        fs::write(&unrelated, b"keep")?;
        fs::write(&unrelated_temporary, b"keep")?;

        let _reopened = ImageCache::open(&path, 1024)?;
        assert!(!stale.exists());
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
    : > "$6"
    ;;
  info)
    backing="../base/base.qcow2"
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
            let base = path.join("base").join("base.qcow2");
            fs::write(&base, b"base")?;

            let overlay = cache.create_overlay("valid", &base)?;
            assert!(overlay.is_file());
            assert_eq!(cache.create_overlay("valid", &base)?, overlay);

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
