//! Canonical metering definitions, durable observation contract, and bounded
//! usage aggregation semantics (ADR-0183, SPEC-0046).
//!
//! Metering answers "how much did a scope consume over a time interval". It is
//! deliberately distinct from:
//!
//! - quota, which answers "how much is allocated against a limit right now";
//! - audit, which records administrative facts;
//! - diagnostics, which reports health/freshness;
//! - billing/pricing, which is an explicit non-goal.
//!
//! This module owns the *semantics*: meter identity, units, aggregation
//! vocabulary, time boundaries, idempotent observation identity and the
//! bounded query contract. Storage lives behind [`MeteringRepository`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::KernelError;

/// Fixed width of the durable ingest bucket.
///
/// Every closed usage interval is folded into buckets of exactly this width.
/// Aggregates are therefore bounded and additive; queries at this granularity
/// or coarser are served from durable rollups, never from raw history.
pub const INGEST_BUCKET_WIDTH_MS: i64 = 3_600_000;

/// Maximum span of one bounded usage query.
pub const MAX_USAGE_RANGE_MS: i64 = 31_622_400_000; // 366 days

/// Maximum number of output buckets per meter per query.
pub const MAX_USAGE_BUCKETS: usize = 1100;

/// Maximum number of meters selected by one query.
pub const MAX_USAGE_METERS: usize = 8;

/// Maximum number of durable `resource × ingest bucket` aggregate rows one
/// bounded usage query may read, per meter.
///
/// This is the work bound: a query that would read more rows is rejected, never
/// silently truncated. A single series over the advertised 366-day range needs
/// at most 8784 hourly rows, so this bound admits many series at full range.
pub const MAX_USAGE_AGGREGATE_ROWS: usize = 25_000;

/// Maximum number of **distinct** resource series one bounded usage query may
/// read, per meter.
///
/// This bounds response cardinality independently of how many aggregate rows
/// those series span; a query exceeding it is rejected, never truncated.
pub const MAX_USAGE_SERIES: usize = 500;

/// Maximum number of open (unclosed) intervals read by one query.
pub const MAX_OPEN_INTERVALS: usize = 20_000;

/// Maximum number of ingest buckets one interval may span when folded.
///
/// An interval longer than this fails closed instead of silently truncating.
pub const MAX_INGEST_BUCKETS: usize = 96_000; // ~11 years of hourly buckets

/// Canonical clock port for metering observations.
///
/// Production uses [`SystemClock`]; tests use a deterministic clock so interval
/// arithmetic is provable without sleeping.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch, UTC.
    fn now_unix_ms(&self) -> i64;
}

/// Production clock reading the real system time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

/// Aggregation vocabulary. `Integral` accumulates `quantity × duration`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterAggregation {
    /// Usage is the time integral of a quantity over the reported period.
    Integral,
}

/// Unit vocabulary. Every meter names exactly one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterUnit {
    /// Number of allocated instances multiplied by seconds.
    InstanceSecond,
    /// Allocated virtual CPUs multiplied by seconds.
    VcpuSecond,
    /// Allocated bytes multiplied by seconds.
    ByteSecond,
}

impl MeterUnit {
    /// Stable wire/contract spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstanceSecond => "instance_second",
            Self::VcpuSecond => "vcpu_second",
            Self::ByteSecond => "byte_second",
        }
    }
}

/// Output bucket granularity supported by the bounded usage query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageGranularity {
    /// One UTC hour.
    Hour,
    /// One UTC day.
    Day,
}

impl UsageGranularity {
    /// Value carried on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    /// Bucket width in milliseconds.
    #[must_use]
    pub fn width_ms(self) -> i64 {
        match self {
            Self::Hour => INGEST_BUCKET_WIDTH_MS,
            Self::Day => 86_400_000,
        }
    }
}

/// Machine-readable completeness of a usage response.
///
/// A usage number without completeness semantics is dangerous: O3K must never
/// present a period it did not authoritatively observe as if it had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    /// The whole requested period is covered by O3K metering authority.
    Complete,
    /// Part of the requested period predates O3K metering authority and is
    /// therefore not counted. The covered remainder is authoritative.
    Partial,
    /// The whole requested period predates O3K metering authority. No usage is
    /// reported and none may be inferred by the client.
    Unavailable,
}

