//! Tenant-safe read access to durable service-neutral operations.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, Operation, ResourceTarget,
    ResourceType,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
    pagination::RepositoryPage,
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
    ) -> Result<RepositoryPage<Operation>, NativeReadError>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct OperationListResponse {
    pub items: Vec<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// GET /o3k/v1/operations
pub async fn list_operations(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.operation_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operation authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let operation_target = ResourceTarget::collection(
        ResourceType::new_unchecked("operation", "operation"),
        Some(auth.0.effective_scope().id().clone()),
    );
    if !matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: &auth.0,
            action: ActionId::new_unchecked("operation", "ReadOperation"),
            resource_target: operation_target,
        }),
        AuthorizationDecision::Allow
    ) {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "operation authorization denied")
            .with_request_id(request_id.0)
            .into_response();
    }
    if !state.cursor_config.is_available() {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation pagination is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    let scope_id = auth.0.effective_scope().id().as_str().to_owned();
    let resource_query = match state.cursor_config.validate_query(
        query.limit.as_deref(),
        query.cursor.as_deref(),
        &scope_id,
        "operation",
    ) {
        Ok(query) => query,
        Err(_) => {
            return ProblemDetails::bad_request("invalid operation query")
                .with_request_id(request_id.0)
                .into_response();
        }
    };
    let cursor = match resource_query.continuation_key() {
        Some(id) => match Uuid::parse_str(id) {
            Ok(id) => Some(id),
            Err(_) => {
                return ProblemDetails::bad_request("invalid operation cursor")
                    .with_request_id(request_id.0)
                    .into_response();
            }
        },
        None => None,
    };
    let limit = resource_query.limit();
    match reader.list_operations_page(&auth.0, cursor, limit).await {
        Ok(repository) => {
            let page = match state
                .cursor_config
                .complete_page(&resource_query, repository)
                .map_err(|_| NativeReadError::Internal)
            {
                Ok(page) => page,
                Err(_) => {
                    return ProblemDetails::internal()
                        .with_request_id(request_id.0)
                        .into_response();
                }
            };
            (axum::http::StatusCode::OK, Json(page)).into_response()
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
    let Some(reader) = state.operation_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operation authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let operation_target = ResourceTarget::instance(
        ResourceType::new_unchecked("operation", "operation"),
        o3k_kernel::ResourceId::new_unchecked(id.to_string()),
        Some(auth.0.effective_scope().id().clone()),
    );
    if !matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: &auth.0,
            action: ActionId::new_unchecked("operation", "ReadOperation"),
            resource_target: operation_target,
        }),
        AuthorizationDecision::Allow
    ) {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "operation authorization denied")
            .with_request_id(request_id.0)
            .into_response();
    }

    match reader.show_operation(&auth.0, id).await {
        Ok(operation) => (axum::http::StatusCode::OK, Json(operation)).into_response(),
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
