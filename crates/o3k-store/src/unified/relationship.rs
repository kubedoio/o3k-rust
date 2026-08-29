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
        self.reserve_relationship(record).await
    }

    async fn get_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        self.get_relationship(parent_resource_id, slot).await
    }

    async fn list_relationships(
        &self,
        parent_resource_id: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        self.list_relationships(parent_resource_id).await
    }

    async fn bind_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        child_resource_id: Uuid,
        child_operation_id: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        self.bind_relationship(
            parent_resource_id,
            slot,
            child_resource_id,
            child_operation_id,
        )
        .await
    }

    async fn set_relationship_state(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        self.set_relationship_state(parent_resource_id, slot, state)
            .await
    }
}
