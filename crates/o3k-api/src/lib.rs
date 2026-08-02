use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use o3k_compute::{ComputeError, ComputeService, Flavor, Server};
use o3k_compute_agent::NodeRegistry;
use o3k_console::{ConsoleError, ConsoleService};
use o3k_identity::{AuthError, TokenRequest, TokenService};
use o3k_image::{ImageError, ImageRecord, ImageService};
use o3k_network::{NetworkError, NetworkRecord, NetworkService, PortRecord, SubnetRecord};
use o3k_provider::InstanceAction;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;

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
        .route("/placement", get(placement_discovery))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v3/auth/tokens", post(issue_token))
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

async fn placement_discovery() -> impl IntoResponse {
    Json(serde_json::json!({
        "versions": [{"id": "1.0", "status": "stable", "links": [{"rel": "self", "href": "/placement"}]}]
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

async fn issue_token(
    State(state): State<AppState>,
    request: Result<Json<TokenRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(request)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid authentication request",
        );
    };
    let Some(service) = state.identity else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "identity is not configured",
        );
    };
    match service.issue(&request, SystemTime::now()) {
        Ok((value, response)) => {
            let Ok(subject_token) = HeaderValue::from_str(&value) else {
                return keystone_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "token could not be encoded",
                );
            };
            (
                StatusCode::CREATED,
                [
                    (
                        header::HeaderName::from_static("x-subject-token"),
                        subject_token,
                    ),
                    (header::VARY, HeaderValue::from_static("X-Auth-Token")),
                ],
                Json(response),
            )
                .into_response()
        }
        Err(AuthError::InvalidRequest) => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid authentication request",
        ),
        Err(AuthError::Unauthorized) => keystone_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The request has not been authenticated.",
        ),
        Err(AuthError::InvalidToken | AuthError::ExpiredToken | AuthError::WeakSigningKey) => {
            keystone_error(
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "The request has not been authenticated.",
            )
        }
    }
}

#[derive(Serialize)]
struct KeystoneErrorResponse {
    error: KeystoneErrorBody,
}

#[derive(Serialize)]
struct KeystoneErrorBody {
    code: u16,
    title: &'static str,
    message: &'static str,
}

fn keystone_error(
    status: StatusCode,
    title: &'static str,
    message: &'static str,
) -> axum::response::Response {
    (
        status,
        Json(KeystoneErrorResponse {
            error: KeystoneErrorBody {
                code: status.as_u16(),
                title,
                message,
            },
        }),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct CreateImageRequest {
    name: String,
    #[serde(default = "default_visibility")]
    visibility: String,
    container_format: String,
    disk_format: String,
}

#[derive(serde::Serialize)]
struct ImageResponse {
    id: String,
    name: String,
    owner: String,
    status: o3k_image::ImageStatus,
    visibility: String,
    container_format: String,
    disk_format: String,
    size: Option<u64>,
    checksum: Option<String>,
}

#[derive(serde::Serialize)]
struct ImageListResponse {
    images: Vec<ImageResponse>,
}

fn default_visibility() -> String {
    "private".to_owned()
}

fn image_response(image: ImageRecord) -> ImageResponse {
    ImageResponse {
        id: image.id.to_string(),
        name: image.name,
        owner: image.project_id,
        status: image.status,
        visibility: image.visibility,
        container_format: image.container_format,
        disk_format: image.disk_format,
        size: image.size,
        checksum: image.checksum,
    }
}

fn require_token(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<o3k_identity::VerifiedToken, axum::response::Response> {
    let Some(service) = &state.identity else {
        return Err(keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "identity is not configured",
        ));
    };
    let token = headers
        .get("x-auth-token")
        .or_else(|| headers.get("x-subject-token"))
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            keystone_error(
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "The request has not been authenticated.",
            )
        })?;
    service.verify(token, SystemTime::now()).map_err(|_| {
        keystone_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The request has not been authenticated.",
        )
    })
}

