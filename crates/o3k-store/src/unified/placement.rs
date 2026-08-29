use super::*;

#[async_trait]
impl PlacementRepository for O3kStore {
    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<PlacementProviderRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_provider(provider_id).await,
            Self::Postgres(s) => s.get_provider(provider_id).await,
        }
    }

    async fn list_providers(&self) -> Result<Vec<PlacementProviderRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_providers().await,
            Self::Postgres(s) => s.list_providers().await,
        }
    }

    async fn register_provider(
        &self,
        node_id: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.register_provider(node_id, inventories).await,
            Self::Postgres(s) => s.register_provider(node_id, inventories).await,
        }
    }

    async fn sync_provider(
        &self,
        node_id: &str,
        state: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.sync_provider(node_id, state, inventories).await,
            Self::Postgres(s) => s.sync_provider(node_id, state, inventories).await,
        }
    }

    async fn refresh_inventories(
        &self,
        provider_id: &str,
        expected_generation: u64,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.refresh_inventories(provider_id, expected_generation, inventories)
                    .await
            }
            Self::Postgres(s) => {
                s.refresh_inventories(provider_id, expected_generation, inventories)
                    .await
            }
        }
    }

    async fn set_provider_state(&self, provider_id: &str, state: &str) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.set_provider_state(provider_id, state).await,
            Self::Postgres(s) => s.set_provider_state(provider_id, state).await,
        }
    }

    async fn commit_allocation(
        &self,
        provider_id: &str,
        expected_generation: u64,
        allocation: &PlacementAllocationRecord,
    ) -> Result<PlacementAllocationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.commit_allocation(provider_id, expected_generation, allocation)
                    .await
            }
            Self::Postgres(s) => {
                s.commit_allocation(provider_id, expected_generation, allocation)
                    .await
            }
        }
    }

    async fn release_allocation(
        &self,
        provider_id: &str,
        allocation_id: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_allocation(provider_id, allocation_id).await,
            Self::Postgres(s) => s.release_allocation(provider_id, allocation_id).await,
        }
    }

    async fn upsert_intent(
        &self,
        intent: &PlacementIntentRecord,
    ) -> Result<PlacementIntentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.upsert_intent(intent).await,
            Self::Postgres(s) => s.upsert_intent(intent).await,
        }
    }

    async fn get_intent(
        &self,
        allocation_id: &str,
    ) -> Result<Option<PlacementIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_intent(allocation_id).await,
            Self::Postgres(s) => s.get_intent(allocation_id).await,
        }
    }

    async fn list_intents(&self) -> Result<Vec<PlacementIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_intents().await,
            Self::Postgres(s) => s.list_intents().await,
        }
    }

    async fn delete_intent(&self, allocation_id: &str) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_intent(allocation_id).await,
            Self::Postgres(s) => s.delete_intent(allocation_id).await,
        }
    }

    async fn reconcile_consumers(
        &self,
        durable_consumer_ids: &[String],
    ) -> Result<PlacementReconcileRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.reconcile_consumers(durable_consumer_ids).await,
            Self::Postgres(s) => s.reconcile_consumers(durable_consumer_ids).await,
        }
    }

    async fn import_provider(&self, provider: &PlacementProviderRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.import_provider(provider).await,
            Self::Postgres(s) => s.import_provider(provider).await,
        }
    }
}
