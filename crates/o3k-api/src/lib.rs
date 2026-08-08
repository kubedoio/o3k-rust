//! O3K public API adapter crate root: the public surface (`AppState`,
//! `router`, `router_with_state`), router composition, and version/health
//! discovery. Protocol adapters live in the sibling modules (`identity`,
//! `image`, `network`, `compute`, `placement`, `volume_attachment`);
//! Axum routing/extractors and OpenStack JSON wire models stay in this crate.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use o3k_compute::ComputeService;
use o3k_compute_agent::NodeRegistry;
use o3k_console::ConsoleService;
use o3k_identity::TokenService;
use o3k_image::ImageService;
use o3k_network::NetworkService;
use serde::Serialize;

mod auth;
mod compute;
mod error;
mod identity;
mod image;
mod middleware;
mod network;
mod placement;
mod volume_attachment;

use crate::{
    compute::{
        create_flavor, create_keypair, create_server, delete_flavor, delete_keypair, delete_server,
        list_flavors, list_keypairs, list_servers, server_action, show_flavor, show_keypair,
        show_server,
    },
    identity::{check_token, issue_token, validate_token},
    image::{create_image, delete_image, download_image, list_images, show_image, upload_image},
    middleware::microversion_middleware,
    network::{
        create_network, create_port, create_subnet, delete_network, delete_port, delete_subnet,
        list_extensions, list_networks, list_ports, list_subnets, show_network, show_port,
        show_subnet,
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

pub fn router() -> Router {
    let state = AppState::new();
    state.set_ready(true);
    router_with_state(state)
}

#[derive(Clone, Default)]
pub struct AppState {
    ready: Arc<AtomicBool>,
    identity: Option<Arc<TokenService>>,
    image: Option<Arc<ImageService>>,
    network: Option<Arc<NetworkService>>,
    compute: Option<Arc<ComputeService>>,
    console: Option<Arc<ConsoleService>>,
    agent_registry: Option<NodeRegistry>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    #[must_use]
    pub fn with_identity(mut self, service: TokenService) -> Self {
        self.identity = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_image(mut self, service: ImageService) -> Self {
        self.image = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_network(mut self, service: NetworkService) -> Self {
        self.network = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_compute(mut self, service: ComputeService) -> Self {
        self.compute = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_console(mut self, service: ConsoleService) -> Self {
        self.console = Some(Arc::new(service));
        self
    }

    #[must_use]
    pub fn with_agent_registry(mut self, registry: NodeRegistry) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

pub fn router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/", get(keystone_root))
        .route("/v3", get(keystone_v3))
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
            get(show_network).delete(delete_network),
        )
        .route("/v2.0/subnets", get(list_subnets).post(create_subnet))
        .route("/v2.0/subnets/{id}", get(show_subnet).delete(delete_subnet))
        .route("/v2.0/ports", get(list_ports).post(create_port))
        .route("/v2.0/ports/{id}", get(show_port).delete(delete_port))
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
            "/v2.1/{project_id}/servers/{id}",
            get(show_server).delete(delete_server),
        )
        .route(
            "/v2.1/{project_id}/servers/{id}/action",
            post(server_action),
        )
        .route(
            "/v2.1/{project_id}/servers/{server_id}/os-volume_attachments",
            get(list_volume_attachments).post(attach_volume),
        )
        .route(
            "/v2.1/{project_id}/servers/{server_id}/os-volume_attachments/{attachment_id}",
            get(show_volume_attachment).delete(delete_volume_attachment),
        )
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
