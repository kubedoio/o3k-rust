//! O3K Native Resource API — service-namespaced REST surface over Cloud Kernel
//! resources.
//!
//! This crate is a northbound protocol adapter, sibling to `o3k-api` (the
//! OpenStack compatibility adapter). Both consume the same canonical
//! application/domain services. See ADR-0173, ADR-0174, and SPEC-0030.
//!
//! ## Routing convention
//!
//! ```text
//! /o3k/v1/{service-namespace}/{collection}
//! ```
//!
//! ## Current v1 endpoints
//!
//! - `GET /o3k/v1`                        — API version/entry discovery
//! - `GET /o3k/v1/services`              — registered services
//! - `GET /o3k/v1/resource-types`        — registered resource types
//! - `POST /o3k/v1/identity/tokens`      — issue bearer token (native IAM)
//! - `GET /o3k/v1/identity/me`           — current auth context
//! - `GET /o3k/v1/compute/servers`       — list compute:server resources
//! - `GET /o3k/v1/compute/servers/{id}`  — show compute:server resource
//! - `GET /o3k/v1/volume/volumes`        — list volume:volume resources
//! - `GET /o3k/v1/volume/volumes/{id}`   — show volume:volume resource

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use o3k_kernel::{ManifestRegistry, ServiceLifecycleState};
use serde::Serialize;

pub mod auth;
pub mod compute;
pub mod error;
pub mod identity;
pub mod pagination;
pub mod volume;

/// Shared application state for the native API router.
///
/// Extends the P12.1 `ManifestRegistry` with optional service reader
/// ports wired from the composition root (`o3kd`).
#[derive(Clone, Default)]
pub struct NativeApiState {
    /// Manifest registry as the canonical discovery source.
    pub registry: Option<ManifestRegistry>,

    /// Optional token issuer (wraps `TokenService` from o3k-identity).
    pub token_issuer: Option<std::sync::Arc<dyn auth::TokenIssuer>>,

    /// Optional server reader (wraps `ComputeService` from o3k-compute).
    pub server_reader: Option<std::sync::Arc<dyn compute::ServerReader>>,

    /// Optional volume reader (wraps `StorageRepository` from o3k-store).
    pub volume_reader: Option<std::sync::Arc<dyn volume::VolumeReader>>,
}

impl NativeApiState {
    /// Creates a new `NativeApiState`.
    #[must_use]
    pub fn new(
        registry: Option<ManifestRegistry>,
        token_issuer: Option<std::sync::Arc<dyn auth::TokenIssuer>>,
        server_reader: Option<std::sync::Arc<dyn compute::ServerReader>>,
        volume_reader: Option<std::sync::Arc<dyn volume::VolumeReader>>,
    ) -> Self {
        Self {
            registry,
            token_issuer,
            server_reader,
            volume_reader,
        }
    }
}

/// Builds the native API router with the given state.
pub fn router(state: NativeApiState) -> Router {
    Router::new()
        .route("/", get(api_root))
        .route("/services", get(discover_services))
        .route("/resource-types", get(discover_resource_types))
        .route("/identity/tokens", post(identity::issue_token))
        .route("/identity/me", get(identity::current_context))
        .route("/compute/servers", get(compute::list_servers))
        .route("/compute/servers/{id}", get(compute::show_server))
        .route("/volume/volumes", get(volume::list_volumes))
        .route("/volume/volumes/{id}", get(volume::show_volume))
        .with_state(state)
}

// ── API root ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ApiRootResponse {
    api_version: &'static str,
    endpoints: Vec<&'static str>,
}

pub async fn api_root() -> Json<ApiRootResponse> {
    Json(ApiRootResponse {
        api_version: "o3k.io/v1",
        endpoints: vec![
            "/o3k/v1/services",
            "/o3k/v1/resource-types",
            "/o3k/v1/identity/tokens",
            "/o3k/v1/identity/me",
            "/o3k/v1/compute/servers",
            "/o3k/v1/volume/volumes",
        ],
    })
}

// ── Service discovery ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DiscoveredService {
    id: String,
    namespace: String,
    service_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ownership: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lifecycle_state: Option<String>,
}

#[derive(Serialize)]
pub struct ServicesResponse {
    services: Vec<DiscoveredService>,
    count: usize,
}

pub async fn discover_services(State(state): State<NativeApiState>) -> impl IntoResponse {
    let Some(registry) = &state.registry else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"services": [], "count": 0})),
        );
    };

    let services: Vec<DiscoveredService> = registry
        .all()
        .iter()
        .map(|m| {
            let lc = registry
                .controller(&m.service_id)
                .map(|c| c.state.to_string())
                .unwrap_or_else(|| ServiceLifecycleState::Declared.to_string());
            DiscoveredService {
                id: m.service_id.clone(),
                namespace: m.namespace.clone(),
                service_version: m.service_version.clone(),
                ownership: Some(m.ownership.to_string()),
                lifecycle_state: Some(lc),
            }
        })
        .collect();

    let count = services.len();
    (
        StatusCode::OK,
        Json(serde_json::to_value(ServicesResponse { services, count }).unwrap_or_default()),
    )
}

// ── Resource-type discovery ────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DiscoveredResourceType {
    namespace: String,
    name: String,
    service: String,
}

#[derive(Serialize)]
pub struct ResourceTypesResponse {
    resource_types: Vec<DiscoveredResourceType>,
    count: usize,
}

pub async fn discover_resource_types(State(state): State<NativeApiState>) -> impl IntoResponse {
    let Some(registry) = &state.registry else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"resource_types": [], "count": 0})),
        );
    };

    let rts = registry.all_resource_types();
    let mut resource_types: Vec<DiscoveredResourceType> = Vec::new();
    for rt in &rts {
        for m in registry.all() {
            if m.namespace == rt.namespace() {
                resource_types.push(DiscoveredResourceType {
                    namespace: rt.namespace().to_owned(),
                    name: rt.name().to_owned(),
                    service: m.service_id.clone(),
                });
                break;
            }
        }
    }

    let count = resource_types.len();
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(ResourceTypesResponse {
                resource_types,
                count,
            })
            .unwrap_or_default(),
        ),
    )
}
