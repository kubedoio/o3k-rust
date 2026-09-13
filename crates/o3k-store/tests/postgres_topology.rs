#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use o3k_kernel::{
    BindingTarget, BindingTargetKind, FailureDomain, FailureDomainClass, KernelError,
    TopologyBinding, TopologyStore,
};
use o3k_store::{AuditRepository, PostgresStore};
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;

fn domain(id: &str, class: FailureDomainClass, az: &str, parent: Option<&str>) -> FailureDomain {
    FailureDomain {
        id: id.to_owned(),
        class,
        name: id.to_owned(),
        availability_domain: az.to_owned(),
        parent: parent.map(str::to_owned),
        generation: 1,
        metadata: BTreeMap::new(),
    }
}

fn binding(failure_domain: &str, kind: BindingTargetKind, id: &str) -> TopologyBinding {
    TopologyBinding {
        failure_domain: failure_domain.to_owned(),
        target: BindingTarget {
            kind,
            id: id.to_owned(),
        },
    }
}

fn is_conflict(error: &KernelError, needle: &str) -> bool {
    matches!(error, KernelError::TopologyCorrupt(reason) if reason.contains(needle))
}

fn audit_event(event_id: &str, principal: &str) -> o3k_kernel::AuditEvent {
    use o3k_kernel::{
        ActionId, AuditOutcome, EventId, OwnershipScope, PrincipalId, PrincipalKind, ResourceId,
        ResourceType, ScopeId, ScopeKind, ServiceNamespace,
    };
    o3k_kernel::AuditEvent {
        event_id: EventId::from_string(event_id.to_owned()),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        request_id: "req-1".to_owned(),
        audit_id: "trace-1".to_owned(),
        principal_id: PrincipalId::new_unchecked(principal),
        principal_kind: PrincipalKind::User,
        effective_scope: OwnershipScope::new(
            ScopeId::new_unchecked("system"),
            ScopeKind::System,
            None,
            None,
        ),
        service_namespace: ServiceNamespace::new_unchecked("topology".to_owned()),
        action: ActionId::new_unchecked("topology", "ManageTopology"),
        resource_type: Some(ResourceType::new_unchecked("topology", "failure_domain")),
        resource_id: Some(ResourceId::new_unchecked("rack-1")),
        owner_scope: None,
        authorization_decision: None,
        operation_id: None,
        outcome: AuditOutcome::Succeeded,
        reason_category: None,
        service_principal: None,
    }
}

/// The conformance binary runs tests concurrently. Serialize fixture
/// provisioning/teardown; each test still uses its own disposable database.
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// A disposable PostgreSQL database so concurrently running PostgreSQL
/// integration binaries cannot invalidate this test's migrations or state.
struct Fixture {
    admin_url: String,
    database: String,
    url: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let url = std::env::var("O3K_DATABASE_URL").ok()?;
        let parsed = url::Url::parse(&url).ok()?;
        let database = format!("o3k_topology_{}", uuid::Uuid::now_v7().simple());
        let mut admin_url = parsed.clone();
        admin_url.set_path("/postgres");
        let admin_url = admin_url.to_string();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .ok()?;
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&admin)
            .await
            .ok()?;
        // Close the creator session before any store connects, so the admin
        // connection can never be the lingering backend that blocks the drop.
        admin.close().await;
        let mut isolated = parsed;
        isolated.set_path(&format!("/{database}"));
        Some(Self {
            admin_url,
            database,
            url: isolated.to_string(),
        })
    }

    async fn store(&self) -> PostgresStore {
        PostgresStore::connect(&self.url).await.unwrap()
    }

    async fn dispose(self, stores: &[&PostgresStore]) {
        for store in stores {
            store.pool().close().await;
        }
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        // Under nextest every test runs in its own process, so the process-wide
        // `test_lock` cannot serialize fixture teardown. Make `dispose`
        // self-sufficient: terminate any leftover backend, then force the drop
        // with a bounded retry.
        let _ = sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
        )
        .bind(&self.database)
        .execute(&admin)
        .await;
        let mut last_error: Option<sqlx::Error> = None;
        for attempt in 0..5u64 {
            match sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
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
        let last_error = last_error.unwrap_or_else(|| {
            sqlx::Error::Protocol("DROP DATABASE retry loop produced no error".into())
        });
        admin.close().await;
        // Retries exhausted: fail the test with the final driver error.
        let drop_result: Result<(), sqlx::Error> = Err(last_error);
        assert!(
            drop_result.is_ok(),
            "failed to drop disposable database {} after retries: {drop_result:?}",
            self.database
        );
    }
}

