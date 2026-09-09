use async_trait::async_trait;
use sqlx::Row;

use crate::{
    MeteringAggregate, MeteringEventRecord, MeteringRepository, PostgresStore, StoreError,
};

pub(crate) async fn append_metering_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &MeteringEventRecord,
) -> Result<(), StoreError> {
    let quantity = i64::try_from(event.quantity)
        .map_err(|_| StoreError::Corrupt("meter quantity overflow".into()))?;
    let result = sqlx::query(
            "INSERT INTO metering_events
             (event_id,project_id,meter_id,resource_id,quantity,unit,effective_at,recorded_at,source,payload_fingerprint)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event.event_id.to_string())
        .bind(&event.project_id)
        .bind(&event.meter_id)
        .bind(event.resource_id.map(|id| id.to_string()))
        .bind(quantity)
        .bind(&event.unit)
        .bind(&event.effective_at)
        .bind(&event.recorded_at)
        .bind(&event.source)
        .bind(&event.payload_fingerprint)
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Database)?;
    if result.rows_affected() == 0 {
        let row = sqlx::query("SELECT project_id,meter_id,resource_id,quantity,unit,effective_at,recorded_at,source,payload_fingerprint FROM metering_events WHERE event_id = $1")
                .bind(event.event_id.to_string()).fetch_one(&mut **tx).await
                .map_err(StoreError::Database)?;
        let same = row.get::<String, _>("project_id") == event.project_id
            && row.get::<String, _>("meter_id") == event.meter_id
            && row.get::<Option<String>, _>("resource_id")
                == event.resource_id.map(|id| id.to_string())
            && row.get::<i64, _>("quantity") == quantity
            && row.get::<String, _>("unit") == event.unit
            // Lifecycle retries may be reconstructed after restart and thus
            // receive a fresh observation timestamp.  The deterministic
            // payload fingerprint is the event identity; timestamps are
            // metadata and must not turn an equivalent replay into a
            // duplicate/conflict.
            && row.get::<String, _>("source") == event.source
            && row.get::<String, _>("payload_fingerprint") == event.payload_fingerprint;
        if !same {
            return Err(StoreError::IdempotencyConflict);
        }
    }
    Ok(())
}

#[async_trait]
impl MeteringRepository for PostgresStore {
    async fn append_metering_event(&self, event: &MeteringEventRecord) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        append_metering_event_tx(&mut tx, event).await?;
        tx.commit().await.map_err(StoreError::Database)
    }

    async fn aggregate_metering_events(
        &self,
        project_id: &str,
        meter_id: &str,
        effective_from: &str,
        effective_to: &str,
        limit: usize,
    ) -> Result<MeteringAggregate, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("meter event limit overflow".into()))?;
        if !(1..=100_000).contains(&limit) || effective_from >= effective_to {
            return Err(StoreError::Corrupt("invalid bounded meter query".into()));
        }
        // Probe only one row beyond the requested bound; completeness must
        // not require an unbounded tenant-history COUNT.
        let probe_limit = limit.saturating_add(1);
        let row = sqlx::query(
            "SELECT COALESCE(SUM(quantity),0) AS total_quantity, COUNT(*) AS event_count, MIN(unit) AS unit, MAX(unit) AS max_unit
             FROM (SELECT quantity,unit FROM metering_events
                   WHERE project_id = $1 AND meter_id = $2 AND effective_at >= $3 AND effective_at < $4
                   ORDER BY effective_at,event_id LIMIT $5) bounded",
        )
        .bind(project_id).bind(meter_id).bind(effective_from).bind(effective_to).bind(limit)
        .fetch_one(&self.pool).await.map_err(StoreError::Database)?;
        let total_row = sqlx::query(
            "SELECT COUNT(*) AS total_event_count FROM
             (SELECT event_id FROM metering_events
              WHERE project_id = $1 AND meter_id = $2 AND effective_at >= $3 AND effective_at < $4
              ORDER BY effective_at,event_id LIMIT $5) bounded_count",
        )
        .bind(project_id)
        .bind(meter_id)
        .bind(effective_from)
        .bind(effective_to)
        .bind(probe_limit)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        let unit = row
            .get::<Option<String>, _>("unit")
            .unwrap_or_else(|| "count".to_owned());
        if row.get::<Option<String>, _>("max_unit").as_deref() != Some(unit.as_str()) {
            return Err(StoreError::Corrupt(
                "meter aggregate has mixed units".into(),
            ));
        }
        Ok(MeteringAggregate {
            project_id: project_id.to_owned(),
            meter_id: meter_id.to_owned(),
            unit,
            total_quantity: u64::try_from(row.get::<i64, _>("total_quantity"))
                .map_err(|_| StoreError::Corrupt("negative meter aggregate".into()))?,
            event_count: u64::try_from(row.get::<i64, _>("event_count"))
                .map_err(|_| StoreError::Corrupt("negative meter event count".into()))?,
            complete: total_row.get::<i64, _>("total_event_count") <= limit,
            effective_from: effective_from.to_owned(),
            effective_to: effective_to.to_owned(),
        })
    }
}
