//! Native network resource read endpoints.
//!
//! Exposes canonical O3K Network resources (`network:address_realm`)
//! through the accepted `NativeResourceV1` wire envelope.
//!
//! The canonical address realm read path uses durable canonical Network and
//! AddressRealm records. NetworkIntent is only a transitional derived/execution
//! aggregate and is not a source of native resource authority.

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

// ── NetworkReader trait ───────────────────────────────────────────────────

/// Lightweight read port for canonical O3K network resources.
#[async_trait::async_trait]
pub trait NetworkReader: Send + Sync {
    /// Show a single address realm by ID.
    async fn show_address_realm(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<AddressRealmItem, NativeReadError>;
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
    pub state: String,
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
            "state": realm.state,
        }
    })
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /o3k/v1/network/address-realms/{id}
pub async fn show_address_realm(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(ref reader) = state.network_reader else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "network service is not configured",
        )
        .with_request_id(request_id.0.clone())
        .into_response();
    };

    let ctx = auth.0;
    match reader.show_address_realm(&ctx, id).await {
        Ok(realm) => {
            let envelope = realm_to_native_v1(&realm);
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
    fn network_envelope_conforms_to_schema() {
        let value = realm_to_native_v1(&AddressRealmItem {
            id: "realm-a".into(),
            project_id: "project-a".into(),
            prefix: "10.0.0.0/24".into(),
            overlapping_prefixes: false,
            created_at: None,
            generation: 1,
            state: "active".into(),
        });
        crate::assert_resource_envelope_schema(&value);
    }
}