fn image_error(error: ImageError) -> axum::response::Response {
    match error {
        ImageError::NotFound => {
            keystone_error(StatusCode::NOT_FOUND, "Not Found", "image was not found")
        }
        ImageError::Conflict => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "image operation is not allowed",
        ),
        ImageError::InvalidMetadata => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid image metadata",
        ),
        ImageError::TooLarge => keystone_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request Entity Too Large",
            "image upload exceeds the configured limit",
        ),
        ImageError::UnsupportedFormat | ImageError::ChecksumMismatch | ImageError::InvalidPath => {
            keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "image content or path is invalid",
            )
        }
        ImageError::OverlayFailed | ImageError::FormatVerificationFailed => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "image overlay creation failed",
        ),
        ImageError::Storage(_) | ImageError::CorruptMetadata(_) => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "image storage is unavailable",
        ),
    }
}

async fn download_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.resolve_artifact(&token.project_id, id) {
        Ok(artifact) => {
            let mut response = (StatusCode::OK, artifact.content).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            if let Ok(value) = HeaderValue::from_str(&artifact.checksum) {
                response
                    .headers_mut()
                    .insert("x-image-meta-checksum", value);
            }
            response
        }
        Err(error) => image_error(error),
    }
}

async fn create_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<CreateImageRequest>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    let Ok(Json(request)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid image metadata",
        );
    };
    match service.create(
        &token.project_id,
        request.name,
        request.visibility,
        request.container_format,
        request.disk_format,
    ) {
        Ok(image) => (StatusCode::CREATED, Json(image_response(image))).into_response(),
        Err(error) => image_error(error),
    }
}

async fn list_images(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.list(&token.project_id) {
        Ok(images) => Json(ImageListResponse {
            images: images.into_iter().map(image_response).collect(),
        })
        .into_response(),
        Err(error) => image_error(error),
    }
}

async fn show_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.get(&token.project_id, id) {
        Ok(image) => Json(image_response(image)).into_response(),
        Err(error) => image_error(error),
    }
}

async fn upload_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    body: Bytes,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("application/octet-stream") {
        return keystone_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type",
            "image content must be application/octet-stream",
        );
    }
    if let Some(declared) = headers.get("x-openstack-image-size") {
        let Ok(declared) = declared
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or(())
        else {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "image size header is invalid",
            );
        };
        if declared != body.len() {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "declared image size does not match content",
            );
        }
    }
    if let Some(declared) = headers.get("x-openstack-image-sha256") {
        let Ok(declared) = declared.to_str() else {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "image checksum header is invalid",
            );
        };
        let actual = format!("{:x}", Sha256::digest(&body));
        if declared != actual {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "image checksum does not match content",
            );
        }
    }
    match service.upload(&token.project_id, id, &body) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_error(error),
    }
}

async fn delete_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.delete(&token.project_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_error(error),
    }
}

#[derive(serde::Deserialize)]
struct NetworkRequestBody {
    network: CreateNetworkRequest,
}
#[derive(serde::Deserialize)]
struct CreateNetworkRequest {
    name: String,
}
#[derive(serde::Serialize)]
struct NetworkEnvelope {
    network: NetworkResponse,
}
#[derive(serde::Serialize)]
struct NetworkList {
    networks: Vec<NetworkResponse>,
}
#[derive(serde::Serialize)]
struct NetworkResponse {
    id: String,
    name: String,
    project_id: String,
    status: String,
}

fn network_response(value: NetworkRecord) -> NetworkResponse {
    NetworkResponse {
        id: value.id.to_string(),
        name: value.name,
        project_id: value.project_id,
        status: value.status,
    }
}

