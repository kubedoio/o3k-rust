//! O3K public API adapter crate root: the public surface (`AppState`,
//! `router`, `router_with_state`), router composition, and version/health
//! discovery. Protocol adapters live in the sibling modules (`identity`,
//! `image`, `network`, `compute`, `placement`, `volume_attachment`), shared
//! helpers in `auth` (token validation) and `error` (error envelopes), and
//! the router-wide microversion negotiation in `middleware`. Axum
//! routing/extractors and OpenStack JSON wire models stay in this crate.
//!
//! Note: this crate also hosts the native API routes (o3k-native-api) when
//! `AppState.native_api` is configured. This is a pragmatic composition
//! choice: both the OpenStack and native adapters are northbound protocol
//! adapters over the same canonical application services, and sharing the
//! axum Router/state type avoids a complex nested-routing layer. The
//! architectural intent (sibling adapters) is unchanged — see ADR-0173 §13.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
};
use o3k_compute::ComputeService;
use o3k_compute_agent::NodeRegistry;
use o3k_console::ConsoleService;
use o3k_identity::TokenService;
use o3k_image::ImageService;
use o3k_native_api::NativeApiState;
use o3k_network::NetworkService;
use o3k_network::PublicAddressAllocator;
use serde::Serialize;

mod auth;
mod compute;
mod error;
mod identity;
mod image;
mod middleware;
mod network;
mod placement;
mod volume;
mod volume_attachment;

pub use network::recover_l3_gateway_operations;

use crate::{
    compute::{
        create_flavor, create_keypair, create_server, delete_flavor, delete_keypair, delete_server,
        list_flavor_extra_specs, list_flavors, list_keypairs, list_servers, server_action,
        show_flavor, show_keypair, show_server, show_server_metadata, update_server,
    },
    identity::{check_token, issue_token, validate_token},
    image::{create_image, delete_image, download_image, list_images, show_image, upload_image},
    middleware::{compatibility_trace_middleware, microversion_middleware},
    network::{
        add_router_interface, create_floating_ip, create_network, create_network_policy,
        create_port, create_router, create_security_group, create_security_group_rule,
        create_subnet, delete_floating_ip, delete_network, delete_network_policy, delete_port,
        delete_router, delete_security_group, delete_security_group_rule, delete_subnet,
        list_extensions, list_floating_ips, list_network_policies, list_networks, list_ports,
        list_routers, list_security_group_rules, list_security_groups, list_subnets,
        remove_router_interface, show_floating_ip, show_network, show_network_policy, show_port,
        show_router, show_security_group, show_security_group_rule, show_subnet,
        update_floating_ip, update_network, update_network_policy, update_port, update_router,
        update_security_group, update_subnet,
    },
    placement::placement_discovery,
    volume_attachment::{
        attach_volume, delete_volume_attachment, list_volume_attachments, show_volume_attachment,
    },
};

/// The public console request and the agent observation must share one
/// bounded budget.  The protected real-host harness allows fifteen seconds
/// for a console query, so the API must not expire the agent dispatch sooner.
pub const CONSOLE_AGENT_DISPATCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// Builds the default router: `AppState::new()` marked ready immediately and
/// passed to [`router_with_state`]. Intended for tests and standalone runs.
pub fn router() -> Router {
    let state = AppState::new();
    state.set_ready(true);
    router_with_state(state)
}

/// Composition-root state for the O3K API router: the optional service
/// instances (identity, image, network, compute, console, agent registry)
/// that the protocol adapters dispatch to, plus the readiness flag.
#[derive(Clone, Default)]
pub struct AppState {
    ready: Arc<AtomicBool>,
    identity: Option<Arc<TokenService>>,
    image: Option<Arc<ImageService>>,
    network: Option<Arc<NetworkService>>,
    public_allocator: Option<Arc<PublicAddressAllocator>>,
    network_external_realm_id: Option<uuid::Uuid>,
    network_dispatcher: Option<Arc<dyn o3k_network::NetworkPlanDispatcher>>,
    network_controller: Option<o3k_network::NetworkControllerLease>,
    network_agent: Option<o3k_network::NetworkAgentIdentity>,
    compute: Option<Arc<ComputeService>>,
    console: Option<Arc<ConsoleService>>,
    agent_registry: Option<NodeRegistry>,
    volume_attachments_enabled: bool,
    storage_store: Option<Arc<dyn o3k_store::StorageRepository>>,
    storage_provider: Option<Arc<dyn o3k_storage::StorageProvider>>,
    native_api: Option<NativeApiState>,
}

