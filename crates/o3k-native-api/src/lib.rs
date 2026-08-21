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
//! - `GET /o3k/v1/identity/me`           — current auth context (stub)

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use o3k_kernel::{ManifestRegistry, ServiceLifecycleState};
use serde::Serialize;

pub mod identity;

/// Shared state wrapping the manifest registry for native service/resource
/// discovery.
///
/// The `ManifestRegistry` is the authoritative source for native service
/// discovery (ADR-0174/SPEC-0031). Static P0-P11 core services are
/// represented through a migration adapter (see `ManifestRegistry::seed_core()`).
#[derive(Clone, Default)]
pub struct NativeApiState {
    /// Manifest registry as the canonical discovery source.
    pub registry: Option<ManifestRegistry>,
}

impl NativeApiState {
    /// Creates a new `NativeApiState` with an optional manifest registry.
    #[must_use]
    pub fn new(registry: Option<ManifestRegistry>) -> Self {
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
    /// Lifecycle state from service registration + controller health.
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
    // Map resource types to their owning service
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::ServiceManifest;

    fn test_manifest_registry() -> ManifestRegistry {
        let mut reg = ManifestRegistry::new();
        let m = ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "0.4.0".to_owned(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            resource_types: vec!["compute:server".to_owned(), "compute:flavor".to_owned()],
            actions: vec![
                "compute:ListServers".to_owned(),
                "compute:CreateServer".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            region: None,
            availability_domain: None,
            controller: None,
            health: None,
        };
        let _ = reg.register(m);
        reg
    }

    #[tokio::test]
    async fn api_root_returns_version() {
        let state = NativeApiState::new(Some(test_manifest_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["api_version"], "o3k.io/v1");
    }

    #[tokio::test]
    async fn discover_services_uses_manifest_registry() {
        let state = NativeApiState::new(Some(test_manifest_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/services")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["count"].as_u64().unwrap_or(0), 1);
        assert_eq!(body["services"][0]["namespace"], "compute");
        assert_eq!(body["services"][0]["lifecycle_state"], "declared");
        assert_eq!(body["services"][0]["ownership"], "o3k-implemented");
    }

    #[tokio::test]
    async fn discover_services_stable_wire_values() {
        // Verify that wire values use stable contract strings, not Rust Debug
        // formatting.
        let mut reg = ManifestRegistry::new();
        reg.seed_core();
        let state = NativeApiState::new(Some(reg));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/services")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        // All services must have stable lifecycle_state and ownership values.
        let services = body["services"].as_array().unwrap();
        assert!(services.len() >= 6, "expected at least 6 seeded services");
        for svc in services {
            let lc = svc["lifecycle_state"].as_str().unwrap_or("");
            assert!(
                ["declared", "ready", "not_ready", "disabled", "incompatible"].contains(&lc),
                "unexpected lifecycle_state: {lc}"
            );
            let ownership = svc["ownership"].as_str().unwrap_or("");
            assert!(
                ["o3k-implemented", "external-hosted"].contains(&ownership),
                "unexpected ownership: {ownership}"
            );
        }
    }

    #[tokio::test]
    async fn discover_resource_types_from_manifest_registry() {
        let state = NativeApiState::new(Some(test_manifest_registry()));
        let app = router(state);
        let response = axum::http::Request::builder()
            .uri("/resource-types")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        // Should contain compute:server and compute:flavor
        assert!(body["count"].as_u64().unwrap_or(0) >= 2);
    }
}