#[derive(serde::Deserialize)]
struct SubnetRequestBody {
    subnet: CreateSubnetRequest,
}
#[derive(serde::Deserialize)]
struct CreateSubnetRequest {
    name: String,
    network_id: uuid::Uuid,
    cidr: String,
    gateway_ip: Option<Ipv4Addr>,
    allocation_pools: Option<Vec<AllocationPool>>,
}
#[derive(serde::Deserialize)]
struct AllocationPool {
    start: Ipv4Addr,
    end: Ipv4Addr,
}
#[derive(serde::Serialize)]
struct SubnetEnvelope {
    subnet: SubnetResponse,
}
#[derive(serde::Serialize)]
struct SubnetList {
    subnets: Vec<SubnetResponse>,
}
#[derive(serde::Serialize)]
struct SubnetResponse {
    id: String,
    network_id: String,
    name: String,
    project_id: String,
    cidr: String,
    gateway_ip: Ipv4Addr,
    allocation_pools: Vec<AllocationPoolResponse>,
}
#[derive(serde::Serialize)]
struct AllocationPoolResponse {
    start: Ipv4Addr,
    end: Ipv4Addr,
}

fn subnet_response(value: SubnetRecord) -> SubnetResponse {
    SubnetResponse {
        id: value.id.to_string(),
        network_id: value.network_id.to_string(),
        name: value.name,
        project_id: value.project_id,
        cidr: value.cidr,
        gateway_ip: value.gateway_ip,
        allocation_pools: vec![AllocationPoolResponse {
            start: value.allocation_start,
            end: value.allocation_end,
        }],
    }
}

#[derive(serde::Deserialize)]
struct PortRequestBody {
    port: CreatePortRequest,
}
#[derive(serde::Deserialize)]
struct CreatePortRequest {
    name: String,
    network_id: uuid::Uuid,
}
#[derive(serde::Serialize)]
struct PortEnvelope {
    port: PortResponse,
}
#[derive(serde::Serialize)]
struct PortList {
    ports: Vec<PortResponse>,
}
#[derive(serde::Serialize)]
struct PortResponse {
    id: String,
    network_id: String,
    project_id: String,
    name: String,
    mac_address: String,
    fixed_ips: Vec<FixedIpResponse>,
    status: String,
}
#[derive(serde::Serialize)]
struct FixedIpResponse {
    subnet_id: String,
    ip_address: Ipv4Addr,
}

fn port_response(value: PortRecord) -> PortResponse {
    PortResponse {
        id: value.id.to_string(),
        network_id: value.network_id.to_string(),
        project_id: value.project_id,
        name: value.name,
        mac_address: value.mac_address,
        fixed_ips: (!value.subnet_id.is_nil())
            .then_some(FixedIpResponse {
                subnet_id: value.subnet_id.to_string(),
                ip_address: value.fixed_ip,
            })
            .into_iter()
            .collect(),
        status: value.status,
    }
}

fn network_error(error: NetworkError) -> axum::response::Response {
    match error {
        NetworkError::NotFound => keystone_error(
            StatusCode::NOT_FOUND,
            "Not Found",
            "network resource was not found",
        ),
        NetworkError::Conflict | NetworkError::PoolExhausted => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "network operation is not allowed",
        ),
        NetworkError::InvalidRequest => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid network request",
        ),
        NetworkError::Storage(_) | NetworkError::CorruptMetadata(_) => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "network storage is unavailable",
        ),
    }
}

fn network_service(state: &AppState) -> Result<&Arc<NetworkService>, axum::response::Response> {
    state.network.as_ref().ok_or_else(|| {
        keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "network service is not configured",
        )
    })
}

async fn list_extensions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(response) = require_token(&state, &headers) {
        return response;
    }
    if let Err(response) = network_service(&state) {
        return response;
    }
    Json(serde_json::json!({"extensions": []})).into_response()
}

