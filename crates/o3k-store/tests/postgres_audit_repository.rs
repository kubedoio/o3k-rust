#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use o3k_store::{AuditEventRecord, AuditRepository, PostgresStore, StoreError};

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
    sqlx::query("DELETE FROM audit_events")
        .execute(store.pool())
        .await
        .ok()?;
    Some(store)
}

#[tokio::test]
async fn postgres_audit_repository_conformance() {
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
}
