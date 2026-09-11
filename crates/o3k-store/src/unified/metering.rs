use async_trait::async_trait;
use o3k_kernel::{KernelError, MeterObservation, MeterUsageReport, MeteringRepository, UsageQuery};

use super::O3kStore;

#[async_trait]
impl MeteringRepository for O3kStore {
    async fn ensure_authority(&self, now_ms: i64) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.ensure_authority(now_ms).await,
            Self::Postgres(store) => store.ensure_authority(now_ms).await,
        }
    }

    async fn record_observation(&self, observation: &MeterObservation) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.record_observation(observation).await,
            Self::Postgres(store) => store.record_observation(observation).await,
        }
    }

    async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, KernelError> {
        match self {
            Self::Sqlite(store) => store.usage(query).await,
            Self::Postgres(store) => store.usage(query).await,
        }
    }
}
