//! Tenant-safe read access to durable service-neutral operations.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, Operation, OperationState,
    ResourceId, ResourceTarget, ResourceType,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

/// Application boundary for operation visibility. Implementations must apply
/// authorization and ownership checks before returning an operation.
#[async_trait::async_trait]
pub trait OperationReader: Send + Sync {
    async fn show_operation(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<Operation, NativeReadError>;
    async fn list_operations_page(
        &self,
        auth: &AuthContext,
        after_id: Option<Uuid>,
        limit: usize,
        filters: &OperationFilters,
        scope: Option<&str>,
    ) -> Result<Vec<Operation>, NativeReadError>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
    pub service: Option<String>,
    pub action: Option<String>,
    pub state: Option<String>,
    /// Optional owner-scope narrowing for an explicitly system-scoped query.
    /// Tenant callers cannot select another scope.
    pub scope: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct OperationFilters {
    pub service: Option<String>,
    pub action: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct OperationListResponse {
    pub items: Vec<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Project durable operations into the public contract without exposing
/// provider/controller exception text. Such text can contain private paths,
/// connection details, or credentials accidentally included by a backend.
fn public_operation(mut operation: Operation) -> Operation {
    operation.error = match operation.state {
        OperationState::Retryable => Some("operation_retryable".to_owned()),
        OperationState::UnknownOutcome => Some("unknown_outcome".to_owned()),
        OperationState::Failed => Some("operation_failed".to_owned()),
        OperationState::Pending | OperationState::Running | OperationState::Succeeded => None,
    };
    operation
}

/// GET /o3k/v1/operations
pub async fn list_operations(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operation authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let system_scope = auth.0.effective_scope().kind() == o3k_kernel::ScopeKind::System;
    let read_action = if system_scope {
        ActionId::new_unchecked("operator", "ReadOperations")
    } else {
        ActionId::new_unchecked("operation", "Read")
    };
    if !matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: &auth.0,
            action: read_action,
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("operation", "operation"),
                Some(auth.0.effective_scope().id().clone()),
            ),
        }),
        AuthorizationDecision::Allow
    ) {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "operation authorization denied")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(reader) = state.operation_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let filters = OperationFilters {
        service: query.service,
        action: query.action,
        state: query.state,
    };
    if !system_scope && query.scope.is_some() {
        return ProblemDetails::bad_request(
            "scope filter is only valid for system operation queries",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    if query.scope.as_deref().is_some_and(|value| {
        value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
    }) {
        return ProblemDetails::bad_request("invalid operation scope filter")
            .with_request_id(request_id.0)
            .into_response();
    }
    // Validate each filter independently.  Do not use `service.or(action)`
    // here: that short-circuits and would let a malformed action through when
    // a valid service filter is also present.
    let invalid_filter = |value: Option<&str>| {
        value.is_some_and(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        })
    };
    if invalid_filter(filters.service.as_deref()) || invalid_filter(filters.action.as_deref()) {
        return ProblemDetails::bad_request("invalid operation filter")
            .with_request_id(request_id.0)
            .into_response();
    }
    if filters.state.as_deref().is_some_and(|state| {
        !matches!(
            state,
            "pending" | "running" | "succeeded" | "retryable" | "failed" | "unknown_outcome"
        )
    }) {
        return ProblemDetails::bad_request("invalid operation state filter")
            .with_request_id(request_id.0)
            .into_response();
    }
    let query_hash = serde_json::to_string(&(filters.clone(), query.scope.as_deref()))
        .ok()
        .map(|value| crate::pagination::query_hash(&value))
        .unwrap_or_default();
    let scope_id = auth.0.effective_scope().id().as_str().to_owned();
    let cursor = match query.cursor {
        Some(value) => {
            match state
                .cursor_config
                .decode_cursor(&value, &scope_id, "operation", &query_hash)
            {
                Ok(payload) => match Uuid::parse_str(&payload.last_id) {
                    Ok(id) => Some(id),
                    Err(_) => {
                        return ProblemDetails::bad_request("invalid operation cursor")
                            .with_request_id(request_id.0)
                            .into_response();
                    }
                },
                Err(_) => {
                    return ProblemDetails::bad_request("invalid operation cursor")
                        .with_request_id(request_id.0)
                        .into_response();
                }
            }
        }
        None => None,
    };
    match reader
        .list_operations_page(&auth.0, cursor, limit + 1, &filters, query.scope.as_deref())
        .await
    {
        Ok(mut operations) => {
            let next_cursor = if operations.len() > limit {
                operations.truncate(limit);
                operations.last().map(|op| {
                    state
                        .cursor_config
                        .encode_cursor(&crate::pagination::CursorPayload {
                            last_id: op.id.to_string(),
                            scope_id: scope_id.clone(),
                            resource_type: "operation".into(),
                            query_hash: query_hash.clone(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(OperationListResponse {
                    items: operations.into_iter().map(public_operation).collect(),
                    next_cursor,
                }),
            )
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(None)
                .with_request_id(request_id.0)
                .into_response()
        }
    }
}

/// GET /o3k/v1/operations/{id}
pub async fn show_operation(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operation authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let system_scope = auth.0.effective_scope().kind() == o3k_kernel::ScopeKind::System;
    let read_action = if system_scope {
        ActionId::new_unchecked("operator", "ReadOperations")
    } else {
        ActionId::new_unchecked("operation", "Read")
    };
    if !matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: &auth.0,
            action: read_action,
            resource_target: ResourceTarget::instance(
                ResourceType::new_unchecked("operation", "operation"),
                ResourceId::new_unchecked(id.to_string()),
                Some(auth.0.effective_scope().id().clone()),
            ),
        }),
        AuthorizationDecision::Allow
    ) {
        return ProblemDetails::not_found(Some(&id.to_string()))
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(reader) = state.operation_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };

    match reader.show_operation(&auth.0, id).await {
        Ok(operation) => (
            axum::http::StatusCode::OK,
            Json(public_operation(operation)),
        )
            .into_response(),
        // Foreign operations must be indistinguishable from missing IDs.
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(Some(&id.to_string()))
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod contract_tests {
    use super::*;
    use o3k_kernel::{OwnershipScope, ResourceType, ScopeId, ScopeKind};

    #[test]
    fn operation_projection_matches_versioned_schema() {
        let operation = Operation::new(
            Uuid::new_v4(),
            "compute",
            ActionId::new_unchecked("compute", "StartServer"),
            "principal:user-1",
            OwnershipScope::new(
                ScopeId::new_unchecked("project-a"),
                ScopeKind::Project,
                None,
                None,
            ),
            ResourceType::new_unchecked("compute", "server"),
            Some(ResourceId::new_unchecked("server-1")),
            Some("request-1".into()),
        );
        let value = serde_json::to_value(operation).expect("operation serializes");
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-operation-v1.schema.json"
        )))
        .expect("operation schema parses");
        let validator = jsonschema::validator_for(&schema).expect("operation schema compiles");
        assert!(
            validator.is_valid(&value),
            "operation must conform: {value}"
        );
    }

    #[test]
    fn public_operation_redacts_backend_error_text() {
        let mut operation = Operation::new(
            Uuid::new_v4(),
            "compute",
            ActionId::new_unchecked("compute", "StartServer"),
            "principal:user-1",
            OwnershipScope::new(
                ScopeId::new_unchecked("project-a"),
                ScopeKind::Project,
                None,
                None,
            ),
            ResourceType::new_unchecked("compute", "server"),
            Some(ResourceId::new_unchecked("server-1")),
            None,
        );
        operation.state = OperationState::Failed;
        operation.error = Some("provider password=super-secret /var/lib/private".into());
        let public = public_operation(operation);
        assert_eq!(public.error.as_deref(), Some("operation_failed"));
        assert!(
            !serde_json::to_string(&public)
                .expect("operation serializes")
                .contains("super-secret")
        );
    }

    #[test]
    fn public_operation_exposes_only_state_appropriate_error_categories() {
        let operation = Operation::new(
            Uuid::new_v4(),
            "compute",
            ActionId::new_unchecked("compute", "StartServer"),
            "principal:user-1",
            OwnershipScope::project(ScopeId::new_unchecked("project-a"), None, None),
            ResourceType::new_unchecked("compute", "server"),
            None,
            None,
        );

        for (state, expected) in [
            (OperationState::Pending, None),
            (OperationState::Running, None),
            (OperationState::Succeeded, None),
            (OperationState::Retryable, Some("operation_retryable")),
            (OperationState::UnknownOutcome, Some("unknown_outcome")),
            (OperationState::Failed, Some("operation_failed")),
        ] {
            let mut operation = operation.clone();
            operation.state = state;
            operation.error = Some("backend secret=must-not-escape".to_owned());
            assert_eq!(public_operation(operation).error.as_deref(), expected);
        }
    }
}