impl FromRef<AppState> for NativeApiState {
    fn from_ref(state: &AppState) -> Self {
        state.native_api.clone().unwrap_or_default()
    }
}

impl AppState {
    /// Creates an empty state with no services configured and readiness unset.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the router ready (or not) for `/readyz` reporting.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    /// Configures the Keystone-compatible token service.
    #[must_use]
    pub fn with_identity(mut self, service: TokenService) -> Self {
        self.identity = Some(Arc::new(service));
        self
    }

    /// Configures the Glance-compatible image service.
    #[must_use]
    pub fn with_image(mut self, service: ImageService) -> Self {
        self.image = Some(Arc::new(service));
        self
    }

    /// Configures the Neutron-compatible network service.
    #[must_use]
    pub fn with_network(mut self, service: NetworkService) -> Self {
        self.network = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_public_allocator(mut self, allocator: PublicAddressAllocator) -> Self {
        self.public_allocator = Some(Arc::new(allocator));
        self
    }

    #[must_use]
    pub fn with_network_external_realm(mut self, realm_id: uuid::Uuid) -> Self {
        self.network_external_realm_id = Some(realm_id);
        self
    }

    #[must_use]
    pub fn with_network_dispatcher(
        mut self,
        dispatcher: Arc<dyn o3k_network::NetworkPlanDispatcher>,
        controller: o3k_network::NetworkControllerLease,
    ) -> Self {
        self.network_dispatcher = Some(dispatcher);
        self.network_controller = Some(controller);
        self
    }

    /// Configures the explicitly selected host-local network executor. This
    /// identity is used only when the network executor is intentionally
    /// separate from the compute-agent registry; the dispatcher still proves
    /// liveness and fencing over the authenticated network-agent transport.
    #[must_use]
    pub fn with_network_agent_identity(mut self, agent: o3k_network::NetworkAgentIdentity) -> Self {
        self.network_agent = Some(agent);
        self
    }

    /// Configures the Nova-compatible compute service.
    #[must_use]
    pub fn with_compute(mut self, service: ComputeService) -> Self {
        self.compute = Some(Arc::new(service));
        self
    }

    /// Configures the console service used for serial-console output.
    #[must_use]
    pub fn with_console(mut self, service: ConsoleService) -> Self {
        self.console = Some(Arc::new(service));
        self
    }

    /// Configures the compute-agent node registry used for agent dispatch.
    #[must_use]
    pub fn with_agent_registry(mut self, registry: NodeRegistry) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Enables the external-Cinder Nova attachment surface for an explicitly
    /// configured hosted-service profile. The native ephemeral-root profile
    /// leaves this disabled, so attachment routes are not publicly registered.
    #[must_use]
    pub fn with_volume_attachments_enabled(mut self, enabled: bool) -> Self {
        self.volume_attachments_enabled = enabled;
        self
    }

    /// Configures the canonical native storage repository used by the
    /// bounded Cinder projection.  The compatibility adapter never stores a
    /// second volume authority.
    #[must_use]
    pub fn with_storage_store<S>(mut self, store: Arc<S>) -> Self
    where
        S: o3k_store::StorageRepository + 'static,
    {
        self.storage_store = Some(store);
        self
    }

    /// Configures the optional native storage execution provider.  Provider
    /// observations update canonical state; they never replace it.
    #[must_use]
    pub fn with_storage_provider(
        mut self,
        provider: Arc<dyn o3k_storage::StorageProvider>,
    ) -> Self {
        self.storage_provider = Some(provider);
        self
    }

    /// Configures the native O3K API (ADR-0173/SPEC-0030) discovery and
    /// resource endpoints under `/o3k/v1`.
    #[must_use]
    pub fn with_native_api(mut self, state: NativeApiState) -> Self {
        self.native_api = Some(state);
        self
    }

    /// Reports whether the router is ready (`/readyz` returns 200 when true).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

/// Builds the O3K API router from the given composition-root state.
///
/// Route registration is the single source of public API truth: paths,
/// method bindings, and layer order (microversion negotiation middleware,
/// then the image upload body-size limit) must not change without a
/// compatibility record update.
pub fn router_with_state(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/", get(keystone_root))
        .route("/v3", get(keystone_v3))
        .route(
            "/v3/{project_id}/volumes",
            get(volume::list).post(volume::create),
        )
        .route(
            "/v3/{project_id}/volumes/{id}",
            get(volume::show).put(volume::update).delete(volume::delete),
        )
        .route("/v2.1", get(nova_v2_1_version))
        .route("/v2.1/", get(nova_v2_1_version))
        .route("/placement", get(placement_discovery))
        .route("/placement/", get(placement_discovery))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route(
            "/v3/auth/tokens",
            post(issue_token).get(validate_token).head(check_token),
        )
        .route("/v2/images", get(list_images).post(create_image))
        .route("/v2/images/{id}", get(show_image).delete(delete_image))
        .route(
            "/v2/images/{id}/file",
            get(download_image).put(upload_image),
        )
        .route("/v2.0/extensions", get(list_extensions))
        .route("/v2.0/networks", get(list_networks).post(create_network))
        .route(
            "/v2.0/networks/{id}",
            get(show_network).put(update_network).delete(delete_network),
        )
        .route("/v2.0/subnets", get(list_subnets).post(create_subnet))
        .route(
            "/v2.0/subnets/{id}",
            get(show_subnet).put(update_subnet).delete(delete_subnet),
        )
        .route("/v2.0/ports", get(list_ports).post(create_port))
        .route(
            "/v2.0/ports/{id}",
            get(show_port).put(update_port).delete(delete_port),
        )
        .route(
            "/v2.0/security-groups",
            get(list_security_groups).post(create_security_group),
        )
        .route(
            "/v2.0/security-groups/{id}",
            get(show_security_group)
                .put(update_security_group)
                .delete(delete_security_group),
        )
        .route(
            "/v2.0/security-group-rules",
            get(list_security_group_rules).post(create_security_group_rule),
        )
        .route(
            "/v2.0/security-group-rules/{id}",
            get(show_security_group_rule).delete(delete_security_group_rule),
        )
        .route(
            "/v2.0/network-policies",
            get(list_network_policies).post(create_network_policy),
        )
        .route(
            "/v2.0/network-policies/{id}",
            get(show_network_policy)
                .put(update_network_policy)
                .delete(delete_network_policy),
        )
        .route("/v2.0/routers", get(list_routers).post(create_router))
        .route(
            "/v2.0/routers/{id}",
            get(show_router).put(update_router).delete(delete_router),
        )
        .route(
            "/v2.0/routers/{id}/add_router_interface",
            put(add_router_interface),
        )
        .route(
            "/v2.0/routers/{id}/remove_router_interface",
            put(remove_router_interface),
        )
        .route(
            "/v2.0/floatingips",
            get(list_floating_ips).post(create_floating_ip),
        )
        .route(
            "/v2.0/floatingips/{id}",
            get(show_floating_ip)
                .put(update_floating_ip)
                .delete(delete_floating_ip),
        )
        .route(
            "/v2.1/{project_id}/flavors",
            get(list_flavors).post(create_flavor),
        )
        .route("/v2.1/{project_id}/flavors/detail", get(list_flavors))
        .route(
            "/v2.1/{project_id}/flavors/{id}",
            get(show_flavor).delete(delete_flavor),
        )
        .route(
            "/v2.1/{project_id}/flavors/{id}/os-extra_specs",
            get(list_flavor_extra_specs),
        )
        .route(
            "/v2.1/{project_id}/os-keypairs",
            get(list_keypairs).post(create_keypair),
        )
        .route(
            "/v2.1/{project_id}/os-keypairs/{name}",
            get(show_keypair).delete(delete_keypair),
        )
        .route(
            "/v2.1/{project_id}/servers",
            get(list_servers).post(create_server),
        )
        .route("/v2.1/{project_id}/servers/detail", get(list_servers))
        .route(
            "/v2.1/{project_id}/servers/{id}/metadata",
            get(show_server_metadata),
        )
        .route(
            "/v2.1/{project_id}/servers/{id}",
            get(show_server).put(update_server).delete(delete_server),
        )
        .route(
            "/v2.1/{project_id}/servers/{id}/action",
            post(server_action),
        );
    if state.volume_attachments_enabled {
        router = router
            .route(
                "/v2.1/{project_id}/servers/{server_id}/os-volume_attachments",
                get(list_volume_attachments).post(attach_volume),
            )
            .route(
                "/v2.1/{project_id}/servers/{server_id}/os-volume_attachments/{attachment_id}",
                get(show_volume_attachment).delete(delete_volume_attachment),
            );
    }
    // Native API routes mounted at /o3k/v1/... (ADR-0173/SPEC-0030). Keep
    // these bindings in the composition root so handlers can extract the
    // normalized NativeApiState through AppState::FromRef.
    if state.native_api.is_some() {
        router = router
            .route("/o3k/v1", get(o3k_native_api::api_root))
            .route("/o3k/v1/services", get(o3k_native_api::discover_services))
            .route(
                "/o3k/v1/resource-types",
                get(o3k_native_api::discover_resource_types),
            )
            .route(
                "/o3k/v1/identity/tokens",
                post(o3k_native_api::identity::issue_token),
            )
            .route(
                "/o3k/v1/identity/me",
                get(o3k_native_api::identity::current_context),
            )
            .route(
                "/o3k/v1/compute/servers",
                get(o3k_native_api::compute::list_servers)
                    .post(o3k_native_api::resource::create_compute),
            )
            .route(
                "/o3k/v1/compute/servers/{id}",
                get(o3k_native_api::compute::show_server)
                    .delete(o3k_native_api::resource::delete_fixed),
            )
            .route(
                "/o3k/v1/volume/volumes",
                get(o3k_native_api::volume::list_volumes)
                    .post(o3k_native_api::resource::create_volume),
            )
            .route(
                "/o3k/v1/volume/volumes/{id}",
                get(o3k_native_api::volume::show_volume)
                    .delete(o3k_native_api::resource::delete_volume),
            )
            .route(
                "/o3k/v1/network/address-realms",
                get(o3k_native_api::network::list_address_realms),
            )
            .route(
                "/o3k/v1/network/address-realms/{id}",
                get(o3k_native_api::network::show_address_realm),
            )
            .route(
                "/o3k/v1/operations/{id}",
                get(o3k_native_api::operation::show_operation),
            );
        router = router
            .route(
                "/o3k/v1/{namespace}/{collection}",
                get(o3k_native_api::resource::list).post(o3k_native_api::resource::create),
            )
            .route(
                "/o3k/v1/{namespace}/{collection}/{id}",
                get(o3k_native_api::resource::show).delete(o3k_native_api::resource::delete),
            );
    }
    router
        .layer(axum::middleware::from_fn(compatibility_trace_middleware))
        .layer(axum::middleware::from_fn(microversion_middleware))
        .layer(axum::extract::DefaultBodyLimit::max(
            o3k_image::DEFAULT_MAX_UPLOAD_BYTES,
        ))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

fn keystone_version(endpoint: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "v3",
        "status": "stable",
        "updated": "2024-01-01T00:00:00Z",
        "links": [{"rel": "self", "href": format!("{endpoint}/v3")}],
        "media-types": [{"base": "application/json", "type": "application/vnd.openstack.identity-v3+json"}]
    })
}

fn identity_endpoint(state: &AppState) -> String {
    state.identity.as_ref().map_or_else(
        || "http://127.0.0.1:8080".to_owned(),
        |service| service.catalog_endpoint().to_owned(),
    )
}

async fn keystone_root(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::MULTIPLE_CHOICES,
        Json(
            serde_json::json!({"versions": {"values": [keystone_version(&identity_endpoint(&state))]}}),
        ),
    )
}

async fn keystone_v3(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"version": keystone_version(&identity_endpoint(&state))}))
}

async fn nova_v2_1_version() -> impl IntoResponse {
    Json(serde_json::json!({
        "version": {
            "id": "v2.1",
            "status": "CURRENT",
            "version": "2.1",
            "min_version": "2.1",
            "updated": "2013-07-23T00:00:00Z",
            "links": [
                {
                    "rel": "self",
                    "href": "/v2.1/"
                }
            ]
        }
    }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, Json(HealthResponse { status: "ready" }))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "not_ready",
            }),
        )
    }
}
