#![allow(clippy::unwrap_used, clippy::expect_used)]

use o3k_kernel::{AuditQuery, OwnershipScope, ScopeId};
use o3k_store::{AuditEventRecord, AuditRepository, O3kStore, SqliteStore, StoreError};

fn event(id: &str, scope: &str, service: &str) -> AuditEventRecord {
    AuditEventRecord {
        event_id: id.into(),
        timestamp: format!("2026-01-01T00:00:{id}Z"),
        request_id: format!("req-{id}"),
        audit_id: format!("audit-{id}"),
        principal_id: "principal-a".into(),
        principal_kind: "user".into(),
        effective_scope: scope.into(),
        service: service.into(),
        action: format!("{service}:read"),
        resource_type: Some("compute:server".into()),
        resource_id: Some(format!("resource-{id}")),
        owner_scope: Some(scope.into()),
        operation_id: None,
        outcome: "succeeded".into(),
        reason_category: Some("ok".into()),
    }
}

#[tokio::test]
async fn sqlite_audit_identity_paging_and_restart_are_durable() {
    let path = std::env::temp_dir().join(format!("o3k-audit-{}.db", uuid::Uuid::now_v7()));
    let store = SqliteStore::connect_file(&path).await.unwrap();
    let first = event("0001", "project-a", "compute");
    store.insert_audit_event(&first).await.unwrap();
    store.insert_audit_event(&first).await.unwrap();

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
    let mut foreign_conflict = first.clone();
    foreign_conflict.effective_scope = "project-b".into();
    assert!(matches!(
        store.insert_audit_event(&foreign_conflict).await,
        Err(StoreError::AuditEventConflict)
    ));

    for id in ["0002", "0003", "0004"] {
        store
            .insert_audit_event(&event(id, "project-a", "compute"))
            .await
            .unwrap();
    }
    store
        .insert_audit_event(&event("0005", "project-b", "compute"))
        .await
        .unwrap();

    let page = store
        .list_audit_events_page("project-a", None, 2)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.has_more);
    assert_eq!(page.continuation_key.as_deref(), Some("0002"));
    let final_page = store
        .list_audit_events_page("project-a", page.continuation_key.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(final_page.items.len(), 2);
    assert!(!final_page.has_more);
    assert_eq!(
        store
            .prune_audit_events_before("2026-01-01T00:00:02Z", 1)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        store.get_audit_event("project-a", "0001").await,
        Err(StoreError::ResourceNotFound)
    ));

    drop(store);
    let reopened = SqliteStore::connect_file(&path).await.unwrap();
    assert!(matches!(
        reopened.get_audit_event("project-a", "0001").await,
        Err(StoreError::ResourceNotFound)
    ));
    assert_eq!(
        reopened
            .get_audit_event("project-a", "0002")
            .await
            .unwrap()
            .event_id,
        "0002"
    );
    assert!(matches!(
        reopened.get_audit_event("project-b", "0001").await,
        Err(StoreError::ResourceNotFound)
    ));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn unified_audit_query_pushes_supported_filters_and_scope() {
    let store = O3kStore::connect_sqlite_memory().await.unwrap();
    let sqlite = match &store {
        O3kStore::Sqlite(s) => s,
        _ => unreachable!(),
    };
    sqlite
        .insert_audit_event(&event("0001", "project-a", "compute"))
        .await
        .unwrap();
    sqlite
        .insert_audit_event(&event("0002", "project-a", "network"))
        .await
        .unwrap();
    sqlite
        .insert_audit_event(&event("0003", "project-b", "compute"))
        .await
        .unwrap();
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
        request_id: Some("req-0001".into()),
        audit_id: Some("audit-0001".into()),
        from_timestamp: Some("2025-01-01".into()),
        until_timestamp: Some("2027-01-01".into()),
    };
    let page = o3k_kernel::DurableAuditRepository::page(&store, &query)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].event_id.as_str(), "0001");
}
