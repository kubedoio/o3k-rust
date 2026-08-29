use async_trait::async_trait;

use crate::{ComputeRepository, ResourceRecord, StoreError};

use super::O3kStore;

#[async_trait]
impl ComputeRepository for O3kStore {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_resources_by_kind(kind).await,
            Self::Postgres(s) => s.list_resources_by_kind(kind).await,
        }
    }
}
