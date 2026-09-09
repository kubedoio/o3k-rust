use async_trait::async_trait;
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditQuery, DurableAuditPage, DurableAuditRepository,
    EventId, OwnershipScope, PrincipalId, PrincipalKind, ResourceId, ResourceType, ScopeId,
    ServiceNamespace,
};

use super::O3kStore;

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
        let scope = event.effective_scope.id().as_str();
        let values = (
            event.event_id.as_str(),
            event.timestamp.as_str(),
            event.request_id.as_str(),
            event.audit_id.as_str(),
            event.principal_id.to_string(),
            format!("{:?}", event.principal_kind),
            scope,
            event.service_namespace.to_string(),
            event.action.to_string(),
            event.resource_type.as_ref().map(ToString::to_string),
            event.resource_id.as_ref().map(ToString::to_string),
            event
                .owner_scope
                .as_ref()
                .map(|s| s.id().as_str().to_owned()),
            event.operation_id.map(|v| v.to_string()),
            event.outcome.to_string(),
            event.reason_category.clone(),
        );
        match self {
            Self::Sqlite(s) => sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(event_id) DO NOTHING")
                .bind(values.0).bind(values.1).bind(values.2).bind(values.3).bind(values.4).bind(values.5).bind(values.6).bind(values.7).bind(values.8).bind(values.9).bind(values.10).bind(values.11).bind(values.12).bind(values.13).bind(values.14).execute(&s.pool).await.map_err(err).map(|_| ()),
            Self::Postgres(s) => sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT(event_id) DO NOTHING")
                .bind(values.0).bind(values.1).bind(values.2).bind(values.3).bind(values.4).bind(values.5).bind(values.6).bind(values.7).bind(values.8).bind(values.9).bind(values.10).bind(values.11).bind(values.12).bind(values.13).bind(values.14).execute(&s.pool).await.map_err(err).map(|_| ()),
        }
    }

    async fn page(&self, query: &AuditQuery) -> Result<DurableAuditPage, o3k_kernel::KernelError> {
        query.validate()?;
        // Query execution is intentionally kept in the store and uses LIMIT+1.
        // Rich filter projection is added by the native adapter; scope and the
        // continuation are always enforced at this boundary.
        let scope = query.scope.id().as_str();
        let n = (query.limit + 1) as i64;
        let rows: Vec<AuditRow> = match self {
            Self::Sqlite(s) => sqlx::query_as("SELECT event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category FROM audit_events WHERE effective_scope = ? AND (? IS NULL OR event_id > ?) ORDER BY event_id LIMIT ?")
                .bind(scope).bind(&query.after_event_id).bind(&query.after_event_id).bind(n).fetch_all(&s.pool).await.map_err(err)?,
            Self::Postgres(s) => sqlx::query_as("SELECT event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category FROM audit_events WHERE effective_scope = $1 AND ($2 IS NULL OR event_id > $2) ORDER BY event_id LIMIT $3")
                .bind(scope).bind(&query.after_event_id).bind(n).fetch_all(&s.pool).await.map_err(err)?,
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
        Ok(match self {
            Self::Sqlite(s) => sqlx::query("DELETE FROM audit_events WHERE timestamp < ?")
                .bind(cutoff)
                .execute(&s.pool)
                .await
                .map_err(err)?
                .rows_affected(),
            Self::Postgres(s) => sqlx::query("DELETE FROM audit_events WHERE timestamp < $1")
                .bind(cutoff)
                .execute(&s.pool)
                .await
                .map_err(err)?
                .rows_affected(),
        })
    }
}