impl UsageStatus {
    /// Stable wire/contract spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A machine-readable meter definition.
///
/// Definitions describe semantics only. They never contain pricing, layout or
/// executable formulas, and a meter is only advertised when O3K can actually
/// produce it for the active profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterDefinition {
    /// Stable namespaced key, e.g. `compute:instance_seconds`.
    pub key: &'static str,
    /// Canonical O3K service namespace that owns the meter.
    pub owning_service: &'static str,
    /// Unit of the reported quantity.
    pub unit: MeterUnit,
    /// How the quantity is aggregated over time.
    pub aggregation: MeterAggregation,
    /// Canonical resource type whose lifecycle feeds the meter.
    pub resource_type: &'static str,
    /// Granularities this meter supports.
    pub supported_granularities: &'static [UsageGranularity],
    /// Whether an authenticated tenant may read its own usage.
    pub tenant_visible: bool,
    /// Whether a durable system/operator may read cross-scope usage.
    pub operator_visible: bool,
    /// Short, secret-free human description.
    pub description: &'static str,
    /// Definition version.
    pub version: u32,
}

const SUPPORTED_GRANULARITIES: &[UsageGranularity] =
    &[UsageGranularity::Hour, UsageGranularity::Day];

/// Authoritative meter catalog for the supported native profile.
///
/// Only meters whose source of truth is canonical O3K lifecycle authority are
/// listed. Notably absent (and deliberately not fabricated): CPU utilisation,
/// network bytes, storage IOPS/bandwidth, provider telemetry, request counts,
/// cost and prices.
pub const METER_CATALOG: &[MeterDefinition] = &[
    MeterDefinition {
        key: "compute:instance_seconds",
        owning_service: "compute",
        unit: MeterUnit::InstanceSecond,
        aggregation: MeterAggregation::Integral,
        resource_type: "compute_instance",
        supported_granularities: SUPPORTED_GRANULARITIES,
        tenant_visible: true,
        operator_visible: true,
        description: "Allocated compute instance time while the instance is running.",
        version: 1,
    },
    MeterDefinition {
        key: "volume:allocated_byte_seconds",
        owning_service: "volume",
        unit: MeterUnit::ByteSecond,
        aggregation: MeterAggregation::Integral,
        resource_type: "volume",
        supported_granularities: SUPPORTED_GRANULARITIES,
        tenant_visible: true,
        operator_visible: true,
        description: "Allocated block-volume capacity time while the volume exists.",
        version: 1,
    },
];

/// Looks up an advertised meter definition by its stable key.
#[must_use]
pub fn meter_definition(key: &str) -> Option<&'static MeterDefinition> {
    METER_CATALOG
        .iter()
        .find(|definition| definition.key == key)
}

/// One authoritative lifecycle observation recorded by an O3K-owned authority.
///
/// The observation is idempotent: applying the same logical transition twice
/// must not open, close or accrue usage twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterObservation {
    /// Advertised meter key.
    pub meter_key: String,
    /// Effective ownership scope the resource belongs to.
    pub scope: String,
    /// Canonical resource identity.
    pub resource_id: String,
    /// Unit quantity while consuming (`> 0`).
    pub quantity: u64,
    /// `true` while the resource consumes the meter, `false` once it stops.
    pub consuming: bool,
    /// Authoritative observation instant, milliseconds since the Unix epoch.
    pub observed_at_ms: i64,
    /// Stable authority label, never a free-form provider string.
    pub authority: String,
}

impl MeterObservation {
    /// Validates the observation before it reaches durable storage.
    pub fn validate(&self) -> Result<(), KernelError> {
        if meter_definition(&self.meter_key).is_none() {
            return Err(KernelError::InvalidIdentifier("meter key".into()));
        }
        if self.scope.is_empty() || self.scope.len() > 256 {
            return Err(KernelError::InvalidScopeId(self.scope.clone()));
        }
        if self.resource_id.is_empty() || self.resource_id.len() > 256 {
            return Err(KernelError::InvalidResourceId(self.resource_id.clone()));
        }
        if self.consuming && self.quantity == 0 {
            return Err(KernelError::InvalidIdentifier("meter quantity".into()));
        }
        if !self.consuming && self.quantity != 0 {
            return Err(KernelError::InvalidIdentifier("meter quantity".into()));
        }
        if self.observed_at_ms < 0 {
            return Err(KernelError::InvalidIdentifier("observation time".into()));
        }
        if self.authority.is_empty() || self.authority.len() > 64 {
            return Err(KernelError::InvalidIdentifier("meter authority".into()));
        }
        Ok(())
    }
}

