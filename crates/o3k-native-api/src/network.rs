//! Native network resource read endpoints.
//!
//! Exposes canonical O3K Network resources (`network:address_realm`)
//! through the accepted `NativeResourceV1` wire envelope.
//!
//! The canonical address realm read path uses the generic store resource
//! table. O3K AddressRealm concepts (ADR-0168/ADR-0171) are the native
//! network model — not Neutron network/subnet/port compatibility shapes.

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
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::{CursorPayload, parse_page_size},
};

// ── NetworkReader trait ───────────────────────────────────────────────────

/// Lightweight read port for canonical O3K network resources.
#[async_trait::async_trait]
pub trait NetworkReader: Send + Sync {
    /// List address realms visible to the given project.
    async fn list_address_realms(&self, project_id: &str) -> Result<Vec<AddressRealmItem>, ()>;
    /// Show a single address realm by ID.
    async fn show_address_realm(&self, project_id: &str, id: Uuid) -> Result<AddressRealmItem, ()>;
}

/// Canonical native address realm representation.
#[derive(Debug, Clone, Serialize)]
pub struct AddressRealmItem {
    pub id: String,
    pub project_id: String,
    pub prefix: String,
    pub overlapping_prefixes: bool,
    pub created_at: Option<String>,
    pub generation: i64,
}

// ── Query parameters ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

// ── Responses ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AddressRealmListResponse {
    pub items: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

fn realm_to_native_v1(realm: &AddressRealmItem) -> serde_json::Value {
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "network:address_realm",
        "metadata": {
            "id": realm.id,
            "owner_scope": realm.project_id,
            "generation": realm.generation,
            "created_at": realm.created_at,
        },
        "spec": {
            "prefix": realm.prefix,
            "overlapping_prefixes": realm.overlapping_prefixes,
        },
        "status": {
            "state": "active",
        }
    })
}

// ── Handlers ──────────────────────────────────────────────────────────────

const RESOURCE_TYPE: &str = "network:address_realm";

/// GET /o3k/v1/network/address-realms
pub async fn list_address_realms(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(ref reader) = state.network_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "network service is not configured",
        )
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
        .into_response();
    }

    match reader.list_address_realms(&project_id).await {
        Ok(realms) => {
            let total = realms.len();
            let last_item_id_full = realms.last().map(|r| r.id.clone());
            let paged: Vec<AddressRealmItem> = if let Some(ref cursor) = query.cursor {
                if let Ok(payload) = cursor_cfg.decode_cursor(cursor, &project_id, RESOURCE_TYPE) {
                    let start_idx = realms
                        .iter()
                        .position(|r| r.id == payload.last_id)
                        .map(|i| i + 1)
                        .unwrap_or(total);
                    realms.into_iter().skip(start_idx).take(page_size).collect()
                } else {
                    realms.into_iter().take(page_size).collect()
                }
            } else {
                realms.into_iter().take(page_size).collect()
            };

            let items: Vec<serde_json::Value> = paged.iter().map(realm_to_native_v1).collect();

            let next_cursor = if paged.len() == page_size && total > page_size {
                let is_last = paged.last().map(|r| r.id.as_str()) == last_item_id_full.as_deref();
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
                Json(AddressRealmListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(_) => ProblemDetails::internal().into_response(),
    }
}

/// GET /o3k/v1/network/address-realms/{id}
pub async fn show_address_realm(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.network_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "network service is not configured",
        )
        .into_response();
    };

    let ctx = auth.0;
    let project_id = ctx.effective_scope().id().to_string();

    match reader.show_address_realm(&project_id, id).await {
        Ok(realm) => {
            let envelope = realm_to_native_v1(&realm);
            (StatusCode::OK, Json(envelope)).into_response()
        }
        Err(_) => ProblemDetails::not_found(Some(&id.to_string())).into_response(),
    }
}
