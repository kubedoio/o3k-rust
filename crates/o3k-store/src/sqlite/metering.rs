//! SQLite durable metering store (`MeteringRepository`).
//!
//! Bucket arithmetic, usage status vocabulary and quantity formatting are owned
//! by `o3k_kernel::metering`; this module only persists intervals and folds
//! closed usage into bounded ingest buckets. All SQL here is additive and
//! idempotent so replayed observations converge instead of double counting.

use async_trait::async_trait;
use o3k_kernel::metering::{MAX_OPEN_INTERVALS, MAX_USAGE_AGGREGATE_ROWS, UsageAccumulator};
use o3k_kernel::{
    INGEST_BUCKET_WIDTH_MS, KernelError, MAX_USAGE_SERIES, MeterObservation, MeterUsage,
    MeterUsageReport, MeteringRepository, UsageBucket, UsageQuery, UsageStatus,
    bucket_contributions, format_quantity_millis, meter_definition,
};
use sqlx::Row;
use std::collections::HashSet;

use super::SqliteStore;

/// Maps any store/driver failure to a secret-free metering-unavailable error.
fn store_err(error: impl std::fmt::Display) -> KernelError {
    KernelError::MeteringUnavailable(error.to_string())
}

/// SQLite extended codes for a unique constraint (2067) or primary key
/// (1555) violation. Used to converge a concurrent duplicate open interval.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            matches!(database.code().as_deref(), Some("1555") | Some("2067"))
        }
        _ => false,
    }
}

/// Deterministic interval identity so a retried open observation reuses the
/// same durable row instead of opening a second interval.
///
/// Length prefixes make the preimage injective: `scope="p:q", resource="r"`
/// and `scope="p", resource="q:r"` must not collide.
fn interval_id(observation: &MeterObservation) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!(
            "o3k:metering:interval:{}:{}:{}:{}:{}:{}:{}",
            observation.meter_key.len(),
            observation.meter_key,
            observation.scope.len(),
            observation.scope,
            observation.resource_id.len(),
            observation.resource_id,
            observation.observed_at_ms
        )
        .as_bytes(),
    )
    .to_string()
}

/// Completeness of the requested period relative to O3K metering authority.
///
/// A period that extends beyond `evaluated_at_ms` is never `Complete`: O3K has
/// no authoritative observation for the not-yet-elapsed future, so the store
/// reports `Partial` even if authority covers the elapsed remainder.
fn usage_status(
    authority_started_at_ms: Option<i64>,
    start_ms: i64,
    end_ms: i64,
    evaluated_at_ms: i64,
) -> UsageStatus {
    match authority_started_at_ms {
        None => UsageStatus::Unavailable,
        Some(authority) => {
            if end_ms <= authority {
                UsageStatus::Unavailable
            } else if start_ms < authority || end_ms > evaluated_at_ms {
                UsageStatus::Partial
            } else {
                UsageStatus::Complete
            }
        }
    }
}

/// Assembles one meter's bounded response. `Unavailable` periods report no
/// usage at all, so no pre-authority consumption can be inferred.
fn meter_usage(
    meter_key: String,
    query: &UsageQuery,
    status: UsageStatus,
    accumulator: UsageAccumulator,
) -> Result<MeterUsage, KernelError> {
    let unit = meter_definition(&meter_key)
        .map(|definition| definition.unit)
        .ok_or_else(|| KernelError::MeteringCorrupt("meter definition disappeared".into()))?;
    if status == UsageStatus::Unavailable {
        return Ok(MeterUsage {
            meter_key,
            unit,
            granularity: query.granularity,
            status,
            buckets: Vec::new(),
            total: format_quantity_millis(0),
        });
    }
    let mut total: i128 = 0;
    let mut buckets = Vec::new();
    for (bucket_start_ms, value) in accumulator.into_buckets() {
        if bucket_start_ms < query.start_ms || bucket_start_ms >= query.end_ms {
            continue;
        }
        total = total
            .checked_add(value)
            .ok_or_else(|| KernelError::MeteringCorrupt("metering usage overflow".into()))?;
        buckets.push(UsageBucket {
            bucket_start_ms,
            bucket_width_ms: query.granularity.width_ms(),
            quantity: format_quantity_millis(value),
        });
    }
    Ok(MeterUsage {
        meter_key,
        unit,
        granularity: query.granularity,
        status,
        buckets,
        total: format_quantity_millis(total),
    })
}

