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
//! ## Current v1 endpoints (growing)
//!
//! - `GET /o3k/v1`                        — API version/entry discovery
//! - `GET /o3k/v1/services`              — registered services
//! - `GET /o3k/v1/resource-types`        — registered resource types
//! - `GET /o3k/v1/identity/me`           — current auth context
//! - `GET /o3k/v1/compute/servers`       — server list (read-only)
//! - `GET /o3k/v1/compute/servers/:id`   — server detail (read-only)

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use o3k_kernel::KernelRegistry;
use serde::Serialize;

pub mod identity;

/// Shared state wrapping the kernel registry and (eventually) application
/// services.
#[derive(Clone, Default)]
pub struct NativeApiState {
    /// Kernel registry for service/resource-type discovery.
    registry: Option<KernelRegistry>,
}

impl NativeApiState {
    /// Creates a new `NativeApiState` with an optional registry.
    #[must_use]
    pub fn new(registry: Option<KernelRegistry>) -> Self {
        Self { registry }
    }
}

/// Builds the native API router with the given state.
pub fn router(state: NativeApiState) -> Router {
    Router::new()
        .route("/", get(api_root))
        .route("/services", get(discover_services))
        .route("/resource-types", get(discover_resource_types))
        .route("/identity/me", get(identity::current_context))
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
            "/o3k/v1/identity/me",
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
        .services()
        .iter()
        .map(|svc| DiscoveredService {
            id: svc.id.to_string(),
            namespace: svc.namespace.to_string(),
            service_version: "0.4.0".to_owned(),
            ownership: Some(svc.ownership.to_string()),
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

    let mut resource_types: Vec<DiscoveredResourceType> = Vec::new();
    for svc in registry.services() {
        for rt in &svc.resource_types {
            resource_types.push(DiscoveredResourceType {
                namespace: rt.namespace().to_owned(),
                name: rt.name().to_owned(),
                service: svc.id.to_string(),
            });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> KernelRegistry {
        KernelRegistry::standard("http://127.0.0.1:18080", None)
    }

    #[tokio::test]
    async fn api_root_returns_version() {
        let state = NativeApiState::new(Some(test_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["api_version"], "o3k.io/v1");
    }

    #[tokio::test]
    async fn discover_services_returns_registered() {
        let state = NativeApiState::new(Some(test_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/services")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert!(body["count"].as_u64().unwrap_or(0) >= 5);
    }

    #[tokio::test]
    async fn discover_resource_types_returns_known() {
        let state = NativeApiState::new(Some(test_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/resource-types")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert!(body["count"].as_u64().unwrap_or(0) > 0);
    }
}
