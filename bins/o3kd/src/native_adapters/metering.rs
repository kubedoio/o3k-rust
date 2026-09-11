//! Production `o3kd` projection of canonical O3K metering (#904).
//!
//! This adapter is the single production implementation of the two metering
//! ports:
//!
//! - [`o3k_native_api::metering::MeteringReader`], the bounded read-only
//!   projection served under `/o3k/v1/metering`;
//! - [`o3k_kernel::LifecycleMeteringObserver`], the write-side projection that
//!   turns canonical O3K lifecycle authority into durable meter observations.
//!
//! It never invents historical usage, never derives a reading from provider
//! telemetry, and never returns a usage number without the completeness status
//! the kernel already carries. Definitions and usage are projected straight
//! from the canonical catalog and the durable store.

use std::sync::Arc;

use async_trait::async_trait;
use o3k_kernel::{
    Clock, KernelError, LifecycleMeteringObserver, METER_CATALOG, MeterObservation,
    MeteringRepository,
};
use o3k_native_api::metering::{
    MeterDefinitionView, MeterDefinitionsPage, MeteringError, MeteringReader, encode_cursor,
};
use o3k_store::unified::O3kStore;

/// Canonical resource kind whose lifecycle states drive the compute runtime
/// meter.
const COMPUTE_INSTANCE_KIND: &str = "compute_instance";
/// Meter fed by compute-instance lifecycle state.
pub const COMPUTE_INSTANCE_METER: &str = "compute:instance_seconds";
/// Stable authority label recorded on every observation. It is a fixed O3K
/// vocabulary value, never a free-form provider string.
const LIFECYCLE_AUTHORITY: &str = "o3k-lifecycle";

/// The canonical volume allocation meter key is owned by the canonical volume
/// authority in `o3k-api`; the adapter re-exports it so every caller shares one
/// definition instead of repeating the literal.
pub use o3k_api::VOLUME_ALLOCATION_METER;

/// The catalog meters the active composition can actually produce.
///
/// The compute-instance meter requires the compute authority; the `o3kd`
/// composition always configures it, but the condition is explicit so a future
/// composition without compute does not advertise it. The volume allocation
/// meter requires a configured native storage provider: without one no volume
/// can be durably created or deleted, so the meter must not be advertised or
/// queryable.
#[must_use]
pub fn producible_meters(
    compute_configured: bool,
    native_storage_configured: bool,
) -> Vec<&'static str> {
    let mut meters = Vec::new();
    if compute_configured {
        meters.push(COMPUTE_INSTANCE_METER);
    }
    if native_storage_configured {
        meters.push(VOLUME_ALLOCATION_METER);
    }
    meters
}

/// Decodes the storage-encoded compute-instance state into whether the
/// instance consumes instance-seconds.
///
/// The single canonical state→consuming mapping now lives in
/// [`o3k_reconciler::compute_instance_state_consuming`]; this adapter re-exports
/// it so `o3kd` and the compute/reconciler repair paths share one definition.
/// `Some(true)` means the instance still holds its runtime, `Some(false)` means
/// it does not, and `None` is corrupt authority that must never be treated as
/// idle (an unknown state could silently stop accruing owned usage).
pub use o3k_reconciler::compute_instance_state_consuming;

/// Maps a canonical durable metering failure into the bounded native error
/// vocabulary. Bounds violations are client errors; everything else is either
/// an unavailable or a corrupt authority and is never silently reported as
/// zero usage.
fn map_kernel_error(error: KernelError) -> MeteringError {
    match error {
        KernelError::InvalidIdentifier(_)
        | KernelError::InvalidScopeId(_)
        | KernelError::InvalidResourceId(_) => MeteringError::BoundsExceeded,
        KernelError::MeteringUnavailable(_) => MeteringError::Unavailable,
        KernelError::MeteringCorrupt(_) => MeteringError::Corrupt,
        _ => MeteringError::Corrupt,
    }
}

/// Production adapter over canonical O3K metering authority.
pub struct MeteringAdapter {
    store: Arc<O3kStore>,
    clock: Arc<dyn Clock>,
    /// Catalog meters this composition can actually produce. A meter absent
    /// from this set is never advertised and is rejected for usage.
    producible: Vec<&'static str>,
}

