//! Native volume:volume read endpoints.
//!
//! Uses the same canonical `StorageRepository` as the Cinder-compatible
//! adapter, but returns native `ResourceEnvelope` JSON instead of Cinder
//! wire models.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    envelope::ResourceMeta,
    resource::ResourceId,
    scope::{OwnershipScope, ScopeKind},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::{self, CursorPayload, decode_cursor, encode_cursor},
};

// ── VolumeReader trait ────────────────────────────────────────────────────

/// Lightweight read port for volume:volume resources.
///
/// Concrete implementation wraps `o3k_store::StorageRepository` in `o3kd`.
#[async_trait::async_trait]
pub trait VolumeReader: Send + Sync {
    /// List volumes in the given project scope.
    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeItem>, ProblemDetails>;
    /// Show a single volume by ID within the given project scope.
    async fn show_volume(&self, project_id: &str, id: Uuid) -> Result<VolumeItem, ProblemDetails>;
}

/// Lightweight native volume representation.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeItem {
    pub id: String,
    pub project_id: String,
    pub size_bytes: u64,
    pub volume_type: String,
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
pub struct VolumeListResponse {
    pub items: Vec<VolumeEnvelope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Native resource envelope wrapper for a volume.
#[derive(Debug, Serialize)]
pub struct VolumeEnvelope {
    pub api_version: &'static str,
    pub kind: &'static str,
    pub metadata: ResourceMeta,
    pub spec: VolumeSpec,
    pub status: VolumeStatus,
}

#[derive(Debug, Serialize)]
pub struct VolumeSpec {
    pub size_bytes: u64,
    pub volume_type: String,
}

#[derive(Debug, Serialize)]
pub struct VolumeStatus {
    pub state: String,
    pub created_at: String,
}

fn scope_from_project_id(project_id: &str) -> OwnershipScope {
    OwnershipScope::new(
        o3k_kernel::scope::ScopeId::new_unchecked(project_id.to_owned()),
        ScopeKind::Project,
        None,
        None,
    )
}

fn volume_to_envelope(vol: &VolumeItem) -> VolumeEnvelope {
    let meta = ResourceMeta {
        id: ResourceId::new_unchecked(vol.id.clone()),
        owner_scope: scope_from_project_id(&vol.project_id),
        generation: 0,
        created_at: Some(vol.created_at.clone()),
        updated_at: None,
        region: None,
        availability_domain: None,
        labels: None,
        annotations: None,
    };
    VolumeEnvelope {
        api_version: "o3k.io/v1",
        kind: "volume:volume",
        metadata: meta,
        spec: VolumeSpec {
            size_bytes: vol.size_bytes,
            volume_type: vol.volume_type.clone(),
        },
        status: VolumeStatus {
            state: vol.state.clone(),
            created_at: vol.created_at.clone(),
        },
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /o3k/v1/volume/volumes
pub async fn list_volumes(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(ref reader) = state.volume_reader else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "volume service is not configured",
            )),
        )
            .into_response();
    };

    let ctx = auth.0;
    let project_id = ctx.effective_scope().id().to_string();
    let page_size = pagination::parse_page_size(query.limit.as_deref());

    let cursor_invalid = query
        .cursor
        .as_deref()
        .is_some_and(|c| decode_cursor(c, &project_id).is_err());
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

    match reader.list_volumes(&project_id).await {
        Ok(volumes) => {
            let total = volumes.len();
            let paged: Vec<VolumeItem> = if let Some(ref cursor) = query.cursor {
                let payload = decode_cursor(cursor, &project_id).ok();
                if let Some(p) = payload {
                    let start_idx = volumes
                        .iter()
                        .position(|v| v.id == p.last_id)
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    volumes
                        .into_iter()
                        .skip(start_idx)
                        .take(page_size)
                        .collect()
                } else {
                    volumes.into_iter().take(page_size).collect()
                }
            } else {
                volumes.into_iter().take(page_size).collect()
            };

            let next_cursor = if total > page_size {
                paged.last().map(|last| {
                    encode_cursor(&CursorPayload {
                        last_id: last.id.clone(),
                        scope_id: project_id.clone(),
                        version: 1,
                    })
                })
            } else {
                None
            };

            let items: Vec<VolumeEnvelope> = paged.iter().map(volume_to_envelope).collect();

            (
                StatusCode::OK,
                Json(VolumeListResponse { items, next_cursor }),
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

/// GET /o3k/v1/volume/volumes/{id}
pub async fn show_volume(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.volume_reader else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "volume service is not configured",
            )),
        )
            .into_response();
    };

    let ctx = auth.0;
    let project_id = ctx.effective_scope().id().to_string();

    match reader.show_volume(&project_id, id).await {
        Ok(volume) => {
            let envelope = volume_to_envelope(&volume);
            (StatusCode::OK, Json(envelope)).into_response()
        }
        Err(pd) => {
            let status =
                StatusCode::from_u16(pd.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(pd)).into_response()
        }
    }
}