struct OpenInterval {
    interval_id: String,
    quantity: i64,
    started_at_ms: i64,
    authority: String,
}

impl SqliteStore {
    async fn load_open_interval(
        connection: &mut sqlx::sqlite::SqliteConnection,
        observation: &MeterObservation,
    ) -> Result<Option<OpenInterval>, KernelError> {
        let row = sqlx::query(
            "SELECT interval_id, quantity, started_at_ms, authority FROM metering_intervals \
             WHERE meter_key = ?1 AND scope = ?2 AND resource_id = ?3 AND ended_at_ms IS NULL \
             ORDER BY started_at_ms DESC LIMIT 1",
        )
        .bind(&observation.meter_key)
        .bind(&observation.scope)
        .bind(&observation.resource_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(store_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(OpenInterval {
            interval_id: row.try_get("interval_id").map_err(store_err)?,
            quantity: row.try_get("quantity").map_err(store_err)?,
            started_at_ms: row.try_get("started_at_ms").map_err(store_err)?,
            authority: row.try_get("authority").map_err(store_err)?,
        }))
    }

    /// Whether a row already carries the observation's deterministic interval
    /// id and describes exactly the same interval. A match means a replayed
    /// open may converge; a mismatch is corruption rather than something to
    /// silently swallow.
    async fn deterministic_interval_matches(
        connection: &mut sqlx::sqlite::SqliteConnection,
        observation: &MeterObservation,
        quantity: i64,
    ) -> Result<bool, KernelError> {
        let row = sqlx::query(
            "SELECT meter_key, scope, resource_id, quantity, authority \
             FROM metering_intervals WHERE interval_id = ?1",
        )
        .bind(interval_id(observation))
        .fetch_optional(&mut *connection)
        .await
        .map_err(store_err)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let meter_key: String = row.try_get("meter_key").map_err(store_err)?;
        let scope: String = row.try_get("scope").map_err(store_err)?;
        let resource_id: String = row.try_get("resource_id").map_err(store_err)?;
        let stored_quantity: i64 = row.try_get("quantity").map_err(store_err)?;
        let authority: String = row.try_get("authority").map_err(store_err)?;
        if meter_key != observation.meter_key
            || scope != observation.scope
            || resource_id != observation.resource_id
            || stored_quantity != quantity
            || authority != observation.authority
        {
            return Err(KernelError::MeteringCorrupt(
                "metering interval identity conflict".into(),
            ));
        }
        Ok(true)
    }

    /// Latest end instant already folded for the series, if any. Used to reject
    /// an out-of-order open that would overlap closed history.
    async fn max_ended_at(
        connection: &mut sqlx::sqlite::SqliteConnection,
        observation: &MeterObservation,
    ) -> Result<Option<i64>, KernelError> {
        let max_ended: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(ended_at_ms) FROM metering_intervals \
             WHERE meter_key = ?1 AND scope = ?2 AND resource_id = ?3",
        )
        .bind(&observation.meter_key)
        .bind(&observation.scope)
        .bind(&observation.resource_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(store_err)?;
        Ok(max_ended)
    }

    async fn record_observation_tx(
        connection: &mut sqlx::sqlite::SqliteConnection,
        observation: &MeterObservation,
    ) -> Result<(), KernelError> {
        let quantity = i64::try_from(observation.quantity)
            .map_err(|_| KernelError::InvalidIdentifier("meter quantity".into()))?;

        // Anchor authority if absent, then advance the durable watermark. The
        // watermark only moves forward so a late/out-of-order replay cannot
        // roll observed coverage backwards.
        sqlx::query(
            "INSERT INTO metering_authority (id, authority_started_at_ms, last_observed_at_ms) \
             VALUES (1, ?1, ?1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(observation.observed_at_ms)
        .execute(&mut *connection)
        .await
        .map_err(store_err)?;
        sqlx::query(
            "UPDATE metering_authority \
             SET last_observed_at_ms = MAX(last_observed_at_ms, ?1) WHERE id = 1",
        )
        .bind(observation.observed_at_ms)
        .execute(&mut *connection)
        .await
        .map_err(store_err)?;
        // Start-of-authority is a hard floor: an observation older than the
        // anchor predates O3K metering authority, so folding it would report
        // consumption O3K never owned.
        let anchor: i64 =
            sqlx::query("SELECT authority_started_at_ms FROM metering_authority WHERE id = 1")
                .fetch_one(&mut *connection)
                .await
                .map_err(store_err)?
                .try_get("authority_started_at_ms")
                .map_err(store_err)?;
        if observation.observed_at_ms < anchor {
            return Err(KernelError::MeteringCorrupt(
                "observation predates metering authority".into(),
            ));
        }

        let mut open = Self::load_open_interval(connection, observation).await?;
        if open.is_none() && observation.consuming {
            let inserted = sqlx::query(
                "INSERT INTO metering_intervals \
                 (interval_id, meter_key, scope, resource_id, quantity, started_at_ms, ended_at_ms, authority) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            )
            .bind(interval_id(observation))
            .bind(&observation.meter_key)
            .bind(&observation.scope)
            .bind(&observation.resource_id)
            .bind(quantity)
            .bind(observation.observed_at_ms)
            .bind(&observation.authority)
            .execute(&mut *connection)
            .await;
            match inserted {
                Ok(_) => {
                    // A newly opened interval must not start before already
                    // folded history ends; starting exactly at the prior end is
                    // legal (adjacent, not overlapping). Returning an error
                    // rolls the just-inserted row back.
                    let overlaps = Self::max_ended_at(connection, observation)
                        .await?
                        .is_some_and(|max_ended| observation.observed_at_ms < max_ended);
                    if overlaps {
                        return Err(KernelError::MeteringCorrupt(
                            "metering interval overlap".into(),
                        ));
                    }
                    return Ok(());
                }
                Err(error) if is_unique_violation(&error) => {
                    // Either the deterministic interval already exists (replay
                    // of an already-closed interval, or a concurrent open of the
                    // same one) or a concurrent writer owns a different open
                    // interval for the series. A replay must match the existing
                    // row exactly; a mismatch is corruption, never swallowed.
                    if Self::deterministic_interval_matches(connection, observation, quantity)
                        .await?
                    {
                        return Ok(());
                    }
                    open = Self::load_open_interval(connection, observation).await?;
                }
                Err(error) => return Err(store_err(error)),
            }
        }

        match open {
            None => Ok(()),
            Some(existing) => {
                if observation.observed_at_ms < existing.started_at_ms {
                    return Err(KernelError::MeteringCorrupt(
                        "metering clock regression".into(),
                    ));
                }
                if observation.consuming {
                    if existing.quantity != quantity {
                        return Err(KernelError::MeteringCorrupt(
                            "metering quantity changed within an open interval".into(),
                        ));
                    }
                    if existing.authority != observation.authority {
                        return Err(KernelError::MeteringCorrupt(
                            "metering authority changed within an open interval".into(),
                        ));
                    }
                    return Ok(());
                }
                Self::close_and_fold(connection, &existing, observation).await
            }
        }
    }

    async fn close_and_fold(
        connection: &mut sqlx::sqlite::SqliteConnection,
        open: &OpenInterval,
        observation: &MeterObservation,
    ) -> Result<(), KernelError> {
        let updated = sqlx::query(
            "UPDATE metering_intervals SET ended_at_ms = ?1 \
             WHERE interval_id = ?2 AND ended_at_ms IS NULL",
        )
        .bind(observation.observed_at_ms)
        .bind(&open.interval_id)
        .execute(&mut *connection)
        .await
        .map_err(store_err)?;
        if updated.rows_affected() == 0 {
            return Self::replay_or_corrupt(connection, open).await;
        }

        // A zero-duration interval accrues nothing. Folding would ask
        // `bucket_contributions` to split an empty span, which fails closed;
        // the close itself has already happened and must stand.
        if observation.observed_at_ms <= open.started_at_ms {
            return Ok(());
        }

        let quantity = u64::try_from(open.quantity)
            .map_err(|_| KernelError::MeteringCorrupt("metering quantity is negative".into()))?;
        let contributions = bucket_contributions(
            open.started_at_ms,
            observation.observed_at_ms,
            quantity,
            INGEST_BUCKET_WIDTH_MS,
        )?;
        for (bucket_start_ms, contribution) in contributions {
            // Read-modify-write is deliberate: overflow fails closed here
            // instead of silently wrapping inside SQL.
            let existing = sqlx::query(
                "SELECT quantity_millis FROM metering_aggregates \
                 WHERE scope = ?1 AND meter_key = ?2 AND resource_id = ?3 AND bucket_start_ms = ?4",
            )
            .bind(&observation.scope)
            .bind(&observation.meter_key)
            .bind(&observation.resource_id)
            .bind(bucket_start_ms)
            .fetch_optional(&mut *connection)
            .await
            .map_err(store_err)?;
            let current: i64 = match existing {
                Some(row) => row.try_get("quantity_millis").map_err(store_err)?,
                None => 0,
            };
            let combined = i128::from(current)
                .checked_add(i128::from(contribution))
                .ok_or_else(|| {
                    KernelError::MeteringCorrupt("metering aggregate overflow".into())
                })?;
            let combined = i64::try_from(combined)
                .map_err(|_| KernelError::MeteringCorrupt("metering aggregate overflow".into()))?;
            sqlx::query(
                "INSERT INTO metering_aggregates \
                 (scope, meter_key, resource_id, bucket_start_ms, bucket_width_ms, quantity_millis) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (scope, meter_key, resource_id, bucket_start_ms) \
                 DO UPDATE SET quantity_millis = excluded.quantity_millis, \
                               bucket_width_ms = excluded.bucket_width_ms",
            )
            .bind(&observation.scope)
            .bind(&observation.meter_key)
            .bind(&observation.resource_id)
            .bind(bucket_start_ms)
            .bind(INGEST_BUCKET_WIDTH_MS)
            .bind(combined)
            .execute(&mut *connection)
            .await
            .map_err(store_err)?;
        }
        Ok(())
    }

    /// A concurrent writer already closed the interval, so this close folded
    /// nothing. It is an idempotent no-op for any end instant.
    async fn replay_or_corrupt(
        connection: &mut sqlx::sqlite::SqliteConnection,
        open: &OpenInterval,
    ) -> Result<(), KernelError> {
        let row = sqlx::query("SELECT interval_id FROM metering_intervals WHERE interval_id = ?1")
            .bind(&open.interval_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(store_err)?;
        match row {
            // A concurrent writer already closed the interval. Two truthful
            // close observations of the same transition (a retried delete
            // racing recovery) can carry different instants; the loser folded
            // nothing, so it is an idempotent no-op regardless of the end
            // instant. Only a vanished interval is corruption.
            Some(_) => Ok(()),
            None => Err(KernelError::MeteringCorrupt(
                "metering interval disappeared during close".into(),
            )),
        }
    }

    async fn load_aggregates(
        connection: &mut sqlx::sqlite::SqliteConnection,
        query: &UsageQuery,
        meter_key: &str,
    ) -> Result<Vec<(String, i64, i64)>, KernelError> {
        // The work bound is one row per `resource × ingest bucket`; the
        // cardinality bound is how many distinct series those rows span. Both
        // are enforced here and reject rather than truncate.
        let limit = i64::try_from(MAX_USAGE_AGGREGATE_ROWS + 1)
            .map_err(|_| KernelError::InvalidIdentifier("usage aggregate rows bound".into()))?;
        let rows = if let Some(resource_id) = &query.resource_id {
            sqlx::query(
                "SELECT resource_id, bucket_start_ms, quantity_millis FROM metering_aggregates \
                 WHERE scope = ?1 AND meter_key = ?2 AND resource_id = ?3 \
                   AND bucket_start_ms >= ?4 AND bucket_start_ms < ?5 \
                 ORDER BY bucket_start_ms, resource_id LIMIT ?6",
            )
            .bind(&query.scope)
            .bind(meter_key)
            .bind(resource_id)
            .bind(query.start_ms)
            .bind(query.end_ms)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await
            .map_err(store_err)?
        } else {
            sqlx::query(
                "SELECT resource_id, bucket_start_ms, quantity_millis FROM metering_aggregates \
                 WHERE scope = ?1 AND meter_key = ?2 \
                   AND bucket_start_ms >= ?3 AND bucket_start_ms < ?4 \
                 ORDER BY bucket_start_ms, resource_id LIMIT ?5",
            )
            .bind(&query.scope)
            .bind(meter_key)
            .bind(query.start_ms)
            .bind(query.end_ms)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await
            .map_err(store_err)?
        };
        if rows.len() > MAX_USAGE_AGGREGATE_ROWS {
            return Err(KernelError::InvalidIdentifier(
                "usage aggregate rows bound".into(),
            ));
        }
        let mut parsed = Vec::with_capacity(rows.len());
        let mut series = HashSet::with_capacity(rows.len().min(MAX_USAGE_SERIES + 1));
        for row in &rows {
            let resource_id: String = row.try_get("resource_id").map_err(store_err)?;
            series.insert(resource_id.clone());
            parsed.push((
                resource_id,
                row.try_get("bucket_start_ms").map_err(store_err)?,
                row.try_get("quantity_millis").map_err(store_err)?,
            ));
        }
        if series.len() > MAX_USAGE_SERIES {
            return Err(KernelError::InvalidIdentifier("usage series bound".into()));
        }
        Ok(parsed)
    }

    async fn load_open_intervals(
        connection: &mut sqlx::sqlite::SqliteConnection,
        query: &UsageQuery,
        meter_key: &str,
    ) -> Result<Vec<(i64, i64)>, KernelError> {
        let limit = i64::try_from(MAX_OPEN_INTERVALS + 1)
            .map_err(|_| KernelError::InvalidIdentifier("usage interval bound".into()))?;
        let rows = if let Some(resource_id) = &query.resource_id {
            sqlx::query(
                "SELECT quantity, started_at_ms FROM metering_intervals \
                 WHERE meter_key = ?1 AND scope = ?2 AND resource_id = ?3 \
                   AND ended_at_ms IS NULL AND started_at_ms < ?4 \
                 ORDER BY started_at_ms, resource_id LIMIT ?5",
            )
            .bind(meter_key)
            .bind(&query.scope)
            .bind(resource_id)
            .bind(query.end_ms)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await
            .map_err(store_err)?
        } else {
            sqlx::query(
                "SELECT quantity, started_at_ms FROM metering_intervals \
                 WHERE meter_key = ?1 AND scope = ?2 AND ended_at_ms IS NULL AND started_at_ms < ?3 \
                 ORDER BY started_at_ms, resource_id LIMIT ?4",
            )
            .bind(meter_key)
            .bind(&query.scope)
            .bind(query.end_ms)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await
            .map_err(store_err)?
        };
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("quantity").map_err(store_err)?,
                    row.try_get("started_at_ms").map_err(store_err)?,
                ))
            })
            .collect()
    }

