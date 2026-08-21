//! Native compute:server read endpoints.
//!
//! Uses the same canonical `ComputeService` as the OpenStack Nova-compatible
//! adapter, but returns the accepted `NativeResourceV1` wire envelope.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use o3k_kernel::AuthContext;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::{CursorPayload, parse_page_size},
};

// ── ServerReader trait ────────────────────────────────────────────────────

/// Lightweight read port for compute:server resources.
#[async_trait::async_trait]
pub trait ServerReader: Send + Sync {
    /// List servers visible to the given auth context.
    async fn list_servers(&self, auth: &AuthContext) -> Result<Vec<ServerItem>, ()>;
    /// Show a single server by ID within the auth scope.
    async fn show_server(&self, auth: &AuthContext, id: Uuid) -> Result<ServerItem, ()>;
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
}

// ── Query parameters ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

// ── List response (NativeResourceV1 items) ─────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ServerListResponse {
    pub items: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

fn server_to_native_v1(server: &ServerItem) -> serde_json::Value {
    serde_json::json!({
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
    })
}

// ── Handlers ──────────────────────────────────────────────────────────────

const RESOURCE_TYPE: &str = "compute:server";

/// GET /o3k/v1/compute/servers
pub async fn list_servers(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(ref reader) = state.server_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "compute service is not configured",
        )
        .into_response();
    };

    let ctx = auth.0;
    let scope_id = ctx.effective_scope().id().to_string();
    let page_size = parse_page_size(query.limit.as_deref());
    let cursor_cfg = &state.cursor_config;

    // Validate cursor if provided
    let cursor_invalid = query.cursor.as_deref().is_some_and(|c| {
        cursor_cfg
            .decode_cursor(c, &scope_id, RESOURCE_TYPE)
            .is_err()
    });
    if cursor_invalid {
        return ProblemDetails::with_detail(
            ErrorCode::InvalidCursor,
            "cursor is malformed or belongs to a different scope/resource",
        )
        .into_response();
    }

    match reader.list_servers(&ctx).await {
        Ok(servers) => {
            let total = servers.len();
            let last_item_id_full = servers.last().map(|s| s.id.clone());
            let paged: Vec<ServerItem> = if let Some(ref cursor) = query.cursor {
                if let Ok(payload) = cursor_cfg.decode_cursor(cursor, &scope_id, RESOURCE_TYPE) {
                    let start_idx = servers
                        .iter()
                        .position(|s| s.id == payload.last_id)
                        .map(|i| i + 1)
                        .unwrap_or(total);
                    servers
                        .into_iter()
                        .skip(start_idx)
                        .take(page_size)
                        .collect()
                } else {
                    servers.into_iter().take(page_size).collect()
                }
            } else {
                servers.into_iter().take(page_size).collect()
            };

            let items: Vec<serde_json::Value> = paged.iter().map(server_to_native_v1).collect();

            let next_cursor = if paged.len() == page_size && total > page_size {
                let is_last = paged.last().map(|s| s.id.as_str()) == last_item_id_full.as_deref();
                if !is_last {
                    paged.last().map(|last| {
                        cursor_cfg.encode_cursor(&CursorPayload {
                            last_id: last.id.clone(),
                            scope_id,
                            resource_type: RESOURCE_TYPE.to_owned(),
                            version: 1,
                        })
                    })
                } else {
                    None
                }
            } else {
                None
            };

            (
                StatusCode::OK,
                Json(ServerListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(_) => ProblemDetails::internal().into_response(),
    }
}

/// GET /o3k/v1/compute/servers/{id}
pub async fn show_server(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.server_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "compute service is not configured",
        )
        .into_response();
    };

    let ctx = auth.0;

    match reader.show_server(&ctx, id).await {
        Ok(server) => {
            let envelope = server_to_native_v1(&server);
            (StatusCode::OK, Json(envelope)).into_response()
        }
        Err(_) => ProblemDetails::not_found(Some(&id.to_string())).into_response(),
    }
}
