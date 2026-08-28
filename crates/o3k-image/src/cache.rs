use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::internal::{
    TemporaryKind, ensure_managed_directory, is_checksum, overlay_virtual_size,
    reject_qcow2_dependencies, remove_temporary_files, validate_verified_base, verify_image_format,
    verify_overlay,
};
use crate::qemu_img::run_qemu_img;
use crate::record::{CachedImageArtifact, ImageArtifact, ImageError};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Content-addressed image cache for verified base images and overlays.
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

    pub(crate) fn open_with_qemu_img(
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
