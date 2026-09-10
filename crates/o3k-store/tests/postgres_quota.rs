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
    let (a, b) = tokio::join!(
        store_a.set_limit_if_generation(&scope, &key, LimitValue::Maximum(2), 0),
        store_b.set_limit_if_generation(&scope, &key, LimitValue::Maximum(9), 0),
    );
    let successes = [a.as_ref(), b.as_ref()]
        .iter()
        .filter(|result| result.is_ok())
        .count();
    assert_eq!(
        successes, 1,
        "exactly one independent writer must win: {a:?} / {b:?}"
    );
    assert!(matches!(
        (&a, &b),
        (Err(StoreError::QuotaGenerationConflict), Ok(1))
            | (Ok(1), Err(StoreError::QuotaGenerationConflict))
    ));
    let (limit, generation) = store_b.get_limit_state(&scope, &key).await.unwrap();
    assert!(matches!(
        limit,
        LimitValue::Maximum(2) | LimitValue::Maximum(9)
    ));
    assert_eq!(generation, 1);
    drop(store_a);
    let restarted = PostgresStore::connect(&url).await.unwrap();
    assert_eq!(
        restarted.get_limit_state(&scope, &key).await.unwrap(),
        (limit, 1)
    );
}
