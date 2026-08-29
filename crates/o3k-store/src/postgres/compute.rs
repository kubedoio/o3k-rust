use async_trait::async_trait;

use crate::{ComputeRepository, ResourceRecord, StoreError};

use super::{PostgresStore, helpers::row_to_resource};

#[async_trait]
impl ComputeRepository for PostgresStore {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM resources WHERE kind = $1 AND UPPER(observed_state) != 'DELETED' ORDER BY created_at ASC",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(row_to_resource).collect()
    }
}
