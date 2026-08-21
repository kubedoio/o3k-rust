//! Native volume:volume read endpoints.
//!
//! Uses the same canonical `StorageRepository` as the Cinder-compatible
//! adapter, but returns the accepted `NativeResourceV1` wire envelope.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
    pagination::{CursorPayload, parse_page_size},
};

// ── VolumeReader trait ────────────────────────────────────────────────────

/// Lightweight read port for volume:volume resources.
#[async_trait::async_trait]
pub trait VolumeReader: Send + Sync {
    /// List volumes in the given project scope.
    async fn list_volumes(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<VolumeItem>, NativeReadError>;
    /// Show a single volume by ID.
    async fn show_volume(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<VolumeItem, NativeReadError>;
}

/// Canonical native volume representation from domain state.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeItem {
    pub id: String,
    pub project_id: String,
    pub size_bytes: u64,
    pub volume_type: String,
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

// ── List response ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct VolumeListResponse {
    pub items: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

fn volume_to_native_v1(vol: &VolumeItem) -> serde_json::Value {
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "volume:volume",
        "metadata": {
            "id": vol.id,
            "owner_scope": vol.project_id,
            "generation": vol.generation,
            "created_at": vol.created_at,
        },
        "spec": {
            "size_bytes": vol.size_bytes,
            "volume_type": vol.volume_type,
        },
        "status": {
            "state": vol.state,
        }
    })
}

// ── Handlers ──────────────────────────────────────────────────────────────

const RESOURCE_TYPE: &str = "volume:volume";

/// GET /o3k/v1/volume/volumes
pub async fn list_volumes(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(ref reader) = state.volume_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "volume service is not configured",
        )
        .with_request_id(request_id.0.clone())
        .into_response();
    };

    let ctx = auth.0;
    let project_id = ctx.effective_scope().id().to_string();
    let page_size = parse_page_size(query.limit.as_deref());
    let cursor_cfg = &state.cursor_config;

    let cursor_invalid = query.cursor.as_deref().is_some_and(|c| {
        cursor_cfg
            .decode_cursor(c, &project_id, RESOURCE_TYPE)
            .is_err()
    });
    if cursor_invalid {
        return ProblemDetails::with_detail(
            ErrorCode::InvalidCursor,
            "cursor is malformed or belongs to a different scope/resource",
        )
        .with_request_id(request_id.0.clone())
        .into_response();
    }

    match reader.list_volumes(&ctx).await {
        Ok(mut volumes) => {
            volumes.sort_by(|a, b| a.id.cmp(&b.id));
            let total = volumes.len();
            let last_item_id_full = volumes.last().map(|v| v.id.clone());
            let paged: Vec<VolumeItem> = if let Some(ref cursor) = query.cursor {
                if let Ok(payload) = cursor_cfg.decode_cursor(cursor, &project_id, RESOURCE_TYPE) {
                    let start_idx = match crate::pagination::continuation_index(
                        &volumes.iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
                        &payload.last_id,
                    ) {
                        Ok(index) => index,
                        Err(_) => {
                            return ProblemDetails::with_detail(
                                ErrorCode::InvalidCursor,
                                "cursor anchor is stale",
                            )
                            .with_request_id(request_id.0.clone())
                            .into_response();
                        }
                    };
                    volumes
                        .into_iter()
                        .skip(start_idx)
                        .take(page_size)
                        .collect()
                } else {
                    return ProblemDetails::with_detail(
                        ErrorCode::InvalidCursor,
                        "cursor is malformed",
                    )
                    .with_request_id(request_id.0.clone())
                    .into_response();
                }
            } else {
                volumes.into_iter().take(page_size).collect()
            };

            let items: Vec<serde_json::Value> = paged.iter().map(volume_to_native_v1).collect();

            let next_cursor = if paged.len() == page_size && total > page_size {
                let is_last = paged.last().map(|v| v.id.as_str()) == last_item_id_full.as_deref();
                if !is_last {
                    paged.last().map(|last| {
                        cursor_cfg.encode_cursor(&CursorPayload {
                            last_id: last.id.clone(),
                            scope_id: project_id,
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
                Json(VolumeListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(NativeReadError::Forbidden) => ProblemDetails::forbidden(None)
            .with_request_id(request_id.0)
            .into_response(),
        Err(NativeReadError::NotFound) => ProblemDetails::not_found(None)
            .with_request_id(request_id.0)
            .into_response(),
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

/// GET /o3k/v1/volume/volumes/{id}
pub async fn show_volume(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.volume_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "volume service is not configured",
        )
        .with_request_id(request_id.0.clone())
        .into_response();
    };

    let ctx = auth.0;
    match reader.show_volume(&ctx, id).await {
        Ok(volume) => {
            let envelope = volume_to_native_v1(&volume);
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
    fn volume_envelope_conforms_to_schema() {
        let value = volume_to_native_v1(&VolumeItem {
            id: "volume-a".into(),
            project_id: "project-a".into(),
            size_bytes: 1024,
            volume_type: "lvm".into(),
            state: "available".into(),
            created_at: None,
            generation: 1,
        });
        crate::assert_resource_envelope_schema(&value);
    }
}
