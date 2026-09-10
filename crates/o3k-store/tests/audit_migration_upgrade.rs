#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use o3k_store::{AuditEventRecord, AuditRepository, SqliteStore};
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn sqlite_pre_audit_schema_upgrades_without_losing_existing_state() {
    let path =
        std::env::temp_dir().join(format!("o3k-audit-migration-{}.db", uuid::Uuid::now_v7()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
    let all = sqlx::migrate!("./migrations");
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

    let store = SqliteStore::connect_file(&path).await.unwrap();
    let event = AuditEventRecord {
        event_id: "migration-event".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        request_id: "request-migration".into(),
        audit_id: "audit-migration".into(),
        principal_id: "principal-a".into(),
        principal_kind: "user".into(),
        effective_scope: "project-a".into(),
        service: "compute".into(),
        action: "compute:read".into(),
        resource_type: Some("compute:server".into()),
        resource_id: Some("migration-resource".into()),
        owner_scope: Some("project-a".into()),
        operation_id: None,
        outcome: "succeeded".into(),
        reason_category: Some("ok".into()),
    };
    store.insert_audit_event(&event).await.unwrap();
    assert_eq!(
        store
            .get_audit_event("project-a", "migration-event")
            .await
            .unwrap(),
        event
    );
    drop(store);
    let reopened = SqliteStore::connect_file(&path).await.unwrap();
    assert!(
        reopened
            .get_audit_event("project-a", "migration-event")
            .await
            .is_ok()
    );
    let _ = std::fs::remove_file(path);
}
