use async_trait::async_trait;
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditQuery, DurableAuditPage, DurableAuditRepository,
    EventId, OwnershipScope, PrincipalId, PrincipalKind, ResourceId, ResourceType, ScopeId,
    ServiceNamespace,
};

use super::O3kStore;
use crate::AuditEventRecord;
use crate::port::service_repos::AuditRepository;

fn err<E: std::fmt::Display>(e: E) -> o3k_kernel::KernelError {
    o3k_kernel::KernelError::AuditUnavailable(e.to_string())
}

type AuditRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

fn row_event(r: AuditRow) -> Result<AuditEvent, o3k_kernel::KernelError> {
    let (_, action_name) =
        r.8.split_once(':')
            .ok_or_else(|| err("invalid audit action"))?;
    let service = ServiceNamespace::new(r.7).map_err(err)?;
    let action = ActionId::new(service.as_str(), action_name).map_err(err)?;
    let scope = OwnershipScope::project(ScopeId::new_unchecked(r.6), None, None);
    let resource_type =
        r.9.as_deref()
            .and_then(|v| v.split_once(':'))
            .map(|(ns, n)| ResourceType::new_unchecked(ns, n));
    Ok(AuditEvent {
        event_id: EventId::from_string(r.0),
        timestamp: r.1,
        request_id: r.2,
        audit_id: r.3,
        principal_id: PrincipalId::new_unchecked(r.4),
        principal_kind: if r.5.to_lowercase().contains("service") {
            PrincipalKind::Service
        } else {
            PrincipalKind::User
        },
        effective_scope: scope,
        service_namespace: service,
        action,
        resource_type,
        resource_id: r.10.map(ResourceId::new_unchecked),
        owner_scope: r
            .11
            .map(|v| OwnershipScope::project(ScopeId::new_unchecked(v), None, None)),
        authorization_decision: None,
        operation_id: r.12.and_then(|v| uuid::Uuid::parse_str(&v).ok()),
        outcome: match r.13.as_str() {
            "allowed" => AuditOutcome::Allowed,
            "denied" => AuditOutcome::Denied,
            "failed" => AuditOutcome::Failed,
            "unknown_outcome" => AuditOutcome::UnknownOutcome,
            _ => AuditOutcome::Succeeded,
        },
        reason_category: r.14,
        service_principal: None,
    })
}

#[async_trait]
impl DurableAuditRepository for O3kStore {
    async fn append(&self, event: &AuditEvent) -> Result<(), o3k_kernel::KernelError> {
        let record = AuditEventRecord::from_kernel_event(event);
        let result = match self {
            Self::Sqlite(s) => s.insert_audit_event(&record).await,
            Self::Postgres(s) => s.insert_audit_event(&record).await,
        };
        result.map_err(|error| {
            if matches!(error, crate::StoreError::AuditEventConflict) {
                o3k_kernel::KernelError::AuditConflict
            } else {
                err(error)
            }
        })
    }

