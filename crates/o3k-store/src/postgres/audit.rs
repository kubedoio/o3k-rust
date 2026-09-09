use crate::{AuditEventFilters, AuditEventRecord, AuditRepository, PostgresStore, StoreError};
use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

fn record(row: &sqlx::postgres::PgRow) -> Result<AuditEventRecord, StoreError> {
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
impl AuditRepository for PostgresStore {
    async fn append_audit_event(&self, event: &AuditEventRecord) -> Result<(), StoreError> {
        let result = sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT (event_id) DO NOTHING")
            .bind(&event.event_id).bind(&event.timestamp).bind(&event.request_id).bind(&event.audit_id).bind(&event.principal_id).bind(&event.effective_scope).bind(&event.service_namespace).bind(&event.action).bind(&event.resource_type).bind(&event.resource_id).bind(&event.owner_scope).bind(event.operation_id.map(|v| v.to_string())).bind(&event.outcome).bind(&event.reason_category).bind(&event.event_json).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            let existing = sqlx::query("SELECT * FROM audit_events WHERE event_id = $1")
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
            // Retention is time based.  event_id is opaque and is not a safe
            // ordering key for imported/replayed audit events.
            "WITH victims AS (SELECT event_id FROM audit_events WHERE timestamp < $1 ORDER BY timestamp ASC, event_id ASC LIMIT $2) DELETE FROM audit_events WHERE event_id IN (SELECT event_id FROM victims)",
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
        let rows = sqlx::query("SELECT * FROM audit_events WHERE effective_scope = $1 AND ($2::text IS NULL OR event_id > $2) AND ($3::text IS NULL OR event_id = $3) AND ($4::text IS NULL OR timestamp >= $4) AND ($5::text IS NULL OR timestamp <= $5) AND ($6::text IS NULL OR principal_id = $6) AND ($7::text IS NULL OR service_namespace = $7) AND ($8::text IS NULL OR action = $8) AND ($9::text IS NULL OR outcome = $9) AND ($10::text IS NULL OR resource_type = $10) AND ($11::text IS NULL OR resource_id = $11) AND ($12::text IS NULL OR operation_id = $12) AND ($13::text IS NULL OR request_id = $13) AND ($14::text IS NULL OR audit_id = $14) ORDER BY event_id ASC LIMIT $15")
            .bind(scope).bind(after)
            .bind(&filters.event_id)
            .bind(&filters.timestamp_from).bind(&filters.timestamp_to)
            .bind(&filters.principal_id).bind(&filters.service_namespace)
            .bind(&filters.action).bind(&filters.outcome)
            .bind(&filters.resource_type).bind(&filters.resource_id)
            .bind(&filters.operation_id).bind(&filters.request_id)
            .bind(&filters.audit_id).bind(limit)
            .fetch_all(&self.pool).await.map_err(StoreError::Database)?;
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
        let rows = sqlx::query("SELECT * FROM audit_events WHERE ($1::text IS NULL OR effective_scope = $1) AND ($2::text IS NULL OR event_id > $2) AND ($3::text IS NULL OR event_id = $3) AND ($4::text IS NULL OR timestamp >= $4) AND ($5::text IS NULL OR timestamp <= $5) AND ($6::text IS NULL OR principal_id = $6) AND ($7::text IS NULL OR service_namespace = $7) AND ($8::text IS NULL OR action = $8) AND ($9::text IS NULL OR outcome = $9) AND ($10::text IS NULL OR resource_type = $10) AND ($11::text IS NULL OR resource_id = $11) AND ($12::text IS NULL OR operation_id = $12) AND ($13::text IS NULL OR request_id = $13) AND ($14::text IS NULL OR audit_id = $14) ORDER BY event_id ASC LIMIT $15")
            .bind(scope).bind(after).bind(&filters.event_id)
            .bind(&filters.timestamp_from).bind(&filters.timestamp_to)
            .bind(&filters.principal_id).bind(&filters.service_namespace)
            .bind(&filters.action).bind(&filters.outcome)
            .bind(&filters.resource_type).bind(&filters.resource_id)
            .bind(&filters.operation_id).bind(&filters.request_id)
            .bind(&filters.audit_id).bind(limit)
            .fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(record).collect()
    }
    async fn get_audit_event(
        &self,
        scope: &str,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError> {
        sqlx::query("SELECT * FROM audit_events WHERE effective_scope = $1 AND event_id = $2")
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
        sqlx::query("SELECT * FROM audit_events WHERE event_id = $1")
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .as_ref()
            .map(record)
            .transpose()
    }
}
