use std::sync::Arc;

use o3k_kernel::{
    ActionId, AuditEvent, AuthContext, LimitKey, LimitValue, OwnershipScope, ServiceNamespace,
};
use o3k_store::{AuditEventRecord, StoreError, quota::QuotaRepository};

pub struct QuotaReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

impl QuotaReaderAdapter {
    pub fn new(store: Arc<o3k_store::unified::O3kStore>) -> Self {
        Self { store }
    }
}

impl QuotaReaderAdapter {
    async fn project(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<o3k_native_api::quota::QuotaDimension, o3k_native_api::quota::QuotaError> {
        let (limit, generation) = self
            .store
            .get_limit_state(scope, key)
            .await
            .map_err(map_store)?;
        let usage = self.store.get_usage(scope, key).await.map_err(map_store)?;
        Ok(o3k_native_api::quota::QuotaDimension {
            key: key.resource().to_owned(),
            namespace: key.namespace().as_str().to_owned(),
            unit: key.unit().to_owned(),
            scope: scope.kind().as_str().to_owned(),
            limit,
            usage: usage.in_use,
            generation,
        })
    }
}

fn map_store(error: StoreError) -> o3k_native_api::quota::QuotaError {
    match error {
        StoreError::QuotaGenerationConflict => o3k_native_api::quota::QuotaError::StaleGeneration,
        StoreError::Corrupt(_) => o3k_native_api::quota::QuotaError::Corrupt,
        StoreError::ResourceNotFound => o3k_native_api::quota::QuotaError::NotFound,
        _ => o3k_native_api::quota::QuotaError::Unavailable,
    }
}

#[async_trait::async_trait]
impl o3k_native_api::quota::QuotaReader for QuotaReaderAdapter {
    async fn list(
        &self,
        scope: &OwnershipScope,
    ) -> Result<Vec<o3k_native_api::quota::QuotaDimension>, o3k_native_api::quota::QuotaError> {
        let mut out = Vec::with_capacity(LimitKey::KNOWN_DIMENSIONS.len());
        for (namespace, resource) in LimitKey::KNOWN_DIMENSIONS {
            out.push(
                self.project(
                    scope,
                    &LimitKey::new(namespace, resource)
                        .map_err(|_| o3k_native_api::quota::QuotaError::Corrupt)?,
                )
                .await?,
            );
        }
        Ok(out)
    }

    async fn set(
        &self,
        auth: &AuthContext,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
        expected_generation: Option<u64>,
    ) -> Result<o3k_native_api::quota::QuotaDimension, o3k_native_api::quota::QuotaError> {
        let expected_generation =
            expected_generation.ok_or(o3k_native_api::quota::QuotaError::Invalid)?;
        let limit = match limit {
            LimitValue::Unlimited => LimitValue::Unlimited,
            LimitValue::Maximum(value) => LimitValue::new_maximum_checked(value)
                .map_err(|_| o3k_native_api::quota::QuotaError::Invalid)?,
        };
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked("quota".to_owned()),
            ActionId::new_unchecked("quota", "ManageQuota"),
            o3k_kernel::AuditOutcome::Succeeded,
        )
        .with_resource(
            o3k_kernel::ResourceType::new_unchecked("quota", "quota"),
            Some(format!("{}:{}", key.namespace(), key.resource())),
            Some(scope.clone()),
        )
        .with_reason(format!("dimension={}", key));
        let next_generation = self
            .store
            .set_limit_if_generation_with_audit(
                scope,
                key,
                limit,
                expected_generation,
                &AuditEventRecord::from_kernel_event(&event),
            )
            .await
            .map_err(map_store)?;
        let mut item = self.project(scope, key).await?;
        item.generation = next_generation;
        Ok(item)
    }

    async fn clear(
        &self,
        auth: &AuthContext,
        scope: &OwnershipScope,
        key: &LimitKey,
        expected_generation: Option<u64>,
    ) -> Result<o3k_native_api::quota::QuotaDimension, o3k_native_api::quota::QuotaError> {
        self.set(auth, scope, key, LimitValue::Unlimited, expected_generation)
            .await
    }
}
