//! Bounded, tenant-scoped native Audit read API.
use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::RepositoryPage,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use o3k_kernel::{AuditEvent, AuditQuery, AuthContext};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[async_trait::async_trait]
pub trait AuditReader: Send + Sync {
    async fn list_page(
        &self,
        auth: &AuthContext,
        query: AuditQuery,
    ) -> Result<RepositoryPage<AuditEvent>, String>;
    async fn show(&self, auth: &AuthContext, id: &str) -> Result<AuditEvent, String>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
    pub service: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub operation_id: Option<String>,
    pub principal_id: Option<String>,
    pub request_id: Option<String>,
    pub audit_id: Option<String>,
    pub from: Option<String>,
    pub until: Option<String>,
}

fn query_identity(q: &ListQuery) -> String {
    let mut h = Sha256::new();
    h.update(b"o3k/audit-query/v1\0");
    for value in [
        &q.service,
        &q.action,
        &q.outcome,
        &q.resource_type,
        &q.resource_id,
        &q.operation_id,
        &q.principal_id,
        &q.request_id,
        &q.audit_id,
        &q.from,
        &q.until,
    ] {
        h.update(value.as_deref().unwrap_or("").as_bytes());
        h.update([0]);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize())
}

pub async fn list_audit(
    auth: BearerAuth,
    Query(q): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.audit_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let scope = auth.0.effective_scope().clone();
    let identity = query_identity(&q);
    let validated = match state.cursor_config.validate_query_with_identity(
        q.limit.as_deref(),
        q.cursor.as_deref(),
        scope.id().as_str(),
        "audit",
        &identity,
    ) {
        Ok(v) => v,
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let query = AuditQuery {
        scope,
        after_event_id: validated.continuation_key().map(str::to_owned),
        event_id: None,
        limit: validated.limit(),
        service: q.service,
        action: q.action,
        outcome: q.outcome,
        resource_type: q.resource_type,
        resource_id: q.resource_id,
        operation_id: q.operation_id,
        principal_id: q.principal_id,
        request_id: q.request_id,
        audit_id: q.audit_id,
        from_timestamp: q.from,
        until_timestamp: q.until,
    };
    let page = match reader.list_page(&auth.0, query).await {
        Ok(p) => p,
        Err(_) => return ProblemDetails::new(ErrorCode::InternalError).into_response(),
    };
    let page = match state.cursor_config.complete_page(&validated, page) {
        Ok(p) => p,
        Err(_) => return ProblemDetails::new(ErrorCode::InternalError).into_response(),
    };
    Json(page).into_response()
}

pub async fn show_audit(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.audit_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if id.is_empty() || id.len() > 256 || id.bytes().any(|b| b == 0) {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    match reader.show(&auth.0, &id).await {
        Ok(event) => Json(event).into_response(),
        Err(error) if error == "not found" => {
            ProblemDetails::new(ErrorCode::ResourceNotFound).into_response()
        }
        Err(_) => ProblemDetails::new(ErrorCode::InternalError).into_response(),
    }
}
