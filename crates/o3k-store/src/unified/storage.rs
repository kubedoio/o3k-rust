use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    SnapshotRecord, StorageBackendRecord, StorageRepository, StoreError, VolumeAttachmentRecordV1,
    VolumeRecord,
};

use super::O3kStore;

#[async_trait]
impl StorageRepository for O3kStore {
    async fn insert_storage_backend(
        &self,
        record: &StorageBackendRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_storage_backend(record).await,
            Self::Postgres(store) => store.insert_storage_backend(record).await,
        }
    }

    async fn get_storage_backend(
        &self,
        id: &str,
    ) -> Result<Option<StorageBackendRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_storage_backend(id).await,
            Self::Postgres(store) => store.get_storage_backend(id).await,
        }
    }

    async fn list_storage_backends(&self) -> Result<Vec<StorageBackendRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_storage_backends().await,
            Self::Postgres(store) => store.list_storage_backends().await,
        }
    }

    async fn insert_volume(&self, record: &VolumeRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_volume(record).await,
            Self::Postgres(store) => store.insert_volume(record).await,
        }
    }

    async fn get_volume(&self, id: Uuid) -> Result<Option<VolumeRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_volume(id).await,
            Self::Postgres(store) => store.get_volume(id).await,
        }
    }

    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_volumes(project_id).await,
            Self::Postgres(store) => store.list_volumes(project_id).await,
        }
    }

    async fn list_all_volumes(&self) -> Result<Vec<VolumeRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_all_volumes().await,
            Self::Postgres(store) => store.list_all_volumes().await,
        }
    }

    async fn update_volume(
        &self,
        expected_generation: u64,
        record: &VolumeRecord,
    ) -> Result<VolumeRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.update_volume(expected_generation, record).await,
            Self::Postgres(store) => store.update_volume(expected_generation, record).await,
        }
    }

    async fn delete_volume(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_volume(project_id, id).await,
            Self::Postgres(store) => store.delete_volume(project_id, id).await,
        }
    }

    async fn insert_volume_attachment_v1(
        &self,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_volume_attachment_v1(record).await,
            Self::Postgres(store) => store.insert_volume_attachment_v1(record).await,
        }
    }

    async fn get_volume_attachment_v1(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecordV1>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_volume_attachment_v1(id).await,
            Self::Postgres(store) => store.get_volume_attachment_v1(id).await,
        }
    }

    async fn list_volume_attachments_v1(
        &self,
        project_id: &str,
    ) -> Result<Vec<VolumeAttachmentRecordV1>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_volume_attachments_v1(project_id).await,
            Self::Postgres(store) => store.list_volume_attachments_v1(project_id).await,
        }
    }

    async fn update_volume_attachment_v1(
        &self,
        expected_generation: u64,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<VolumeAttachmentRecordV1, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .update_volume_attachment_v1(expected_generation, record)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .update_volume_attachment_v1(expected_generation, record)
                    .await
            }
        }
    }

    async fn delete_volume_attachment_v1(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_volume_attachment_v1(project_id, id).await,
            Self::Postgres(store) => store.delete_volume_attachment_v1(project_id, id).await,
        }
    }

    async fn insert_snapshot(&self, record: &SnapshotRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_snapshot(record).await,
            Self::Postgres(store) => store.insert_snapshot(record).await,
        }
    }

    async fn get_snapshot(&self, id: Uuid) -> Result<Option<SnapshotRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_snapshot(id).await,
            Self::Postgres(store) => store.get_snapshot(id).await,
        }
    }

    async fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_snapshots(project_id).await,
            Self::Postgres(store) => store.list_snapshots(project_id).await,
        }
    }

    async fn update_snapshot(
        &self,
        expected_generation: u64,
        record: &SnapshotRecord,
    ) -> Result<SnapshotRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.update_snapshot(expected_generation, record).await,
            Self::Postgres(store) => store.update_snapshot(expected_generation, record).await,
        }
    }

    async fn delete_snapshot(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_snapshot(project_id, id).await,
            Self::Postgres(store) => store.delete_snapshot(project_id, id).await,
        }
    }
}
