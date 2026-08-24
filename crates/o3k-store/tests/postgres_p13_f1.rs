#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use o3k_store::{CanonicalNetworkRecord, PostgresStore, StoreError};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::sync::Mutex;
use uuid::Uuid;

static TEST_DATABASE_LOCK: Mutex<()> = Mutex::const_new(());

fn database_url() -> String {
    std::env::var("O3K_DATABASE_URL")
        .expect("O3K_DATABASE_URL must be set for P13.1F1 PostgreSQL conformance")
}

async fn fresh_pool(url: &str) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(url)
        .await
        .expect("connect to PostgreSQL");
    sqlx::query("DROP SCHEMA IF EXISTS public CASCADE")
        .execute(&pool)
        .await
        .expect("drop test schema");
    sqlx::query("CREATE SCHEMA public")
        .execute(&pool)
        .await
        .expect("create test schema");
    pool
}

async fn apply_legacy_migrations(pool: &PgPool) {
    let all = sqlx::migrate!("./migrations_postgres");
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(all.migrations.iter().take(9).cloned().collect()),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    legacy.run(pool).await.expect("apply legacy migrations");
}

async fn seed_legacy_state(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let network_a = Uuid::from_u128(0x1301);
    let network_b = Uuid::from_u128(0x1302);
    let realm_a = Uuid::from_u128(0x1311);
    let realm_b = Uuid::from_u128(0x1312);
    let endpoint_a = Uuid::from_u128(0x1321);

    sqlx::query(
        "INSERT INTO network_networks (id, name, project_id, status) VALUES ($1, $2, $3, $4), ($5, $6, $7, $8)",
    )
    .bind(network_a.to_string())
    .bind("network-a")
    .bind("project-a")
    .bind("ACTIVE")
    .bind(network_b.to_string())
    .bind("network-b")
    .bind("project-b")
    .bind("ACTIVE")
    .execute(pool)
    .await
    .expect("legacy networks");

    sqlx::query(
        "INSERT INTO network_intents (id, project_id, generation, payload, status) VALUES ($1, $2, 7, $3, 'active')",
    )
    .bind(network_a.to_string())
    .bind("project-a")
    .bind(format!(r#"{{"id":"{}"}}"#, network_a))
    .execute(pool)
    .await
    .expect("legacy network intent");

    sqlx::query(
        "INSERT INTO network_subnets (id, network_id, name, project_id, cidr, gateway_ip, allocation_start, allocation_end) VALUES ($1, $2, $3, $4, $5, $6, $7, $8), ($9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(realm_a.to_string())
    .bind(network_a.to_string())
    .bind("subnet-a")
    .bind("project-a")
    .bind("10.0.0.0/24")
    .bind("10.0.0.1")
    .bind("10.0.0.2")
    .bind("10.0.0.254")
    .bind(realm_b.to_string())
    .bind(network_b.to_string())
    .bind("subnet-b")
    .bind("project-b")
    .bind("10.0.0.0/24")
    .bind("10.0.0.1")
    .bind("10.0.0.2")
    .bind("10.0.0.254")
    .execute(pool)
    .await
    .expect("legacy subnets");

    sqlx::query(
        "INSERT INTO network_ports (id, network_id, subnet_id, project_id, name, mac_address, fixed_ip, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(endpoint_a.to_string())
    .bind(network_a.to_string())
    .bind(realm_a.to_string())
    .bind("project-a")
    .bind("port-a")
    .bind("02:00:00:00:13:21")
    .bind("10.0.0.10")
    .bind("ACTIVE")
    .execute(pool)
    .await
    .expect("legacy endpoint");

    (network_a, network_b, realm_a, endpoint_a)
}

#[tokio::test]
#[ignore = "requires the configured PostgreSQL conformance database"]
async fn postgres_p13_f1_migrates_and_reopens_canonical_network_state() {
    let _database_guard = TEST_DATABASE_LOCK.lock().await;
    let url = database_url();
    let pool = fresh_pool(&url).await;
    apply_legacy_migrations(&pool).await;
    let (network_a, network_b, realm_a, endpoint_a) = seed_legacy_state(&pool).await;

    let store = PostgresStore::connect_pool(pool.clone())
        .await
        .expect("run canonical migration and backfill");
    let network = store
        .get_canonical_network("project-a", &network_a)
        .await
        .expect("get migrated network")
        .expect("network exists");
    assert_eq!(
        network,
        CanonicalNetworkRecord {
            id: network_a,
            project_id: "project-a".into(),
            name: "network-a".into(),
            generation: 7,
            state: "active".into(),
        }
    );
    assert!(
        store
            .list_canonical_realms("project-a", &Uuid::from_u128(0x1fff))
            .await
            .expect("empty realm lookup")
            .is_empty()
    );

    let realms_a = store
        .list_canonical_realms("project-a", &network_a)
        .await
        .expect("list migrated realms");
    assert_eq!(realms_a.len(), 1);
    assert_eq!(realms_a[0].id, realm_a);
    assert_eq!(realms_a[0].network_id, network_a);
    assert_eq!(realms_a[0].project_id, "project-a");

    let pools = store
        .list_canonical_pools("project-a", &realm_a)
        .await
        .expect("list migrated pools");
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].id, realm_a);
    assert_eq!(pools[0].realm_id, realm_a);

    let endpoints = store
        .list_canonical_endpoints("project-a", &realm_a)
        .await
        .expect("list migrated endpoints");
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].id, endpoint_a);
    assert_eq!(endpoints[0].fixed_ip.to_string(), "10.0.0.10");

    let overlap = store
        .list_canonical_realms("project-b", &network_b)
        .await
        .expect("list overlapping realm");
    assert_eq!(overlap.len(), 1);
    let same_ip = o3k_store::CanonicalEndpointRecord {
        id: Uuid::from_u128(0x1322),
        realm_id: overlap[0].id,
        project_id: "project-b".into(),
        fixed_ip: "10.0.0.10".parse().unwrap(),
        mac: "02:00:00:00:13:22".into(),
        generation: 1,
        state: "active".into(),
    };
    store
        .insert_canonical_endpoint(&same_ip)
        .await
        .expect("same IP in another realm");
    let duplicate = o3k_store::CanonicalEndpointRecord {
        id: Uuid::from_u128(0x1323),
        mac: "02:00:00:00:13:23".into(),
        ..same_ip.clone()
    };
    assert!(matches!(
        store.insert_canonical_endpoint(&duplicate).await,
        Err(StoreError::ResourceAlreadyExists)
    ));

    assert!(matches!(
        store
            .insert_canonical_realm(&o3k_store::CanonicalAddressRealmRecord {
                id: Uuid::from_u128(0x1331),
                network_id: network_a,
                project_id: "project-b".into(),
                prefix: "10.2.0.0/24".into(),
                overlapping_prefixes: false,
                generation: 1,
                state: "active".into(),
            })
            .await,
        Err(StoreError::OwnershipConflict)
    ));

    let reopened = PostgresStore::connect(&url).await.expect("reopen store");
    assert_eq!(
        reopened
            .get_canonical_network("project-a", &network_a)
            .await
            .expect("reopened network")
            .expect("reopened network exists")
            .generation,
        7
    );
    reopened
        .delete_canonical_realm("project-a", &realm_a)
        .await
        .expect_err("dependent pool/endpoint blocks realm deletion");
    sqlx::query("DELETE FROM canonical_endpoints WHERE realm_id = $1")
        .bind(realm_a.to_string())
        .execute(reopened.pool())
        .await
        .expect("remove test endpoint");
    sqlx::query("DELETE FROM canonical_address_pools WHERE realm_id = $1")
        .bind(realm_a.to_string())
        .execute(reopened.pool())
        .await
        .expect("remove test pool");
    reopened
        .delete_canonical_realm("project-a", &realm_a)
        .await
        .expect("remove realm");
    assert!(
        reopened
            .list_canonical_realms("project-a", &network_a)
            .await
            .expect("list after realm removal")
            .is_empty()
    );
    assert!(
        reopened
            .get_canonical_network("project-a", &network_a)
            .await
            .expect("network after realm removal")
            .is_some()
    );

    let constraints: Vec<String> = sqlx::query(
        "SELECT conname FROM pg_constraint WHERE conrelid IN ('canonical_networks'::regclass, 'canonical_address_realms'::regclass, 'canonical_address_pools'::regclass, 'canonical_endpoints'::regclass, 'canonical_realm_encapsulation_bindings'::regclass) ORDER BY conname",
    )
    .fetch_all(reopened.pool())
    .await
    .expect("inspect canonical constraints")
    .into_iter()
    .map(|row| row.get("conname"))
    .collect();
    assert!(
        constraints
            .iter()
            .any(|name| name == "canonical_networks_pkey")
    );
    assert!(
        constraints
            .iter()
            .any(|name| name == "canonical_address_realms_network_id_fkey")
    );
    assert!(
        constraints
            .iter()
            .any(|name| name == "canonical_endpoints_realm_id_fixed_ip_key")
    );
    reopened
        .clean_tables_for_testing()
        .await
        .expect("clean conformance database");
}

