//! Native volume:volume read endpoints.
//!
//! Uses the same canonical `StorageRepository` as the Cinder-compatible
//! adapter, but returns the accepted `NativeResourceV1` wire envelope.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

// ── VolumeReader trait ────────────────────────────────────────────────────

/// Lightweight read port for volume:volume resources.
#[async_trait::async_trait]
pub trait VolumeReader: Send + Sync {
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
    pub name: String,
    pub description: String,
    pub metadata: serde_json::Value,
    pub availability_zone: Option<String>,
    pub size_bytes: u64,
    pub volume_type: String,
    pub state: String,
    pub created_at: Option<String>,
    pub generation: i64,
}

fn volume_to_native_v1(vol: &VolumeItem) -> serde_json::Value {
    let mut body = serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": "volume:volume",
        "metadata": {
            "id": vol.id,
            "owner_scope": vol.project_id,
            "generation": vol.generation,
            "created_at": vol.created_at,
        },
        "spec": {
            "name": vol.name,
            "description": vol.description,
            "metadata": vol.metadata,
            "availability_zone": vol.availability_zone,
            "size_bytes": vol.size_bytes,
            "volume_type": vol.volume_type,
        },
        "status": {
            "state": vol.state,
        }
    });
    for key in ["migration_id", "source_key"] {
        if let Some(metadata_value) = vol.metadata.get(key).and_then(serde_json::Value::as_str) {
            body["metadata"][key] = metadata_value.into();
        }
    }
    body
}

// ── Handlers ──────────────────────────────────────────────────────────────

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
            name: "volume-a".into(),
            description: String::new(),
            metadata: serde_json::json!({}),
            availability_zone: None,
            size_bytes: 1024,
            volume_type: "lvm".into(),
            state: "available".into(),
            created_at: None,
            generation: 1,
        });
        crate::assert_resource_envelope_schema(&value);
    }
}
