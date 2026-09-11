#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    borrow::Cow,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use o3k_kernel::metering::MAX_OPEN_INTERVALS;
use o3k_kernel::{
    INGEST_BUCKET_WIDTH_MS, KernelError, MAX_USAGE_SERIES, MeterObservation, MeterUsageReport,
    MeteringRepository, UsageGranularity, UsageQuery, UsageStatus,
};
use o3k_store::PostgresStore;
use sqlx::postgres::PgPoolOptions;

const HOUR: i64 = INGEST_BUCKET_WIDTH_MS;
const DAY: i64 = 86_400_000;
const T0: i64 = 100 * HOUR;
const METER: &str = "compute:instance_seconds";

/// Number of distinct resource series used by the concurrent snapshot test.
const SERIES: i64 = 50;
/// Gap between consecutive series start instants.
const STEP: i64 = 30_000;

fn observation(
    scope: &str,
    resource: &str,
    quantity: u64,
    consuming: bool,
    at: i64,
) -> MeterObservation {
    MeterObservation {
        meter_key: METER.into(),
        scope: scope.into(),
        resource_id: resource.into(),
        quantity,
        consuming,
        observed_at_ms: at,
        authority: "o3k-lifecycle".into(),
    }
}

fn query(scope: &str, start: i64, end: i64, evaluated_at: i64) -> UsageQuery {
    UsageQuery {
        scope: scope.into(),
        meter_keys: vec![METER.into()],
        start_ms: start,
        end_ms: end,
        granularity: UsageGranularity::Hour,
        resource_id: None,
        evaluated_at_ms: evaluated_at,
    }
}

fn hourly_total(report: &MeterUsageReport) -> String {
    assert_eq!(report.meters.len(), 1);
    report.meters[0].total.clone()
}

/// Parses a three-fractional-digit report total back into `quantity ×
/// milliseconds`.
fn parse_total_ms(total: &str) -> i128 {
    let (whole, fraction) = total.split_once('.').expect("formatted total");
    let whole: i128 = whole.parse().expect("whole digits");
    let fraction: i128 = fraction.parse().expect("fraction digits");
    whole * 1000 + fraction
}

/// All series in the concurrent test are still consuming when the reader
/// evaluates, and close exactly here, so an open interval's live contribution
/// equals its eventual folded contribution.
fn concurrent_close_at() -> i64 {
    T0 + HOUR / 2
}

fn concurrent_expected_ms() -> i128 {
    (0..SERIES)
        .map(|index| i128::from(concurrent_close_at() - (T0 + index * STEP)))
        .sum()
}

/// Every total a consistent snapshot can legitimately report: the writer opens
/// series strictly in order, so a reader observes the sum of a prefix of them.
/// A value outside this set means an interval fell out of (or was counted twice
/// by) a query.
fn concurrent_prefix_states() -> std::collections::BTreeSet<i128> {
    let close_at = concurrent_close_at();
    let mut states = std::collections::BTreeSet::new();
    let mut running = 0i128;
    states.insert(running);
    for index in 0..SERIES {
        running += i128::from(close_at - (T0 + index * STEP));
        states.insert(running);
    }
    states
}

/// The conformance binary runs tests concurrently. Serialize fixture
/// provisioning/teardown; each test still uses its own disposable database.
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
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
        let database = format!("o3k_metering_{}", uuid::Uuid::now_v7().simple());
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

