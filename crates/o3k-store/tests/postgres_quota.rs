#![allow(clippy::expect_used, clippy::unwrap_used)]

use o3k_kernel::{LimitKey, LimitValue, OwnershipScope, ScopeId};
use o3k_store::{PostgresStore, StoreError, quota::QuotaRepository};

#[tokio::test]
async fn postgres_quota_generation_is_durable_and_cas_safe() {
    let Some(url) = std::env::var("O3K_DATABASE_URL").ok() else {
        eprintln!("skipping PostgreSQL quota CAS: O3K_DATABASE_URL unavailable");
        return;
    };
    let store_a = PostgresStore::connect(&url).await.unwrap();
    let store_b = PostgresStore::connect(&url).await.unwrap();
    let scope = OwnershipScope::project(
        ScopeId::new_unchecked(format!("quota-cas-{}", uuid::Uuid::now_v7())),
        None,
        None,
    );
    let key = LimitKey::compute_servers();
    let first = store_a
        .set_limit_if_generation(&scope, &key, LimitValue::Maximum(2), 0)
        .await
        .unwrap();
    assert_eq!(first, 1);
    let stale = store_b
        .set_limit_if_generation(&scope, &key, LimitValue::Maximum(9), 0)
        .await;
    assert!(matches!(stale, Err(StoreError::QuotaGenerationConflict)));
    let (limit, generation) = store_b.get_limit_state(&scope, &key).await.unwrap();
    assert_eq!(limit, LimitValue::Maximum(2));
    assert_eq!(generation, 1);
    drop(store_a);
    let restarted = PostgresStore::connect(&url).await.unwrap();
    assert_eq!(
        restarted.get_limit_state(&scope, &key).await.unwrap(),
        (LimitValue::Maximum(2), 1)
    );
}
