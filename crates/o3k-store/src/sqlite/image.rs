use super::{SqliteStore, helpers::image_metadata_from_row};
use async_trait::async_trait;
use uuid::Uuid;

use crate::port::durable::DurableStore;
use crate::{AuditEventRecord, ImageMetadataRecord, ImageRepository, StoreError};

impl SqliteStore {
    pub async fn list_images_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("image page limit overflow".to_owned()))?;
        let rows = if let Some(after_id) = after_id {
            sqlx::query("SELECT id, name, project_id, status, visibility, container_format, disk_format, size, checksum FROM image_metadata WHERE project_id = ? AND id > ? ORDER BY id LIMIT ?").bind(project_id).bind(after_id).bind(limit).fetch_all(&self.pool).await
        } else {
            sqlx::query("SELECT id, name, project_id, status, visibility, container_format, disk_format, size, checksum FROM image_metadata WHERE project_id = ? ORDER BY id LIMIT ?").bind(project_id).bind(limit).fetch_all(&self.pool).await
        }.map_err(StoreError::Database)?;
        rows.iter().map(image_metadata_from_row).collect()
    }
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

    pub async fn insert_image_with_audit(
        &self,
        image: &ImageMetadataRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("INSERT INTO image_metadata (id, name, project_id, status, visibility, container_format, disk_format, size, checksum) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(image.id.to_string()).bind(&image.name).bind(&image.project_id)
            .bind(&image.status).bind(&image.visibility).bind(&image.container_format)
            .bind(&image.disk_format).bind(image.size).bind(&image.checksum)
            .execute(&mut *tx).await.map_err(|error| match error {
                sqlx::Error::Database(error) if error.is_unique_violation() => StoreError::ResourceAlreadyExists,
                error => StoreError::Database(error),
            })?;
        sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id)
            .bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.effective_scope)
            .bind(&audit.service_namespace).bind(&audit.action).bind(&audit.resource_type)
            .bind(&audit.resource_id).bind(&audit.owner_scope)
            .bind(audit.operation_id.map(|id| id.to_string())).bind(&audit.outcome)
            .bind(&audit.reason_category).bind(&audit.event_json)
            .execute(&mut *tx).await.map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)
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

    pub async fn activate_image_with_audit(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
        audit: &AuditEventRecord,
    ) -> Result<ImageMetadataRecord, StoreError> {
        let size = i64::try_from(size)
            .map_err(|_| StoreError::Corrupt("image size exceeds SQLite range".to_owned()))?;
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let result = sqlx::query("UPDATE image_metadata SET status = 'active', size = ?, checksum = ? WHERE id = ? AND project_id = ? AND status = 'queued'")
            .bind(size).bind(checksum).bind(id.to_string()).bind(project_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }
        sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id)
            .bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action)
            .bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope)
            .bind(audit.operation_id.map(|v| v.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json)
            .execute(&mut *tx).await.map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)?;
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

    pub async fn delete_image_with_audit(
        &self,
        project_id: &str,
        id: &Uuid,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let result = sqlx::query("DELETE FROM image_metadata WHERE id = ? AND project_id = ?")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }
        sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id)
            .bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action)
            .bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope)
            .bind(audit.operation_id.map(|v| v.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json)
            .execute(&mut *tx).await.map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)
    }
}

#[async_trait]
impl ImageRepository for SqliteStore {
    async fn create_or_replay_canonical_image_operation(
        &self,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
    ) -> Result<crate::IdempotencyReservation, StoreError> {
        self.create_or_replay_canonical_scoped_operation(operation, canonical, request)
            .await
    }

    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        self.insert_image(image).await
    }
    async fn insert_image_with_audit(
        &self,
        image: &ImageMetadataRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        self.insert_image_with_audit(image, audit).await
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        self.list_images(project_id).await
    }
    async fn list_images_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        self.list_images_page(project_id, after_id, limit).await
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
    async fn activate_image_with_audit(
        &self,
        p: &str,
        i: &Uuid,
        s: u64,
        c: &str,
        a: &AuditEventRecord,
    ) -> Result<ImageMetadataRecord, StoreError> {
        self.activate_image_with_audit(p, i, s, c, a).await
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        self.delete_image(project_id, id).await
    }
    async fn delete_image_with_audit(
        &self,
        p: &str,
        i: &Uuid,
        a: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        self.delete_image_with_audit(p, i, a).await
    }
}
