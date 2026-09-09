use async_trait::async_trait;
use sqlx::Row;

use crate::{MeteringAggregate, MeteringEventRecord, MeteringRepository, SqliteStore, StoreError};

pub(crate) async fn append_metering_event_tx(
    tx: &mut sqlx::SqliteConnection,
    event: &MeteringEventRecord,
) -> Result<(), StoreError> {
    let quantity = i64::try_from(event.quantity)
        .map_err(|_| StoreError::Corrupt("meter quantity overflow".into()))?;
    let result = sqlx::query(
            "INSERT OR IGNORE INTO metering_events
             (event_id,project_id,meter_id,resource_id,quantity,unit,effective_at,recorded_at,source,payload_fingerprint)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
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
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
    if result.rows_affected() == 0 {
        let row = sqlx::query("SELECT project_id,meter_id,resource_id,quantity,unit,effective_at,recorded_at,source,payload_fingerprint FROM metering_events WHERE event_id = ?")
                .bind(event.event_id.to_string()).fetch_one(&mut *tx).await
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
impl MeteringRepository for SqliteStore {
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
        // Read at most `limit` rows for the aggregate and one additional row
        // to determine completeness.  Never run an unbounded COUNT over the
        // tenant's entire history merely to set the completeness bit.
        let probe_limit = limit.saturating_add(1);
        let row = sqlx::query(
            "SELECT COALESCE(SUM(quantity),0) AS total_quantity, COUNT(*) AS event_count,
                    MIN(unit) AS unit, MAX(unit) AS max_unit
             FROM (SELECT quantity,unit FROM metering_events
                   WHERE project_id = ? AND meter_id = ? AND effective_at >= ? AND effective_at < ?
                   ORDER BY effective_at,event_id LIMIT ?)",
        )
        .bind(project_id)
        .bind(meter_id)
        .bind(effective_from)
        .bind(effective_to)
        .bind(limit)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        let total_row = sqlx::query(
            "SELECT COUNT(*) AS total_event_count FROM
             (SELECT event_id FROM metering_events
              WHERE project_id = ? AND meter_id = ? AND effective_at >= ? AND effective_at < ?
              ORDER BY effective_at,event_id LIMIT ?) bounded_count",
        )
        .bind(project_id)
        .bind(meter_id)
        .bind(effective_from)
        .bind(effective_to)
        .bind(probe_limit)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        let total = row.get::<i64, _>("total_quantity");
        let unit = row
            .get::<Option<String>, _>("unit")
            .unwrap_or_else(|| "count".to_owned());
        if row.get::<Option<String>, _>("max_unit").as_deref() != Some(unit.as_str()) {
            // A meter is a typed contract: summing observations expressed in
            // different units would manufacture authoritative usage.
            return Err(StoreError::Corrupt(
                "meter aggregate has mixed units".into(),
            ));
        }
        Ok(MeteringAggregate {
            project_id: project_id.to_owned(),
            meter_id: meter_id.to_owned(),
            unit,
            total_quantity: u64::try_from(total)
                .map_err(|_| StoreError::Corrupt("negative meter aggregate".into()))?,
            event_count: u64::try_from(row.get::<i64, _>("event_count"))
                .map_err(|_| StoreError::Corrupt("negative meter event count".into()))?,
            complete: total_row.get::<i64, _>("total_event_count") <= limit,
            effective_from: effective_from.to_owned(),
            effective_to: effective_to.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurableStore, OperationRecord, OperationState, ResourceRecord};
    use uuid::Uuid;

    fn event(id: Uuid, quantity: u64, fingerprint: &str) -> MeteringEventRecord {
        MeteringEventRecord {
            event_id: id,
            project_id: "project-a".into(),
            meter_id: "compute:server_count".into(),
            resource_id: None,
            quantity,
            unit: "count".into(),
            effective_at: "2026-01-01T00:00:00Z".into(),
            recorded_at: "2026-01-01T00:00:01Z".into(),
            source: "o3k-resource-lifecycle".into(),
            payload_fingerprint: fingerprint.into(),
        }
    }

    #[tokio::test]
    async fn event_replay_is_idempotent_and_conflict_is_rejected() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let id = Uuid::new_v4();
        let first = event(id, 1, "a");
        store.append_metering_event(&first).await?;
        store.append_metering_event(&first).await?;
        let mut conflicting = first.clone();
        conflicting.quantity = 2;
        assert!(matches!(
            store.append_metering_event(&conflicting).await,
            Err(StoreError::IdempotencyConflict)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_is_project_scoped_and_limit_bounded() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        store
            .append_metering_event(&event(Uuid::new_v4(), 2, "a"))
            .await?;
        let mut other = event(Uuid::new_v4(), 99, "b");
        other.project_id = "project-b".into();
        store.append_metering_event(&other).await?;
        let aggregate = store
            .aggregate_metering_events(
                "project-a",
                "compute:server_count",
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                10,
            )
            .await?;
        assert_eq!(aggregate.total_quantity, 2);
        assert_eq!(aggregate.event_count, 1);
        assert!(
            store
                .aggregate_metering_events(
                    "project-a",
                    "compute:server_count",
                    "2026-01-02T00:00:00Z",
                    "2026-01-01T00:00:00Z",
                    10,
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn bounded_probe_marks_partial_without_scanning_full_history() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        store
            .append_metering_event(&event(Uuid::new_v4(), 2, "a"))
            .await?;
        store
            .append_metering_event(&event(Uuid::new_v4(), 3, "b"))
            .await?;
        let aggregate = store
            .aggregate_metering_events(
                "project-a",
                "compute:server_count",
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                1,
            )
            .await?;
        assert_eq!(aggregate.event_count, 1);
        assert!(!aggregate.complete);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_rejects_mixed_units_in_one_meter() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        store
            .append_metering_event(&event(Uuid::new_v4(), 1, "a"))
            .await?;
        let mut incompatible = event(Uuid::new_v4(), 1, "b");
        incompatible.unit = "bytes".into();
        store.append_metering_event(&incompatible).await?;
        assert!(matches!(
            store.aggregate_metering_events(
                "project-a", "compute:server_count",
                "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z", 10
            ).await,
            Err(StoreError::Corrupt(message)) if message.contains("mixed units")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn placement_backed_create_records_one_atomic_lifecycle_event() -> Result<(), StoreError>
    {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let resource_id = Uuid::new_v4();
        let resource = ResourceRecord {
            id: resource_id,
            kind: "compute:server".into(),
            project_id: "project-a".into(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".into(),
            observed_state: "unknown".into(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: Uuid::new_v4(),
            resource_id,
            kind: "create".into(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        store
            .insert_resource_and_operation(&resource, &operation, None)
            .await?;

        let aggregate = store
            .aggregate_metering_events(
                "project-a",
                "compute:server:lifecycle_created",
                "2026-01-01T00:00:00Z",
                "9999-01-01T00:00:00Z",
                10,
            )
            .await?;
        assert_eq!(aggregate.total_quantity, 1);
        assert_eq!(aggregate.event_count, 1);
        assert!(aggregate.complete);

        // The event identity is deterministic across reconciliation/restart;
        // replaying the same observation cannot double-count it.
        let event = MeteringEventRecord::resource_lifecycle(&resource, "created");
        store.append_metering_event(&event).await?;
        let replay = store
            .aggregate_metering_events(
                "project-a",
                "compute:server:lifecycle_created",
                "2026-01-01T00:00:00Z",
                "9999-01-01T00:00:00Z",
                10,
            )
            .await?;
        assert_eq!(replay.total_quantity, 1);
        assert_eq!(replay.event_count, 1);
        Ok(())
    }
}
