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
use o3k_kernel::{LocationRegistry, ManifestRegistry, ServiceLifecycleState, ServiceManifest};
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
pub mod resource_contract;
pub mod volume;

use resource::{LifecycleOperation, ResourceDescriptor};

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
    /// Canonical O3K location topology (regions and availability domains).
    /// This is the single authoritative location source; service manifests
    /// reference canonical IDs only. See ADR-0181 / SPEC-0038.
    pub locations: Option<LocationRegistry>,
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
            locations: None,
        })
    }

    /// Sets the canonical location registry served by the native API.
    #[must_use]
    pub fn with_locations(mut self, locations: LocationRegistry) -> Self {
        self.locations = Some(locations);
        self
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
        .route(
            "/resource-schemas/{namespace}/{collection}/{version}",
            get(discover_resource_schema),
        )
        .route("/regions", get(discover_regions))
        .route("/identity/tokens", post(identity::issue_token))
        .route(
            "/identity/scopes",
            post(identity::discover_federated_scopes),
        )
        .route("/identity/me", get(identity::current_context))
        .route("/operator/profile", get(identity::operator_profile))
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
        .route("/operations", get(operation::list_operations))
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
pub(crate) fn assert_location_discovery_schema(value: &serde_json::Value) {
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/native-location-discovery-v1.schema.json"
    )))
    .expect("valid native location discovery schema");
    let validator = jsonschema::validator_for(&schema).expect("compiled native location schema");
    if let Err(errors) = validator.validate(value) {
        panic!("native location discovery schema violation: {errors}");
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
            "/o3k/v1/resource-schemas/{namespace}/{collection}/{version}",
            "/o3k/v1/regions",
            "/o3k/v1/identity/tokens",
            "/o3k/v1/identity/scopes",
            "/o3k/v1/identity/me",
            "/o3k/v1/operator/profile",
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
    /// Placement scope derived from the owning service's canonical region
    /// declaration: `"global"` (no regional restriction) or `"regional"`.
    placement: String,
    /// Canonical region IDs the resource is available in (`regional` only).
    /// Empty for global resources so global placement is never falsely
    /// advertised as regional.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    regions: Vec<String>,
    /// Availability-domain selection capability: `"unsupported"`, `"optional"`
    /// or `"required"`, derived from the owning service's canonical presence of
    /// a region/AZ declaration. Provider/host/backend identity is never
    /// exposed here.
    availability_domain_selection: String,
    schema: SchemaReference,
    actions: Vec<ActionSchemaMetadata>,
}

#[derive(Serialize, Clone)]
struct SchemaReference {
    id: String,
    version: String,
    representation: String,
}

#[derive(Serialize, Clone)]
struct ActionSchemaMetadata {
    name: String,
    action_id: String,
    target: String,
    input: Option<String>,
    output: Option<String>,
    asynchronous: bool,
}

fn schema_id(namespace: &str, collection: &str, version: &str) -> String {
    format!("https://o3k.io/schemas/{namespace}/{collection}/{version}/resource")
}

fn create_input_schema_id(namespace: &str, collection: &str, version: &str) -> String {
    format!(
        "{}#/allOf/1/properties/spec",
        schema_id(namespace, collection, version)
    )
}

fn action_metadata(descriptor: &ResourceDescriptor) -> Vec<ActionSchemaMetadata> {
    let mut actions: Vec<_> =
        descriptor
            .lifecycle_actions
            .iter()
            .map(|(operation, action)| {
                let name = format!("{operation:?}").to_lowercase();
                let target = match operation {
                    LifecycleOperation::List | LifecycleOperation::Create => "collection",
                    LifecycleOperation::Show
                    | LifecycleOperation::Update
                    | LifecycleOperation::Delete => "instance",
                };
                ActionSchemaMetadata {
                name,
                action_id: action.to_string(),
                target: target.to_owned(),
                    input: match operation {
                    LifecycleOperation::Create => resource_contract::ContractKind::for_resource(
                        &descriptor.resource_type.to_string(),
                        &descriptor.schema_version,
                    ).map(|_| create_input_schema_id(
                        descriptor.resource_type.namespace(),
                        &descriptor.collection,
                        &descriptor.schema_version,
                    )),
                    _ => None,
                },
                output: Some(match operation {
                    LifecycleOperation::List =>
                        "https://o3k.io/contracts/native-resource-list-response-v1.schema.json",
                    LifecycleOperation::Show =>
                        "https://o3k.io/contracts/native-resource-envelope-v1.schema.json",
                    LifecycleOperation::Create
                    | LifecycleOperation::Update
                    | LifecycleOperation::Delete =>
                        "https://o3k.io/contracts/native-mutation-result-v1.schema.json",
                }.to_owned()),
                // Lifecycle mutations return a canonical operation_id. Reads
                // do not create operations, but are still represented here.
                asynchronous: matches!(
                    operation,
                    LifecycleOperation::Create
                        | LifecycleOperation::Update
                        | LifecycleOperation::Delete
                ),
            }
            })
            .collect();
    actions.sort_by(|a, b| a.name.cmp(&b.name));
    actions
}

