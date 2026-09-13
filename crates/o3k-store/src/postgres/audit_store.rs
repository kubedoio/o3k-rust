use super::PostgresStore;
use crate::{AuditEventRecord, AuditRepository, RepositoryPage, StoreError};
use async_trait::async_trait;
use o3k_kernel::AuditQuery;
use sqlx::Row;

pub(crate) fn row(r: &sqlx::postgres::PgRow) -> Result<AuditEventRecord, StoreError> {
    Ok(AuditEventRecord {
        event_id: r.try_get("event_id").map_err(StoreError::Database)?,
        timestamp: r.try_get("timestamp").map_err(StoreError::Database)?,
        request_id: r.try_get("request_id").map_err(StoreError::Database)?,
        audit_id: r.try_get("audit_id").map_err(StoreError::Database)?,
        principal_id: r.try_get("principal_id").map_err(StoreError::Database)?,
        principal_kind: r.try_get("principal_kind").map_err(StoreError::Database)?,
        effective_scope: r.try_get("effective_scope").map_err(StoreError::Database)?,
        service: r.try_get("service").map_err(StoreError::Database)?,
        action: r.try_get("action").map_err(StoreError::Database)?,
        resource_type: r.try_get("resource_type").map_err(StoreError::Database)?,
        resource_id: r.try_get("resource_id").map_err(StoreError::Database)?,
        owner_scope: r.try_get("owner_scope").map_err(StoreError::Database)?,
        operation_id: r.try_get("operation_id").map_err(StoreError::Database)?,
        outcome: r.try_get("outcome").map_err(StoreError::Database)?,
        reason_category: r.try_get("reason_category").map_err(StoreError::Database)?,
    })
}

/// Inserts one audit row within an existing transaction using the canonical
/// `audit_events` semantics: `ON CONFLICT(event_id) DO NOTHING`, and when a row
/// with the same `event_id` already exists it must be equivalent to the
/// requested one or the write fails with `StoreError::AuditEventConflict`.
///
/// Shared so every write path (the standalone `AuditRepository` and the
/// transaction-folded topology mutations) reuses the same column mapping and
/// can never drift.
pub(crate) async fn insert_audit_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    e: &AuditEventRecord,
) -> Result<(), StoreError> {
    let result = sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT(event_id) DO NOTHING")
        .bind(&e.event_id).bind(&e.timestamp).bind(&e.request_id).bind(&e.audit_id).bind(&e.principal_id).bind(&e.principal_kind).bind(&e.effective_scope).bind(&e.service).bind(&e.action).bind(&e.resource_type).bind(&e.resource_id).bind(&e.owner_scope).bind(&e.operation_id).bind(&e.outcome).bind(&e.reason_category).execute(&mut **tx).await.map_err(StoreError::Database)?;
    if result.rows_affected() == 0 {
        let existing = sqlx::query("SELECT * FROM audit_events WHERE event_id=$1")
            .bind(&e.event_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or_else(|| StoreError::Corrupt("audit conflict row disappeared".into()))?;
        if row(&existing)? != *e {
            return Err(StoreError::AuditEventConflict);
        }
    }
    Ok(())
}

#[async_trait]
impl AuditRepository for PostgresStore {
    async fn insert_audit_event(&self, e: &AuditEventRecord) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        insert_audit_event_tx(&mut tx, e).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }
    async fn get_audit_event(&self, scope: &str, id: &str) -> Result<AuditEventRecord, StoreError> {
        row(
            &sqlx::query("SELECT * FROM audit_events WHERE effective_scope=$1 AND event_id=$2")
                .bind(scope)
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?
                .ok_or(StoreError::ResourceNotFound)?,
        )
    }
    async fn list_audit_events_page(
        &self,
        scope: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<AuditEventRecord>, StoreError> {
        let n = crate::port::durable::bounded_fetch_limit(limit)?;
        let rs = sqlx::query("SELECT * FROM audit_events WHERE effective_scope=$1 AND ($2::text IS NULL OR event_id>$2) ORDER BY event_id LIMIT $3").bind(scope).bind(after).bind(n).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        let more = rs.len() > limit;
        let key = more.then(|| rs[limit - 1].get("event_id"));
        let items = rs
            .iter()
            .take(limit)
            .map(row)
            .collect::<Result<Vec<_>, _>>()?;
        RepositoryPage::new(items, more, key, limit)
    }

    async fn list_audit_events_page_query(
        &self,
        query: &AuditQuery,
    ) -> Result<RepositoryPage<AuditEventRecord>, StoreError> {
        let n = crate::port::durable::bounded_fetch_limit(query.limit)?;
        let mut sql = String::from("SELECT * FROM audit_events WHERE effective_scope = $1");
        let mut binds: Vec<&str> = vec![query.scope.id().as_str()];
        let mut index = 2;
        macro_rules! eq_filter {
            ($value:expr, $column:literal) => {
                if let Some(value) = $value.as_deref() {
                    sql.push_str(&format!(" AND {} = ${index}", $column));
                    binds.push(value);
                    index += 1;
                }
            };
        }
        if let Some(after) = query.after_event_id.as_deref() {
            sql.push_str(&format!(" AND event_id > ${index}"));
            binds.push(after);
            index += 1;
        }
        eq_filter!(query.event_id, "event_id");
        eq_filter!(query.service, "service");
        eq_filter!(query.action, "action");
        eq_filter!(query.outcome, "outcome");
        eq_filter!(query.resource_type, "resource_type");
        eq_filter!(query.resource_id, "resource_id");
        eq_filter!(query.operation_id, "operation_id");
        eq_filter!(query.principal_id, "principal_id");
        eq_filter!(query.request_id, "request_id");
        eq_filter!(query.audit_id, "audit_id");
        if let Some(from) = query.from_timestamp.as_deref() {
            sql.push_str(&format!(" AND timestamp >= ${index}"));
            binds.push(from);
            index += 1;
        }
        if let Some(until) = query.until_timestamp.as_deref() {
            sql.push_str(&format!(" AND timestamp <= ${index}"));
            binds.push(until);
            index += 1;
        }
        sql.push_str(&format!(" ORDER BY event_id LIMIT ${index}"));
        let mut statement = sqlx::query(&sql);
        for value in binds {
            statement = statement.bind(value);
        }
        let rows = statement
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let more = rows.len() > query.limit;
        let key = more.then(|| rows[query.limit - 1].get("event_id"));
        let items = rows
            .iter()
            .take(query.limit)
            .map(row)
            .collect::<Result<Vec<_>, _>>()?;
        RepositoryPage::new(items, more, key, query.limit)
    }
    async fn prune_audit_events_before(
        &self,
        cutoff: &str,
        limit: usize,
    ) -> Result<u64, StoreError> {
        let n = i64::try_from(limit)
            .ok()
            .filter(|n| (1..=200).contains(n))
            .ok_or_else(|| StoreError::Corrupt("audit prune batch out of bounds".into()))?;
        Ok(sqlx::query("DELETE FROM audit_events WHERE event_id IN (SELECT event_id FROM audit_events WHERE timestamp < $1 ORDER BY timestamp,event_id LIMIT $2)").bind(cutoff).bind(n).execute(&self.pool).await.map_err(StoreError::Database)?.rows_affected())
    }
}
