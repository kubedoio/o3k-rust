#![allow(clippy::expect_used, clippy::unwrap_used)]

use o3k_kernel::{LimitKey, LimitValue, OwnershipScope, ScopeId};
use o3k_store::{PostgresStore, StoreError, quota::QuotaRepository};
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn postgres_quota_generation_is_durable_and_cas_safe() {
    let Some(url) = std::env::var("O3K_DATABASE_URL").ok() else {
        eprintln!("skipping PostgreSQL quota CAS: O3K_DATABASE_URL unavailable");
        return;
    };
    // Use a disposable database so concurrently running PostgreSQL integration
    // binaries that reset the shared fixture schema cannot invalidate this
    // test's migration or CAS authority.
    let parsed = url::Url::parse(&url).unwrap();
    let database = format!("o3k_quota_cas_{}", uuid::Uuid::now_v7().simple());
    let mut admin_url = parsed.clone();
    admin_url.set_path("/postgres");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url.as_str())
        .await
        .unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let mut isolated_url = parsed;
    isolated_url.set_path(&format!("/{database}"));
    let isolated_url = isolated_url.to_string();
    let store_a = PostgresStore::connect(&isolated_url).await.unwrap();
    let store_b = PostgresStore::connect(&isolated_url).await.unwrap();
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
    let restarted = PostgresStore::connect(&isolated_url).await.unwrap();
    assert_eq!(
        restarted.get_limit_state(&scope, &key).await.unwrap(),
        (limit, 1)
    );
    store_a.pool().close().await;
    store_b.pool().close().await;
    restarted.pool().close().await;
    drop_disposable_database(admin_url.as_str(), &database).await;
}

// Under nextest every test runs in its own process, so process-wide locks
// cannot serialize fixture teardown: a closed pool's server-side backend may
// still be terminating when the drop runs (SQLSTATE 55006). Terminate
// leftover backends, then force the drop with a bounded retry (mirrors
// postgres_metering.rs).
async fn drop_disposable_database(admin_url: &str, database: &str) {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .unwrap();
    let _ = sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(database)
    .execute(&admin)
    .await;
    let mut last_error: Option<sqlx::Error> = None;
    for attempt in 0..5u64 {
        match sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .execute(&admin)
            .await
        {
            Ok(_) => {
                admin.close().await;
                return;
            }
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(100 * (attempt + 1))).await;
            }
        }
    }
    admin.close().await;
    // Retries exhausted: fail the test with the final driver error.
    let drop_result: Result<(), sqlx::Error> = Err(last_error.unwrap_or_else(|| {
        sqlx::Error::Protocol("DROP DATABASE retry loop produced no error".into())
    }));
    assert!(
        drop_result.is_ok(),
        "failed to drop disposable database {database} after retries: {drop_result:?}"
    );
}