#[tokio::test]
async fn postgres_interval_folding_matches_kernel_semantics() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering folding: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();

    // 2 units for 30 minutes in each of two hourly buckets: no gap, no double
    // count across the hour boundary.
    let start = T0 + HOUR / 2;
    store
        .record_observation(&observation("project-a", "server-1", 2, true, start))
        .await
        .unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            0,
            false,
            start + HOUR,
        ))
        .await
        .unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + 2 * HOUR, T0 + 2 * HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].status, UsageStatus::Complete);
    let buckets: Vec<_> = report.meters[0]
        .buckets
        .iter()
        .map(|bucket| (bucket.bucket_start_ms, bucket.quantity.clone()))
        .collect();
    assert_eq!(
        buckets,
        vec![
            (T0, "3600.000".to_owned()),
            (T0 + HOUR, "3600.000".to_owned())
        ]
    );
    assert_eq!(hourly_total(&report), "7200.000");

    // A quantity change within an open interval and a clock regression both
    // fail closed.
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            1,
            true,
            T0 + 3 * HOUR,
        ))
        .await
        .unwrap();
    assert!(matches!(
        store
            .record_observation(&observation(
                "project-a",
                "server-1",
                2,
                true,
                T0 + 3 * HOUR + 10_000
            ))
            .await,
        Err(KernelError::MeteringCorrupt(_))
    ));
    assert!(matches!(
        store
            .record_observation(&observation(
                "project-a",
                "server-1",
                0,
                false,
                T0 + 3 * HOUR - 1
            ))
            .await,
        Err(KernelError::MeteringCorrupt(_))
    ));

    // Closing without an open interval is an idempotent no-op.
    store
        .record_observation(&observation("project-b", "server-2", 0, false, T0 + 60_000))
        .await
        .unwrap();

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_replay_is_idempotent() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering replay: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    let open = observation("project-a", "server-1", 1, true, T0);
    let close = observation("project-a", "server-1", 0, false, T0 + 60_000);
    for _ in 0..2 {
        store.record_observation(&open).await.unwrap();
    }
    for _ in 0..2 {
        store.record_observation(&close).await.unwrap();
    }

    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_restart_preserves_authority_and_aggregates() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering restart: O3K_DATABASE_URL unavailable");
        return;
    };
    let first = fixture.store().await;
    first.ensure_authority(T0).await.unwrap();
    first
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    first
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    let restarted = fixture.store().await;
    let report = restarted
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&report), "60.000");
    assert_eq!(report.authority_started_at_ms, Some(T0));
    assert_eq!(report.last_observed_at_ms, Some(T0 + 60_000));

    fixture.dispose(&[&first, &restarted]).await;
}

