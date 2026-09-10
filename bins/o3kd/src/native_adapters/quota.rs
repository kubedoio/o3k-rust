use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use o3k_kernel::{
    ActionId, AuditEvent, AuthContext, LimitKey, LimitValue, OwnershipScope,
    RequiredAuditPublisher, ServiceNamespace,
};
use o3k_store::quota::QuotaRepository;

pub struct QuotaReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub audit: Arc<dyn RequiredAuditPublisher>,
    generations: Arc<Mutex<HashMap<String, u64>>>,
}

impl QuotaReaderAdapter {
    pub fn new(
        store: Arc<o3k_store::unified::O3kStore>,
        audit: Arc<dyn RequiredAuditPublisher>,
    ) -> Self {
        Self {
            store,
            audit,
            generations: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn unit(namespace: &str, resource: &str) -> &'static str {
    match (namespace, resource) {
        ("compute", "memory_mb") => "mebibytes",
        ("compute", "disk_gb") => "gibibytes",
        ("image", "bytes") => "bytes",
        _ => "count",
    }
}

impl QuotaReaderAdapter {
    async fn project(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<o3k_native_api::quota::QuotaDimension, ()> {
        let limit = self.store.get_limit(scope, key).await.map_err(|_| ())?;
        let usage = self.store.get_usage(scope, key).await.map_err(|_| ())?;
        let generation = self
            .generations
            .lock()
            .await
            .get(&format!("{}:{}:{}", scope, key.namespace(), key.resource()))
            .copied()
            .unwrap_or(0);
        Ok(o3k_native_api::quota::QuotaDimension {
            key: key.resource().to_owned(),
            namespace: key.namespace().as_str().to_owned(),
            unit: unit(key.namespace().as_str(), key.resource()).to_owned(),
            scope: scope.kind().as_str().to_owned(),
            limit,
            usage: usage.in_use,
            generation,
        })
    }
}

#[async_trait::async_trait]
impl o3k_native_api::quota::QuotaReader for QuotaReaderAdapter {
    async fn list(
        &self,
        scope: &OwnershipScope,
    ) -> Result<Vec<o3k_native_api::quota::QuotaDimension>, ()> {
        let mut out = Vec::with_capacity(LimitKey::KNOWN_DIMENSIONS.len());
        for (namespace, resource) in LimitKey::KNOWN_DIMENSIONS {
            out.push(
                self.project(scope, &LimitKey::new(namespace, resource).map_err(|_| ())?)
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
    ) -> Result<o3k_native_api::quota::QuotaDimension, ()> {
        let generation_key = format!("{}:{}:{}", scope, key.namespace(), key.resource());
        let mut generations = self.generations.lock().await;
        let current_generation = *generations.get(&generation_key).unwrap_or(&0);
        if expected_generation.is_some_and(|generation| generation != current_generation) {
            return Err(());
        }
        let limit = match limit {
            LimitValue::Unlimited => LimitValue::Unlimited,
            LimitValue::Maximum(value) => LimitValue::new_maximum_checked(value).map_err(|_| ())?,
        };
        self.store
            .set_limit(scope, key, limit)
            .await
            .map_err(|_| ())?;
        let next_generation = current_generation.saturating_add(1);
        generations.insert(generation_key, next_generation);
        drop(generations);
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked("quota".to_owned()),
            ActionId::new_unchecked("quota", "ManageQuota"),
            o3k_kernel::AuditOutcome::Succeeded,
        )
        .with_resource(
            o3k_kernel::ResourceType::new_unchecked("quota", "quota"),
            None,
            Some(scope.clone()),
        )
        .with_reason(format!("dimension={}", key));
        self.audit.publish(&event).await.map_err(|_| ())?;
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
    ) -> Result<o3k_native_api::quota::QuotaDimension, ()> {
        self.set(auth, scope, key, LimitValue::Unlimited, expected_generation)
            .await
    }
}
