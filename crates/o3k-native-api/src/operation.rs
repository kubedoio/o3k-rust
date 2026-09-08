//! Tenant-safe read access to durable service-neutral operations.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{AuthContext, Operation};
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
    ) -> Result<Vec<Operation>, NativeReadError>;
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct OperationListResponse {
    pub items: Vec<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let scope_id = auth.0.effective_scope().id().as_str().to_owned();
    let cursor = match query.cursor {
        Some(value) => match state
            .cursor_config
            .decode_cursor(&value, &scope_id, "operation", "")
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
        },
        None => None,
    };
    match reader
        .list_operations_page(&auth.0, cursor, limit + 1)
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
                            query_hash: String::new(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(OperationListResponse {
                    items: operations,
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
    let Some(reader) = state.operation_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "operation service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };

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