#[tokio::test]
async fn postgres_scope_isolation_hides_foreign_usage() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering scope isolation: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    for (scope, resource, quantity) in [
        ("project-a", "server-1", 1_u64),
        ("project-b", "server-2", 3_u64),
    ] {
        store
            .record_observation(&observation(scope, resource, quantity, true, T0))
            .await
            .unwrap();
        store
            .record_observation(&observation(scope, resource, 0, false, T0 + 60_000))
            .await
            .unwrap();
    }
    let a = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&a), "60.000");
    let b = store
        .usage(&query("project-b", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&b), "180.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_oversized_series_result_is_rejected() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering bounds: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    sqlx::query(
        "INSERT INTO metering_aggregates \
         (scope, meter_key, resource_id, bucket_start_ms, bucket_width_ms, quantity_millis) \
         SELECT $1, $2, 'server-' || g, $3, $4, 1000 \
         FROM generate_series(0, $5) AS g",
    )
    .bind("project-a")
    .bind(METER)
    .bind(T0)
    .bind(HOUR)
    .bind(i64::try_from(MAX_USAGE_SERIES).unwrap())
    .execute(store.pool())
    .await
    .unwrap();

    let error = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::InvalidIdentifier(_)));

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_full_range_single_series_day_query_is_served() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering full range: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    let d0 = 100 * DAY;
    store.ensure_authority(d0).await.unwrap();
    // One series consuming continuously across the whole advertised range: 366
    // days of hourly ingest rows, which must not be mistaken for 8784 series.
    store
        .record_observation(&observation("project-a", "server-1", 1, true, d0))
        .await
        .unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            0,
            false,
            d0 + 366 * DAY,
        ))
        .await
        .unwrap();

    let mut day_query = query("project-a", d0, d0 + 366 * DAY, d0 + 366 * DAY);
    day_query.granularity = UsageGranularity::Day;
    let report = store.usage(&day_query).await.unwrap();
    assert_eq!(report.meters[0].status, UsageStatus::Complete);
    assert_eq!(report.meters[0].buckets.len(), 366);
    assert_eq!(report.meters[0].buckets[0].quantity, "86400.000");
    assert_eq!(
        report.meters[0].buckets[365].bucket_start_ms,
        d0 + 365 * DAY
    );
    assert_eq!(hourly_total(&report), "31622400.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_oversized_aggregate_rows_result_is_rejected() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering aggregate-row bound: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    let d0 = 100 * DAY;
    store.ensure_authority(d0).await.unwrap();
    // 25001 rows spread over three series (8784 hourly buckets each) is below
    // the distinct-series bound but above the aggregate-row bound.
    sqlx::query(
        "INSERT INTO metering_aggregates \
         (scope, meter_key, resource_id, bucket_start_ms, bucket_width_ms, quantity_millis) \
         SELECT $1, $2, 'server-' || (g / 8784), $3 + (g % 8784) * $4, $4, 1000 \
         FROM generate_series(0, $5) AS g",
    )
    .bind("project-a")
    .bind(METER)
    .bind(d0)
    .bind(HOUR)
    .bind(25_000_i64)
    .execute(store.pool())
    .await
    .unwrap();

    let mut day_query = query("project-a", d0, d0 + 366 * DAY, d0 + 366 * DAY);
    day_query.granularity = UsageGranularity::Day;
    let error = store.usage(&day_query).await.unwrap_err();
    assert!(matches!(error, KernelError::InvalidIdentifier(_)));

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_concurrent_release_folds_exactly_once() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering concurrency: O3K_DATABASE_URL unavailable");
        return;
    };
    // Two independent stores = two independent pools, so the release truly
    // races rather than sharing a connection.
    let store_a = fixture.store().await;
    let store_b = fixture.store().await;
    store_a.ensure_authority(T0).await.unwrap();
    store_a
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();

    let release = observation("project-a", "server-1", 0, false, T0 + 60_000);
    let (a, b) = tokio::join!(
        store_a.record_observation(&release),
        store_b.record_observation(&release),
    );
    assert!(a.is_ok(), "first concurrent release failed: {a:?}");
    assert!(b.is_ok(), "second concurrent release failed: {b:?}");

    let report = store_b
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");

    // Replaying the same release concurrently is still a no-op.
    let (a, b) = tokio::join!(
        store_a.record_observation(&release),
        store_b.record_observation(&release),
    );
    assert!(a.is_ok() && b.is_ok());
    let report = store_a
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&report), "60.000");

    fixture.dispose(&[&store_a, &store_b]).await;
}

#[tokio::test]
async fn postgres_concurrent_reads_never_gap_or_double_count() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!(
            "skipping PostgreSQL metering snapshot concurrency: O3K_DATABASE_URL unavailable"
        );
        return;
    };
    // Two independent stores = two independent pools, so the reader observes
    // real concurrent commits rather than sharing a connection.
    let writer = fixture.store().await;
    let reader = fixture.store().await;
    writer.ensure_authority(T0).await.unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let expected = concurrent_expected_ms();
    let close_at = concurrent_close_at();

    let writer_loop = {
        let done = Arc::clone(&done);
        let writer = &writer;
        async move {
            // Open every series first, then close them all at one instant: each
            // open interval's live contribution equals its eventual folded
            // contribution, so a correct snapshot total is monotonic and never
            // exceeds `expected`. A lost interval would show as a decrease.
            for index in 0..SERIES {
                let start = T0 + index * STEP;
                writer
                    .record_observation(&observation(
                        "project-a",
                        &format!("server-{index}"),
                        1,
                        true,
                        start,
                    ))
                    .await
                    .unwrap();
            }
            for index in 0..SERIES {
                writer
                    .record_observation(&observation(
                        "project-a",
                        &format!("server-{index}"),
                        0,
                        false,
                        close_at,
                    ))
                    .await
                    .unwrap();
            }
            done.store(true, Ordering::SeqCst);
        }
    };

    let reader_loop = {
        let done = Arc::clone(&done);
        let reader = &reader;
        async move {
            let mut samples = Vec::new();
            loop {
                let report = reader
                    .usage(&query("project-a", T0, T0 + HOUR, close_at))
                    .await
                    .unwrap();
                samples.push(parse_total_ms(&hourly_total(&report)));
                if done.load(Ordering::SeqCst) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            samples
        }
    };

    let ((), samples) = tokio::join!(writer_loop, reader_loop);
    assert!(!samples.is_empty());
    let valid = concurrent_prefix_states();
    for sample in &samples {
        assert!(
            *sample >= 0 && *sample <= expected,
            "snapshot total out of range: {sample} (expected max {expected})"
        );
        assert!(
            valid.contains(sample),
            "snapshot total {sample} is not a consistent prefix state: {samples:?}"
        );
    }
    assert!(
        samples.windows(2).all(|window| window[1] >= window[0]),
        "usage total must never decrease across snapshots: {samples:?}"
    );

    let final_report = reader
        .usage(&query("project-a", T0, T0 + HOUR, close_at))
        .await
        .unwrap();
    assert_eq!(parse_total_ms(&hourly_total(&final_report)), expected);

    fixture.dispose(&[&writer, &reader]).await;
}

