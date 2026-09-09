use async_trait::async_trait;
use uuid::Uuid;

use crate::{AuditEventRecord, ImageMetadataRecord, ImageRepository, StoreError};

use super::O3kStore;

#[async_trait]
impl ImageRepository for O3kStore {
    async fn create_or_replay_canonical_image_operation(
        &self,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
    ) -> Result<crate::IdempotencyReservation, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .create_or_replay_canonical_image_operation(operation, canonical, request)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .create_or_replay_canonical_image_operation(operation, canonical, request)
                    .await
            }
        }
    }

    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_image(image).await,
            Self::Postgres(s) => s.insert_image(image).await,
        }
    }
    async fn insert_image_with_audit(
        &self,
        image: &ImageMetadataRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_image_with_audit(image, audit).await,
            Self::Postgres(s) => s.insert_image_with_audit(image, audit).await,
        }
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_images(project_id).await,
            Self::Postgres(s) => s.list_images(project_id).await,
        }
    }
    async fn list_images_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_images_page(project_id, after_id, limit).await,
            Self::Postgres(s) => s.list_images_page(project_id, after_id, limit).await,
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
    async fn activate_image_with_audit(
        &self,
        p: &str,
        i: &Uuid,
        s: u64,
        c: &str,
        a: &AuditEventRecord,
    ) -> Result<ImageMetadataRecord, StoreError> {
        match self {
            Self::Sqlite(v) => v.activate_image_with_audit(p, i, s, c, a).await,
            Self::Postgres(v) => v.activate_image_with_audit(p, i, s, c, a).await,
        }
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_image(project_id, id).await,
            Self::Postgres(s) => s.delete_image(project_id, id).await,
        }
    }
    async fn delete_image_with_audit(
        &self,
        p: &str,
        i: &Uuid,
        a: &AuditEventRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(v) => v.delete_image_with_audit(p, i, a).await,
            Self::Postgres(v) => v.delete_image_with_audit(p, i, a).await,
        }
    }
}
