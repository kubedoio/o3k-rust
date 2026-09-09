use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

use crate::{AuditEventFilters, AuditEventRecord, AuditRepository, SqliteStore, StoreError};

fn record(row: &sqlx::sqlite::SqliteRow) -> Result<AuditEventRecord, StoreError> {
    Ok(AuditEventRecord {
        event_id: row.get("event_id"),
        timestamp: row.get("timestamp"),
        request_id: row.get("request_id"),
        audit_id: row.get("audit_id"),
        principal_id: row.get("principal_id"),
        effective_scope: row.get("effective_scope"),
        service_namespace: row.get("service_namespace"),
        action: row.get("action"),
        resource_type: row.get("resource_type"),
        resource_id: row.get("resource_id"),
        owner_scope: row.get("owner_scope"),
        operation_id: row
            .get::<Option<String>, _>("operation_id")
            .map(|v| Uuid::parse_str(&v))
            .transpose()
            .map_err(StoreError::InvalidUuid)?,
        outcome: row.get("outcome"),
        reason_category: row.get("reason_category"),
        event_json: row.get("event_json"),
    })
}

#[async_trait]
impl AuditRepository for SqliteStore {
    async fn append_audit_event(&self, event: &AuditEventRecord) -> Result<(), StoreError> {
        let result = sqlx::query("INSERT OR IGNORE INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&event.event_id).bind(&event.timestamp).bind(&event.request_id).bind(&event.audit_id)
            .bind(&event.principal_id).bind(&event.effective_scope).bind(&event.service_namespace).bind(&event.action)
            .bind(&event.resource_type).bind(&event.resource_id).bind(&event.owner_scope)
            .bind(event.operation_id.map(|v| v.to_string())).bind(&event.outcome).bind(&event.reason_category).bind(&event.event_json)
            .execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            let existing = sqlx::query("SELECT * FROM audit_events WHERE event_id = ?")
                .bind(&event.event_id)
                .fetch_one(&self.pool)
                .await
                .map_err(StoreError::Database)
                .and_then(|row| record(&row))?;
            if existing != *event {
                return Err(StoreError::IdempotencyConflict);
            }
        }
        Ok(())
    }
    async fn prune_audit_events_before(
        &self,
        timestamp: &str,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("audit retention limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "audit retention limit outside 1..=1000".into(),
            ));
        }
        let result = sqlx::query(
            // Retention is time based.  Ordering only by event_id (which is
            // opaque and may not be time ordered for imported/replayed
            // events) can retain older rows while deleting newer rows.
            "DELETE FROM audit_events WHERE event_id IN (SELECT event_id FROM audit_events WHERE timestamp < ? ORDER BY timestamp ASC, event_id ASC LIMIT ?)",
        )
        .bind(timestamp)
        .bind(limit)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        usize::try_from(result.rows_affected())
            .map_err(|_| StoreError::Corrupt("audit retention count overflow".into()))
    }
    async fn list_audit_events_page(
        &self,
        scope: &str,
        after: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("audit page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "audit page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query("SELECT * FROM audit_events WHERE effective_scope = ? AND (? IS NULL OR event_id > ?) AND (? IS NULL OR event_id = ?) AND (? IS NULL OR timestamp >= ?) AND (? IS NULL OR timestamp <= ?) AND (? IS NULL OR principal_id = ?) AND (? IS NULL OR service_namespace = ?) AND (? IS NULL OR action = ?) AND (? IS NULL OR outcome = ?) AND (? IS NULL OR resource_type = ?) AND (? IS NULL OR resource_id = ?) AND (? IS NULL OR operation_id = ?) AND (? IS NULL OR request_id = ?) AND (? IS NULL OR audit_id = ?) ORDER BY event_id ASC LIMIT ?")
            .bind(scope).bind(after).bind(after)
            .bind(&filters.event_id).bind(&filters.event_id)
            .bind(&filters.timestamp_from).bind(&filters.timestamp_from)
            .bind(&filters.timestamp_to).bind(&filters.timestamp_to)
            .bind(&filters.principal_id).bind(&filters.principal_id)
            .bind(&filters.service_namespace).bind(&filters.service_namespace)
            .bind(&filters.action).bind(&filters.action)
            .bind(&filters.outcome).bind(&filters.outcome)
            .bind(&filters.resource_type).bind(&filters.resource_type)
            .bind(&filters.resource_id).bind(&filters.resource_id)
            .bind(&filters.operation_id).bind(&filters.operation_id)
            .bind(&filters.request_id).bind(&filters.request_id)
            .bind(&filters.audit_id).bind(&filters.audit_id)
            .bind(limit).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(record).collect()
    }
    async fn list_audit_events_system_page(
        &self,
        scope: Option<&str>,
        after: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("audit page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "audit page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query("SELECT * FROM audit_events WHERE (? IS NULL OR effective_scope = ?) AND (? IS NULL OR event_id > ?) AND (? IS NULL OR event_id = ?) AND (? IS NULL OR timestamp >= ?) AND (? IS NULL OR timestamp <= ?) AND (? IS NULL OR principal_id = ?) AND (? IS NULL OR service_namespace = ?) AND (? IS NULL OR action = ?) AND (? IS NULL OR outcome = ?) AND (? IS NULL OR resource_type = ?) AND (? IS NULL OR resource_id = ?) AND (? IS NULL OR operation_id = ?) AND (? IS NULL OR request_id = ?) AND (? IS NULL OR audit_id = ?) ORDER BY event_id ASC LIMIT ?")
            .bind(scope).bind(scope).bind(after).bind(after)
            .bind(&filters.event_id).bind(&filters.event_id)
            .bind(&filters.timestamp_from).bind(&filters.timestamp_from)
            .bind(&filters.timestamp_to).bind(&filters.timestamp_to)
            .bind(&filters.principal_id).bind(&filters.principal_id)
            .bind(&filters.service_namespace).bind(&filters.service_namespace)
            .bind(&filters.action).bind(&filters.action)
            .bind(&filters.outcome).bind(&filters.outcome)
            .bind(&filters.resource_type).bind(&filters.resource_type)
            .bind(&filters.resource_id).bind(&filters.resource_id)
            .bind(&filters.operation_id).bind(&filters.operation_id)
            .bind(&filters.request_id).bind(&filters.request_id)
            .bind(&filters.audit_id).bind(&filters.audit_id)
            .bind(limit).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(record).collect()
    }
    async fn get_audit_event(
        &self,
        scope: &str,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError> {
        sqlx::query("SELECT * FROM audit_events WHERE effective_scope = ? AND event_id = ?")
            .bind(scope)
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .as_ref()
            .map(record)
            .transpose()
    }
    async fn get_audit_event_system(
        &self,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError> {
        sqlx::query("SELECT * FROM audit_events WHERE event_id = ?")
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .as_ref()
            .map(record)
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, scope: &str) -> AuditEventRecord {
        AuditEventRecord {
            event_id: id.into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            request_id: "req".into(),
            audit_id: "audit".into(),
            principal_id: "principal".into(),
            effective_scope: scope.into(),
            service_namespace: "compute".into(),
            action: "server:list".into(),
            resource_type: None,
            resource_id: None,
            owner_scope: None,
            operation_id: None,
            outcome: "succeeded".into(),
            reason_category: None,
            event_json: "{}".into(),
        }
    }

    #[tokio::test]
    async fn audit_pages_are_scope_bound_and_bounded() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        store
            .append_audit_event(&event("0001", "project-a"))
            .await?;
        store
            .append_audit_event(&event("0001", "project-a"))
            .await?;
        store
            .append_audit_event(&event("0002", "project-a"))
            .await?;
        store
            .append_audit_event(&event("0003", "project-b"))
            .await?;
        assert_eq!(
            store
                .list_audit_events_page("project-a", None, 1, &AuditEventFilters::default())
                .await?
                .len(),
            1
        );
        assert_eq!(
            store
                .list_audit_events_page(
                    "project-a",
                    Some("0001"),
                    10,
                    &AuditEventFilters::default()
                )
                .await?[0]
                .event_id,
            "0002"
        );
        assert_eq!(
            store
                .list_audit_events_page(
                    "project-a",
                    None,
                    10,
                    &AuditEventFilters {
                        service_namespace: Some("compute".into()),
                        action: Some("server:list".into()),
                        outcome: Some("succeeded".into()),
                        ..AuditEventFilters::default()
                    },
                )
                .await?
                .len(),
            2
        );
        assert!(
            store
                .list_audit_events_page("project-a", None, 1001, &AuditEventFilters::default())
                .await
                .is_err()
        );
        assert!(store.get_audit_event("project-a", "0003").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn audit_retention_is_bounded_and_does_not_cross_cutoff() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let mut old_a = event("0001", "project-a");
        old_a.timestamp = "2025-12-31T23:59:59Z".into();
        let mut old_b = event("0002", "project-b");
        old_b.timestamp = "2025-12-31T23:59:58Z".into();
        let mut current = event("0003", "project-a");
        current.timestamp = "2026-01-01T00:00:00Z".into();
        store.append_audit_event(&old_a).await?;
        store.append_audit_event(&old_b).await?;
        store.append_audit_event(&current).await?;

        assert_eq!(
            store
                .prune_audit_events_before("2026-01-01T00:00:00Z", 1)
                .await?,
            1
        );
        assert!(store.get_audit_event_system("0001").await?.is_some());
        // 0002 has the oldest timestamp and is therefore the first retention
        // victim, regardless of its opaque event-id ordering.
        assert!(store.get_audit_event_system("0002").await?.is_none());
        assert!(store.get_audit_event_system("0003").await?.is_some());
        assert!(
            store
                .prune_audit_events_before("2026-01-01T00:00:00Z", 1001)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .prune_audit_events_before("2026-01-01T00:00:00Z", 100)
                .await?,
            1
        );
        assert!(store.get_audit_event_system("0001").await?.is_none());
        assert!(store.get_audit_event_system("0003").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn audit_retention_removes_oldest_timestamp_not_event_id() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        // Deliberately make the event-id order disagree with event time.  This
        // models imported/replayed events and proves retention is chronological.
        let mut older = event("zzzz", "project-a");
        older.timestamp = "2025-01-01T00:00:00Z".into();
        let mut newer = event("0000", "project-a");
        newer.timestamp = "2025-06-01T00:00:00Z".into();
        store.append_audit_event(&older).await?;
        store.append_audit_event(&newer).await?;

        assert_eq!(
            store
                .prune_audit_events_before("2026-01-01T00:00:00Z", 1)
                .await?,
            1
        );
        assert!(store.get_audit_event_system("zzzz").await?.is_none());
        assert!(store.get_audit_event_system("0000").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn audit_event_id_reuse_with_different_payload_is_rejected() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let first = event("same-id", "project-a");
        store.append_audit_event(&first).await?;
        let mut conflicting = first.clone();
        conflicting.action = "compute:server:delete".into();
        assert!(matches!(
            store.append_audit_event(&conflicting).await,
            Err(StoreError::IdempotencyConflict)
        ));
        Ok(())
    }
}
