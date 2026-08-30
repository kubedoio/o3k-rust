use super::SqliteStore;
use async_trait::async_trait;
use uuid::Uuid;

use crate::{StoreError, VolumeAttachmentRecord, VolumeAttachmentRepository};

impl SqliteStore {
    pub async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "INSERT INTO volume_attachments (id, server_id, volume_id, device, tag, delete_on_termination, created_at, status, operation_id, idempotency_key, cinder_attachment_id, connector_host, connector_ip, connector_initiator, driver_volume_type, target_iqn, target_portal, target_lun, connection_info_digest, error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.to_string())
        .bind(record.server_id.to_string())
        .bind(record.volume_id.to_string())
        .bind(&record.device)
        .bind(&record.tag)
        .bind(if record.delete_on_termination { 1 } else { 0 })
        .bind(&record.created_at)
        .bind(&record.status)
        .bind(record.operation_id.map(|id| id.to_string()))
        .bind(&record.idempotency_key)
        .bind(&record.cinder_attachment_id)
        .bind(&record.connector_host)
        .bind(&record.connector_ip)
        .bind(&record.connector_initiator)
        .bind(&record.driver_volume_type)
        .bind(&record.target_iqn)
        .bind(&record.target_portal)
        .bind(record.target_lun.map(i64::from))
        .bind(&record.connection_info_digest)
        .bind(&record.error)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    /// Advances (or regresses) an attachment's durable phase. Phase is
    /// persisted before the matching external side effect.
    pub async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        sqlx::query("UPDATE volume_attachments SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    /// Persists the non-secret outcome data observed after an external step.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        // Phase transition persistence: None leaves the durable field untouched
        // (COALESCE), so a transition that only updates status/device/one field
        // never wipes the connector or connection-information data persisted by
        // an earlier phase.
        sqlx::query(
            "UPDATE volume_attachments SET status = ?, cinder_attachment_id = COALESCE(?, cinder_attachment_id), connector_host = COALESCE(?, connector_host), connector_ip = COALESCE(?, connector_ip), connector_initiator = COALESCE(?, connector_initiator), driver_volume_type = COALESCE(?, driver_volume_type), target_iqn = COALESCE(?, target_iqn), target_portal = COALESCE(?, target_portal), target_lun = COALESCE(?, target_lun), connection_info_digest = COALESCE(?, connection_info_digest), device = COALESCE(?, device) WHERE id = ?",
        )
        .bind(status)
        .bind(cinder_attachment_id)
        .bind(connector_host)
        .bind(connector_ip)
        .bind(connector_initiator)
        .bind(driver_volume_type)
        .bind(target_iqn)
        .bind(target_portal)
        .bind(target_lun.map(i64::from))
        .bind(connection_info_digest)
        .bind(device)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    pub async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = ?")
            .bind(volume_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row =
            sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = ? AND server_id = ?")
                .bind(volume_id.to_string())
                .bind(server_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE idempotency_key = ?")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    /// Lists non-terminal attachments for restart reconciliation.
    pub async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        if terminal.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = terminal.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let query = format!(
            "SELECT * FROM volume_attachments WHERE status NOT IN ({placeholders}) ORDER BY created_at"
        );
        let mut builder = sqlx::query(&query);
        for status in terminal {
            builder = builder.bind(status);
        }
        let rows = builder
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter().map(Self::volume_attachment_from_row).collect()
    }

    pub async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM volume_attachments WHERE server_id = ? ORDER BY created_at")
                .bind(server_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(StoreError::Database)?;

        rows.iter().map(Self::volume_attachment_from_row).collect()
    }

    pub async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE server_id = ? AND id = ?")
            .bind(server_id.to_string())
            .bind(attachment_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM volume_attachments WHERE server_id = ? AND id = ?")
            .bind(server_id.to_string())
            .bind(attachment_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if result.rows_affected() == 0 {
            Err(StoreError::ResourceNotFound)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl VolumeAttachmentRepository for SqliteStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        self.insert_volume_attachment(record).await
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        self.update_volume_attachment_phase(id, status, error).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        self.update_volume_attachment_outcome(
            id,
            status,
            cinder_attachment_id,
            connector_host,
            connector_ip,
            connector_initiator,
            driver_volume_type,
            target_iqn,
            target_portal,
            target_lun,
            connection_info_digest,
            device,
        )
        .await
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_id(id).await
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_volume(volume_id).await
    }

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_volume_for_server(volume_id, server_id)
            .await
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_idempotency(idempotency_key)
            .await
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        self.list_volume_attachments_by_status(terminal).await
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        self.list_volume_attachments(server_id).await
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment(server_id, attachment_id).await
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        self.delete_volume_attachment(server_id, attachment_id)
            .await
    }
}
