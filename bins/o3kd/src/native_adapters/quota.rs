use std::sync::Arc;

use o3k_kernel::{LimitKey, OwnershipScope};
use o3k_native_api::{
    error::NativeReadError,
    quota::{self, QuotaDimensionView},
};
use o3k_store::{O3kStore, QuotaRepository};

pub struct QuotaReaderAdapter {
    pub store: Arc<O3kStore>,
}

#[async_trait::async_trait]
impl quota::QuotaReader for QuotaReaderAdapter {
    async fn read_scope(
        &self,
        scope: &OwnershipScope,
    ) -> Result<Vec<QuotaDimensionView>, NativeReadError> {
        let mut result = Vec::with_capacity(LimitKey::KNOWN_DIMENSIONS.len());
        for (namespace, resource) in LimitKey::KNOWN_DIMENSIONS {
            let key = LimitKey::new(namespace, resource).map_err(|_| NativeReadError::Internal)?;
            let limit = self.store.get_limit(scope, &key).await.map_err(|error| {
                tracing::error!(%error, "native quota limit read failed");
                NativeReadError::Internal
            })?;
            let usage = self.store.get_usage(scope, &key).await.map_err(|error| {
                tracing::error!(%error, "native quota usage read failed");
                NativeReadError::Internal
            })?;
            result.push(quota::view(&key, limit, &usage));
        }
        Ok(result)
    }
}
