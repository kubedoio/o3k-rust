#![allow(clippy::expect_used, clippy::unwrap_used)]

use o3k_store::{
    AuditEventRecord, CreateAssignmentOutcome, GovernanceAssignmentFilter, GovernanceReferences,
    GovernanceRepository, IdentityRepository, KeystoneDomainRecord, KeystoneProjectRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneUserRecord, PostgresStore,
    StoreError,
};
use sqlx::postgres::PgPoolOptions;

const NOW: &str = "2026-01-01T00:00:00Z";

fn assignment(id: &str, user: &str, project: &str, role: &str) -> KeystoneRoleAssignmentRecord {
    KeystoneRoleAssignmentRecord {
        id: id.to_owned(),
        user_id: user.to_owned(),
        project_id: project.to_owned(),
        role_id: role.to_owned(),
        created_at: NOW.to_owned(),
    }
}

fn audit_event(tag: &str) -> AuditEventRecord {
    AuditEventRecord {
        event_id: format!("pg-gov-audit-{tag}-{}", uuid::Uuid::now_v7()),
        timestamp: NOW.to_owned(),
        request_id: "pg-gov-request".to_owned(),
        audit_id: "pg-gov-audit".to_owned(),
        principal_id: "operator".to_owned(),
        principal_kind: "user".to_owned(),
        effective_scope: "system".to_owned(),
        service: "identity".to_owned(),
        action: "identity:CreateRoleAssignment".to_owned(),
        resource_type: Some("identity:role_assignment".to_owned()),
        resource_id: None,
        owner_scope: None,
        operation_id: None,
        outcome: "succeeded".to_owned(),
        reason_category: None,
    }
}

