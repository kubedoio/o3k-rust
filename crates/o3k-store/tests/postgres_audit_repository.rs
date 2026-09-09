#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
};

use o3k_kernel::{AuditQuery, DurableAuditRepository, OwnershipScope, ScopeId};
use o3k_store::{AuditEventRecord, AuditRepository, PostgresStore, StoreError};
use sqlx::postgres::PgPoolOptions;

fn event(id: &str, scope: &str) -> AuditEventRecord {
    AuditEventRecord {
        event_id: id.into(),
        timestamp: format!("2026-01-01T00:00:{id}Z"),
        request_id: format!("request-{id}"),
        audit_id: format!("audit-{id}"),
        principal_id: "principal-a".into(),
        principal_kind: "user".into(),
        effective_scope: scope.into(),
        service: "compute".into(),
        action: "compute:read".into(),
        resource_type: Some("compute:server".into()),
        resource_id: Some(format!("resource-{id}")),
        owner_scope: Some(scope.into()),
        operation_id: None,
        outcome: "succeeded".into(),
        reason_category: Some("ok".into()),
    }
}

async fn store() -> Option<PostgresStore> {
    let url = std::env::var("O3K_DATABASE_URL").ok()?;
    let store = PostgresStore::connect(&url).await.ok()?;
    sqlx::query("DELETE FROM audit_events WHERE event_id IN ('0001','0002','0003','0004','0005','0006','concurrent','different-a','different-b')")
        .execute(store.pool())
        .await
        .ok()?;
    Some(store)
}

#[tokio::test]
async fn postgres_unified_audit_query_pushes_all_supported_filters() {
    let _guard = test_lock().await;
    let Some(store) = store().await else {
        eprintln!("skipping PostgreSQL unified Audit query: O3K_DATABASE_URL unavailable");
        return;
    };
    let first = event("0001", "project-a");
    store.insert_audit_event(&first).await.unwrap();
    let unified = o3k_store::O3kStore::Postgres(store);
    let query = AuditQuery {
        scope: OwnershipScope::project(ScopeId::new_unchecked("project-a"), None, None),
        after_event_id: None,
        limit: 10,
        service: Some("compute".into()),
        action: Some("compute:read".into()),
        outcome: Some("succeeded".into()),
        resource_type: Some("compute:server".into()),
        resource_id: Some("resource-0001".into()),
        operation_id: None,
        principal_id: Some("principal-a".into()),
        request_id: Some("request-0001".into()),
        audit_id: Some("audit-0001".into()),
        from_timestamp: Some("2025-01-01".into()),
        until_timestamp: Some("2027-01-01".into()),
    };
    let page = unified.page(&query).await.unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].event_id.as_str(), "0001");
}

