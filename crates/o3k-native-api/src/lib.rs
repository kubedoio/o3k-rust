//! O3K Native Resource API — service-namespaced REST surface over Cloud Kernel
//! resources.
//!
//! Sibling to `o3k-api` (OpenStack compatibility adapter). Both consume the
//! same canonical application/domain services. See ADR-0173, ADR-0174, SPEC-0030.

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
pub mod network;
pub mod pagination;
pub mod volume;

/// Shared application state for the native API router.
#[derive(Clone, Default)]
pub struct NativeApiState {
    pub registry: Option<ManifestRegistry>,
    pub cursor_config: pagination::CursorConfig,
    pub token_issuer: Option<std::sync::Arc<dyn auth::TokenIssuer>>,
    pub server_reader: Option<std::sync::Arc<dyn compute::ServerReader>>,
    pub volume_reader: Option<std::sync::Arc<dyn volume::VolumeReader>>,
    pub network_reader: Option<std::sync::Arc<dyn network::NetworkReader>>,
}

impl NativeApiState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Option<ManifestRegistry>,
        cursor_config: pagination::CursorConfig,
        token_issuer: Option<std::sync::Arc<dyn auth::TokenIssuer>>,
        server_reader: Option<std::sync::Arc<dyn compute::ServerReader>>,
        volume_reader: Option<std::sync::Arc<dyn volume::VolumeReader>>,
        network_reader: Option<std::sync::Arc<dyn network::NetworkReader>>,
    ) -> Self {
        Self {
            registry,
            cursor_config,
            token_issuer,
            server_reader,
            volume_reader,
            network_reader,
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
        .route("/network/address-realms", get(network::list_address_realms))
        .route(
            "/network/address-realms/{id}",
            get(network::show_address_realm),
        )
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
            "/o3k/v1/network/address-realms",
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
        )
            .into_response();
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
        .into_response()
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
        )
            .into_response();
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
        .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::ServiceManifest;
    use o3k_kernel::manifest::{ManifestController, RegisteredResourceType, ResourceScope};
    use o3k_kernel::resource::ResourceType;

    fn test_manifest_registry() -> ManifestRegistry {
        let mut reg = ManifestRegistry::new();
        let m = ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "0.4.0".to_owned(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            resource_types: vec![
                RegisteredResourceType {
                    resource_type: ResourceType::new_unchecked("compute", "server"),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: ResourceScope::Tenant,
                },
                RegisteredResourceType {
                    resource_type: ResourceType::new_unchecked("compute", "flavor"),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: ResourceScope::Tenant,
                },
            ],
            actions: vec![
                "compute:ListServers".to_owned(),
                "compute:CreateServer".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: Some(ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
            health: None,
        };
        let _ = reg.register(m);
        reg
    }

    #[tokio::test]
    async fn api_root_returns_version() {
        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        );
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
        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        );
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
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        let state = NativeApiState::new(
            Some(reg),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        );
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
        let services = body["services"].as_array().unwrap();
        assert!(services.len() >= 3, "expected at least 3 seeded services");
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
        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        );
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
        assert!(body["count"].as_u64().unwrap_or(0) >= 2);
    }

    #[tokio::test]
    async fn endpoint_without_bearer_returns_401() {
        let state = NativeApiState::default();
        let app = router(state);
        // Identity/me requires auth
        let response = axum::http::Request::builder()
            .uri("/identity/me")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(app, response).await.unwrap(),
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get("Content-Type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/problem+json"
        );
    }
}