#[derive(Serialize)]
pub struct ResourceTypesResponse {
    resource_types: Vec<DiscoveredResourceType>,
    count: usize,
}

/// Derives the generic placement semantics for an owning service manifest.
///
/// Placement is *derived* from the single canonical manifest region/AZ
/// declaration — there is no separate placement authority that could drift.
/// Global/regional scope is mutually exclusive by construction (both are
/// functions of the same `regions` field); availability-domain selection is
/// orthogonal placement metadata (see ADR-0181/SPEC-0038).
///
/// Returns `(scope, canonical_regions, availability_domain_selection)`.
fn placement_for_service(
    manifest: &ServiceManifest,
    locations: &Option<LocationRegistry>,
) -> (String, Vec<String>, String) {
    let location_ids = locations
        .as_ref()
        .map(|locations| {
            locations
                .regions()
                .iter()
                .map(|region| region.id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();

    // Only disclose region IDs that are canonical O3K location identity.
    // Declared-but-unknown regions are filtered out (fail closed).
    let mut regions: Vec<String> = manifest
        .regions
        .iter()
        .filter(|region| location_ids.contains(region.as_str()))
        .cloned()
        .collect();
    regions.sort();
    regions.dedup();

    let scope = if manifest.regions.is_empty() {
        "global".to_owned()
    } else {
        "regional".to_owned()
    };

    let availability_domain_selection = if manifest.availability_domains.is_empty() {
        "unsupported".to_owned()
    } else if manifest.regions.is_empty() {
        "required".to_owned()
    } else {
        "optional".to_owned()
    };

    (scope, regions, availability_domain_selection)
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
        let owning_service = state
            .lifecycle_registry
            .as_ref()
            .and_then(|registry| registry.read().ok())
            .and_then(|registry| registry.get(&descriptor.owning_service).cloned())
            .unwrap_or_else(|| ServiceManifest {
                manifest_version: 1,
                service_id: descriptor.owning_service.clone(),
                namespace: descriptor.resource_type.namespace().to_owned(),
                service_version: String::new(),
                ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
                resource_types: vec![],
                actions: vec![],
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: None,
                health: None,
            });
        let (placement, regions, availability_domain_selection) =
            placement_for_service(&owning_service, &state.locations);
        resource_types.push(DiscoveredResourceType {
            namespace: descriptor.resource_type.namespace().to_owned(),
            name: descriptor.resource_type.name().to_owned(),
            service: descriptor.owning_service.clone(),
            schema_version: descriptor.schema_version.clone(),
            collection: descriptor.collection.clone(),
            scope: descriptor.scope.to_string(),
            ready: state.resource_index.is_ready(descriptor),
            lifecycle_actions: actions,
            placement,
            regions,
            availability_domain_selection,
            schema: SchemaReference {
                id: schema_id(
                    descriptor.resource_type.namespace(),
                    &descriptor.collection,
                    &descriptor.schema_version,
                ),
                version: descriptor.schema_version.clone(),
                representation: "native-resource-envelope".to_owned(),
            },
            actions: action_metadata(descriptor),
        });
    }
    resource_types.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));

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

#[derive(serde::Deserialize)]
pub struct ResourceSchemaPath {
    namespace: String,
    collection: String,
    version: String,
}

/// Returns the common envelope schema reference for a manifest-derived
/// resource descriptor. The version is resolved against the descriptor, so a
/// schema cannot be requested for an undeclared resource/version pair.
pub async fn discover_resource_schema(
    State(state): State<NativeApiState>,
    axum::extract::Path(path): axum::extract::Path<ResourceSchemaPath>,
) -> impl IntoResponse {
    let Some(descriptor) = state
        .resource_index
        .resolve(&path.namespace, &path.collection)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "resource schema not found"})),
        )
            .into_response();
    };
    if descriptor.schema_version != path.version {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "resource schema not found"})),
        )
            .into_response();
    }
    let resource_type = descriptor.resource_type.to_string();
    let Some(contract) =
        resource_contract::ContractKind::for_resource(&resource_type, &path.version)
    else {
        // A declared resource without a registered public contract must not
        // be advertised as the misleading `spec: object` contract.
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "resource schema not available"})),
        )
            .into_response();
    };
    let spec_schema = contract.schema();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": schema_id(&path.namespace, &path.collection, &path.version),
            "title": format!("O3K {}:{} resource {}", path.namespace, path.collection, path.version),
            "description": "Canonical native resource representation: the common envelope with a resource-specific, typed spec. Status remains represented only when returned by the owning service.",
            "allOf": [
                {"$ref": "https://o3k.io/contracts/native-resource-envelope-v1.schema.json"},
                {"type": "object", "properties": {"spec": spec_schema}, "required": ["spec"]}
            ],
            "x-o3k-resource-type": resource_type,
            "x-o3k-schema-version": descriptor.schema_version,
        })),
    ).into_response()
}

