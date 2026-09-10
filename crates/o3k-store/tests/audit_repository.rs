#![allow(clippy::unwrap_used, clippy::expect_used)]

use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditQuery, AuthContext, DurableAuditRepository,
    DurableAuditSink, OwnershipScope, Principal, PrincipalId, RequiredAuditPublisher, ScopeId,
    ServiceNamespace, UserPrincipal,
};
use o3k_store::{AuditEventRecord, AuditRepository, O3kStore, SqliteStore, StoreError};
use std::sync::Arc;

struct FailingAuditRepository;

#[async_trait::async_trait]
impl DurableAuditRepository for FailingAuditRepository {
    async fn append(&self, _event: &AuditEvent) -> Result<(), o3k_kernel::KernelError> {
        Err(o3k_kernel::KernelError::AuditUnavailable(
            "database unavailable".into(),
        ))
    }

    async fn page(
        &self,
        _query: &AuditQuery,
    ) -> Result<o3k_kernel::DurableAuditPage, o3k_kernel::KernelError> {
        Err(o3k_kernel::KernelError::AuditUnavailable(
            "database unavailable".into(),
        ))
    }

    async fn prune_before(&self, _cutoff: &str) -> Result<u64, o3k_kernel::KernelError> {
        Err(o3k_kernel::KernelError::AuditUnavailable(
            "database unavailable".into(),
        ))
    }
}

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

fn kernel_event() -> AuditEvent {
    let auth = AuthContext::new(
        Principal::User(UserPrincipal::new(
            PrincipalId::new_unchecked("production-user"),
            "production-user",
            Some("default".into()),
        )),
        OwnershipScope::project(ScopeId::new_unchecked("production-project"), None, None),
        vec!["member".into()],
        0,
        u64::MAX,
        "audit-production",
        "request-production",
        None,
    );
    AuditEvent::from_auth(
        &auth,
        ServiceNamespace::new_unchecked("compute".into()),
        ActionId::new_unchecked("compute", "CreateServer"),
        AuditOutcome::Succeeded,
    )
}

#[tokio::test]
async fn durable_sink_production_like_sqlite_composition_persists_event() {
    let path = std::env::temp_dir().join(format!("o3k-audit-sink-{}.db", uuid::Uuid::now_v7()));
    let store = Arc::new(SqliteStore::connect_file(&path).await.unwrap());
    let unified = Arc::new(O3kStore::Sqlite((*store).clone()));
    let publisher = DurableAuditSink::new(unified.clone());
    let event = kernel_event();
    publisher.publish(&event).await.unwrap();
    let query = AuditQuery {
        scope: event.effective_scope.clone(),
        after_event_id: None,
        event_id: None,
        limit: 10,
        service: None,
        action: None,
        outcome: None,
        resource_type: None,
        resource_id: None,
        operation_id: None,
        principal_id: None,
        request_id: None,
        audit_id: None,
        from_timestamp: None,
        until_timestamp: None,
    };
    let page = unified.page(&query).await.unwrap();
    assert_eq!(page.events, vec![event]);
    drop(publisher);
    drop(unified);
    drop(store);
    let reopened = O3kStore::connect_sqlite_file(&path).await.unwrap();
    let page = reopened.page(&query).await.unwrap();
    assert_eq!(page.events.len(), 1);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn durable_audit_b0_runtime_matrix_covers_mandatory_paths_and_recovery() {
    let path = std::env::temp_dir().join(format!("o3k-audit-b0-{}.db", uuid::Uuid::now_v7()));
    let store = Arc::new(SqliteStore::connect_file(&path).await.unwrap());
    let unified = Arc::new(O3kStore::Sqlite((*store).clone()));
    let publisher = Arc::new(DurableAuditSink::new(unified.clone()));

    // These are the canonical outcomes emitted by compute/image/network
    // mutation paths, including denied, deterministic failure, and timeout.
    let mut events = Vec::new();
    for (id, action, outcome) in [
        ("b0-0001", "CreateServer", AuditOutcome::Succeeded),
        ("b0-0002", "CreateImage", AuditOutcome::Succeeded),
        ("b0-0003", "CreateNetwork", AuditOutcome::Succeeded),
        ("b0-0004", "UpdateVolume", AuditOutcome::Succeeded),
        ("b0-0005", "StartServer", AuditOutcome::Succeeded),
        ("b0-0006", "CreateKeypair", AuditOutcome::Succeeded),
        ("b0-0007", "DeleteServer", AuditOutcome::Denied),
        ("b0-0008", "CreateServer", AuditOutcome::Failed),
        ("b0-0009", "CreateServer", AuditOutcome::UnknownOutcome),
    ] {
        let mut event = kernel_event();
        event.event_id = o3k_kernel::EventId::from_string(id.into());
        event.action = ActionId::new_unchecked("compute", action);
        event.outcome = outcome;
        event.reason_category = Some("bounded-provider-result".into());
        event.request_id = format!("request-{id}");
        event.audit_id = format!("audit-{id}");
        events.push(event);
    }
    for event in &events {
        publisher.publish(event).await.unwrap();
    }
    // Idempotent response-loss replay is safe and does not duplicate evidence.
    publisher.publish(&events[0]).await.unwrap();

    let query = AuditQuery {
        scope: events[0].effective_scope.clone(),
        after_event_id: None,
        event_id: None,
        limit: 20,
        service: None,
        action: None,
        outcome: None,
        resource_type: None,
        resource_id: None,
        operation_id: None,
        principal_id: None,
        request_id: None,
        audit_id: None,
        from_timestamp: None,
        until_timestamp: None,
    };
    let page = unified.page(&query).await.unwrap();
    assert_eq!(page.events.len(), events.len());
    assert!(
        page.events
            .iter()
            .any(|e| e.outcome == AuditOutcome::Denied)
    );
    assert!(
        page.events
            .iter()
            .any(|e| e.outcome == AuditOutcome::Failed)
    );
    assert!(
        page.events
            .iter()
            .any(|e| e.outcome == AuditOutcome::UnknownOutcome)
    );
    let serialized = serde_json::to_string(&page.events).unwrap();
    for secret in [
        "password",
        "bearer",
        "private_key",
        "token",
        "chap",
        "user_data",
    ] {
        assert!(!serialized.to_ascii_lowercase().contains(secret));
    }

    // Concurrent writers preserve every event and event identity.
    let first = events[1].clone();
    let second = events[2].clone();
    let (a, b) = tokio::join!(publisher.publish(&first), publisher.publish(&second));
    assert!(a.is_ok() && b.is_ok());

    drop(publisher);
    drop(unified);
    drop(store);
    let reopened = O3kStore::connect_sqlite_file(&path).await.unwrap();
    assert_eq!(
        reopened.page(&query).await.unwrap().events.len(),
        events.len()
    );
    let _ = std::fs::remove_file(path);

    // Mandatory publication fails closed when the durable repository is down.
    let failing = DurableAuditSink::new(Arc::new(FailingAuditRepository));
    assert!(failing.publish(&events[0]).await.is_err());
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
        event_id: None,
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

#[tokio::test]
async fn sqlite_same_id_concurrent_replay_converges() {
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let e = event("concurrent", "project-a", "compute");
    let (a, b) = tokio::join!(store.insert_audit_event(&e), store.insert_audit_event(&e));
    assert!(a.is_ok() && b.is_ok());
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
