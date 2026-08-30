use super::*;
use async_trait::async_trait;

impl SqliteStore {
    pub async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        let result = sqlx::query(
            "INSERT INTO image_metadata (id, name, project_id, status, visibility, container_format, disk_format, size, checksum) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(image.id.to_string())
        .bind(&image.name)
        .bind(&image.project_id)
        .bind(&image.status)
        .bind(&image.visibility)
        .bind(&image.container_format)
        .bind(&image.disk_format)
        .bind(image.size)
        .bind(&image.checksum)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    pub async fn list_images(
        &self,
        project_id: &str,
    ) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, project_id, status, visibility, container_format, disk_format, size, checksum FROM image_metadata WHERE project_id = ? ORDER BY name ASC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(image_metadata_from_row).collect()
    }

    pub async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, project_id, status, visibility, container_format, disk_format, size, checksum FROM image_metadata WHERE id = ? AND project_id = ?",
        )
        .bind(id.to_string())
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.as_ref().map(image_metadata_from_row).transpose()
    }

    pub async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        let size = i64::try_from(size)
            .map_err(|_| StoreError::Corrupt("image size exceeds SQLite range".to_owned()))?;
        let result = sqlx::query(
            "UPDATE image_metadata SET status = 'active', size = ?, checksum = ? WHERE id = ? AND project_id = ? AND status = 'queued'",
        )
        .bind(size)
        .bind(checksum)
        .bind(id.to_string())
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_image(project_id, id).await? {
                Some(_) => Err(StoreError::ImageAlreadyActive),
                None => Err(StoreError::ImageNotFound),
            };
        }
        self.get_image(project_id, id)
            .await?
            .ok_or(StoreError::Corrupt("activated image is missing".to_owned()))
    }

    pub async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM image_metadata WHERE id = ? AND project_id = ?")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            Err(StoreError::ImageNotFound)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl ImageRepository for SqliteStore {
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        self.insert_image(image).await
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        self.list_images(project_id).await
    }

    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        self.get_image(project_id, id).await
    }

    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        self.activate_image(project_id, id, size, checksum).await
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        self.delete_image(project_id, id).await
    }
}
