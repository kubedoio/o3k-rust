use super::*;

#[async_trait]
impl VolumeAttachmentRepository for O3kStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_volume_attachment(record).await,
            Self::Postgres(s) => s.insert_volume_attachment(record).await,
        }
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.update_volume_attachment_phase(id, status, error).await,
            Self::Postgres(s) => s.update_volume_attachment_phase(id, status, error).await,
        }
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
        match self {
            Self::Sqlite(s) => {
                s.update_volume_attachment_outcome(
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
            Self::Postgres(s) => {
                s.update_volume_attachment_outcome(
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
        }
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment_by_id(id).await,
            Self::Postgres(s) => s.get_volume_attachment_by_id(id).await,
        }
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment_by_volume(volume_id).await,
            Self::Postgres(s) => s.get_volume_attachment_by_volume(volume_id).await,
        }
    }

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_volume_attachment_by_volume_for_server(volume_id, server_id)
                    .await
            }
            Self::Postgres(s) => {
                s.get_volume_attachment_by_volume_for_server(volume_id, server_id)
                    .await
            }
        }
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_volume_attachment_by_idempotency(idempotency_key)
                    .await
            }
            Self::Postgres(s) => {
                s.get_volume_attachment_by_idempotency(idempotency_key)
                    .await
            }
        }
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_volume_attachments_by_status(terminal).await,
            Self::Postgres(s) => s.list_volume_attachments_by_status(terminal).await,
        }
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_volume_attachments(server_id).await,
            Self::Postgres(s) => s.list_volume_attachments(server_id).await,
        }
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment(server_id, attachment_id).await,
            Self::Postgres(s) => s.get_volume_attachment(server_id, attachment_id).await,
        }
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_volume_attachment(server_id, attachment_id).await,
            Self::Postgres(s) => s.delete_volume_attachment(server_id, attachment_id).await,
        }
    }
}
