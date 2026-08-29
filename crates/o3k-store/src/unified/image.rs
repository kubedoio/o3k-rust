use async_trait::async_trait;
use uuid::Uuid;

use crate::{ImageMetadataRecord, ImageRepository, StoreError};

use super::O3kStore;

#[async_trait]
impl ImageRepository for O3kStore {
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_image(image).await,
            Self::Postgres(s) => s.insert_image(image).await,
        }
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_images(project_id).await,
            Self::Postgres(s) => s.list_images(project_id).await,
        }
    }

    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_image(project_id, id).await,
            Self::Postgres(s) => s.get_image(project_id, id).await,
        }
    }

    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.activate_image(project_id, id, size, checksum).await,
            Self::Postgres(s) => s.activate_image(project_id, id, size, checksum).await,
        }
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_image(project_id, id).await,
            Self::Postgres(s) => s.delete_image(project_id, id).await,
        }
    }
}
