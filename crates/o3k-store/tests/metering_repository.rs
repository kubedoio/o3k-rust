#![allow(clippy::unwrap_used, clippy::expect_used)]

use o3k_kernel::metering::MAX_OPEN_INTERVALS;
use o3k_kernel::{
    INGEST_BUCKET_WIDTH_MS, KernelError, MAX_USAGE_SERIES, MeterObservation, MeterUnit,
    MeteringRepository, UsageGranularity, UsageQuery, UsageStatus,
};
use o3k_store::{O3kStore, SqliteStore};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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

async fn memory_store() -> O3kStore {
    O3kStore::connect_sqlite_memory().await.unwrap()
}

/// Total for the single-meter hourly report, with sanity checks on unit/status.
fn hourly_total(report: &o3k_kernel::MeterUsageReport) -> String {
    assert_eq!(report.meters.len(), 1);
    assert_eq!(report.meters[0].unit, MeterUnit::InstanceSecond);
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

#[tokio::test]
async fn open_close_interval_arithmetic_reports_exact_duration() {
    let store = memory_store().await;
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
    assert_eq!(report.meters[0].status, UsageStatus::Complete);
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(report.meters[0].buckets[0].bucket_start_ms, T0);
    assert_eq!(report.meters[0].buckets[0].quantity, "60.000");
    assert_eq!(hourly_total(&report), "60.000");
}

#[tokio::test]
async fn stopped_intervals_do_not_accrue() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    for (start, end) in [(T0, T0 + 60_000), (T0 + 2 * HOUR, T0 + 2 * HOUR + 30_000)] {
        store
            .record_observation(&observation("project-a", "server-1", 1, true, start))
            .await
            .unwrap();
        store
            .record_observation(&observation("project-a", "server-1", 0, false, end))
            .await
            .unwrap();
    }

    let report = store
        .usage(&query("project-a", T0, T0 + 4 * HOUR, T0 + 4 * HOUR))
        .await
        .unwrap();
    let buckets: Vec<_> = report.meters[0]
        .buckets
        .iter()
        .map(|bucket| (bucket.bucket_start_ms, bucket.quantity.clone()))
        .collect();
    assert_eq!(
        buckets,
        vec![
            (T0, "60.000".to_owned()),
            (T0 + 2 * HOUR, "30.000".to_owned())
        ]
    );
    assert_eq!(hourly_total(&report), "90.000");
}

#[tokio::test]
async fn idempotent_replay_does_not_double_count() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    let open = observation("project-a", "server-1", 1, true, T0);
    let close = observation("project-a", "server-1", 0, false, T0 + 60_000);
    store.record_observation(&open).await.unwrap();
    store.record_observation(&open).await.unwrap();
    store.record_observation(&close).await.unwrap();
    store.record_observation(&close).await.unwrap();

    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");
}

#[tokio::test]
async fn duplicate_consume_while_open_does_not_open_a_second_interval() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0 + 10_000))
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
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");
}

#[tokio::test]
async fn close_without_open_is_a_noop() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert!(report.meters[0].buckets.is_empty());
    assert_eq!(hourly_total(&report), "0.000");
}

#[tokio::test]
async fn clock_regression_on_release_fails_closed() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0 + 60_000))
        .await
        .unwrap();
    let error = store
        .record_observation(&observation("project-a", "server-1", 0, false, T0))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::MeteringCorrupt(_)));
}

#[tokio::test]
async fn quantity_change_within_open_interval_fails_closed() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    let error = store
        .record_observation(&observation("project-a", "server-1", 2, true, T0 + 10_000))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::MeteringCorrupt(_)));
}

#[tokio::test]
async fn interval_crossing_hour_boundary_splits_without_gap() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    let start = T0 + HOUR / 2;
    let end = start + HOUR;
    store
        .record_observation(&observation("project-a", "server-1", 2, true, start))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, end))
        .await
        .unwrap();

    let report = store
        .usage(&query("project-a", T0, T0 + 2 * HOUR, T0 + 2 * HOUR))
        .await
        .unwrap();
    let buckets: Vec<_> = report.meters[0]
        .buckets
        .iter()
        .map(|bucket| (bucket.bucket_start_ms, bucket.quantity.clone()))
        .collect();
    // 2 units for 30 minutes in each of two hourly buckets.
    assert_eq!(
        buckets,
        vec![
            (T0, "3600.000".to_owned()),
            (T0 + HOUR, "3600.000".to_owned())
        ]
    );
    assert_eq!(hourly_total(&report), "7200.000");
}