    /// Runs the whole bounded read against one connection/transaction so
    /// aggregates and open intervals come from a single stable snapshot. A
    /// writer that closes an interval between the two reads can therefore never
    /// make it disappear from both results.
    async fn usage_snapshot(
        connection: &mut sqlx::sqlite::SqliteConnection,
        query: &UsageQuery,
    ) -> Result<MeterUsageReport, KernelError> {
        let authority = sqlx::query(
            "SELECT authority_started_at_ms, last_observed_at_ms FROM metering_authority WHERE id = 1",
        )
        .fetch_optional(&mut *connection)
        .await
        .map_err(store_err)?;
        let (authority_started_at_ms, last_observed_at_ms) = match authority {
            Some(row) => (
                Some(row.try_get("authority_started_at_ms").map_err(store_err)?),
                Some(row.try_get("last_observed_at_ms").map_err(store_err)?),
            ),
            None => (None, None),
        };
        let observed_through_ms = query.end_ms.min(query.evaluated_at_ms);
        let status = usage_status(
            authority_started_at_ms,
            query.start_ms,
            query.end_ms,
            query.evaluated_at_ms,
        );

        let mut meters = Vec::with_capacity(query.meter_keys.len());
        for meter_key in &query.meter_keys {
            let mut accumulator = UsageAccumulator::new();

            // `load_aggregates` enforces both the aggregate-row work bound and
            // the distinct-series cardinality bound.
            for (_resource_id, bucket_start_ms, quantity_millis) in
                Self::load_aggregates(connection, query, meter_key).await?
            {
                accumulator.add(query.granularity, bucket_start_ms, quantity_millis)?;
            }

            // Open intervals are never folded into aggregates, so their live
            // in-window segment cannot double count closed usage.
            let open = Self::load_open_intervals(connection, query, meter_key).await?;
            if open.len() > MAX_OPEN_INTERVALS {
                return Err(KernelError::InvalidIdentifier(
                    "usage interval bound".into(),
                ));
            }
            for (quantity, started_at_ms) in open {
                let segment_start = started_at_ms.max(query.start_ms);
                let segment_end = observed_through_ms.min(query.end_ms);
                if segment_end > segment_start {
                    let quantity = u64::try_from(quantity).map_err(|_| {
                        KernelError::MeteringCorrupt("metering quantity is negative".into())
                    })?;
                    for (bucket_start, contribution) in bucket_contributions(
                        segment_start,
                        segment_end,
                        quantity,
                        INGEST_BUCKET_WIDTH_MS,
                    )? {
                        accumulator.add(query.granularity, bucket_start, contribution)?;
                    }
                }
            }

            meters.push(meter_usage(meter_key.clone(), query, status, accumulator)?);
        }

        Ok(MeterUsageReport {
            scope: query.scope.clone(),
            start_ms: query.start_ms,
            end_ms: query.end_ms,
            observed_through_ms,
            authority_started_at_ms,
            last_observed_at_ms,
            meters,
        })
    }
}