async fn seed_region_az(store: &PostgresStore) {
    store.insert_region("region-a", None).await.unwrap();
    store
        .insert_availability_domain("region-a", "az-1", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_topology_audit_is_atomic_with_the_mutation() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL topology audit: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    seed_region_az(&store).await;

    // A successful mutation persists both the domain and its audit row.
    let ok_audit = audit_event("pg-audit-ok", "operator-1");
    store
        .insert_failure_domain(
            &domain("rack-ok", FailureDomainClass::Rack, "az-1", None),
            Some(&ok_audit),
        )
        .await
        .unwrap();
    let stored = store
        .get_audit_event("system", "pg-audit-ok")
        .await
        .unwrap();
    assert_eq!(
        stored,
        o3k_store::AuditEventRecord::from_kernel_event(&ok_audit),
        "audit row persisted atomically with the mutation"
    );

    // An audit conflict (same event_id, different content) must abort the whole
    // mutation: the domain is NOT persisted and the audit row is untouched.
    let prior = audit_event("pg-audit-collide", "someone-else");
    store.record_audit(&prior).await.unwrap();
    let conflicting = audit_event("pg-audit-collide", "operator-1");
    let error = store
        .insert_failure_domain(
            &domain("rack-collide", FailureDomainClass::Rack, "az-1", None),
            Some(&conflicting),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, KernelError::AuditConflict),
        "unexpected: {error:?}"
    );
    assert!(
        store
            .load_snapshot()
            .await
            .unwrap()
            .failure_domains
            .iter()
            .all(|d| d.id != "rack-collide"),
        "failed mutation must not persist"
    );
    let surviving = store
        .get_audit_event("system", "pg-audit-collide")
        .await
        .unwrap();
    assert_eq!(
        surviving,
        o3k_store::AuditEventRecord::from_kernel_event(&prior),
        "pre-existing audit row untouched by the rollback"
    );

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_topology_crud_restart_parity_and_conformance() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL topology: O3K_DATABASE_URL unavailable");
        return;
    };
    let first = fixture.store().await;
    o3k_store::run_topology_store_conformance(&first)
        .await
        .unwrap();

    // Restart parity: a second store against the same database reconstructs
    // identical durable state after the first pool is gone.
    seed_region_az(&first).await;
    first
        .insert_failure_domain(
            &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
            None,
        )
        .await
        .unwrap();
    let mut renamed = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
    renamed.name = "Rack One".to_owned();
    renamed.generation = 2;
    renamed.metadata.insert("aisle".to_owned(), "7".to_owned());
    first
        .update_failure_domain(&renamed, 1, None)
        .await
        .unwrap();
    first
        .insert_binding(&binding("rack-1", BindingTargetKind::Host, "host-1"), None)
        .await
        .unwrap();
    let expected = first.load_snapshot().await.unwrap();
    first.pool().close().await;

    let restarted = fixture.store().await;
    assert_eq!(restarted.load_snapshot().await.unwrap(), expected);
    // The restarted store keeps mutating on the same durable sequence:
    // unbind, then CAS-delete the failure domain.
    restarted
        .delete_binding(&binding("rack-1", BindingTargetKind::Host, "host-1"), None)
        .await
        .unwrap();
    restarted
        .delete_failure_domain("rack-1", 2, None)
        .await
        .unwrap();

    fixture.dispose(&[&restarted]).await;
}