#[tokio::test]
#[ignore = "requires the configured PostgreSQL conformance database"]
async fn postgres_p13_f1_invalid_legacy_state_fails_closed() {
    let _database_guard = TEST_DATABASE_LOCK.lock().await;
    let url = database_url();
    let pool = fresh_pool(&url).await;
    apply_legacy_migrations(&pool).await;
    let network = Uuid::from_u128(0x1401);
    let subnet = Uuid::from_u128(0x1411);
    sqlx::query(
        "INSERT INTO network_networks (id, name, project_id, status) VALUES ($1, 'network', 'project-a', 'ACTIVE')",
    )
    .bind(network.to_string())
    .execute(&pool)
    .await
    .expect("legacy network");
    sqlx::query(
        "INSERT INTO network_subnets (id, network_id, name, project_id, cidr, gateway_ip, allocation_start, allocation_end) VALUES ($1, $2, 'subnet', 'project-b', 'not-a-cidr', '10.0.0.1', '10.0.0.2', '10.0.0.254')",
    )
    .bind(subnet.to_string())
    .bind(network.to_string())
    .execute(&pool)
    .await
    .expect("invalid legacy subnet fixture");

    let result = PostgresStore::connect_pool(pool.clone()).await;
    assert!(matches!(result, Err(StoreError::OwnershipConflict)));
    let canonical_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM canonical_networks WHERE id = $1")
            .bind(network.to_string())
            .fetch_one(&pool)
            .await
            .expect("inspect rolled-back backfill");
    assert_eq!(canonical_count, 0);
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(&pool)
        .await
        .expect("drop invalid fixture schema");
    sqlx::query("CREATE SCHEMA public")
        .execute(&pool)
        .await
        .expect("restore public schema");
}