impl MeteringAdapter {
    /// Creates a new adapter over the canonical store and clock.
    ///
    /// Every catalog meter is producible until the composition root narrows the
    /// set with [`MeteringAdapter::with_producible_meters`].
    #[must_use]
    pub fn new(store: Arc<O3kStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            producible: METER_CATALOG
                .iter()
                .map(|definition| definition.key)
                .collect(),
        }
    }

    /// Restricts the advertised and queryable meters to the set the active
    /// composition can actually produce.
    ///
    /// A meter present in [`METER_CATALOG`] but absent here must not appear in
    /// `definitions`, and a usage request for it is an unknown meter rather
    /// than a zero reading.
    #[must_use]
    pub fn with_producible_meters(mut self, meters: Vec<&'static str>) -> Self {
        self.producible = meters;
        self
    }

    /// Whether this composition can produce the meter.
    #[must_use]
    pub fn produces(&self, meter_key: &str) -> bool {
        self.producible.contains(&meter_key)
    }

    /// Records one observation through the canonical durable authority.
    async fn record(
        &self,
        meter_key: &str,
        scope: &str,
        resource_id: &str,
        quantity: u64,
        consuming: bool,
    ) -> Result<(), KernelError> {
        self.store
            .record_observation(&MeterObservation {
                meter_key: meter_key.to_owned(),
                scope: scope.to_owned(),
                resource_id: resource_id.to_owned(),
                quantity,
                consuming,
                observed_at_ms: self.clock.now_unix_ms(),
                authority: LIFECYCLE_AUTHORITY.to_owned(),
            })
            .await
    }
}

#[async_trait]
impl MeteringReader for MeteringAdapter {
    async fn definitions(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<MeterDefinitionsPage, MeteringError> {
        let mut definitions: Vec<MeterDefinitionView> = METER_CATALOG
            .iter()
            .filter(|definition| self.produces(definition.key))
            .filter(|definition| after.is_none_or(|after| definition.key > after))
            .take(limit + 1)
            .map(MeterDefinitionView::from_definition)
            .collect();
        let has_more = definitions.len() > limit;
        if has_more {
            definitions.truncate(limit);
        }
        let next_cursor = if has_more {
            definitions
                .last()
                .map(|definition| encode_cursor(&definition.key))
        } else {
            None
        };
        Ok(MeterDefinitionsPage {
            definitions,
            has_more,
            next_cursor,
        })
    }

    async fn usage(
        &self,
        query: &o3k_kernel::UsageQuery,
    ) -> Result<o3k_kernel::MeterUsageReport, MeteringError> {
        // A meter this composition cannot produce is an unknown meter, never a
        // zero reading: reporting zero would claim authoritative coverage of a
        // series O3K never observed.
        if query
            .meter_keys
            .iter()
            .any(|key| !self.produces(key.as_str()))
        {
            return Err(MeteringError::BoundsExceeded);
        }
        // The HTTP handler stamps `evaluated_at_ms` from the API process wall
        // clock, but the durable metering authority's clock is the authority
        // for coverage: an open interval's live segment is evaluated up to
        // this instant. Re-stamp from the adapter's injectable clock so
        // observation and evaluation share one time source. In production both
        // are `SystemClock`; tests inject a deterministic clock.
        let mut query = query.clone();
        query.evaluated_at_ms = self.clock.now_unix_ms();
        self.store.usage(&query).await.map_err(map_kernel_error)
    }
}

#[async_trait]
impl LifecycleMeteringObserver for MeteringAdapter {
    async fn observe_resource_state(
        &self,
        kind: &str,
        project_id: &str,
        resource_id: &str,
        observed_state: &str,
    ) -> Result<(), KernelError> {
        if kind != COMPUTE_INSTANCE_KIND {
            return Ok(());
        }
        let consuming = compute_instance_state_consuming(observed_state).ok_or_else(|| {
            KernelError::MeteringCorrupt(format!(
                "unknown compute instance state `{observed_state}`"
            ))
        })?;
        self.record(
            COMPUTE_INSTANCE_METER,
            project_id,
            resource_id,
            if consuming { 1 } else { 0 },
            consuming,
        )
        .await
    }

