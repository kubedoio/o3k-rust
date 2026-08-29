use super::*;

impl PostgresStore {
    pub async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        let result = sqlx::query("INSERT INTO resource_relationships (parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(record.parent_resource_id.to_string()).bind(&record.parent_resource_type).bind(&record.slot).bind(&record.expected_child_resource_type)
            .bind(record.child_resource_id.map(|id| id.to_string())).bind(&record.ownership).bind(record.parent_operation_id.to_string()).bind(record.child_operation_id.map(|id| id.to_string()))
            .bind(&record.owner_scope).bind("reserved").bind(&record.fingerprint).execute(&self.pool).await;
        if let Err(error) = result {
            let conflict = matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation());
            if !conflict {
                return Err(StoreError::Database(error));
            }
            let existing = self
                .get_relationship(record.parent_resource_id, &record.slot)
                .await?;
            if existing.fingerprint == record.fingerprint
                && existing.expected_child_resource_type == record.expected_child_resource_type
                && existing.ownership == record.ownership
                && existing.owner_scope == record.owner_scope
            {
                return Ok(existing);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        self.get_relationship(record.parent_resource_id, &record.slot)
            .await
    }

    pub async fn get_relationship(
        &self,
        parent: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        let row = sqlx::query("SELECT parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint FROM resource_relationships WHERE parent_resource_id=$1 AND slot=$2")
            .bind(parent.to_string()).bind(slot).fetch_optional(&self.pool).await.map_err(StoreError::Database)?.ok_or(StoreError::ResourceNotFound)?;
        relationship_from_pg_row(&row)
    }

    pub async fn list_relationships(
        &self,
        parent: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        let rows = sqlx::query("SELECT parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint FROM resource_relationships WHERE parent_resource_id=$1 ORDER BY slot")
            .bind(parent.to_string()).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(relationship_from_pg_row).collect()
    }

    pub async fn bind_relationship(
        &self,
        parent: Uuid,
        slot: &str,
        child: Uuid,
        child_operation: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        sqlx::query("UPDATE resource_relationships SET child_resource_id=$1,child_operation_id=$2,state='bound' WHERE parent_resource_id=$3 AND slot=$4 AND state IN ('reserved','unknown')")
            .bind(child.to_string()).bind(child_operation.to_string()).bind(parent.to_string()).bind(slot).execute(&self.pool).await.map_err(StoreError::Database)?;
        self.get_relationship(parent, slot).await
    }

    pub async fn set_relationship_state(
        &self,
        parent: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        if !matches!(
            state,
            "reserved" | "bound" | "deleting" | "deleted" | "unknown"
        ) {
            return Err(StoreError::Corrupt("invalid relationship state".into()));
        }
        sqlx::query(
            "UPDATE resource_relationships SET state=$1 WHERE parent_resource_id=$2 AND slot=$3",
        )
        .bind(state)
        .bind(parent.to_string())
        .bind(slot)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        self.get_relationship(parent, slot).await
    }
}

#[async_trait]
impl crate::RelationshipRepository for PostgresStore {
    async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::reserve_relationship(self, record).await
    }

    async fn get_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::get_relationship(self, parent_resource_id, slot).await
    }

    async fn list_relationships(
        &self,
        parent_resource_id: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        Self::list_relationships(self, parent_resource_id).await
    }

    async fn bind_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        child_resource_id: Uuid,
        child_operation_id: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::bind_relationship(
            self,
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
        Self::set_relationship_state(self, parent_resource_id, slot, state).await
    }
}
