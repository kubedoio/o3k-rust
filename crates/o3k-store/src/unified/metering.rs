use async_trait::async_trait;

use crate::{MeteringAggregate, MeteringEventRecord, MeteringRepository, O3kStore, StoreError};

#[async_trait]
impl MeteringRepository for O3kStore {
    async fn append_metering_event(&self, event: &MeteringEventRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.append_metering_event(event).await,
            Self::Postgres(store) => store.append_metering_event(event).await,
        }
    }

    async fn aggregate_metering_events(
        &self,
        project_id: &str,
        meter_id: &str,
        effective_from: &str,
        effective_to: &str,
        limit: usize,
    ) -> Result<MeteringAggregate, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .aggregate_metering_events(
                        project_id,
                        meter_id,
                        effective_from,
                        effective_to,
                        limit,
                    )
                    .await
            }
            Self::Postgres(store) => {
                store
                    .aggregate_metering_events(
                        project_id,
                        meter_id,
                        effective_from,
                        effective_to,
                        limit,
                    )
                    .await
            }
        }
    }
}
