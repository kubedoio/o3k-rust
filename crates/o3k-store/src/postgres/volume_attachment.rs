use async_trait::async_trait;
use uuid::Uuid;

use crate::{StoreError, VolumeAttachmentRecord, VolumeAttachmentRepository};

use super::{
    PostgresStore,
    helpers::{map_pg_error, parse_pg_volume_attachment},
};

#[async_trait]
impl VolumeAttachmentRepository for PostgresStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        let id_str = record.id.to_string();
        let srv_id_str = record.server_id.to_string();
        let vol_id_str = record.volume_id.to_string();
        let op_id_str = record.operation_id.map(|id| id.to_string());

        sqlx::query(
            "INSERT INTO volume_attachments (id, server_id, volume_id, device, tag, delete_on_termination, created_at, status, operation_id, idempotency_key, cinder_attachment_id, connector_host, connector_ip, connector_initiator, driver_volume_type, target_iqn, target_portal, target_lun, connection_info_digest, error)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
        )
        .bind(&id_str)
        .bind(&srv_id_str)
        .bind(&vol_id_str)
        .bind(&record.device)
        .bind(&record.tag)
        .bind(if record.delete_on_termination { 1i32 } else { 0i32 })
        .bind(&record.created_at)
        .bind(&record.status)
        .bind(op_id_str.as_deref())
        .bind(&record.idempotency_key)
        .bind(&record.cinder_attachment_id)
        .bind(&record.connector_host)
        .bind(&record.connector_ip)
        .bind(&record.connector_initiator)
        .bind(&record.driver_volume_type)
        .bind(&record.target_iqn)
        .bind(&record.target_portal)
        .bind(record.target_lun.map(|l| l as i32))
        .bind(&record.connection_info_digest)
        .bind(&record.error)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE volume_attachments
             SET status = $1, error = $2
             WHERE id = $3",
        )
        .bind(status)
        .bind(error)
        .bind(&id_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )));
        }

        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )))
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
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE volume_attachments
             SET status = $1, cinder_attachment_id = COALESCE($2, cinder_attachment_id),
                 connector_host = COALESCE($3, connector_host), connector_ip = COALESCE($4, connector_ip),
                 connector_initiator = COALESCE($5, connector_initiator), driver_volume_type = COALESCE($6, driver_volume_type),
                 target_iqn = COALESCE($7, target_iqn), target_portal = COALESCE($8, target_portal),
                 target_lun = COALESCE($9, target_lun), connection_info_digest = COALESCE($10, connection_info_digest),
                 device = COALESCE($11, device)
             WHERE id = $12",
        )
        .bind(status)
        .bind(cinder_attachment_id)
        .bind(connector_host)
        .bind(connector_ip)
        .bind(connector_initiator)
        .bind(driver_volume_type)
        .bind(target_iqn)
        .bind(target_portal)
        .bind(target_lun.map(|l| l as i32))
        .bind(connection_info_digest)
        .bind(device)
        .bind(&id_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )));
        }

        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )))
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE id = $1")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let vol_id_str = volume_id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = $1")
            .bind(&vol_id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let vol_id_str = volume_id.to_string();
        let srv_id_str = server_id.to_string();
        let row =
            sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = $1 AND server_id = $2")
                .bind(&vol_id_str)
                .bind(&srv_id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE idempotency_key = $1")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM volume_attachments WHERE status = ANY($1) ORDER BY created_at ASC",
        )
        .bind(terminal)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_volume_attachment).collect()
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let srv_id_str = server_id.to_string();
        let rows = sqlx::query(
            "SELECT * FROM volume_attachments WHERE server_id = $1 ORDER BY created_at ASC",
        )
        .bind(&srv_id_str)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_volume_attachment).collect()
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let srv_id_str = server_id.to_string();
        let att_id_str = attachment_id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE server_id = $1 AND id = $2")
            .bind(&srv_id_str)
            .bind(&att_id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        let srv_id_str = server_id.to_string();
        let att_id_str = attachment_id.to_string();
        let res = sqlx::query("DELETE FROM volume_attachments WHERE server_id = $1 AND id = $2")
            .bind(&srv_id_str)
            .bind(&att_id_str)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{attachment_id}` not found"
            )));
        }
        Ok(())
    }
}