    async fn observe_allocation(
        &self,
        meter_key: &str,
        project_id: &str,
        resource_id: &str,
        quantity: u64,
        consuming: bool,
    ) -> Result<(), KernelError> {
        self.record(
            meter_key,
            project_id,
            resource_id,
            if consuming { quantity } else { 0 },
            consuming,
        )
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use o3k_kernel::{UsageGranularity, UsageQuery, UsageStatus};
    use std::sync::atomic::{AtomicI64, Ordering};

    const HOUR: i64 = 3_600_000;
    /// A UTC hour boundary: 2023-11-14T22:00:00Z.
    const BASE: i64 = 1_699_999_200_000;

    #[derive(Clone)]
    struct TestClock(Arc<AtomicI64>);

    impl TestClock {
        fn new(now: i64) -> Self {
            Self(Arc::new(AtomicI64::new(now)))
        }

        fn set(&self, now: i64) {
            self.0.store(now, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_unix_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    async fn build_adapter(now: i64) -> (Arc<MeteringAdapter>, Arc<TestClock>, Arc<O3kStore>) {
        let store = Arc::new(
            O3kStore::connect_sqlite_memory()
                .await
                .expect("in-memory store"),
        );
        let clock = Arc::new(TestClock::new(now));
        let adapter = Arc::new(MeteringAdapter::new(store.clone(), clock.clone()));
        (adapter, clock, store)
    }

    fn query(scope: &str, meter_key: &str, start_ms: i64, end_ms: i64) -> UsageQuery {
        UsageQuery {
            scope: scope.to_owned(),
            meter_keys: vec![meter_key.to_owned()],
            start_ms,
            end_ms,
            granularity: UsageGranularity::Hour,
            resource_id: None,
            evaluated_at_ms: end_ms,
        }
    }

    #[test]
    fn state_mapping_consumes_only_running_lifecycle_states() {
        for state in ["ACTIVE", "active", " REBOOTING ", "STARTING", "STOPPING"] {
            assert_eq!(
                compute_instance_state_consuming(state),
                Some(true),
                "{state}"
            );
        }
        for state in [
            "REQUESTED",
            "BUILD",
            "SHUTOFF",
            "DELETING",
            "DELETED",
            "ERROR",
        ] {
            assert_eq!(
                compute_instance_state_consuming(state),
                Some(false),
                "{state}"
            );
        }
        // Not a canonical storage state: corrupt, never idle.
        for state in ["", "garbage", "RUNNING", "ACTIVE_", "wobbling"] {
            assert_eq!(compute_instance_state_consuming(state), None, "{state}");
        }
    }

    #[tokio::test]
    async fn unknown_state_is_corrupt_and_never_silently_idle() {
        let (adapter, _clock, _store) = build_adapter(0).await;
        let error = adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "WOBBLING")
            .await
            .expect_err("an unknown state must fail closed");
        assert!(matches!(error, KernelError::MeteringCorrupt(_)));
    }

    #[tokio::test]
    async fn non_metered_kind_is_a_no_op() {
        let (adapter, _clock, store) = build_adapter(0).await;
        adapter
            .observe_resource_state("network_network", "project-a", "net-1", "ACTIVE")
            .await
            .expect("a non-metered kind is a no-op");
        let report = store
            .usage(&query("project-a", COMPUTE_INSTANCE_METER, 0, HOUR))
            .await
            .expect("usage");
        // No authority was anchored and no interval exists for the scope.
        assert_eq!(report.authority_started_at_ms, None);
        assert_eq!(report.last_observed_at_ms, None);
        assert_eq!(report.meters[0].status, UsageStatus::Unavailable);
        assert_eq!(report.meters[0].total, "0.000");
    }

    #[tokio::test]
    async fn definitions_are_paged_in_stable_key_order() {
        let (adapter, _clock, _store) = build_adapter(0).await;
        let page = adapter.definitions(50, None).await.expect("definitions");
        assert_eq!(page.definitions.len(), METER_CATALOG.len());
        assert!(!page.has_more);
        assert_eq!(page.definitions[0].key, "compute:instance_seconds");
        assert_eq!(page.definitions[0].unit.as_str(), "instance_second");
        assert_eq!(page.definitions[1].key, "volume:allocated_byte_seconds");
        assert_eq!(page.definitions[1].unit.as_str(), "byte_second");

        let first = adapter.definitions(1, None).await.expect("first page");
        assert!(first.has_more);
        assert_eq!(first.definitions.len(), 1);
        let cursor = first.next_cursor.expect("cursor");
        let decoded = o3k_native_api::metering::DefinitionsQuery {
            limit: Some(1),
            cursor: Some(cursor),
        };
        let after = decoded.after_id().expect("cursor decodes").expect("after");
        let second = adapter
            .definitions(1, Some(after.as_str()))
            .await
            .expect("second page");
        assert_eq!(second.definitions.len(), 1);
        assert_eq!(second.definitions[0].key, "volume:allocated_byte_seconds");
        assert!(!second.has_more);
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn allocation_observation_closes_with_zero_quantity() {
        let (adapter, clock, store) = build_adapter(0).await;
        clock.set(0);
        adapter
            .observe_allocation(
                VOLUME_ALLOCATION_METER,
                "project-a",
                "vol-1",
                1_000_000,
                true,
            )
            .await
            .expect("open allocation");
        clock.set(HOUR);
        adapter
            .observe_allocation(
                VOLUME_ALLOCATION_METER,
                "project-a",
                "vol-1",
                1_000_000,
                false,
            )
            .await
            .expect("close allocation");

        let report = store
            .usage(&query("project-a", VOLUME_ALLOCATION_METER, 0, HOUR))
            .await
            .expect("usage");
        assert_eq!(report.meters[0].status, UsageStatus::Complete);
        assert_eq!(report.meters[0].unit.as_str(), "byte_second");
        // 1_000_000 bytes * 3600 s = 3_600_000_000 byte-seconds.
        assert_eq!(report.meters[0].total, "3600000000.000");
    }

    #[tokio::test]
    async fn lifecycle_sequence_accrues_exact_instance_seconds_without_double_counting() {
        let (adapter, clock, store) = build_adapter(BASE).await;
        store.ensure_authority(BASE).await.expect("authority");

        clock.set(BASE);
        adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "ACTIVE")
            .await
            .expect("active");
        clock.set(BASE + HOUR);
        adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "SHUTOFF")
            .await
            .expect("shutoff");
        clock.set(BASE + 2 * HOUR);
        adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "ACTIVE")
            .await
            .expect("active again");
        clock.set(BASE + 3 * HOUR);
        adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "DELETED")
            .await
            .expect("deleted");

        let report = store
            .usage(&query(
                "project-a",
                COMPUTE_INSTANCE_METER,
                BASE,
                BASE + 3 * HOUR,
            ))
            .await
            .expect("usage");
        let usage = &report.meters[0];
        assert_eq!(usage.status, UsageStatus::Complete);
        assert_eq!(usage.unit.as_str(), "instance_second");
        assert_eq!(usage.total, "7200.000");
        assert_eq!(usage.buckets.len(), 2);
        assert_eq!(usage.buckets[0].bucket_start_ms, BASE);
        assert_eq!(usage.buckets[0].quantity, "3600.000");
        assert_eq!(usage.buckets[1].bucket_start_ms, BASE + 2 * HOUR);
        assert_eq!(usage.buckets[1].quantity, "3600.000");

        // Replaying the terminal delete is an idempotent close, never a re-open
        // and never a second accrual.
        adapter
            .observe_resource_state("compute_instance", "project-a", "server-1", "DELETED")
            .await
            .expect("replay deleted");
        let replay = store
            .usage(&query(
                "project-a",
                COMPUTE_INSTANCE_METER,
                BASE,
                BASE + 3 * HOUR,
            ))
            .await
            .expect("usage replay");
        assert_eq!(replay.meters[0].total, "7200.000");
    }

