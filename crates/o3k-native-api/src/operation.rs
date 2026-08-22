//! Tenant-safe read access to durable service-neutral operations.

use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{AuthContext, Operation};
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
