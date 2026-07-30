use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const DEFAULT_MAX_UPLOAD_BYTES: usize = 16 * 1024 * 1024;

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
        fs::create_dir_all(root.join("content")).map_err(ImageError::Storage)?;
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
        let temporary_path = content_path.with_extension(format!("upload-{}", std::process::id()));
        fs::write(&temporary_path, content).map_err(ImageError::Storage)?;
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
        .join(format!("metadata.json.tmp-{}", std::process::id()));
    let encoded = serde_json::to_vec_pretty(&inner.images).map_err(ImageError::CorruptMetadata)?;
    fs::write(&temporary_path, encoded).map_err(ImageError::Storage)?;
    if let Err(error) = fs::rename(&temporary_path, &metadata_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(ImageError::Storage(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let reopened = ImageService::open(&path, DEFAULT_MAX_UPLOAD_BYTES)?;
        assert_eq!(reopened.get("project-a", image.id)?, uploaded);
        fs::remove_dir_all(path)?;
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
}