#[async_trait]
impl MeteringRepository for SqliteStore {
    async fn ensure_authority(&self, now_ms: i64) -> Result<(), KernelError> {
        sqlx::query(
            "INSERT INTO metering_authority (id, authority_started_at_ms, last_observed_at_ms) \
             VALUES (1, ?1, ?1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn record_observation(&self, observation: &MeterObservation) -> Result<(), KernelError> {
        observation.validate()?;
        // `BEGIN IMMEDIATE` takes the write lock up front so the configured
        // `busy_timeout` applies (matching `sqlite/placement.rs`). sqlx's
        // `begin_with` still returns a `Transaction`, which rolls back on drop,
        // so a cancelled future can no longer leak an open write transaction
        // into the pool.
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        Self::record_observation_tx(&mut transaction, observation).await?;
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, KernelError> {
        query.validate()?;
        // sqlx's transaction rolls back on drop, so a cancelled future cannot
        // return a connection to the pool inside an open transaction. A sqlx
        // SQLite transaction is deferred by default, preserving the WAL read
        // snapshot for authority, aggregates and open intervals together.
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        let report = Self::usage_snapshot(&mut transaction, query).await?;
        transaction.commit().await.map_err(store_err)?;
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use o3k_kernel::MeteringRepository;

    const T0: i64 = 100 * INGEST_BUCKET_WIDTH_MS;

    fn observation(consuming: bool, at: i64) -> MeterObservation {
        MeterObservation {
            meter_key: "compute:instance_seconds".into(),
            scope: "project-a".into(),
            resource_id: "server-1".into(),
            quantity: if consuming { 1 } else { 0 },
            consuming,
            observed_at_ms: at,
            authority: "o3k-lifecycle".into(),
        }
    }

    /// `replay_or_corrupt` is only reached when a concurrent writer closes the
    /// interval between this transaction's load and its guarded update, which
    /// the public API cannot force. Call it directly to pin the semantics.
    #[tokio::test]
    async fn replay_or_corrupt_is_idempotent_for_any_closed_end_instant() {
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let open_observation = observation(true, T0);
        store.ensure_authority(T0).await.unwrap();
        store.record_observation(&open_observation).await.unwrap();
        store
            .record_observation(&observation(false, T0 + 60_000))
            .await
            .unwrap();

        let closed = OpenInterval {
            interval_id: interval_id(&open_observation),
            quantity: 1,
            started_at_ms: T0,
            authority: "o3k-lifecycle".into(),
        };
        let mut connection = store.pool.acquire().await.unwrap();
        // Already closed by the winner; the loser folded nothing and must not
        // be turned into an error just because its instant differs.
        SqliteStore::replay_or_corrupt(&mut connection, &closed)
            .await
            .unwrap();

        // A vanished interval is still corruption.
        let vanished = OpenInterval {
            interval_id: "missing-interval".into(),
            ..closed
        };
        assert!(matches!(
            SqliteStore::replay_or_corrupt(&mut connection, &vanished).await,
            Err(KernelError::MeteringCorrupt(_))
        ));
    }

    #[tokio::test]
    async fn dropped_write_transaction_rolls_back_and_keeps_the_pool_usable() {
        // In-memory store => a single pooled connection, so a write transaction
        // that leaked back into the pool would be observed immediately.
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        {
            let mut transaction = store.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            sqlx::query(
                "INSERT INTO metering_authority (id, authority_started_at_ms, last_observed_at_ms) \
                 VALUES (1, 5, 5) ON CONFLICT (id) DO NOTHING",
            )
            .execute(&mut *transaction)
            .await
            .unwrap();
            drop(transaction);
        }

        // The dropped transaction must have rolled back: the later anchor wins.
        store.ensure_authority(7).await.unwrap();
        let anchor: i64 = sqlx::query_scalar(
            "SELECT authority_started_at_ms FROM metering_authority WHERE id = 1",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(anchor, 7);
    }
}