/// Durable open or closed usage interval for one resource series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterIntervalRecord {
    pub interval_id: String,
    pub meter_key: String,
    pub scope: String,
    pub resource_id: String,
    pub quantity: u64,
    pub started_at_ms: i64,
    /// `None` while the interval is still open.
    pub ended_at_ms: Option<i64>,
    pub authority: String,
}

/// One durable aggregate row: `quantity × milliseconds` for a single series
/// and ingest bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterAggregateRecord {
    pub scope: String,
    pub meter_key: String,
    pub resource_id: String,
    pub bucket_start_ms: i64,
    pub bucket_width_ms: i64,
    pub quantity_millis: i64,
}

/// Bounded usage query. The effective scope is always derived from
/// `AuthContext` by the caller and is never taken from request JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    pub scope: String,
    pub meter_keys: Vec<String>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub granularity: UsageGranularity,
    /// Optional single authorized resource series filter.
    pub resource_id: Option<String>,
    /// Evaluation instant. The response never claims coverage beyond this.
    pub evaluated_at_ms: i64,
}

impl UsageQuery {
    /// Validates bounds and normalizes nothing: callers receive a hard error
    /// instead of a silently truncated authoritative result.
    pub fn validate(&self) -> Result<(), KernelError> {
        if self.scope.is_empty() || self.scope.len() > 256 {
            return Err(KernelError::InvalidScopeId(self.scope.clone()));
        }
        if self.meter_keys.is_empty() || self.meter_keys.len() > MAX_USAGE_METERS {
            return Err(KernelError::InvalidIdentifier("meter selection".into()));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.meter_keys.len());
        for key in &self.meter_keys {
            let Some(definition) = meter_definition(key) else {
                return Err(KernelError::InvalidIdentifier(format!("meter {key}")));
            };
            if !definition
                .supported_granularities
                .contains(&self.granularity)
            {
                return Err(KernelError::InvalidIdentifier(format!(
                    "granularity for meter {key}"
                )));
            }
            if seen.contains(&key.as_str()) {
                return Err(KernelError::InvalidIdentifier("duplicate meter".into()));
            }
            seen.push(key.as_str());
        }
        if self.start_ms < 0 || self.end_ms < 0 {
            return Err(KernelError::InvalidIdentifier("usage time range".into()));
        }
        if self.end_ms <= self.start_ms {
            return Err(KernelError::InvalidIdentifier("usage time range".into()));
        }
        if self.end_ms - self.start_ms > MAX_USAGE_RANGE_MS {
            return Err(KernelError::InvalidIdentifier("usage range".into()));
        }
        let width = self.granularity.width_ms();
        if self.start_ms % width != 0 || self.end_ms % width != 0 {
            return Err(KernelError::InvalidIdentifier(
                "usage bucket alignment".into(),
            ));
        }
        let buckets = (self.end_ms - self.start_ms) / width;
        if buckets <= 0 || buckets > MAX_USAGE_BUCKETS as i64 {
            return Err(KernelError::InvalidIdentifier("usage bucket count".into()));
        }
        if let Some(resource_id) = &self.resource_id
            && (resource_id.is_empty() || resource_id.len() > 256)
        {
            return Err(KernelError::InvalidResourceId(resource_id.clone()));
        }
        if self.evaluated_at_ms < 0 {
            return Err(KernelError::InvalidIdentifier(
                "usage evaluation time".into(),
            ));
        }
        Ok(())
    }
}

/// Splits `[start_ms, end_ms)` into ingest-bucket contributions of
/// `quantity × milliseconds`, failing closed on overflow or an over-long span.
pub fn bucket_contributions(
    start_ms: i64,
    end_ms: i64,
    quantity: u64,
    width_ms: i64,
) -> Result<Vec<(i64, i64)>, KernelError> {
    if width_ms <= 0 || end_ms <= start_ms {
        return Err(KernelError::InvalidIdentifier("meter interval".into()));
    }
    let mut contributions = Vec::new();
    let mut cursor = start_ms;
    while cursor < end_ms {
        if contributions.len() >= MAX_INGEST_BUCKETS {
            return Err(KernelError::InvalidIdentifier("meter interval span".into()));
        }
        let bucket_start = cursor.div_euclid(width_ms) * width_ms;
        let bucket_end = bucket_start
            .checked_add(width_ms)
            .ok_or_else(|| KernelError::InvalidIdentifier("meter interval".into()))?;
        let segment_end = end_ms.min(bucket_end);
        let overlap = segment_end - cursor;
        let contribution = (quantity as i128)
            .checked_mul(overlap as i128)
            .ok_or_else(|| KernelError::InvalidIdentifier("meter overflow".into()))?;
        let contribution = i64::try_from(contribution)
            .map_err(|_| KernelError::InvalidIdentifier("meter overflow".into()))?;
        contributions.push((bucket_start, contribution));
        cursor = segment_end;
    }
    Ok(contributions)
}