/// Mechanism test: it replays the exact BEGIN/read-pair/COMMIT the store's
/// `usage()` relies on, but it does NOT call `usage()` itself, so it cannot
/// catch `usage()` losing its transaction. The real-API coverage for that is
/// `postgres_concurrent_reads_never_gap_or_double_count`.
#[tokio::test]
async fn postgres_repeatable_read_snapshot_mechanism_prevents_usage_gap() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering snapshot: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();

    // A second connection replays the exact read pair `usage()` performs, but
    // paused between them so the writer commits a close in the gap.
    let mut reader = store.pool().acquire().await.unwrap();
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *reader)
        .await
        .unwrap();
    let open_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metering_intervals WHERE scope = $1 AND ended_at_ms IS NULL",
    )
    .bind("project-a")
    .fetch_one(&mut *reader)
    .await
    .unwrap();
    assert_eq!(open_before, 1);

    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    // The repeatable-read snapshot still sees the interval as open and cannot
    // see the fold, so the interval is never missing from both reads.
    let open_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metering_intervals WHERE scope = $1 AND ended_at_ms IS NULL",
    )
    .bind("project-a")
    .fetch_one(&mut *reader)
    .await
    .unwrap();
    assert_eq!(
        open_after, 1,
        "repeatable-read snapshot must not drop an open interval mid-query"
    );
    let aggregates_visible: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metering_aggregates WHERE scope = $1")
            .bind("project-a")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
    assert_eq!(
        aggregates_visible, 0,
        "a fold committed after the snapshot must be invisible to it"
    );
    sqlx::query("ROLLBACK").execute(&mut *reader).await.unwrap();
    drop(reader);

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_replay_open_after_close_is_idempotent() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering replay-after-close: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    let open = observation("project-a", "server-1", 1, true, T0);
    let close = observation("project-a", "server-1", 0, false, T0 + 60_000);
    store.record_observation(&open).await.unwrap();
    store.record_observation(&close).await.unwrap();

    // The deterministic primary key already exists (the interval is closed), so
    // this must converge instead of raising SQLSTATE 23505.
    store.record_observation(&open).await.unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");
    let intervals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metering_intervals")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(intervals, 1, "replay must not open a second interval");

    store.record_observation(&close).await.unwrap();
    let again = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&again), "60.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_overlapping_out_of_order_open_is_rejected() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering overlap: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            1,
            true,
            T0 + 2 * HOUR,
        ))
        .await
        .unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            0,
            false,
            T0 + 3 * HOUR,
        ))
        .await
        .unwrap();

    let error = store
        .record_observation(&observation("project-a", "server-1", 1, true, T0 + HOUR))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::MeteringCorrupt(_)));

    let report = store
        .usage(&query("project-a", T0, T0 + 4 * HOUR, T0 + 4 * HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "3600.000");

    // Starting exactly at the prior end is adjacent, not overlapping.
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            1,
            true,
            T0 + 3 * HOUR,
        ))
        .await
        .unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            0,
            false,
            T0 + 4 * HOUR,
        ))
        .await
        .unwrap();
    let adjacent = store
        .usage(&query("project-a", T0, T0 + 4 * HOUR, T0 + 4 * HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&adjacent), "7200.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_zero_duration_close_accrues_nothing() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering zero-duration: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0))
        .await
        .unwrap();

    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert!(report.meters[0].buckets.is_empty());
    assert_eq!(hourly_total(&report), "0.000");
    let open: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metering_intervals WHERE ended_at_ms IS NULL")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(open, 0, "zero-duration close must leave no open interval");

    let mid = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR / 2))
        .await
        .unwrap();
    assert_eq!(mid.meters[0].total, "0.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_colon_separated_series_ids_do_not_collide() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering id injectivity: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("p:q", "r", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("p", "q:r", 1, true, T0))
        .await
        .unwrap();

    let intervals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metering_intervals")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        intervals, 2,
        "distinct series must get distinct interval ids"
    );

    for (scope, resource) in [("p:q", "r"), ("p", "q:r")] {
        store
            .record_observation(&observation(scope, resource, 0, false, T0 + 60_000))
            .await
            .unwrap();
        let report = store
            .usage(&query(scope, T0, T0 + HOUR, T0 + HOUR))
            .await
            .unwrap();
        assert_eq!(hourly_total(&report), "60.000");
    }

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_oversized_open_intervals_result_is_rejected() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering open-interval bound: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    sqlx::query(
        "INSERT INTO metering_intervals \
         (interval_id, meter_key, scope, resource_id, quantity, started_at_ms, ended_at_ms, authority) \
         SELECT 'open-' || g, $1, 'project-a', 'server-' || g, 1, $2, NULL, 'o3k-lifecycle' \
         FROM generate_series(0, $3) AS g",
    )
    .bind(METER)
    .bind(T0)
    .bind(i64::try_from(MAX_OPEN_INTERVALS).unwrap())
    .execute(store.pool())
    .await
    .unwrap();

    let error = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::InvalidIdentifier(_)));

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_aggregate_overflow_fails_closed() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering overflow: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    let huge = i64::MAX as u64;
    store
        .record_observation(&observation("project-a", "server-1", huge, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 1))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", huge, true, T0 + 1))
        .await
        .unwrap();
    let error = store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 2))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::MeteringCorrupt(_)));

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_ensure_authority_never_backdates_and_watermark_is_monotonic() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering authority anchor: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0 + HOUR).await.unwrap();
    store.ensure_authority(T0).await.unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.authority_started_at_ms, Some(T0 + HOUR));

    store
        .record_observation(&observation(
            "project-a",
            "server-1",
            1,
            true,
            T0 + 2 * HOUR,
        ))
        .await
        .unwrap();
    store
        .record_observation(&observation(
            "project-a",
            "server-2",
            1,
            true,
            T0 + HOUR + 1,
        ))
        .await
        .unwrap();
    let predating = store
        .record_observation(&observation(
            "project-a",
            "server-3",
            1,
            true,
            T0 + HOUR / 2,
        ))
        .await;
    assert!(matches!(predating, Err(KernelError::MeteringCorrupt(_))));
    let later = store
        .usage(&query(
            "project-a",
            T0 + 2 * HOUR,
            T0 + 3 * HOUR,
            T0 + 3 * HOUR,
        ))
        .await
        .unwrap();
    assert_eq!(later.authority_started_at_ms, Some(T0 + HOUR));
    assert_eq!(later.last_observed_at_ms, Some(T0 + 2 * HOUR));

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_day_granularity_splits_a_multi_day_interval() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering day granularity: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    let d0 = 100 * DAY;
    store.ensure_authority(d0).await.unwrap();
    let start = d0 + DAY / 2;
    let end = d0 + 2 * DAY + DAY / 2;
    store
        .record_observation(&observation("project-a", "server-1", 1, true, start))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, end))
        .await
        .unwrap();

    let mut day_query = query("project-a", d0, d0 + 3 * DAY, d0 + 3 * DAY);
    day_query.granularity = UsageGranularity::Day;
    let report = store.usage(&day_query).await.unwrap();
    let buckets: Vec<_> = report.meters[0]
        .buckets
        .iter()
        .map(|bucket| (bucket.bucket_start_ms, bucket.quantity.clone()))
        .collect();
    assert_eq!(
        buckets,
        vec![
            (d0, "43200.000".to_owned()),
            (d0 + DAY, "86400.000".to_owned()),
            (d0 + 2 * DAY, "43200.000".to_owned()),
        ]
    );
    assert_eq!(hourly_total(&report), "172800.000");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_pre_metering_schema_upgrades_with_usable_metering_tables() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering migration: O3K_DATABASE_URL unavailable");
        return;
    };

    // Apply every migration before the metering one, i.e. a database created
    // before 0028_metering.sql.
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
                .filter(|migration| migration.version < 28)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    legacy.run(&pool).await.unwrap();
    let present: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('metering_intervals')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        present.is_none(),
        "metering_intervals must not exist before the metering migration"
    );
    pool.close().await;

    // A normal connect runs the remaining migration and the tables are usable.
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&report), "60.000");

    let index_present: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('idx_metering_intervals_open_series')::text")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(
        index_present.is_some(),
        "the open-interval read index must exist after the upgrade"
    );

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_replay_close_at_any_instant_is_idempotent() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering close replay: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    // Same instant, later instant and earlier instant: only one close folds,
    // and a different-instant replay of an already-closed interval is a no-op.
    for at in [T0 + 60_000, T0 + 90_000, T0 + 30_000] {
        store
            .record_observation(&observation("project-a", "server-1", 0, false, at))
            .await
            .unwrap();
    }

    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");

    let aggregates: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metering_aggregates")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(aggregates, 1, "exactly one folded effect");
    let intervals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metering_intervals")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(intervals, 1, "replays must not open extra intervals");

    fixture.dispose(&[&store]).await;
}

