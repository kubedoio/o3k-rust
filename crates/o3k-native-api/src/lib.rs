//! O3K Native Resource API — service-namespaced REST surface over Cloud Kernel
//! resources.
//!
//! Sibling to `o3k-api` (OpenStack compatibility adapter). Both consume the
//! same canonical application/domain services. See ADR-0173, ADR-0174, SPEC-0030.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use o3k_kernel::{ManifestRegistry, ServiceLifecycleState};
use serde::Serialize;
use std::sync::{Arc, RwLock};

pub mod auth;
pub mod compute;
pub mod error;
pub mod identity;
pub mod network;
pub mod operation;
pub mod pagination;
pub mod resource;
pub mod volume;

/// Shared application state for the native API router.
#[derive(Clone, Default)]
pub struct NativeApiState {
    pub registry: Option<ManifestRegistry>,
    lifecycle_registry: Option<Arc<RwLock<ManifestRegistry>>>,
    pub cursor_config: pagination::CursorConfig,
    pub token_issuer: Option<std::sync::Arc<dyn auth::TokenIssuer>>,
    pub server_reader: Option<std::sync::Arc<dyn compute::ServerReader>>,
    pub volume_reader: Option<std::sync::Arc<dyn volume::VolumeReader>>,
    pub network_reader: Option<std::sync::Arc<dyn network::NetworkReader>>,
    pub operation_reader: Option<std::sync::Arc<dyn operation::OperationReader>>,
    /// Validated generic resource descriptors.  This is the northbound
    /// registry; applications below it are intentionally controller-agnostic.
    resource_index: resource::ResourceDispatcher,
    pub resource_application: Option<resource::SharedResourceApplication>,
    pub authorizer: Option<std::sync::Arc<dyn o3k_kernel::Authorizer>>,
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
    ) -> Result<Self, String> {
        let lifecycle_registry = registry.map(|registry| Arc::new(RwLock::new(registry)));
        let resource_index = lifecycle_registry
            .as_ref()
            .map(|registry| {
                resource::ResourceDispatcher::from_shared_manifest_registry(registry.clone())
            })
            .transpose()
            .map_err(|error| format!("native resource dispatcher construction failed: {error:?}"))?
            .unwrap_or_default();
        let registry = lifecycle_registry
            .as_ref()
            .and_then(|registry| registry.read().ok().map(|registry| registry.clone()));
        Ok(Self {
            registry,
            lifecycle_registry,
            cursor_config,
            token_issuer,
            server_reader,
            volume_reader,
            network_reader,
            operation_reader: None,
            resource_index,
            resource_application: None,
            authorizer: None,
        })
    }

    #[must_use]
    pub fn with_operation_reader(
        mut self,
        reader: std::sync::Arc<dyn operation::OperationReader>,
    ) -> Self {
        self.operation_reader = Some(reader);
        self
    }

    #[must_use]
    pub fn with_resource_application(
        mut self,
        application: resource::SharedResourceApplication,
    ) -> Self {
        self.resource_application = Some(application);
        self
    }

    #[must_use]
    pub fn with_authorizer(
        mut self,
        authorizer: std::sync::Arc<dyn o3k_kernel::Authorizer>,
    ) -> Self {
        self.authorizer = Some(authorizer);
        self
    }

    /// Returns the shared lifecycle registry used by native discovery and
    /// mutation gating. Runtime health transitions must update this registry
    /// so every cloned request state observes the same readiness.
    #[must_use]
    pub fn lifecycle_registry(&self) -> Option<Arc<RwLock<ManifestRegistry>>> {
        self.lifecycle_registry.clone()
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
        .route(
            "/{namespace}/{collection}",
            get(resource::list).post(resource::create),
        )
        .route(
            "/{namespace}/{collection}/{id}",
            get(resource::show).delete(resource::delete),
        )
        .route("/operations/{id}", get(operation::show_operation))
        .layer(DefaultBodyLimit::max(1_048_576))
        .with_state(state)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
pub(crate) fn assert_resource_envelope_schema(value: &serde_json::Value) {
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/native-resource-envelope-v1.schema.json"
    )))
    .expect("valid native envelope schema");
    let validator = jsonschema::validator_for(&schema).expect("compiled native envelope schema");
    if let Err(errors) = validator.validate(value) {
        panic!("native envelope schema violation: {errors}");
    }
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
            "/o3k/v1/operations/{id}",
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
    let Some(registry) = state.lifecycle_registry.as_ref() else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"services": [], "count": 0})),
        )
            .into_response();
    };

    let Ok(registry) = registry.read() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
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
    schema_version: String,
    collection: String,
    scope: String,
    ready: bool,
    lifecycle_actions: std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