    async fn page(&self, query: &AuditQuery) -> Result<DurableAuditPage, o3k_kernel::KernelError> {
        query.validate()?;
        let scope = query.scope.id().as_str();
        let n = (query.limit + 1) as i64;
        let rows: Vec<AuditRow> = match self {
            Self::Sqlite(s) => {
                let mut sql = String::from(
                    "SELECT event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category FROM audit_events WHERE effective_scope = ?",
                );
                let mut binds: Vec<&str> = vec![scope];
                macro_rules! f {
                    ($v:expr, $c:literal) => {
                        if let Some(v) = $v.as_deref() {
                            sql.push_str(concat!(" AND ", $c, " = ?"));
                            binds.push(v);
                        }
                    };
                }
                if let Some(v) = query.after_event_id.as_deref() {
                    sql.push_str(" AND event_id > ?");
                    binds.push(v);
                }
                f!(query.service, "service");
                f!(query.action, "action");
                f!(query.outcome, "outcome");
                f!(query.resource_type, "resource_type");
                f!(query.resource_id, "resource_id");
                f!(query.operation_id, "operation_id");
                f!(query.principal_id, "principal_id");
                f!(query.request_id, "request_id");
                f!(query.audit_id, "audit_id");
                if let Some(v) = query.from_timestamp.as_deref() {
                    sql.push_str(" AND timestamp >= ?");
                    binds.push(v);
                }
                if let Some(v) = query.until_timestamp.as_deref() {
                    sql.push_str(" AND timestamp <= ?");
                    binds.push(v);
                }
                sql.push_str(" ORDER BY event_id LIMIT ?");
                let mut q = sqlx::query_as::<_, AuditRow>(&sql);
                for v in binds {
                    q = q.bind(v);
                }
                q.bind(n).fetch_all(&s.pool).await.map_err(err)?
            }
            Self::Postgres(s) => {
                let mut sql = String::from(
                    "SELECT event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category FROM audit_events WHERE effective_scope = $1",
                );
                let mut binds: Vec<&str> = vec![scope];
                let mut i = 2;
                macro_rules! f {
                    ($v:expr, $c:literal) => {
                        if let Some(v) = $v.as_deref() {
                            sql.push_str(&format!(" AND {} = ${i}", $c));
                            binds.push(v);
                            i += 1;
                        }
                    };
                }
                if let Some(v) = query.after_event_id.as_deref() {
                    sql.push_str(&format!(" AND event_id > ${i}"));
                    binds.push(v);
                    i += 1;
                }
                f!(query.service, "service");
                f!(query.action, "action");
                f!(query.outcome, "outcome");
                f!(query.resource_type, "resource_type");
                f!(query.resource_id, "resource_id");
                f!(query.operation_id, "operation_id");
                f!(query.principal_id, "principal_id");
                f!(query.request_id, "request_id");
                f!(query.audit_id, "audit_id");
                if let Some(v) = query.from_timestamp.as_deref() {
                    sql.push_str(&format!(" AND timestamp >= ${i}"));
                    binds.push(v);
                    i += 1;
                }
                if let Some(v) = query.until_timestamp.as_deref() {
                    sql.push_str(&format!(" AND timestamp <= ${i}"));
                    binds.push(v);
                    i += 1;
                }
                sql.push_str(&format!(" ORDER BY event_id LIMIT ${i}"));
                let mut q = sqlx::query_as::<_, AuditRow>(&sql);
                for v in binds {
                    q = q.bind(v);
                }
                q.bind(n).fetch_all(&s.pool).await.map_err(err)?
            }
        };
        let has_more = rows.len() > query.limit;
        let continuation_key = has_more.then(|| rows[query.limit - 1].0.clone());
        let events = rows
            .into_iter()
            .take(query.limit)
            .map(row_event)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DurableAuditPage {
            events,
            has_more,
            continuation_key,
        })
    }

    async fn prune_before(&self, cutoff: &str) -> Result<u64, o3k_kernel::KernelError> {
        // Retention is deliberately batch-bounded; callers can repeat this
        // privileged operation until the horizon is reached.
        const BATCH: i64 = 200;
        Ok(match self {
            Self::Sqlite(s) => sqlx::query("DELETE FROM audit_events WHERE event_id IN (SELECT event_id FROM audit_events WHERE timestamp < ? ORDER BY timestamp,event_id LIMIT ?)")
                .bind(cutoff)
                .bind(BATCH)
                .execute(&s.pool)
                .await
                .map_err(err)?
                .rows_affected(),
            Self::Postgres(s) => sqlx::query("DELETE FROM audit_events WHERE event_id IN (SELECT event_id FROM audit_events WHERE timestamp < $1 ORDER BY timestamp,event_id LIMIT $2)")
                .bind(cutoff)
                .bind(BATCH)
                .execute(&s.pool)
                .await
                .map_err(err)?
                .rows_affected(),
        })
    }
}
