//! Native compute:server read endpoints.
//!
//! Uses the same canonical `ComputeService` as the OpenStack Nova-compatible
//! adapter, but returns native `ResourceEnvelope` JSON instead of Nova wire
//! models.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    response::Response,
};
use o3k_kernel::{
    AuthContext, envelope::ResourceMeta, resource::ResourceId, scope::OwnershipScope,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::{self, CursorPayload, decode_cursor, encode_cursor},
};

// ── ServerReader trait ────────────────────────────────────────────────────

/// Lightweight read port for compute:server resources.
///
/// Concrete implementation wraps `o3k_compute::ComputeService` in `o3kd`.
#[async_trait::async_trait]
pub trait ServerReader: Send + Sync {
    /// List servers visible to the given auth context.
    async fn list_servers(&self, auth: &AuthContext) -> Result<Vec<ServerItem>, ProblemDetails>;
    /// Show a single server by ID within the auth scope.
    async fn show_server(&self, auth: &AuthContext, id: Uuid)
    -> Result<ServerItem, ProblemDetails>;
}

/// Lightweight native server representation.
#[derive(Debug, Clone, Serialize)]
pub struct ServerItem {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub flavor_id: String,
    pub image_id: String,
    pub state: String,
    pub created_at: String,
}

// ── Query parameters ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(rename = "limit")]
    pub limit: Option<String>,
    #[serde(rename = "cursor")]
    pub cursor: Option<String>,
}

// ── List response ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ServerListResponse {
    pub items: Vec<ServerEnvelope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Native resource envelope wrapper for a server.
#[derive(Debug, Serialize)]
pub struct ServerEnvelope {
    pub api_version: &'static str,
    pub kind: &'static str,
    pub metadata: ResourceMeta,
    pub spec: ServerSpec,
    pub status: ServerStatus,
}

#[derive(Debug, Serialize)]
pub struct ServerSpec {
    pub name: String,
    pub flavor_id: String,
    pub image_id: String,
}

#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub state: String,
    pub created_at: String,
}

fn server_to_envelope(server: &ServerItem, scope: &OwnershipScope) -> ServerEnvelope {
    let meta = ResourceMeta {
        id: ResourceId::new_unchecked(server.id.clone()),
        owner_scope: scope.clone(),
        generation: 0,
        created_at: Some(server.created_at.clone()),
        updated_at: None,
        region: None,
        availability_domain: None,
        labels: None,
        annotations: None,
    };
    ServerEnvelope {
        api_version: "o3k.io/v1",
        kind: "compute:server",
        metadata: meta,
        spec: ServerSpec {
            name: server.name.clone(),
            flavor_id: server.flavor_id.clone(),
            image_id: server.image_id.clone(),
        },
        status: ServerStatus {
            state: server.state.clone(),
            created_at: server.created_at.clone(),
        },
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /o3k/v1/compute/servers
pub async fn list_servers(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(ref reader) = state.server_reader else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "compute service is not configured",
            )),
        )
            .into_response();
    };

    let ctx = auth.0;
    let scope_id = ctx.effective_scope().id().to_string();
    let page_size = pagination::parse_page_size(query.limit.as_deref());

    // Validate cursor if provided (combined condition avoids collapsible_if)
    let cursor_invalid = query
        .cursor
        .as_deref()
        .is_some_and(|c| decode_cursor(c, &scope_id).is_err());
    if cursor_invalid {
        return (
            StatusCode::BAD_REQUEST,
            Json(ProblemDetails::with_detail(
                ErrorCode::InvalidCursor,
                "cursor is malformed or belongs to a different scope",
            )),
        )
            .into_response();
    }

    match reader.list_servers(&ctx).await {
        Ok(servers) => {
            let total = servers.len();
            let paged: Vec<ServerItem> = if let Some(ref cursor) = query.cursor {
                let payload = decode_cursor(cursor, &scope_id).ok();
                if let Some(p) = payload {
                    let start_idx = servers
                        .iter()
                        .position(|s| s.id == p.last_id)
                        .map(|i| i + 1)
                        .unwrap_or(0);
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

            let next_cursor = if total > page_size {
                paged.last().map(|last| {
                    encode_cursor(&CursorPayload {
                        last_id: last.id.clone(),
                        scope_id,
                        version: 1,
                    })
                })
            } else {
                None
            };

            let items: Vec<ServerEnvelope> = paged
                .iter()
                .map(|s| server_to_envelope(s, ctx.effective_scope()))
                .collect();

            (
                StatusCode::OK,
                Json(ServerListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(pd) => {
            let status =
                StatusCode::from_u16(pd.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(pd)).into_response()
        }
    }
}

/// GET /o3k/v1/compute/servers/{id}
pub async fn show_server(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.server_reader else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "compute service is not configured",
            )),
        )
            .into_response();
    };

    let ctx = auth.0;

    match reader.show_server(&ctx, id).await {
        Ok(server) => {
            let envelope = server_to_envelope(&server, ctx.effective_scope());
            (StatusCode::OK, Json(envelope)).into_response()
        }
        Err(pd) => {
            let status =
                StatusCode::from_u16(pd.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(pd)).into_response()
        }
    }
}
