#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use o3k_kernel::{
    INGEST_BUCKET_WIDTH_MS, MeterObservation, MeteringRepository, UsageGranularity, UsageQuery,
};
use o3k_store::SqliteStore;
use sqlx::sqlite::SqlitePoolOptions;

const HOUR: i64 = INGEST_BUCKET_WIDTH_MS;
const T0: i64 = 100 * HOUR;
const METER: &str = "compute:instance_seconds";

fn observation(resource: &str, quantity: u64, consuming: bool, at: i64) -> MeterObservation {
    MeterObservation {
        meter_key: METER.into(),
        scope: "project-a".into(),
        resource_id: resource.into(),
        quantity,
        consuming,
        observed_at_ms: at,
        authority: "o3k-lifecycle".into(),
    }
}

fn query() -> UsageQuery {
    UsageQuery {
        scope: "project-a".into(),
        meter_keys: vec![METER.into()],
        start_ms: T0,
        end_ms: T0 + HOUR,
        granularity: UsageGranularity::Hour,
        resource_id: None,
        evaluated_at_ms: T0 + HOUR,
    }
}

async fn table_exists(pool: &sqlx::SqlitePool, name: &str) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    count > 0
}

#[tokio::test]
async fn sqlite_pre_metering_schema_upgrades_with_usable_metering_tables() {
    let path = std::env::temp_dir().join(format!(
        "o3k-metering-migration-{}.db",
        uuid::Uuid::now_v7().simple()
    ));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();

    // Apply every migration before the metering one, i.e. a database created
    // before 0045_metering.sql.
    let all = sqlx::migrate!("./migrations");
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            all.migrations
                .iter()
                .filter(|migration| migration.version < 45)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    legacy.run(&pool).await.unwrap();
    for name in [
        "metering_authority",
        "metering_intervals",
        "metering_aggregates",
    ] {
        assert!(
            !table_exists(&pool, name).await,
            "{name} must not exist before the metering migration"
        );
    }
    pool.close().await;

    // A normal connect runs the remaining migration and the tables are usable.
    let store = SqliteStore::connect_file(&path).await.unwrap();
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    let report = store.usage(&query()).await.unwrap();
    assert_eq!(report.meters[0].total, "60.000");
    drop(store);

    let check = SqlitePoolOptions::new().connect(&url).await.unwrap();
    for name in [
        "metering_authority",
        "metering_intervals",
        "metering_aggregates",
    ] {
        assert!(
            table_exists(&check, name).await,
            "{name} must exist after the upgrade"
        );
    }
    check.close().await;

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}