#[tokio::test]
async fn long_interval_crosses_many_buckets_and_excludes_the_end_instant() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    let start = T0 + 15 * 60_000;
    let end = T0 + 3 * HOUR + 15 * 60_000;
    store
        .record_observation(&observation("project-a", "server-1", 1, true, start))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, end))
        .await
        .unwrap();

    let report = store
        .usage(&query("project-a", T0, T0 + 4 * HOUR, T0 + 4 * HOUR))
        .await
        .unwrap();
    let buckets: Vec<_> = report.meters[0]
        .buckets
        .iter()
        .map(|bucket| (bucket.bucket_start_ms, bucket.quantity.clone()))
        .collect();
    assert_eq!(
        buckets,
        vec![
            (T0, "2700.000".to_owned()),
            (T0 + HOUR, "3600.000".to_owned()),
            (T0 + 2 * HOUR, "3600.000".to_owned()),
            (T0 + 3 * HOUR, "900.000".to_owned()),
        ]
    );
    assert_eq!(hourly_total(&report), "10800.000");

    // An interval ending exactly on the hour excludes the end instant.
    let boundary = memory_store().await;
    boundary.ensure_authority(T0).await.unwrap();
    boundary
        .record_observation(&observation("project-b", "server-2", 1, true, T0))
        .await
        .unwrap();
    boundary
        .record_observation(&observation("project-b", "server-2", 0, false, T0 + HOUR))
        .await
        .unwrap();
    let report = boundary
        .usage(&query("project-b", T0, T0 + 2 * HOUR, T0 + 2 * HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(report.meters[0].buckets[0].quantity, "3600.000");
}

#[tokio::test]
async fn open_interval_live_contribution_advances_without_folding() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 2, true, T0))
        .await
        .unwrap();

    let first = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + 30_000))
        .await
        .unwrap();
    assert_eq!(first.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&first), "60.000");
    assert_eq!(first.observed_through_ms, T0 + 30_000);

    let second = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + 50_000))
        .await
        .unwrap();
    assert_eq!(hourly_total(&second), "100.000");

    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    let folded = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&folded), "120.000");
}

#[tokio::test]
async fn usage_status_reflects_metering_authority() {
    let store = memory_store().await;
    let absent = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(absent.meters[0].status, UsageStatus::Unavailable);
    assert!(absent.meters[0].buckets.is_empty());
    assert_eq!(absent.meters[0].total, "0.000");
    assert_eq!(absent.authority_started_at_ms, None);

    store.ensure_authority(T0 + 2 * HOUR).await.unwrap();
    let before = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(before.meters[0].status, UsageStatus::Unavailable);
    assert!(before.meters[0].buckets.is_empty());

    let straddling = store
        .usage(&query("project-a", T0, T0 + 3 * HOUR, T0 + 3 * HOUR))
        .await
        .unwrap();
    assert_eq!(straddling.meters[0].status, UsageStatus::Partial);
    assert_eq!(straddling.authority_started_at_ms, Some(T0 + 2 * HOUR));

    let after = store
        .usage(&query(
            "project-a",
            T0 + 2 * HOUR,
            T0 + 3 * HOUR,
            T0 + 3 * HOUR,
        ))
        .await
        .unwrap();
    assert_eq!(after.meters[0].status, UsageStatus::Complete);
}

#[tokio::test]
async fn future_end_is_partial_and_buckets_stop_at_evaluated_at() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    // Fully closed interval contributes 60s to the first bucket.
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    // Still-open interval from half past the hour.
    let open_start = T0 + 30 * 60_000;
    store
        .record_observation(&observation("project-a", "server-1", 1, true, open_start))
        .await
        .unwrap();

    // `end` is one hour but evaluation happens at 50 minutes: the period is not
    // fully covered, so it must not be reported as complete.
    let evaluated = T0 + 50 * 60_000;
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, evaluated))
        .await
        .unwrap();
    assert_eq!(report.meters[0].status, UsageStatus::Partial);
    assert_eq!(report.observed_through_ms, evaluated);
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(report.meters[0].buckets[0].bucket_start_ms, T0);
    // 60s closed + 20 minutes live, clamped to evaluated_at.
    assert_eq!(hourly_total(&report), "1260.000");

    // Once the period has elapsed the same window is complete.
    let elapsed = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(elapsed.meters[0].status, UsageStatus::Complete);
    assert_eq!(elapsed.observed_through_ms, T0 + HOUR);
    assert_eq!(hourly_total(&elapsed), "1860.000");
}

