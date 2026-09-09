//! Native compute:server read endpoints.
//!
//! Uses the same canonical `ComputeService` as the OpenStack Nova-compatible
//! adapter, but returns the accepted `NativeResourceV1` wire envelope.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use o3k_kernel::AuthContext;

use serde::Serialize;
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

// ── ServerReader trait ────────────────────────────────────────────────────

/// Lightweight read port for compute:server resources.
#[async_trait::async_trait]
pub trait ServerReader: Send + Sync {
    /// Show a single server by ID within the auth scope.
    async fn show_server(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<ServerItem, NativeReadError>;
}

/// Canonical native server representation from domain state.
#[derive(Debug, Clone, Serialize)]
pub struct ServerItem {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub flavor_id: String,
    pub image_id: String,
    pub state: String,
    pub created_at: Option<String>,
    pub generation: i64,
    #[serde(skip_serializing)]
    pub migration_id: Option<String>,
    #[serde(skip_serializing)]
    pub source_key: Option<String>,
}

fn server_to_native_v1(server: &ServerItem) -> serde_json::Value {
    let mut value = serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "compute:server",
        "metadata": {
            "id": server.id,
            "owner_scope": server.project_id,
            "generation": server.generation,
            "created_at": server.created_at,
        },
        "spec": {
            "name": server.name,
            "flavor_id": server.flavor_id,
            "image_id": server.image_id,
        },
        "status": {
            "state": server.state,
        }
    });
    if let Some(migration_id) = &server.migration_id {
        value["metadata"]["migration_id"] = migration_id.clone().into();
    }
    if let Some(source_key) = &server.source_key {
        value["metadata"]["source_key"] = source_key.clone().into();
    }
    value
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /o3k/v1/compute/servers/{id}
pub async fn show_server(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.server_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "compute service is not configured",
        )
        .with_request_id(request_id.0.clone())
        .into_response();
    };

    let ctx = auth.0;

    match reader.show_server(&ctx, id).await {
        Ok(server) => {
            let envelope = server_to_native_v1(&server);
            (StatusCode::OK, Json(envelope)).into_response()
        }
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
mod envelope_tests {
    use super::*;

    #[test]
    fn compute_envelope_conforms_to_schema() {
        let value = server_to_native_v1(&ServerItem {
            id: "server-a".into(),
            name: "demo".into(),
            project_id: "project-a".into(),
            flavor_id: "flavor-a".into(),
            image_id: "image-a".into(),
            state: "active".into(),
            created_at: None,
            generation: 1,
            migration_id: None,
            source_key: None,
        });
        crate::assert_resource_envelope_schema(&value);
    }
}