pub struct ResourceTypesResponse {
    resource_types: Vec<DiscoveredResourceType>,
    count: usize,
}

pub async fn discover_resource_types(State(state): State<NativeApiState>) -> impl IntoResponse {
    if state.lifecycle_registry.is_none() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"resource_types": [], "count": 0})),
        )
            .into_response();
    };

    let mut resource_types: Vec<DiscoveredResourceType> = Vec::new();
    for descriptor in state.resource_index.all() {
        let mut actions = std::collections::HashMap::new();
        for (op, action) in &descriptor.lifecycle_actions {
            actions.insert(format!("{op:?}").to_lowercase(), action.to_string());
        }
        resource_types.push(DiscoveredResourceType {
            namespace: descriptor.resource_type.namespace().to_owned(),
            name: descriptor.resource_type.name().to_owned(),
            service: descriptor.owning_service.clone(),
            schema_version: descriptor.schema_version.clone(),
            collection: descriptor.collection.clone(),
            scope: descriptor.scope.to_string(),
            ready: state.resource_index.is_ready(descriptor),
            lifecycle_actions: actions,
        });
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
                    operations: std::collections::HashMap::new(),
                },
                RegisteredResourceType {
                    resource_type: ResourceType::new_unchecked("compute", "flavor"),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: ResourceScope::Tenant,
                    operations: std::collections::HashMap::new(),
                },
            ],
            actions: vec![
                "compute:ListServers".to_owned(),
                "compute:CreateServer".to_owned(),
                "compute:UpdateServer".to_owned(),
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        let mut registry = ManifestRegistry::new();
        registry.seed_core().unwrap();
        let state = NativeApiState::new(
            Some(registry),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
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
        let kinds: Vec<String> = body["resource_types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some(format!(
                    "{}:{}",
                    item["namespace"].as_str()?,
                    item["name"].as_str()?
                ))
            })
            .collect();
        assert!(kinds.iter().any(|kind| kind == "compute:server"));
        assert!(kinds.iter().any(|kind| kind == "network:address_realm"));
        assert!(kinds.iter().any(|kind| kind == "volume:volume"));
    }

    #[tokio::test]
    async fn resource_discovery_tracks_shared_readiness_transition() {
        let mut registry = ManifestRegistry::new();
        registry.seed_core().unwrap();
        registry
            .register_in_process_controller("compute", true, None)
            .unwrap();
        let state = NativeApiState::new(
            Some(registry),
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let lifecycle = state.lifecycle_registry().unwrap();
        let app = router(state);

        let request = || {
            axum::http::Request::builder()
                .uri("/resource-types")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let response = tower::ServiceExt::oneshot(app.clone(), request())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let compute = body["resource_types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["service"] == "compute")
            .unwrap();
        assert!(compute["ready"].as_bool().unwrap());

        lifecycle
            .write()
            .unwrap()
            .update_controller_health(
                "compute",
                o3k_kernel::controller::ControllerHealth {
                    healthy: false,
                    detail: Some("provider unavailable".to_owned()),
                    protocol_version: o3k_kernel::controller::ProtocolVersion::V1,
                },
            )
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request()).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let compute = body["resource_types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["service"] == "compute")
            .unwrap();
        assert!(!compute["ready"].as_bool().unwrap());
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