/// Mechanism test: it replays the exact BEGIN/read-pair/COMMIT the store's
/// `usage()` relies on, but it does NOT call `usage()` itself, so it cannot
/// catch `usage()` losing its transaction. The real-API coverage for that is
/// `concurrent_usage_reads_never_gap_or_double_count`.
#[tokio::test]
async fn deferred_read_snapshot_mechanism_prevents_usage_gap() {
    let path = std::env::temp_dir().join(format!(
        "o3k-metering-snapshot-{}.db",
        uuid::Uuid::now_v7().simple()
    ));
    let store = SqliteStore::connect_file(&path).await.unwrap();
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();

    // A second WAL connection replays the exact read pair `usage()` performs,
    // but paused between them so the writer commits a close in the gap.
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(&path);
    let reader_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut reader = reader_pool.acquire().await.unwrap();
    sqlx::query("BEGIN DEFERRED")
        .execute(&mut *reader)
        .await
        .unwrap();
    let open_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metering_intervals WHERE scope = ?1 AND ended_at_ms IS NULL",
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

    // The deferred read transaction keeps its snapshot: the interval is still
    // open and the just-committed fold is still invisible, so the interval can
    // never fall out of both reads.
    let open_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metering_intervals WHERE scope = ?1 AND ended_at_ms IS NULL",
    )
    .bind("project-a")
    .fetch_one(&mut *reader)
    .await
    .unwrap();
    assert_eq!(
        open_after, 1,
        "deferred snapshot must not drop an open interval mid-query"
    );
    let aggregates_visible: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metering_aggregates WHERE scope = ?1")
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

    drop(store);
    drop(reader_pool);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn concurrent_usage_reads_never_gap_or_double_count() {
    let path = std::env::temp_dir().join(format!(
        "o3k-metering-concurrent-{}.db",
        uuid::Uuid::now_v7().simple()
    ));
    let writer = SqliteStore::connect_file(&path).await.unwrap();
    let reader = SqliteStore::connect_file(&path).await.unwrap();
    writer.ensure_authority(T0).await.unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let expected = concurrent_expected_ms();
    let close_at = concurrent_close_at();

    let writer_loop = {
        let done = Arc::clone(&done);
        let writer = &writer;
        async move {
            // Open every series first, then close them all: each open interval's
            // live contribution equals its eventual folded contribution, so a
            // correct snapshot total is monotonic and never exceeds `expected`.
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

    drop(writer);
    drop(reader);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn scope_isolation_hides_foreign_usage() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    for (scope, resource) in [("project-a", "server-1"), ("project-b", "server-2")] {
        store
            .record_observation(&observation(scope, resource, 1, true, T0))
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
    assert_eq!(hourly_total(&b), "60.000");
}

#[tokio::test]
async fn unrelated_resource_filter_returns_nothing() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();

    let mut filtered = query("project-a", T0, T0 + HOUR, T0 + HOUR);
    filtered.resource_id = Some("ghost".into());
    let report = store.usage(&filtered).await.unwrap();
    assert!(report.meters[0].buckets.is_empty());
    assert_eq!(report.meters[0].total, "0.000");

    let mut allowed = query("project-a", T0, T0 + HOUR, T0 + HOUR);
    allowed.resource_id = Some("server-1".into());
    let report = store.usage(&allowed).await.unwrap();
    assert_eq!(hourly_total(&report), "60.000");
}

#[tokio::test]
async fn oversized_series_result_is_rejected_not_truncated() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    for index in 0..=MAX_USAGE_SERIES {
        let resource = format!("server-{index}");
        store
            .record_observation(&observation("project-a", &resource, 1, true, T0))
            .await
            .unwrap();
        store
            .record_observation(&observation("project-a", &resource, 0, false, T0 + 1_000))
            .await
            .unwrap();
    }

    let error = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::InvalidIdentifier(_)));
}

#[tokio::test]
async fn restart_durability_matches_single_process() {
    let path =
        std::env::temp_dir().join(format!("o3k-metering-{}.db", uuid::Uuid::now_v7().simple()));

    let store = SqliteStore::connect_file(&path).await.unwrap();
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteStore::connect_file(&path).await.unwrap();
    reopened
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    let report = reopened
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&report), "60.000");

    let single = memory_store().await;
    single.ensure_authority(T0).await.unwrap();
    single
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    single
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 60_000))
        .await
        .unwrap();
    let expected = single
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report, expected);

    drop(reopened);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn replay_open_after_close_is_idempotent() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    let open = observation("project-a", "server-1", 1, true, T0);
    let close = observation("project-a", "server-1", 0, false, T0 + 60_000);
    store.record_observation(&open).await.unwrap();
    store.record_observation(&close).await.unwrap();

    // Replaying the already-closed open must converge (the deterministic
    // primary key already exists) instead of opening a second interval.
    store.record_observation(&open).await.unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.meters[0].buckets.len(), 1);
    assert_eq!(hourly_total(&report), "60.000");

    // The replay did not reopen the series: a repeated close stays at 60.000.
    store.record_observation(&close).await.unwrap();
    let again = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&again), "60.000");
}