/// Accumulates ingest-bucket contributions into output buckets of a coarser
/// granularity, failing closed on overflow.
///
/// Contributions are keyed by the aligned bucket start so one request cannot
/// pay a quadratic scan when many ingest buckets are folded.
#[derive(Debug, Default)]
pub struct UsageAccumulator {
    buckets: BTreeMap<i64, i128>,
}

impl UsageAccumulator {
    /// Creates an empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: BTreeMap::new(),
        }
    }

    /// Adds one ingest-bucket contribution. `quantity_millis` must be `>= 0`.
    pub fn add(
        &mut self,
        granularity: UsageGranularity,
        bucket_start_ms: i64,
        quantity_millis: i64,
    ) -> Result<(), KernelError> {
        if quantity_millis < 0 {
            return Err(KernelError::InvalidIdentifier("meter aggregate".into()));
        }
        let width = granularity.width_ms();
        let aligned = bucket_start_ms.div_euclid(width) * width;
        let total = self.buckets.entry(aligned).or_insert(0);
        *total = (*total)
            .checked_add(i128::from(quantity_millis))
            .ok_or_else(|| KernelError::InvalidIdentifier("meter overflow".into()))?;
        Ok(())
    }

    /// Number of distinct output buckets accumulated so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether no contribution has been accumulated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Consumes the accumulator, returning buckets ordered by start instant.
    #[must_use]
    pub fn into_buckets(self) -> Vec<(i64, i128)> {
        self.buckets.into_iter().collect()
    }
}

/// Formats `quantity × milliseconds` as a decimal value of the meter unit with
/// exactly three fractional digits, preserving precision beyond `f64`.
#[must_use]
pub fn format_quantity_millis(quantity_millis: i128) -> String {
    let negative = quantity_millis < 0;
    let magnitude = quantity_millis.unsigned_abs();
    let whole = magnitude / 1000;
    let fraction = magnitude % 1000;
    if negative {
        format!("-{whole}.{fraction:03}")
    } else {
        format!("{whole}.{fraction:03}")
    }
}

/// One bounded output bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBucket {
    pub bucket_start_ms: i64,
    pub bucket_width_ms: i64,
    /// Decimal quantity in the meter unit, three fractional digits.
    pub quantity: String,
}

/// Usage for one meter over the requested period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterUsage {
    pub meter_key: String,
    pub unit: MeterUnit,
    pub granularity: UsageGranularity,
    pub status: UsageStatus,
    pub buckets: Vec<UsageBucket>,
    /// Decimal total in the meter unit, three fractional digits.
    pub total: String,
}

/// Complete bounded usage response for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterUsageReport {
    pub scope: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Instant through which the response reflects authoritative usage.
    pub observed_through_ms: i64,
    /// Instant O3K metering authority began for this scope, if any.
    pub authority_started_at_ms: Option<i64>,
    /// Most recent durable observation processed by the whole metering
    /// authority, if any. This is authority-wide observability, **not**
    /// per-scope: whichever scope was observed most recently sets it, so a
    /// quiet scope can appear fresher than it is. Authority start and
    /// `observed_through` carry the per-scope completeness meaning.
    pub last_observed_at_ms: Option<i64>,
    pub meters: Vec<MeterUsage>,
}

/// Durable metering authority port.
#[async_trait]
pub trait MeteringRepository: Send + Sync {
    /// Anchors start-of-O3K-authority the first time it is called and is a
    /// no-op afterwards. Historical consumption before this instant is never
    /// invented.
    async fn ensure_authority(&self, now_ms: i64) -> Result<(), KernelError>;

    /// Applies one observation: opens, refreshes or closes a durable interval
    /// and folds any closed usage into bounded aggregates.
    ///
    /// Implementations must be idempotent: replaying the same logical
    /// transition must not open, close or accrue usage twice, and two
    /// concurrent writers must produce exactly one durable effect.
    async fn record_observation(&self, observation: &MeterObservation) -> Result<(), KernelError>;

