use async_trait::async_trait;
use uuid::Uuid;

use crate::{RelationshipRepository, ResourceRelationshipRecord, StoreError};

use super::O3kStore;

#[async_trait]
impl RelationshipRepository for O3kStore {
    async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.reserve_relationship(record).await,
            Self::Postgres(s) => s.reserve_relationship(record).await,
        }
    }

    async fn get_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_relationship(parent_resource_id, slot).await,
            Self::Postgres(s) => s.get_relationship(parent_resource_id, slot).await,
        }
    }

    async fn list_relationships(
        &self,
        parent_resource_id: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_relationships(parent_resource_id).await,
            Self::Postgres(s) => s.list_relationships(parent_resource_id).await,
        }
    }

    async fn list_relationships_page(
        &self,
        parent_resource_id: Uuid,
        after_slot: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.list_relationships_page(parent_resource_id, after_slot, limit)
                    .await
            }
            Self::Postgres(s) => {
                s.list_relationships_page(parent_resource_id, after_slot, limit)
                    .await
            }
        }
    }

    async fn bind_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        child_resource_id: Uuid,
        child_operation_id: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.bind_relationship(
                    parent_resource_id,
                    slot,
                    child_resource_id,
                    child_operation_id,
                )
                .await
            }
            Self::Postgres(s) => {
                s.bind_relationship(
                    parent_resource_id,
                    slot,
                    child_resource_id,
                    child_operation_id,
                )
                .await
            }
        }
    }

    async fn set_relationship_state(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.set_relationship_state(parent_resource_id, slot, state)
                    .await
            }
            Self::Postgres(s) => {
                s.set_relationship_state(parent_resource_id, slot, state)
                    .await
            }
        }
    }
}