#[tokio::test]
async fn postgres_pre_audit_schema_upgrades_without_losing_existing_state() {
    let _guard = test_lock().await;
    let Some(url) = std::env::var("O3K_DATABASE_URL").ok() else {
        eprintln!("skipping PostgreSQL migration upgrade: O3K_DATABASE_URL unavailable");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("DROP SCHEMA IF EXISTS public CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE SCHEMA public")
        .execute(&pool)
        .await
        .unwrap();
    let all = sqlx::migrate!("./migrations_postgres");
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            all.migrations
                .iter()
                .take(all.migrations.len() - 2)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    legacy.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO resources (id,kind,project_id,generation,observed_generation,desired_state,observed_state) VALUES ('migration-resource','compute:server','project-a',1,0,'ACTIVE','UNKNOWN')")
        .execute(&pool).await.unwrap();
    pool.close().await;
    let store = PostgresStore::connect(&url).await.unwrap();
    let event = event("migration-event", "project-a");
    store.insert_audit_event(&event).await.unwrap();
    assert!(
        store
            .get_audit_event("project-a", "migration-event")
            .await
            .is_ok()
    );
    sqlx::query("DELETE FROM audit_events WHERE event_id = 'migration-event'")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM resources WHERE id = 'migration-resource'")
        .execute(store.pool())
        .await
        .unwrap();
}

// The conformance binary runs tests concurrently against one disposable database.
// Serialize fixture setup/teardown while retaining true concurrent writes inside
// the dedicated race test below.
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[tokio::test]
async fn postgres_audit_repository_conformance() {
    let _guard = test_lock().await;
    let Some(store) = store().await else {
        eprintln!("skipping PostgreSQL Audit conformance: O3K_DATABASE_URL unavailable");
        return;
    };

    let first = event("0001", "project-a");
    store.insert_audit_event(&first).await.unwrap();
    assert!(store.insert_audit_event(&first).await.is_ok());

    let mut conflict = first.clone();
    conflict.outcome = "failed".into();
    assert!(matches!(
        store.insert_audit_event(&conflict).await,
        Err(StoreError::AuditEventConflict)
    ));
    assert_eq!(
        store.get_audit_event("project-a", "0001").await.unwrap(),
        first
    );

    let mut foreign = first.clone();
    foreign.effective_scope = "project-b".into();
    foreign.owner_scope = Some("project-b".into());
    assert!(matches!(
        store.insert_audit_event(&foreign).await,
        Err(StoreError::AuditEventConflict)
    ));

    for id in ["0002", "0003", "0004", "0005"] {
        store
            .insert_audit_event(&event(id, "project-a"))
            .await
            .unwrap();
    }
    store
        .insert_audit_event(&event("0006", "project-b"))
        .await
        .unwrap();

    let page = store
        .list_audit_events_page("project-a", None, 2)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        ["0001", "0002"]
    );
    assert!(page.has_more);
    let page2 = store
        .list_audit_events_page("project-a", page.continuation_key.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        page2
            .items
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        ["0003", "0004"]
    );
    assert!(page2.has_more);
    let page3 = store
        .list_audit_events_page("project-a", page2.continuation_key.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        page3
            .items
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        ["0005"]
    );
    assert!(!page3.has_more);
    assert!(matches!(
        store.get_audit_event("project-b", "0001").await,
        Err(StoreError::ResourceNotFound)
    ));

    assert_eq!(
        store
            .prune_audit_events_before("2026-01-01T00:00:03Z", 2)
            .await
            .unwrap(),
        2
    );
    assert!(matches!(
        store.get_audit_event("project-a", "0001").await,
        Err(StoreError::ResourceNotFound)
    ));
    assert!(store.get_audit_event("project-a", "0003").await.is_ok());
}

#[tokio::test]
async fn postgres_audit_same_id_concurrent_replay_converges() {
    let _guard = test_lock().await;
    let Some(store) = store().await else {
        eprintln!("skipping PostgreSQL Audit concurrency: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = Arc::new(store);
    let e = event("concurrent", "project-a");
    let (a, b) = tokio::join!(store.insert_audit_event(&e), store.insert_audit_event(&e));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(
        store
            .list_audit_events_page("project-a", None, 10)
            .await
            .unwrap()
            .items
            .len(),
        1
    );

    let mut conflicting = e.clone();
    conflicting.outcome = "failed".into();
    let (a, b) = tokio::join!(
        store.insert_audit_event(&conflicting),
        store.insert_audit_event(&conflicting)
    );
    assert!(matches!(a, Err(StoreError::AuditEventConflict)));
    assert!(matches!(b, Err(StoreError::AuditEventConflict)));
    assert_eq!(
        store
            .get_audit_event("project-a", "concurrent")
            .await
            .unwrap(),
        e
    );

    let different_a = event("different-a", "project-a");
    let different_b = event("different-b", "project-a");
    let (a, b) = tokio::join!(
        store.insert_audit_event(&different_a),
        store.insert_audit_event(&different_b)
    );
    assert!(a.is_ok() && b.is_ok());
    assert!(
        store
            .get_audit_event("project-a", "different-a")
            .await
            .is_ok()
    );
    assert!(
        store
            .get_audit_event("project-a", "different-b")
            .await
            .is_ok()
    );
}