    #[test]
    fn producible_meters_track_composition_capabilities() {
        let all = producible_meters(true, true);
        assert!(all.contains(&COMPUTE_INSTANCE_METER));
        assert!(all.contains(&VOLUME_ALLOCATION_METER));

        let compute_only = producible_meters(true, false);
        assert!(compute_only.contains(&COMPUTE_INSTANCE_METER));
        assert!(!compute_only.contains(&VOLUME_ALLOCATION_METER));

        let storage_only = producible_meters(false, true);
        assert!(!storage_only.contains(&COMPUTE_INSTANCE_METER));
        assert!(storage_only.contains(&VOLUME_ALLOCATION_METER));
    }

    #[tokio::test]
    async fn non_producible_meter_is_hidden_and_rejected() {
        let (_all, clock, store) = build_adapter(0).await;
        let adapter = Arc::new(
            MeteringAdapter::new(store, clock)
                .with_producible_meters(producible_meters(true, false)),
        );
        let page = adapter.definitions(50, None).await.expect("definitions");
        assert_eq!(page.definitions.len(), 1);
        assert_eq!(page.definitions[0].key, COMPUTE_INSTANCE_METER);
        // Usage for the hidden volume meter is an unknown meter, never zero.
        let error = adapter
            .usage(&query("project-a", VOLUME_ALLOCATION_METER, 0, HOUR))
            .await
            .expect_err("a hidden meter must be rejected");
        assert_eq!(error, MeteringError::BoundsExceeded);
    }
}