async fn create_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<NetworkRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid network request",
        );
    };
    match service.create_network(&token.project_id, body.network.name) {
        Ok(value) => (
            StatusCode::CREATED,
            Json(NetworkEnvelope {
                network: network_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

async fn list_networks(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_networks(&token.project_id) {
        Ok(values) => Json(NetworkList {
            networks: values.into_iter().map(network_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn show_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_network(&token.project_id, id) {
        Ok(value) => Json(NetworkEnvelope {
            network: network_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn delete_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_network(&token.project_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

async fn create_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<SubnetRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid subnet request",
        );
    };
    if body
        .subnet
        .allocation_pools
        .as_ref()
        .is_some_and(|values| values.len() > 1)
    {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "multiple allocation pools are not supported by this profile",
        );
    }
    let pool = body
        .subnet
        .allocation_pools
        .as_ref()
        .and_then(|values| values.first());
    match service.create_subnet(
        &token.project_id,
        body.subnet.network_id,
        body.subnet.name,
        body.subnet.cidr,
        body.subnet.gateway_ip,
        pool.map(|v| v.start),
        pool.map(|v| v.end),
    ) {
        Ok(value) => (
            StatusCode::CREATED,
            Json(SubnetEnvelope {
                subnet: subnet_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

async fn list_subnets(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_subnets(&token.project_id) {
        Ok(values) => Json(SubnetList {
            subnets: values.into_iter().map(subnet_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn show_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_subnet(&token.project_id, id) {
        Ok(value) => Json(SubnetEnvelope {
            subnet: subnet_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn delete_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_subnet(&token.project_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

async fn create_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<PortRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid port request",
        );
    };
    match service.create_port(&token.project_id, body.port.network_id, body.port.name) {
        Ok(value) => (
            StatusCode::CREATED,
            Json(PortEnvelope {
                port: port_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

async fn list_ports(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_ports(&token.project_id) {
        Ok(values) => Json(PortList {
            ports: values.into_iter().map(port_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn show_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_port(&token.project_id, id) {
        Ok(value) => Json(PortEnvelope {
            port: port_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

async fn delete_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_port(&token.project_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

#[derive(Serialize)]
struct FlavorResponse {
    id: String,
    name: String,
    vcpus: u32,
    ram: u64,
    disk: u64,
}
#[derive(Serialize)]
struct FlavorListResponse {
    flavors: Vec<FlavorResponse>,
}
#[derive(Serialize)]
struct FlavorEnvelope {
    flavor: FlavorResponse,
}

fn flavor_response(flavor: Flavor) -> FlavorResponse {
    FlavorResponse {
        id: flavor.id.to_string(),
        name: flavor.name,
        vcpus: flavor.vcpus,
        ram: flavor.ram_mib,
        disk: flavor.disk_gib,
    }
}

#[derive(serde::Deserialize)]
struct CreateServerEnvelope {
    server: CreateServerRequest,
}
#[derive(serde::Deserialize)]
struct CreateServerRequest {
    name: String,
    #[serde(alias = "imageRef")]
    image: Option<IdReference>,
    #[serde(alias = "flavorRef")]
    flavor: Option<IdReference>,
    networks: Option<Vec<NetworkReference>>,
    /// Recognized so an unsupported request cannot be silently dropped.
    config_drive: Option<bool>,
    key_name: Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum IdReference {
    Object { id: String },
    String(String),
}

impl IdReference {
    fn into_id(self) -> String {
        match self {
            Self::Object { id } | Self::String(id) => id,
        }
    }
}
#[derive(serde::Deserialize)]
struct NetworkReference {
    uuid: Option<String>,
    port: Option<String>,
}

#[derive(Serialize)]
struct ServerEnvelope {
    server: ServerResponse,
}
#[derive(Serialize)]
struct ServerListResponse {
    servers: Vec<ServerResponse>,
}
#[derive(Serialize)]
struct ServerResponse {
    id: String,
    name: String,
    status: String,
    tenant_id: String,
    project_id: String,
    image: IdResponse,
    flavor: IdResponse,
    addresses: serde_json::Value,
    key_name: Option<String>,
}
#[derive(Serialize)]
struct IdResponse {
    id: String,
}

fn server_response(server: Server) -> ServerResponse {
    ServerResponse {
        id: server.id.to_string(),
        name: server.name,
        status: server.status,
        tenant_id: server.project_id.clone(),
        project_id: server.project_id,
        image: IdResponse {
            id: server.image_id,
        },
        flavor: IdResponse {
            id: server.flavor_id.to_string(),
        },
        addresses: serde_json::json!({}),
        key_name: server.key_name,
    }
}

#[derive(serde::Deserialize)]
struct CreateKeypairEnvelope {
    keypair: CreateKeypairRequest,
}

#[derive(serde::Deserialize)]
struct CreateKeypairRequest {
    name: String,
    public_key: Option<String>,
    #[serde(rename = "type")]
    key_type: Option<String>,
}

#[derive(Serialize)]
struct KeypairEnvelope {
    keypair: KeypairResponse,
}

#[derive(Serialize)]
struct KeypairListResponse {
    keypairs: Vec<KeypairEnvelope>,
}

#[derive(Serialize)]
struct KeypairResponse {
    name: String,
    id: String,
    user_id: String,
    public_key: String,
    fingerprint: String,
    #[serde(rename = "type")]
    key_type: String,
    created_at: String,
}

fn keypair_response(keypair: o3k_compute::Keypair) -> KeypairResponse {
    KeypairResponse {
        name: keypair.name,
        id: keypair.id.to_string(),
        user_id: keypair.user_id,
        public_key: keypair.public_key,
        fingerprint: keypair.fingerprint,
        key_type: "ssh".to_owned(),
        created_at: keypair.created_at,
    }
}

fn compute_error(error: ComputeError) -> axum::response::Response {
    match error {
        ComputeError::NotFound => keystone_error(
            StatusCode::NOT_FOUND,
            "Not Found",
            "compute resource was not found",
        ),
        ComputeError::Conflict => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "compute operation conflicts with current state",
        ),
        ComputeError::Scheduler(_) => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "compute host could not satisfy placement requirements",
        ),
        ComputeError::InvalidRequest => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid compute request",
        ),
        ComputeError::Store(o3k_store::StoreError::KeypairNotFound) => {
            keystone_error(StatusCode::NOT_FOUND, "Not Found", "keypair was not found")
        }
        ComputeError::Store(o3k_store::StoreError::KeypairAlreadyExists) => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "compute resource already exists or conflicts with current state",
        ),
        ComputeError::Store(o3k_store::StoreError::KeypairInUse) => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "keypair is still attached to a server",
        ),
        ComputeError::Store(o3k_store::StoreError::KeypairOwnershipConflict) => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "keypair and server ownership do not match",
        ),
        ComputeError::Store(o3k_store::StoreError::InvalidKeypair(_)) => {
            keystone_error(StatusCode::BAD_REQUEST, "Bad Request", "invalid public key")
        }
        ComputeError::Store(_) | ComputeError::Reconcile(_) | ComputeError::Provider(_) => {
            keystone_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "compute service is unavailable",
            )
        }
    }
}

fn cached_console_response(
    console: &o3k_console::ConsoleService,
    id: uuid::Uuid,
    offset: u64,
    length: usize,
) -> Option<axum::response::Response> {
    console.read_from(id, offset, length).ok().map(|chunk| {
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "output": String::from_utf8_lossy(&chunk.bytes)
            })),
        )
            .into_response()
    })
}

fn should_query_live_console(offset: u64) -> bool {
    offset == 0
}

fn project_token(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    project_id: &str,
) -> Result<o3k_identity::VerifiedToken, axum::response::Response> {
    let token = require_token(state, headers)?;
    if token.project_id != project_id {
        return Err(keystone_error(
            StatusCode::NOT_FOUND,
            "Not Found",
            "compute resource was not found",
        ));
    }
    Ok(token)
}

fn compute_service(state: &AppState) -> Result<&Arc<ComputeService>, axum::response::Response> {
    state.compute.as_ref().ok_or_else(|| {
        keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service is not configured",
        )
    })
}

async fn list_flavors(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.flavors_for_project(&project_id).await {
        Ok(flavors) => Json(FlavorListResponse {
            flavors: flavors.into_iter().map(flavor_response).collect(),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

#[derive(serde::Deserialize)]
struct CreateFlavorEnvelope {
    flavor: CreateFlavorRequest,
}

#[derive(serde::Deserialize)]
struct CreateFlavorRequest {
    name: String,
    vcpus: u32,
    ram: u64,
    disk: u64,
}

async fn create_flavor(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
    request: Result<Json<CreateFlavorEnvelope>, JsonRejection>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid flavor request",
        );
    };
    match service
        .create_flavor(
            &token.project_id,
            body.flavor.name,
            body.flavor.vcpus,
            body.flavor.ram,
            body.flavor.disk,
        )
        .await
    {
        Ok(flavor) => (
            StatusCode::CREATED,
            Json(FlavorEnvelope {
                flavor: flavor_response(flavor),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn show_flavor(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, id)): Path<(String, uuid::Uuid)>,
) -> axum::response::Response {
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.flavor_for_project(&project_id, id).await {
        Ok(flavor) => Json(FlavorEnvelope {
            flavor: flavor_response(flavor),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn delete_flavor(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, id)): Path<(String, uuid::Uuid)>,
) -> axum::response::Response {
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_flavor(&project_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => compute_error(error),
    }
}

async fn list_keypairs(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .list_keypairs(&token.user_id, &token.project_id)
        .await
    {
        Ok(keypairs) => Json(KeypairListResponse {
            keypairs: keypairs
                .into_iter()
                .map(|keypair| KeypairEnvelope {
                    keypair: keypair_response(keypair),
                })
                .collect(),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn create_keypair(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
    request: Result<Json<CreateKeypairEnvelope>, JsonRejection>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid keypair request",
        );
    };
    if body
        .keypair
        .key_type
        .as_deref()
        .is_some_and(|key_type| key_type != "ssh")
    {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "unsupported keypair type",
        );
    }
    let Some(public_key) = body.keypair.public_key else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "public_key is required",
        );
    };
    match service
        .create_keypair(
            &token.user_id,
            &token.project_id,
            body.keypair.name,
            public_key,
        )
        .await
    {
        Ok(keypair) => (
            StatusCode::OK,
            Json(KeypairEnvelope {
                keypair: keypair_response(keypair),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn show_keypair(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, name)): Path<(String, String)>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .show_keypair(&token.user_id, &token.project_id, &name)
        .await
    {
        Ok(keypair) => Json(KeypairEnvelope {
            keypair: keypair_response(keypair),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn delete_keypair(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, name)): Path<(String, String)>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .delete_keypair(&token.user_id, &token.project_id, &name)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => compute_error(error),
    }
}

async fn create_server(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
    request: Result<Json<CreateServerEnvelope>, JsonRejection>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid server request",
        );
    };
    if body.server.config_drive == Some(true) {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "config-drive attachment is not supported by this profile",
        );
    }
    let Some(image) = body
        .server
        .image
        .map(IdReference::into_id)
        .filter(|reference| !reference.trim().is_empty())
    else {
        return keystone_error(StatusCode::BAD_REQUEST, "Bad Request", "image is required");
    };
    let Some(flavor) = body
        .server
        .flavor
        .map(IdReference::into_id)
        .and_then(|reference| reference.parse::<uuid::Uuid>().ok())
    else {
        return keystone_error(StatusCode::BAD_REQUEST, "Bad Request", "flavor is required");
    };
    let Some(networks) = body.server.networks else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "network is required",
        );
    };
    if networks.is_empty()
        || networks
            .iter()
            .any(|network| network.uuid.as_deref().is_none_or(str::is_empty))
    {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "network is required",
        );
    }
    if let Some(image_service) = state.image.as_ref() {
        let image_id = match image.parse::<uuid::Uuid>() {
            Ok(value) => value,
            Err(_) => {
                return keystone_error(
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    "image must be a UUID when image validation is enabled",
                );
            }
        };
        match image_service.get(&token.project_id, image_id) {
            Ok(record) if record.status == o3k_image::ImageStatus::Active => {}
            Ok(_) => {
                return keystone_error(StatusCode::CONFLICT, "Conflict", "image is not active");
            }
            Err(error) => return image_error(error),
        }
    }
    let network_ids = networks
        .iter()
        .filter_map(|network| network.port.as_deref().or(network.uuid.as_deref()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(network_service) = state.network.as_ref() {
        for network_id in &network_ids {
            let port_id = match network_id.parse::<uuid::Uuid>() {
                Ok(value) => value,
                Err(_) => {
                    return keystone_error(
                        StatusCode::BAD_REQUEST,
                        "Bad Request",
                        "network references must be durable port UUIDs when network validation is enabled",
                    );
                }
            };
            if let Err(error) = network_service.get_port(&token.project_id, port_id) {
                return network_error(error);
            }
        }
    }
    let idempotency = headers
        .get("x-openstack-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(&body.server.name)
        .to_owned();
    match service
        .create_server_for_user(o3k_compute::ServerCreateInput {
            user_id: token.user_id,
            project_id: token.project_id,
            name: body.server.name,
            image_id: image,
            flavor_id: flavor,
            network_ids,
            key_name: body.server.key_name,
            idempotency_key: idempotency,
        })
        .await
    {
        Ok(server) => (
            StatusCode::ACCEPTED,
            Json(ServerEnvelope {
                server: server_response(server),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn list_servers(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_servers(&token.project_id).await {
        Ok(servers) => Json(ServerListResponse {
            servers: servers.into_iter().map(server_response).collect(),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn show_server(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, id)): Path<(String, uuid::Uuid)>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.show_server(&token.project_id, id).await {
        Ok(server) => Json(ServerEnvelope {
            server: server_response(server),
        })
        .into_response(),
        Err(error) => compute_error(error),
    }
}

async fn delete_server(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, id)): Path<(String, uuid::Uuid)>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_server(&token.project_id, id).await {
        Ok(()) => {
            if let Some(console) = state.console.as_ref() {
                if let Err(error) = console.cleanup(id) {
                    tracing::warn!(%error, server_id = %id, "deleted server console cleanup failed");
                    return keystone_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal Server Error",
                        "server console cleanup failed",
                    );
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => compute_error(error),
    }
}

async fn server_action(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, id)): Path<(String, uuid::Uuid)>,
    request: Result<Json<serde_json::Value>, JsonRejection>,
) -> axum::response::Response {
    let token = match project_token(&state, &headers, &project_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match compute_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid server action",
        );
    };
    let action = match body
        .as_object()
        .and_then(|object| object.keys().next())
        .map(String::as_str)
    {
        Some("os-getConsoleOutput") => {
            let Some(console) = state.console.as_ref() else {
                return keystone_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Service Unavailable",
                    "console output is not configured",
                );
            };
            if let Err(error) = service.show_server(&token.project_id, id).await {
                return compute_error(error);
            }
            let options = body
                .get("os-getConsoleOutput")
                .and_then(serde_json::Value::as_object);
            let offset = match options.and_then(|value| value.get("offset")) {
                None => Ok(0),
                Some(value) => value.as_u64().ok_or(()),
            };
            let Ok(offset) = offset else {
                return keystone_error(
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    "console output offset is invalid",
                );
            };
            let length = match options.and_then(|value| value.get("length")) {
                None => Ok(o3k_console::MAX_CONSOLE_BYTES as u64),
                Some(value) => value.as_u64().ok_or(()),
            };
            let Ok(length) = length else {
                return keystone_error(
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    "console output length is invalid",
                );
            };
            let Ok(length) = usize::try_from(length) else {
                return keystone_error(
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    "console output length is invalid",
                );
            };
            if length == 0 {
                return (StatusCode::OK, Json(serde_json::json!({"output": ""}))).into_response();
            }
            if should_query_live_console(offset) {
                if let Some(registry) = state.agent_registry.as_ref() {
                    match service.placement_provider_id(&token.project_id, id).await {
                        Ok(Some(agent_id)) => {
                            let Some(node) = registry.snapshot(&agent_id).await else {
                                return keystone_error(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "Service Unavailable",
                                    "compute agent is not registered",
                                );
                            };
                            let operation_id = uuid::Uuid::now_v7().to_string();
                            let command = match o3k_compute_agent::build_console_log_command(
                                &agent_id,
                                &node.agent_epoch,
                                &operation_id,
                                &id.to_string(),
                                offset,
                                length.min(o3k_console::MAX_CONSOLE_BYTES) as u32,
                            ) {
                                Ok(command) => command,
                                Err(_) => {
                                    return keystone_error(
                                        StatusCode::BAD_REQUEST,
                                        "Bad Request",
                                        "console output bounds are invalid",
                                    );
                                }
                            };
                            let observation = match registry
                                .dispatch_command_and_wait(command, Duration::from_secs(5))
                                .await
                            {
                                Ok(observation) => observation,
                                Err(error) => {
                                    tracing::warn!(%error, server_id = %id, "agent console query failed");
                                    if let Some(response) =
                                        cached_console_response(console, id, offset, length)
                                    {
                                        return response;
                                    }
                                    return keystone_error(
                                        StatusCode::SERVICE_UNAVAILABLE,
                                        "Service Unavailable",
                                        "compute agent console output is unavailable",
                                    );
                                }
                            };
                            if let Err(error) = console.write_chunk(
                                id,
                                observation.console_log_offset,
                                &observation.console_log_bytes,
                            ) {
                                tracing::warn!(%error, server_id = %id, "agent console observation persistence failed");
                            }
                            if let Some(response) =
                                cached_console_response(console, id, offset, length)
                            {
                                return response;
                            }
                            return keystone_error(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "Service Unavailable",
                                "compute agent console output could not be persisted",
                            );
                        }
                        Ok(None) => {}
                        Err(error) => return compute_error(error),
                    }
                }
            }
            return match console.read_from(id, offset, length) {
                Ok(chunk) => (
                    StatusCode::OK,
                    Json(serde_json::json!({"output": String::from_utf8_lossy(&chunk.bytes)})),
                )
                    .into_response(),
                Err(ConsoleError::NotFound) => {
                    (StatusCode::OK, Json(serde_json::json!({"output": ""}))).into_response()
                }
                Err(_) => keystone_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "console output is unavailable",
                ),
            };
        }
        Some("os-start") => InstanceAction::Start,
        Some("os-stop") => InstanceAction::Stop,
        Some("reboot") | Some("os-reboot") => InstanceAction::Reboot,
        _ => {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "unsupported server action",
            );
        }
    };
    match service.action(&token.project_id, id, action).await {
        Ok(server) => (
            StatusCode::ACCEPTED,
            Json(ServerEnvelope {
                server: server_response(server),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::should_query_live_console;

    #[test]
    fn live_console_queries_are_limited_to_the_snapshot_offset() {
        assert!(should_query_live_console(0));
        assert!(!should_query_live_console(1));
        assert!(!should_query_live_console(u64::MAX));
    }

    #[test]
    fn unsupported_config_drive_requests_are_explicitly_recognized() -> Result<(), serde_json::Error>
    {
        let request = serde_json::json!({
            "name": "server",
            "image": {"id": "image"},
            "flavor": {"id": "1"},
            "networks": [{"uuid": "network"}],
            "config_drive": true
        });
        let parsed: super::CreateServerRequest = serde_json::from_value(request)?;
        assert_eq!(parsed.config_drive, Some(true));
        Ok(())
    }

    #[test]
    fn nova_server_reference_aliases_accept_standard_cli_wire_fields()
    -> Result<(), serde_json::Error> {
        let request = serde_json::json!({
            "name": "server",
            "imageRef": "image-id",
            "flavorRef": "550e8400-e29b-41d4-a716-446655440000",
            "networks": [{"port": "550e8400-e29b-41d4-a716-446655440001"}]
        });
        let parsed: super::CreateServerRequest = serde_json::from_value(request)?;
        assert_eq!(
            parsed.image.map(super::IdReference::into_id).as_deref(),
            Some("image-id")
        );
        assert_eq!(
            parsed.flavor.map(super::IdReference::into_id).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            parsed
                .networks
                .as_ref()
                .and_then(|networks| networks[0].port.as_deref()),
            Some("550e8400-e29b-41d4-a716-446655440001")
        );
        Ok(())
    }
}