async fn seed(store: &PostgresStore) {
    store
        .insert_keystone_domain(&KeystoneDomainRecord {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            description: None,
            enabled: true,
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    for i in 1..=3 {
        store
            .insert_keystone_project(&KeystoneProjectRecord {
                id: format!("gov-proj-{i}"),
                domain_id: "default".to_owned(),
                name: format!("gov-proj-{i}"),
                description: None,
                enabled: true,
                created_at: NOW.to_owned(),
            })
            .await
            .unwrap();
        store
            .insert_keystone_role(&KeystoneRoleRecord {
                id: format!("gov-role-{i}"),
                name: format!("gov-role-{i}"),
                description: None,
                created_at: NOW.to_owned(),
            })
            .await
            .unwrap();
    }
    store
        .insert_keystone_user(&KeystoneUserRecord {
            id: "gov-user".to_owned(),
            domain_id: "default".to_owned(),
            name: "gov-user".to_owned(),
            password_hash: "pbkdf2_sha256$1$test".to_owned(),
            email: None,
            enabled: true,
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    store
        .insert_keystone_user(&KeystoneUserRecord {
            id: "gov-user-off".to_owned(),
            domain_id: "default".to_owned(),
            name: "gov-user-off".to_owned(),
            password_hash: "pbkdf2_sha256$1$test".to_owned(),
            email: None,
            enabled: false,
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    store
        .insert_keystone_project(&KeystoneProjectRecord {
            id: "gov-proj-off".to_owned(),
            domain_id: "default".to_owned(),
            name: "gov-proj-off".to_owned(),
            description: None,
            enabled: false,
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
}

async fn audit_count(store: &PostgresStore) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn postgres_governance_repository_parity() {
    let Some(url) = std::env::var("O3K_DATABASE_URL").ok() else {
        eprintln!("skipping PostgreSQL governance parity: O3K_DATABASE_URL unavailable");
        return;
    };
    // Use a disposable database so concurrently running PostgreSQL integration
    // binaries cannot reset this test's schema out from under it.
    let parsed = url::Url::parse(&url).unwrap();
    let database = format!("o3k_governance_{}", uuid::Uuid::now_v7().simple());
    let admin_url = {
        let mut admin = parsed.clone();
        admin.set_path("/postgres");
        admin.to_string()
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let isolated_url = {
        let mut target = parsed;
        target.set_path(&format!("/{database}"));
        target.to_string()
    };
    let store = PostgresStore::connect(&isolated_url).await.unwrap();
    let peer = PostgresStore::connect(&isolated_url).await.unwrap();
    seed(&store).await;

    // Bounded keyset pagination with no overlap.
    let first = store.list_governance_projects_page(None, 2).await.unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["gov-proj-1", "gov-proj-2"]
    );
    assert!(first.has_more);
    assert_eq!(first.continuation_key.as_deref(), Some("gov-proj-2"));
    let second = store
        .list_governance_projects_page(first.continuation_key.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["gov-proj-3", "gov-proj-off"]
    );
    assert!(!second.has_more);
    assert!(store.list_governance_projects_page(None, 0).await.is_err());

    // Reference existence/enablement facts.
    let enabled = store
        .check_governance_references("gov-user", "gov-proj-1", "gov-role-1")
        .await
        .unwrap();
    assert_eq!(
        enabled,
        GovernanceReferences {
            principal_exists: true,
            principal_enabled: true,
            project_exists: true,
            project_enabled: true,
            role_exists: true,
        }
    );
    let disabled = store
        .check_governance_references("gov-user-off", "gov-proj-off", "missing-role")
        .await
        .unwrap();
    assert!(disabled.principal_exists && !disabled.principal_enabled);
    assert!(disabled.project_exists && !disabled.project_enabled);
    assert!(!disabled.role_exists);

    // Create once, replay converges on Existing, exactly one audit row.
    let created = store
        .create_role_assignment_with_audit(
            &assignment("pg-assign-1", "gov-user", "gov-proj-1", "gov-role-1"),
            &audit_event("create"),
        )
        .await
        .unwrap();
    assert!(matches!(created, CreateAssignmentOutcome::Created(_)));
    let replay = store
        .create_role_assignment_with_audit(
            &assignment("pg-assign-2", "gov-user", "gov-proj-1", "gov-role-1"),
            &audit_event("replay"),
        )
        .await
        .unwrap();
    assert!(
        matches!(replay, CreateAssignmentOutcome::Existing(_)),
        "replayed assignment was reported as created"
    );
    if let CreateAssignmentOutcome::Existing(existing) = replay {
        assert_eq!(existing.id, "pg-assign-1");
    }
    assert_eq!(audit_count(&store).await, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keystone_role_assignments")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);

    // Filtered assignment listing and role grants.
    let filtered = store
        .list_governance_assignments_page(
            &GovernanceAssignmentFilter {
                principal_id: Some("gov-user".to_owned()),
                project_id: Some("gov-proj-1".to_owned()),
                role_id: None,
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    let grants = store.get_governance_role_grants("gov-user").await.unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].project_id, "gov-proj-1");

    // Required-audit failure rolls the assignment mutation back.
    sqlx::query(
        "CREATE FUNCTION governance_test_fail_audit() RETURNS trigger AS $$
         BEGIN RAISE EXCEPTION 'injected audit failure'; END;
         $$ LANGUAGE plpgsql",
    )
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER governance_test_fail_audit BEFORE INSERT ON audit_events
         FOR EACH ROW EXECUTE FUNCTION governance_test_fail_audit()",
    )
    .execute(store.pool())
    .await
    .unwrap();
    let failed = store
        .create_role_assignment_with_audit(
            &assignment("pg-assign-rollback", "gov-user", "gov-proj-2", "gov-role-2"),
            &audit_event("fail"),
        )
        .await;
    assert!(matches!(failed, Err(StoreError::Database(_))));
    assert_eq!(
        store
            .get_governance_assignment("pg-assign-rollback")
            .await
            .unwrap(),
        None
    );
    let failed_delete = store
        .delete_role_assignment_with_audit("pg-assign-1", &audit_event("fail-delete"))
        .await;
    assert!(matches!(failed_delete, Err(StoreError::Database(_))));
    assert!(
        store
            .get_governance_assignment("pg-assign-1")
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("DROP TRIGGER governance_test_fail_audit ON audit_events")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION governance_test_fail_audit()")
        .execute(store.pool())
        .await
        .unwrap();

    // Delete succeeds and is reported once.
    assert!(
        store
            .delete_role_assignment_with_audit("pg-assign-1", &audit_event("delete"))
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .get_governance_assignment("pg-assign-1")
            .await
            .unwrap(),
        None
    );
    assert!(
        !store
            .delete_role_assignment_with_audit("pg-assign-1", &audit_event("delete-absent"))
            .await
            .unwrap()
    );

    // Two independent writers of the identical assignment converge on one row.
    let race_a = assignment("pg-race-a", "gov-user", "gov-proj-3", "gov-role-3");
    let race_b = assignment("pg-race-b", "gov-user", "gov-proj-3", "gov-role-3");
    let race_a_audit = audit_event("race-a");
    let race_b_audit = audit_event("race-b");
    let (a, b) = tokio::join!(
        store.create_role_assignment_with_audit(&race_a, &race_a_audit),
        peer.create_role_assignment_with_audit(&race_b, &race_b_audit),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let created = [&a, &b]
        .iter()
        .filter(|outcome| matches!(outcome, CreateAssignmentOutcome::Created(_)))
        .count();
    let existing = [&a, &b]
        .iter()
        .filter(|outcome| matches!(outcome, CreateAssignmentOutcome::Existing(_)))
        .count();
    assert_eq!((created, existing), (1, 1), "{a:?} / {b:?}");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM keystone_role_assignments WHERE user_id = 'gov-user' AND project_id = 'gov-proj-3'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(rows, 1);

    store.pool().close().await;
    peer.pool().close().await;
    drop_disposable_database(&admin_url, &database).await;
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