// ── Region location discovery ──────────────────────────────────────────────
//
// Exposes the canonical O3K location topology. O3K is the single authority for
// region and availability-domain identity; this endpoint never derives or
// invents locations from hosts, providers, or backends. See ADR-0181/SPEC-0038.

#[derive(Serialize)]
pub struct DiscoveredAvailabilityDomain {
    id: String,
}

#[derive(Serialize)]
pub struct DiscoveredRegion {
    id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    availability_domains: Vec<DiscoveredAvailabilityDomain>,
}

#[derive(Serialize)]
pub struct RegionsResponse {
    regions: Vec<DiscoveredRegion>,
    count: usize,
}

pub async fn discover_regions(State(state): State<NativeApiState>) -> impl IntoResponse {
    let Some(ref locations) = state.locations else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"regions": [], "count": 0})),
        )
            .into_response();
    };

    let regions: Vec<DiscoveredRegion> = locations
        .regions()
        .iter()
        .map(|region| DiscoveredRegion {
            id: region.id.clone(),
            availability_domains: region
                .availability_domains
                .iter()
                .map(|az| DiscoveredAvailabilityDomain { id: az.id.clone() })
                .collect(),
        })
        .collect();

    let count = regions.len();
    (
        StatusCode::OK,
        Json(serde_json::to_value(RegionsResponse { regions, count }).unwrap_or_default()),
    )
        .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::manifest::{ManifestController, RegisteredResourceType, ResourceScope};
    use o3k_kernel::resource::ResourceType;
    use o3k_kernel::{
        AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, ScopeKind, ServiceManifest,
        UserPrincipal,
    };

    #[derive(Clone)]
    struct TestIssuer(AuthContext);

    #[async_trait::async_trait]
    impl auth::TokenIssuer for TestIssuer {
        async fn issue_native(
            &self,
            _request: &auth::NativeTokenRequestV1,
        ) -> Result<(String, serde_json::Value), error::ProblemDetails> {
            Err(error::ProblemDetails::unauthorized())
        }

        async fn auth_context(&self, _token: &str) -> Result<AuthContext, error::ProblemDetails> {
            Ok(self.0.clone())
        }
    }

    fn test_operator_context(system: bool) -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked("operator-1"),
                "operator",
                None,
            )),
            OwnershipScope::new(
                ScopeId::new_unchecked(if system { "system" } else { "project-a" }),
                if system {
                    ScopeKind::System
                } else {
                    ScopeKind::Project
                },
                None,
                None,
            ),
            vec!["operator".to_owned()],
            1,
            2,
            "audit-test",
            "request-test",
            None,
        )
    }

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

    #[test]
    fn create_action_input_reference_targets_the_typed_spec_fragment() {
        assert_eq!(
            create_input_schema_id("compute", "servers", "v1"),
            "https://o3k.io/schemas/compute/servers/v1/resource#/allOf/1/properties/spec"
        );
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
        assert!(
            body["endpoints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|endpoint| endpoint == "/o3k/v1/identity/scopes")
        );
    }

    #[tokio::test]
    async fn operator_profile_route_is_server_authorized_and_system_scoped() {
        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer(test_operator_context(true)))),
            None,
            None,
            None,
        )
        .unwrap()
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .uri("/operator/profile")
                .header("authorization", "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer(test_operator_context(false)))),
            None,
            None,
            None,
        )
        .unwrap()
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .uri("/operator/profile")
                .header("authorization", "Bearer test-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn federated_scope_discovery_rejects_empty_external_credentials() {
        let state = NativeApiState::new(
            Some(test_manifest_registry()),
            pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer(test_operator_context(false)))),
            None,
            None,
            None,
        )
        .unwrap();
        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri("/identity/scopes")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"federated":{"access_token":""}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

    // ── Location & placement discovery (issue #887) ────────────────────

    fn test_locations() -> o3k_kernel::LocationRegistry {
        use o3k_kernel::{AvailabilityDomain, RegionDeclaration};
        o3k_kernel::LocationRegistry::from_declarations(vec![
            RegionDeclaration {
                id: "region-a".to_owned(),
                availability_domains: vec![
                    AvailabilityDomain {
                        id: "az-1".to_owned(),
                    },
                    AvailabilityDomain {
                        id: "az-2".to_owned(),
                    },
                ],
            },
            RegionDeclaration {
                id: "region-b".to_owned(),
                availability_domains: vec![AvailabilityDomain {
                    id: "az-3".to_owned(),
                }],
            },
        ])
        .unwrap()
    }

    /// Builds a manifest declaring the given canonical region/AZ scope.
    ///
    /// Uses real O3K resource types (`image:image`, `network:address_realm`,
    /// `volume:volume`) rather than fictional test concepts.
    fn scoped_manifest(
        service_id: &str,
        namespace: &str,
        resource_type_name: &str,
        regions: Vec<&str>,
        availability_domains: Vec<&str>,
    ) -> ServiceManifest {
        ServiceManifest {
            manifest_version: 1,
            service_id: service_id.to_owned(),
            namespace: namespace.to_owned(),
            service_version: "0.4.0".to_owned(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new_unchecked(namespace, resource_type_name),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec![format!("{namespace}:ListPrimary")],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: regions.into_iter().map(ToOwned::to_owned).collect(),
            availability_domains: availability_domains
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            controller: Some(ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
            health: None,
        }
    }

    fn state_with_locations(
        registry: Option<ManifestRegistry>,
        locations: Option<o3k_kernel::LocationRegistry>,
    ) -> NativeApiState {
        NativeApiState::new(
            registry,
            pagination::CursorConfig::default(),
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .with_locations(locations.unwrap_or_default())
    }

    async fn get_json(state: NativeApiState, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = axum::response::Response::from(
            tower::ServiceExt::oneshot(
                router(state),
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        );
        let status = resp.status();
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn discover_regions_exposes_multiple_configured_regions() {
        let (status, body) = get_json(
            state_with_locations(None, Some(test_locations())),
            "/regions",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["count"].as_u64().unwrap_or(0), 2);
        let ids: Vec<&str> = body["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|region| region["id"].as_str())
            .collect();
        assert!(ids.contains(&"region-a"));
        assert!(ids.contains(&"region-b"));
        assert_location_discovery_schema(&body);
    }

    #[tokio::test]
    async fn discover_regions_region_has_multiple_availability_domains() {
        let (_, body) = get_json(
            state_with_locations(None, Some(test_locations())),
            "/regions",
        )
        .await;
        let region_a = body["regions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|region| region["id"] == "region-a")
            .unwrap();
        let azs: Vec<&str> = region_a["availability_domains"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|az| az["id"].as_str())
            .collect();
        assert!(azs.contains(&"az-1"));
        assert!(azs.contains(&"az-2"));
    }

    #[tokio::test]
    async fn discover_regions_order_is_deterministic() {
        // Declare regions in non-sorted order so the test genuinely exercises
        // the endpoint's deterministic ordering (would fail if sorting dropped).
        use o3k_kernel::{AvailabilityDomain, RegionDeclaration};
        let unsorted_input = o3k_kernel::LocationRegistry::from_declarations(vec![
            RegionDeclaration {
                id: "region-b".to_owned(),
                availability_domains: Vec::new(),
            },
            RegionDeclaration {
                id: "region-a".to_owned(),
                availability_domains: vec![AvailabilityDomain {
                    id: "az-1".to_owned(),
                }],
            },
        ])
        .unwrap();
        let (_, body) =
            get_json(state_with_locations(None, Some(unsorted_input)), "/regions").await;
        let ids: Vec<String> = body["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|region| region["id"].as_str().map(ToOwned::to_owned))
            .collect();
        assert_eq!(ids, vec!["region-a".to_owned(), "region-b".to_owned()]);
    }

    #[tokio::test]
    async fn discover_regions_empty_when_none_configured() {
        let (status, body) = get_json(state_with_locations(None, None), "/regions").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["count"].as_u64().unwrap_or(1), 0);
        assert_eq!(body["regions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn resource_type_global_does_not_advertise_regional_placement() {
        let mut registry = ManifestRegistry::new();
        registry
            .register(scoped_manifest("image", "image", "image", vec![], vec![]))
            .unwrap();
        let (_, body) = get_json(
            state_with_locations(Some(registry), Some(test_locations())),
            "/resource-types",
        )
        .await;
        let resource = &body["resource_types"][0];
        assert_eq!(resource["placement"], "global");
        // `regions` must be entirely absent for a global resource (never an
        // empty or populated regional advertisement).
        assert!(
            resource.get("regions").is_none(),
            "global resource must not advertise regional placement"
        );
        assert_eq!(resource["availability_domain_selection"], "unsupported");
    }

    #[tokio::test]
    async fn resource_type_regional_exposes_only_authoritative_regions() {
        let mut registry = ManifestRegistry::new();
        // Declares one canonical region and one unknown region; only the
        // canonical region may be disclosed (fail closed on unknown).
        registry
            .register(scoped_manifest(
                "network",
                "network",
                "address_realm",
                vec!["region-a", "region-b", "ghost-region"],
                vec![],
            ))
            .unwrap();
        let (_, body) = get_json(
            state_with_locations(Some(registry), Some(test_locations())),
            "/resource-types",
        )
        .await;
        let resource = &body["resource_types"][0];
        assert_eq!(resource["placement"], "regional");
        let regions: Vec<String> = resource["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|region| region.as_str().map(ToOwned::to_owned))
            .collect();
        assert_eq!(regions, vec!["region-a".to_owned(), "region-b".to_owned()]);
        assert!(!regions.contains(&"ghost-region".to_owned()));
    }

    #[tokio::test]
    async fn resource_type_regional_exposes_only_declared_canonical_subset() {
        let mut registry = ManifestRegistry::new();
        // region-b is canonical but the service only declares region-a: the
        // disclosed regions must be exactly the declared subset, never all
        // canonical regions.
        registry
            .register(scoped_manifest(
                "network",
                "network",
                "address_realm",
                vec!["region-a"],
                vec![],
            ))
            .unwrap();
        let (_, body) = get_json(
            state_with_locations(Some(registry), Some(test_locations())),
            "/resource-types",
        )
        .await;
        let resource = &body["resource_types"][0];
        assert_eq!(resource["placement"], "regional");
        let regions: Vec<String> = resource["regions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|region| region.as_str().map(ToOwned::to_owned))
            .collect();
        assert_eq!(regions, vec!["region-a".to_owned()]);
    }

    #[tokio::test]
    async fn resource_type_az_aware_exposes_capability_without_provider_leakage() {
        let mut registry = ManifestRegistry::new();
        registry
            .register(scoped_manifest(
                "volume",
                "volume",
                "volume",
                vec!["region-a"],
                vec!["az-1"],
            ))
            .unwrap();
        let (_, body) = get_json(
            state_with_locations(Some(registry), Some(test_locations())),
            "/resource-types",
        )
        .await;
        let resource = &body["resource_types"][0];
        assert_eq!(resource["placement"], "regional");
        assert_eq!(resource["availability_domain_selection"], "optional");
        // No provider/host/backend identity is ever disclosed: the placement
        // payload contains only canonical region/AZ IDs and capability verbs.
        let serialized = serde_json::to_string(resource).unwrap();
        for leaked in [
            "provider",
            "hypervisor",
            "ceph",
            "node-id",
            "pool-name",
            "backend",
        ] {
            assert!(
                !serialized.contains(leaked),
                "provider/host/backend token {leaked} leaked into placement"
            );
        }
    }

    #[tokio::test]
    async fn resource_type_az_required_when_only_az_declared() {
        let mut registry = ManifestRegistry::new();
        registry
            .register(scoped_manifest(
                "volume",
                "volume",
                "volume",
                vec![],
                vec!["az-1", "az-2"],
            ))
            .unwrap();
        let (_, body) = get_json(
            state_with_locations(Some(registry), Some(test_locations())),
            "/resource-types",
        )
        .await;
        assert_eq!(
            body["resource_types"][0]["availability_domain_selection"],
            "required"
        );
    }

    #[tokio::test]
    async fn resource_types_order_is_deterministic() {
        let mut registry = ManifestRegistry::new();
        registry.seed_core().unwrap();
        let state = state_with_locations(Some(registry), Some(test_locations()));
        for _ in 0..3 {
            let (_, body) = get_json(state.clone(), "/resource-types").await;
            let keys: Vec<String> = body["resource_types"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| format!("{}:{}", r["namespace"], r["name"]))
                .collect();
            assert_eq!(keys, {
                let mut sorted = keys.clone();
                sorted.sort();
                sorted
            });
        }
    }
}
