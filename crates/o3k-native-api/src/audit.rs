//! Tenant-safe native access to durable Cloud Kernel audit events.
#![allow(clippy::items_after_test_module)]

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, ResourceTarget,
    ResourceType,
};
use serde::{Deserialize, Serialize};

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

#[derive(Debug, Clone, Serialize)]
pub struct AuditEventView {
    pub event_id: String,
    pub timestamp: String,
    pub request_id: String,
    pub audit_id: String,
    pub principal_id: String,
    pub effective_scope: String,
    pub service_namespace: String,
    pub action: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub owner_scope: Option<String>,
    pub operation_id: Option<String>,
    pub outcome: String,
    pub reason_category: Option<String>,
}

/// Application boundary for durable audit visibility. Implementations must
/// enforce the AuthContext scope before returning either a row or a page.
#[async_trait::async_trait]
pub trait AuditReader: Send + Sync {
    async fn show_audit_event(
        &self,
        auth: &AuthContext,
        id: &str,
    ) -> Result<AuditEventView, NativeReadError>;
    async fn list_audit_events_page(
        &self,
        auth: &AuthContext,
        after_id: Option<&str>,
        limit: usize,
        filters: &AuditFilters,
    ) -> Result<Vec<AuditEventView>, NativeReadError>;
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
    pub event_id: Option<String>,
    pub timestamp_from: Option<String>,
    pub timestamp_to: Option<String>,
    pub principal_id: Option<String>,
    pub service: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub operation_id: Option<String>,
    pub request_id: Option<String>,
    pub audit_id: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct AuditFilters {
    pub event_id: Option<String>,
    pub timestamp_from: Option<String>,
    pub timestamp_to: Option<String>,
    pub principal_id: Option<String>,
    pub service: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub operation_id: Option<String>,
    pub request_id: Option<String>,
    pub audit_id: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditListResponse {
    pub items: Vec<AuditEventView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

pub async fn list_audit_events(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "audit authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let system_scope = auth.0.effective_scope().kind() == o3k_kernel::ScopeKind::System;
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        action: if system_scope {
            ActionId::new_unchecked("operator", "ReadAudit")
        } else {
            ActionId::new_unchecked("audit", "Read")
        },
        resource_target: ResourceTarget::collection(
            ResourceType::new_unchecked("audit", "event"),
            Some(if system_scope {
                o3k_kernel::ScopeId::new_unchecked("system")
            } else {
                auth.0.effective_scope().id().clone()
            }),
        ),
    });
    if !matches!(decision, AuthorizationDecision::Allow) {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "audit authorization denied")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(reader) = state.audit_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "audit service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let filters = AuditFilters {
        event_id: query.event_id,
        timestamp_from: query.timestamp_from,
        timestamp_to: query.timestamp_to,
        principal_id: query.principal_id,
        service: query.service,
        action: query.action,
        outcome: query.outcome,
        resource_type: query.resource_type,
        resource_id: query.resource_id,
        operation_id: query.operation_id,
        request_id: query.request_id,
        audit_id: query.audit_id,
        scope: query.scope,
    };
    if filters
        .values()
        .any(|value| value.is_empty() || value.len() > 128 || value.chars().any(char::is_control))
        || filters.timestamp_from.is_some()
            && filters.timestamp_to.is_some()
            && filters.timestamp_from > filters.timestamp_to
    {
        return ProblemDetails::bad_request("invalid audit filter")
            .with_request_id(request_id.0)
            .into_response();
    }
    let scope = auth.0.effective_scope().id().as_str().to_owned();
    if !system_scope && filters.scope.is_some() {
        return ProblemDetails::bad_request("scope filter requires system authorization")
            .with_request_id(request_id.0)
            .into_response();
    }
    let query_hash = serde_json::to_string(&filters)
        .ok()
        .map(|value| crate::pagination::query_hash(&value))
        .unwrap_or_default();
    let after = match query.cursor {
        Some(cursor) => {
            match state
                .cursor_config
                .decode_cursor(&cursor, &scope, "audit", &query_hash)
            {
                Ok(payload) => Some(payload.last_id),
                Err(_) => {
                    return ProblemDetails::bad_request("invalid audit cursor")
                        .with_request_id(request_id.0)
                        .into_response();
                }
            }
        }
        None => None,
    };
    match reader
        .list_audit_events_page(&auth.0, after.as_deref(), limit + 1, &filters)
        .await
    {
        Ok(mut items) => {
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|item| {
                    state
                        .cursor_config
                        .encode_cursor(&crate::pagination::CursorPayload {
                            last_id: item.event_id.clone(),
                            scope_id: scope.clone(),
                            resource_type: "audit".into(),
                            query_hash: query_hash.clone(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(AuditListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(None)
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

impl AuditFilters {
    fn values(&self) -> impl Iterator<Item = &str> {
        [
            self.event_id.as_deref(),
            self.timestamp_from.as_deref(),
            self.timestamp_to.as_deref(),
            self.principal_id.as_deref(),
            self.service.as_deref(),
            self.action.as_deref(),
            self.outcome.as_deref(),
            self.resource_type.as_deref(),
            self.resource_id.as_deref(),
            self.operation_id.as_deref(),
            self.request_id.as_deref(),
            self.audit_id.as_deref(),
            self.scope.as_deref(),
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod contract_tests {
    use super::*;

    #[test]
    fn audit_event_contract_rejects_unadvertised_fields() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-audit-event-v1.schema.json"
        )))
        .expect("valid audit schema");
        let validator = jsonschema::validator_for(&schema).expect("compiled audit schema");
        let value = serde_json::to_value(AuditEventView {
            event_id: "event-1".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            request_id: "request-1".into(),
            audit_id: "audit-1".into(),
            principal_id: "user-1".into(),
            effective_scope: "project-a".into(),
            service_namespace: "compute".into(),
            action: "ReadServer".into(),
            resource_type: None,
            resource_id: None,
            owner_scope: None,
            operation_id: None,
            outcome: "succeeded".into(),
            reason_category: None,
        })
        .expect("audit view serializes");
        assert!(validator.validate(&value).is_ok());
        let mut tampered = value;
        tampered
            .as_object_mut()
            .expect("object")
            .insert("token".into(), "secret".into());
        assert!(validator.validate(&tampered).is_err());
    }
}

pub async fn show_audit_event(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Path(id): Path<String>,
) -> Response {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "audit authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let system_scope = auth.0.effective_scope().kind() == o3k_kernel::ScopeKind::System;
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        // System audit inspection is an operator capability.  Keeping this
        // distinct from the tenant action prevents an operator role from
        // accidentally being treated as a tenant grant (and vice versa).
        action: if system_scope {
            ActionId::new_unchecked("operator", "ReadAudit")
        } else {
            ActionId::new_unchecked("audit", "Read")
        },
        resource_target: ResourceTarget::instance(
            ResourceType::new_unchecked("audit", "event"),
            o3k_kernel::ResourceId::new_unchecked(id.clone()),
            Some(if system_scope {
                o3k_kernel::ScopeId::new_unchecked("system")
            } else {
                auth.0.effective_scope().id().clone()
            }),
        ),
    });
    if !matches!(decision, AuthorizationDecision::Allow) {
        return ProblemDetails::not_found(Some(&id))
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(reader) = state.audit_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "audit service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader.show_audit_event(&auth.0, &id).await {
        Ok(event) => (axum::http::StatusCode::OK, Json(event)).into_response(),
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(Some(&id))
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}