#[tokio::test]
async fn postgres_topology_cas_and_referential_protections() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL topology protections: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    seed_region_az(&store).await;
    store
        .insert_failure_domain(
            &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
            None,
        )
        .await
        .unwrap();
    store
        .insert_failure_domain(
            &domain(
                "chassis-1",
                FailureDomainClass::Chassis,
                "az-1",
                Some("rack-1"),
            ),
            None,
        )
        .await
        .unwrap();
    store
        .insert_binding(
            &binding("chassis-1", BindingTargetKind::Host, "host-1"),
            None,
        )
        .await
        .unwrap();

    // FK protections surface as conflicts.
    let error = store
        .delete_failure_domain("rack-1", 1, None)
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "child failure domains or bindings"));
    let error = store
        .delete_availability_domain("az-1", None)
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "still has failure domains"));
    let error = store.delete_region("region-a", None).await.unwrap_err();
    assert!(is_conflict(&error, "still has availability domains"));
    let error = store
        .insert_failure_domain(
            &domain("rack-2", FailureDomainClass::Rack, "az-1", Some("ghost")),
            None,
        )
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "unknown availability domain or parent"));
    let error = store
        .insert_binding(
            &binding("ghost-fd", BindingTargetKind::Host, "host-9"),
            None,
        )
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "unknown failure domain"));

    // Duplicate identity conflicts.
    let error = store
        .insert_failure_domain(
            &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
            None,
        )
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "duplicate"));

    // CAS update and delete with stale generations conflict truthfully.
    let mut stale = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
    stale.generation = 2;
    let error = store
        .update_failure_domain(&stale, 99, None)
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "stale generation"));
    let error = store
        .update_failure_domain(
            &domain("ghost", FailureDomainClass::Rack, "az-1", None),
            1,
            None,
        )
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "unknown failure domain"));
    let error = store
        .delete_failure_domain("ghost", 1, None)
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "unknown failure domain"));
    let error = store
        .delete_failure_domain("rack-1", 7, None)
        .await
        .unwrap_err();
    assert!(is_conflict(&error, "stale generation"));

    // Replay idempotency: duplicate inserts and absent deletes converge.
    store.insert_region("region-a", None).await.unwrap();
    store
        .insert_availability_domain("region-a", "az-1", None)
        .await
        .unwrap();
    store
        .insert_binding(
            &binding("chassis-1", BindingTargetKind::Host, "host-1"),
            None,
        )
        .await
        .unwrap();
    store
        .delete_binding(
            &binding("chassis-1", BindingTargetKind::Host, "host-9"),
            None,
        )
        .await
        .unwrap();

    let snapshot = store.load_snapshot().await.unwrap();
    assert_eq!(snapshot.failure_domains.len(), 2);
    assert_eq!(snapshot.bindings.len(), 1);
    assert!(
        snapshot
            .failure_domains
            .iter()
            .all(|stored| stored.generation == 1)
    );

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_topology_concurrent_writers_serialize_on_the_advisory_lock() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL topology concurrency: O3K_DATABASE_URL unavailable");
        return;
    };
    // Two independent stores = two independent pools, so the creates truly
    // race; the advisory lock must serialize them into one durable sequence.
    let store_a = fixture.store().await;
    let store_b = fixture.store().await;
    seed_region_az(&store_a).await;

    // Same failure domain id from both pools: exactly one insert wins, and
    // each store still persists its own unique id.
    let duplicate = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
    let (a, b) = tokio::join!(
        store_a.insert_failure_domain(&duplicate, None),
        store_b.insert_failure_domain(&duplicate, None),
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "exactly one racing create may win: {a:?} {b:?}"
    );
    let loser = if a.is_ok() { b } else { a };
    assert!(matches!(loser, Err(KernelError::TopologyCorrupt(_))));

    let unique_a = domain("rack-a-only", FailureDomainClass::Rack, "az-1", None);
    let unique_b = domain("rack-b-only", FailureDomainClass::Rack, "az-1", None);
    let (first, second) = tokio::join!(
        store_a.insert_failure_domain(&unique_a, None),
        store_b.insert_failure_domain(&unique_b, None),
    );
    first.unwrap();
    second.unwrap();

    // Racing CAS updates against generation 1: exactly one bump wins; the
    // loser observes a stale generation, never a lost update.
    let mut bump_a = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
    bump_a.name = "writer-a".to_owned();
    bump_a.generation = 2;
    let mut bump_b = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
    bump_b.name = "writer-b".to_owned();
    bump_b.generation = 2;
    let (a, b) = tokio::join!(
        store_a.update_failure_domain(&bump_a, 1, None),
        store_b.update_failure_domain(&bump_b, 1, None),
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "exactly one racing CAS update may win: {a:?} {b:?}"
    );
    let loser = if a.is_ok() { b } else { a };
    assert!(is_conflict(&loser.unwrap_err(), "stale generation"));

    let snapshot = store_a.load_snapshot().await.unwrap();
    let domain_ids: Vec<&str> = snapshot
        .failure_domains
        .iter()
        .map(|stored| stored.id.as_str())
        .collect();
    assert_eq!(domain_ids, vec!["rack-1", "rack-a-only", "rack-b-only"]);
    let winner = snapshot
        .failure_domains
        .iter()
        .find(|stored| stored.id == "rack-1")
        .unwrap();
    assert_eq!(winner.generation, 2);
    assert!(winner.name == "writer-a" || winner.name == "writer-b");

    fixture.dispose(&[&store_a, &store_b]).await;
}

#[tokio::test]
async fn postgres_pre_topology_schema_upgrades_with_usable_topology_tables() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL topology migration: O3K_DATABASE_URL unavailable");
        return;
    };

    // Apply every migration before the topology one, i.e. a database created
    // before 0029_topology.sql.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&fixture.url)
        .await
        .unwrap();
    let all = sqlx::migrate!("./migrations_postgres");
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            all.migrations
                .iter()
                .filter(|migration| migration.version < 29)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    legacy.run(&pool).await.unwrap();
    let present: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('topology_regions')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        present.is_none(),
        "topology_regions must not exist before the topology migration"
    );
    pool.close().await;

    // A normal connect runs the remaining migration and the tables are usable.
    let store = fixture.store().await;
    store.insert_region("region-a", None).await.unwrap();
    store
        .insert_availability_domain("region-a", "az-1", None)
        .await
        .unwrap();
    store
        .insert_failure_domain(
            &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
            None,
        )
        .await
        .unwrap();
    let snapshot = store.load_snapshot().await.unwrap();
    assert_eq!(snapshot.failure_domains.len(), 1);

    let index_present: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('idx_topology_bindings_target')::text")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(
        index_present.is_some(),
        "the binding target read index must exist after the upgrade"
    );

    fixture.dispose(&[&store]).await;
}