    /// Bounded usage aggregation for one scope.
    async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, KernelError>;
}

/// Port that projects O3K-owned lifecycle authority into metering authority.
///
/// Callers pass the canonical resource kind, ownership scope, resource identity
/// and the durable lifecycle state they just applied. The implementation owns
/// the mapping from that state to meter observations, so no provider-specific
/// or state-encoding knowledge leaks into the calling domain.
///
/// A failure returned here is a real failure of the metering authority and the
/// caller must surface it (and retry the idempotent step) rather than silently
/// dropping usage.
#[async_trait]
pub trait LifecycleMeteringObserver: Send + Sync {
    /// Projects one durable lifecycle state for a resource kind whose meters
    /// are derived from that state (for example compute instance runtime).
    async fn observe_resource_state(
        &self,
        kind: &str,
        project_id: &str,
        resource_id: &str,
        observed_state: &str,
    ) -> Result<(), KernelError>;

    /// Projects an explicit allocation transition for a quantity-bearing
    /// meter, where the quantity is owned by the caller's domain (for example
    /// allocated volume bytes).
    async fn observe_allocation(
        &self,
        meter_key: &str,
        project_id: &str,
        resource_id: &str,
        quantity: u64,
        consuming: bool,
    ) -> Result<(), KernelError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn catalog_keys_are_unique_and_namespaced() {
        for definition in METER_CATALOG {
            assert!(
                definition.key.contains(':'),
                "meter key {} must be namespaced",
                definition.key
            );
            assert!(!definition.description.is_empty());
            assert!(definition.tenant_visible);
        }
        let mut keys: Vec<_> = METER_CATALOG.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }

    #[test]
    fn definitions_do_not_carry_pricing_or_formulas() {
        for definition in METER_CATALOG {
            let description = definition.description.to_ascii_lowercase();
            for forbidden in ["cost", "price", "currency", "invoice", "=", "$"] {
                assert!(
                    !description.contains(forbidden),
                    "meter {} description must not contain {forbidden}",
                    definition.key
                );
            }
        }
    }

    #[test]
    fn bucket_contributions_split_across_boundaries_without_double_counting() {
        // 10:30 -> 11:30 spans two hourly buckets at 30 minutes each.
        let start = 10 * INGEST_BUCKET_WIDTH_MS + INGEST_BUCKET_WIDTH_MS / 2;
        let end = start + INGEST_BUCKET_WIDTH_MS;
        let contributions = bucket_contributions(start, end, 2, INGEST_BUCKET_WIDTH_MS).unwrap();
        assert_eq!(contributions.len(), 2);
        let total: i64 = contributions.iter().map(|(_, value)| *value).sum();
        assert_eq!(total, 2 * INGEST_BUCKET_WIDTH_MS);
        assert_eq!(contributions[0].0, 10 * INGEST_BUCKET_WIDTH_MS);
        assert_eq!(contributions[1].0, 11 * INGEST_BUCKET_WIDTH_MS);
    }

    #[test]
    fn bucket_contributions_exclude_the_end_instant() {
        let start = 5 * INGEST_BUCKET_WIDTH_MS;
        let end = 6 * INGEST_BUCKET_WIDTH_MS;
        let contributions = bucket_contributions(start, end, 1, INGEST_BUCKET_WIDTH_MS).unwrap();
        assert_eq!(contributions, vec![(start, INGEST_BUCKET_WIDTH_MS)]);
    }

    #[test]
    fn bucket_contributions_fail_closed_on_overflow() {
        let start = 0;
        let end = INGEST_BUCKET_WIDTH_MS;
        assert!(bucket_contributions(start, end, u64::MAX, INGEST_BUCKET_WIDTH_MS).is_err());
    }

    #[test]
    fn bucket_contributions_fail_closed_on_bucket_end_overflow() {
        // A near-i64::MAX cursor must fail the checked bucket_end addition
        // instead of wrapping past the end instant.
        let start = i64::MAX - 1;
        let end = i64::MAX;
        let error = bucket_contributions(start, end, 1, INGEST_BUCKET_WIDTH_MS).unwrap_err();
        assert!(matches!(error, KernelError::InvalidIdentifier(_)));
    }

