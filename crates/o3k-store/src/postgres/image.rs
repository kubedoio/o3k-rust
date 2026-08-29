use super::*;

#[async_trait]
impl ImageRepository for PostgresStore {
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        let id_str = image.id.to_string();
        sqlx::query(
            "INSERT INTO image_metadata (id, name, project_id, status, visibility, container_format, disk_format, size, checksum)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&id_str)
        .bind(&image.name)
        .bind(&image.project_id)
        .bind(&image.status)
        .bind(&image.visibility)
        .bind(&image.container_format)
        .bind(&image.disk_format)
        .bind(image.size)
        .bind(&image.checksum)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM image_metadata
             WHERE (project_id = $1 OR visibility = 'public') AND status != 'deleted'
             ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_image).collect()
    }

    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query(
            "SELECT * FROM image_metadata
             WHERE id = $1 AND (project_id = $2 OR visibility = 'public') AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_image(&r)).transpose()
    }

    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE image_metadata
             SET status = 'active', size = $1, checksum = $2
             WHERE id = $3 AND project_id = $4 AND status != 'deleted'",
        )
        .bind(size as i64)
        .bind(checksum)
        .bind(&id_str)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }

        self.get_image(project_id, id)
            .await?
            .ok_or(StoreError::ImageNotFound)
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE image_metadata
             SET status = 'deleted'
             WHERE id = $1 AND project_id = $2 AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }
        Ok(())
    }
}
