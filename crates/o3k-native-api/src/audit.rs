//! Bounded, tenant-scoped native Audit read API.
use axum::{Json, extract::{Query, State}, response::{IntoResponse, Response}};
use o3k_kernel::{AuditEvent, AuditQuery, AuthContext, DurableAuditRepository, OwnershipScope};
use serde::Deserialize;
use crate::{NativeApiState, auth::BearerAuth, error::{ErrorCode, ProblemDetails}, pagination::RepositoryPage};

#[async_trait::async_trait]
pub trait AuditReader: Send + Sync {
    async fn list_page(&self, auth: &AuthContext, query: AuditQuery) -> Result<RepositoryPage<AuditEvent>, String>;
    async fn show(&self, auth: &AuthContext, id: &str) -> Result<AuditEvent, String>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery { pub limit: Option<String>, pub cursor: Option<String> }

pub async fn list_audit(auth: BearerAuth, Query(q): Query<ListQuery>, State(state): State<NativeApiState>) -> Response {
    let Some(reader) = state.audit_reader.as_ref() else { return ProblemDetails::new(ErrorCode::NotAvailable).into_response(); };
    let scope = auth.0.effective_scope().clone();
    let validated = match state.cursor_config.validate_query(q.limit.as_deref(), q.cursor.as_deref(), scope.id().as_str(), "audit") {
        Ok(v) => v, Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let query = AuditQuery { scope, after_event_id: validated.continuation_key().map(str::to_owned), limit: validated.limit(), service: None, action: None, outcome: None, resource_type: None, resource_id: None, operation_id: None, principal_id: None, request_id: None, audit_id: None, from_timestamp: None, until_timestamp: None };
    let page = match reader.list_page(&auth.0, query).await { Ok(p) => p, Err(_) => return ProblemDetails::new(ErrorCode::InternalError).into_response() };
    let page = match state.cursor_config.complete_page(&validated, page) { Ok(p) => p, Err(_) => return ProblemDetails::new(ErrorCode::InternalError).into_response() };
    Json(page).into_response()
}