#[tokio::test]
async fn overlapping_out_of_order_open_is_rejected() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    // Closed and folded interval [T0+2h, T0+3h).
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

    // An out-of-order open before the folded end would overlap and double count.
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
}

#[tokio::test]
async fn zero_duration_close_accrues_nothing() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    store
        .record_observation(&observation("project-a", "server-1", 1, true, T0))
        .await
        .unwrap();
    // Closing at the open instant must close the interval with zero usage,
    // not fail and leave it open.
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

    // The interval is closed: a query evaluated mid-period must not accrue.
    let mid = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR / 2))
        .await
        .unwrap();
    assert_eq!(mid.meters[0].total, "0.000");

    // Closing again has no open interval to close and still accrues nothing.
    store
        .record_observation(&observation("project-a", "server-1", 0, false, T0 + 30_000))
        .await
        .unwrap();
    let after = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&after), "0.000");
}

#[tokio::test]
async fn colon_separated_series_ids_do_not_collide() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    // Naive ':' joining would map these two series to the same interval id.
    let first = observation("p:q", "r", 1, true, T0);
    let second = observation("p", "q:r", 1, true, T0);
    store.record_observation(&first).await.unwrap();
    store.record_observation(&second).await.unwrap();

    for (scope, resource) in [("p:q", "r"), ("p", "q:r")] {
        store
            .record_observation(&observation(scope, resource, 0, false, T0 + 60_000))
            .await
            .unwrap();
        let report = store
            .usage(&query(scope, T0, T0 + HOUR, T0 + HOUR))
            .await
            .unwrap();
        assert_eq!(
            hourly_total(&report),
            "60.000",
            "series {scope}/{resource} lost its interval"
        );
    }
}

#[tokio::test]
async fn oversized_open_intervals_result_is_rejected() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    for index in 0..=MAX_OPEN_INTERVALS {
        store
            .record_observation(&observation(
                "project-a",
                &format!("server-{index}"),
                1,
                true,
                T0,
            ))
            .await
            .unwrap();
    }
    let error = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::InvalidIdentifier(_)));
}

#[tokio::test]
async fn aggregate_overflow_fails_closed() {
    let store = memory_store().await;
    store.ensure_authority(T0).await.unwrap();
    let huge = i64::MAX as u64;
    // Each 1ms interval contributes i64::MAX to the same hourly bucket, so the
    // second fold overflows the signed aggregate and must fail closed.
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
}

#[tokio::test]
async fn ensure_authority_never_backdates_and_watermark_is_monotonic() {
    let store = memory_store().await;
    store.ensure_authority(T0 + HOUR).await.unwrap();
    // A later anchor attempt must not backdate or rewrite the existing anchor.
    store.ensure_authority(T0).await.unwrap();
    let report = store
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(report.authority_started_at_ms, Some(T0 + HOUR));

    // A higher observation advances the watermark; a lower one on a different
    // series must not roll it back.
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
    // An observation older than the anchor predates O3K metering authority and
    // must be refused rather than folded into a period O3K never owned.
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
}

#[tokio::test]
async fn day_granularity_splits_a_multi_day_interval() {
    let store = memory_store().await;
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
}

#[tokio::test]
async fn two_writers_fold_a_release_exactly_once() {
    let path = std::env::temp_dir().join(format!(
        "o3k-metering-two-writers-{}.db",
        uuid::Uuid::now_v7().simple()
    ));
    let store_a = SqliteStore::connect_file(&path).await.unwrap();
    let store_b = SqliteStore::connect_file(&path).await.unwrap();
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
    assert!(a.is_ok(), "first concurrent writer failed: {a:?}");
    assert!(b.is_ok(), "second concurrent writer failed: {b:?}");

    let report = store_b
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&report), "60.000");

    // A concurrent replay is still a no-op.
    let (a, b) = tokio::join!(
        store_a.record_observation(&release),
        store_b.record_observation(&release),
    );
    assert!(a.is_ok() && b.is_ok());
    let again = store_a
        .usage(&query("project-a", T0, T0 + HOUR, T0 + HOUR))
        .await
        .unwrap();
    assert_eq!(hourly_total(&again), "60.000");

    drop(store_a);
    drop(store_b);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}