#[tokio::test]
async fn postgres_replayed_open_with_different_quantity_or_authority_fails_closed() {
    let _guard = test_lock().await;
    let Some(fixture) = Fixture::new().await else {
        eprintln!("skipping PostgreSQL metering identity conflict: O3K_DATABASE_URL unavailable");
        return;
    };
    let store = fixture.store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    // Same started instant but a different quantity must fail closed instead
    // of being silently swallowed as a replay.
    let mut replay = observation("project-a", "server-1", 2, true, T0);
    assert!(matches!(
        store.record_observation(&replay).await,
        Err(KernelError::MeteringCorrupt(_))
    ));

    // Same quantity but a different authority label is likewise corruption.
    replay.quantity = 1;
    replay.authority = "o3k-other".into();
    assert!(matches!(
        store.record_observation(&replay).await,
        Err(KernelError::MeteringCorrupt(_))
    ));

    // While an interval is still open, a same-quantity refresh under a
    // different authority label is rejected too.
    store
        .record_observation(&observation("project-a", "server-2", 1, true, T0))
        .await
        .unwrap();
    let mut foreign_authority = observation("project-a", "server-2", 1, true, T0 + 10_000);
    foreign_authority.authority = "o3k-other".into();
    assert!(matches!(
        store.record_observation(&foreign_authority).await,
        Err(KernelError::MeteringCorrupt(_))
    ));

    // A matching replay still converges and does not change the total.
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    let mut server_one = query("project-a", T0, T0 + HOUR, T0 + HOUR);
    server_one.resource_id = Some("server-1".into());
    let report = store.usage(&server_one).await.unwrap();
    assert_eq!(hourly_total(&report), "60.000");

    fixture.dispose(&[&store]).await;
}