    #[test]
    fn bucket_contributions_reject_more_than_the_ingest_bucket_bound() {
        // width 1ms with a span of MAX_INGEST_BUCKETS + 1 buckets must fail
        // closed rather than silently truncating.
        let start = 0;
        let end = i64::try_from(MAX_INGEST_BUCKETS + 1).unwrap();
        let error = bucket_contributions(start, end, 1, 1).unwrap_err();
        assert!(matches!(error, KernelError::InvalidIdentifier(_)));
    }

    #[test]
    fn accumulator_merges_ingest_buckets_into_daily_buckets() {
        let mut accumulator = UsageAccumulator::new();
        for hour in 0..24 {
            accumulator
                .add(
                    UsageGranularity::Day,
                    hour * INGEST_BUCKET_WIDTH_MS,
                    INGEST_BUCKET_WIDTH_MS,
                )
                .unwrap();
        }
        assert_eq!(accumulator.len(), 1);
        let buckets = accumulator.into_buckets();
        assert_eq!(buckets, vec![(0, 24 * i128::from(INGEST_BUCKET_WIDTH_MS))]);
    }

    #[test]
    fn accumulator_rejects_negative_quantities() {
        let mut accumulator = UsageAccumulator::new();
        assert!(accumulator.add(UsageGranularity::Hour, 0, -1).is_err());
    }

    #[test]
    fn quantity_formatting_keeps_millisecond_precision() {
        assert_eq!(format_quantity_millis(0), "0.000");
        assert_eq!(format_quantity_millis(1000), "1.000");
        assert_eq!(format_quantity_millis(1500), "1.500");
        assert_eq!(format_quantity_millis(1), "0.001");
    }

    #[test]
    fn usage_query_rejects_unbounded_or_misaligned_requests() {
        let base = UsageQuery {
            scope: "project-a".into(),
            meter_keys: vec!["compute:instance_seconds".into()],
            start_ms: 0,
            end_ms: UsageGranularity::Hour.width_ms(),
            granularity: UsageGranularity::Hour,
            resource_id: None,
            evaluated_at_ms: 0,
        };
        assert!(base.validate().is_ok());

        let misaligned = UsageQuery {
            end_ms: base.end_ms + 1,
            ..base.clone()
        };
        assert!(misaligned.validate().is_err());

        let too_wide = UsageQuery {
            end_ms: base.start_ms + MAX_USAGE_RANGE_MS + 1,
            granularity: UsageGranularity::Day,
            ..base.clone()
        };
        assert!(too_wide.validate().is_err());

        let unknown_meter = UsageQuery {
            meter_keys: vec!["compute:cpu_cycles".into()],
            ..base.clone()
        };
        assert!(unknown_meter.validate().is_err());

        let zero_range = UsageQuery {
            end_ms: base.start_ms,
            ..base.clone()
        };
        assert!(zero_range.validate().is_err());
    }

    #[test]
    fn usage_query_bounds_bucket_count() {
        let hourly_over_366_days = UsageQuery {
            scope: "project-a".into(),
            meter_keys: vec!["compute:instance_seconds".into()],
            start_ms: 0,
            end_ms: MAX_USAGE_RANGE_MS,
            granularity: UsageGranularity::Hour,
            resource_id: None,
            evaluated_at_ms: MAX_USAGE_RANGE_MS,
        };
        assert!(hourly_over_366_days.validate().is_err());
    }

    #[test]
    fn usage_bounds_admit_a_full_range_single_series() {
        // The advertised 366-day range needs at most 8784 hourly aggregate rows
        // for one series, so the work bound admits it with room to spare while
        // the series bound stays about cardinality.
        let hourly_rows = usize::try_from(MAX_USAGE_RANGE_MS / INGEST_BUCKET_WIDTH_MS).unwrap();
        assert!(hourly_rows > 5_000);
        assert!(hourly_rows <= MAX_USAGE_AGGREGATE_ROWS);
    }

    #[test]
    fn observations_validate_quantity_consistency() {
        let consuming = MeterObservation {
            meter_key: "compute:instance_seconds".into(),
            scope: "project-a".into(),
            resource_id: "server-1".into(),
            quantity: 1,
            consuming: true,
            observed_at_ms: 0,
            authority: "o3k-lifecycle".into(),
        };
        assert!(consuming.validate().is_ok());
        assert!(
            MeterObservation {
                quantity: 0,
                ..consuming.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            MeterObservation {
                consuming: false,
                quantity: 1,
                ..consuming.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            MeterObservation {
                meter_key: "compute:cpu_cycles".into(),
                ..consuming.clone()
            }
            .validate()
            .is_err()
        );
    }
}
