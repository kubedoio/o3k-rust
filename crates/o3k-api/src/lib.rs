use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
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
use o3k_console::{ConsoleError, ConsoleService};
use o3k_identity::{AuthError, TokenRequest, TokenService};
use o3k_image::{ImageError, ImageRecord, ImageService};
use o3k_network::{NetworkError, NetworkRecord, NetworkService, PortRecord, SubnetRecord};
use o3k_provider::InstanceAction;
use serde::Serialize;
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
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

pub fn router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v3/auth/tokens", post(issue_token))
        .route("/v2/images", get(list_images).post(create_image))
        .route("/v2/images/{id}", get(show_image).delete(delete_image))
        .route("/v2/images/{id}/file", axum::routing::put(upload_image))
        .route("/v2.0/networks", get(list_networks).post(create_network))
        .route(
            "/v2.0/networks/{id}",
            get(show_network).delete(delete_network),
        )
        .route("/v2.0/subnets", get(list_subnets).post(create_subnet))
        .route("/v2.0/subnets/{id}", get(show_subnet).delete(delete_subnet))
        .route("/v2.0/ports", get(list_ports).post(create_port))
        .route("/v2.0/ports/{id}", get(show_port).delete(delete_port))
        .route("/v2.1/{project_id}/flavors", get(list_flavors))
        .route("/v2.1/{project_id}/flavors/{id}", get(show_flavor))
        .route(
            "/v2.1/{project_id}/servers",
            get(list_servers).post(create_server),
        )
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
        ImageError::OverlayFailed => keystone_error(
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
    fixed_ips: Vec<FixedIpResponse>,
    status: String,
}
#[derive(serde::Serialize)]
struct FixedIpResponse {
    subnet_id: String,
    ip_address: Ipv4Addr,
}

fn port_response(value: PortRecord, subnet_id: Option<uuid::Uuid>) -> PortResponse {
    PortResponse {
        id: value.id.to_string(),
        network_id: value.network_id.to_string(),
        project_id: value.project_id,
        name: value.name,
        fixed_ips: subnet_id
            .into_iter()
            .map(|id| FixedIpResponse {
                subnet_id: id.to_string(),
                ip_address: value.fixed_ip,
            })
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
        Ok(value) => {
            let subnet = service
                .list_subnets(&token.project_id)
                .ok()
                .and_then(|values| {
                    values
                        .into_iter()
                        .find(|v| v.network_id == value.network_id)
                        .map(|v| v.id)
                });
            (
                StatusCode::CREATED,
                Json(PortEnvelope {
                    port: port_response(value, subnet),
                }),
            )
                .into_response()
        }
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
            ports: values
                .into_iter()
                .map(|v| {
                    let subnet = service.list_subnets(&token.project_id).ok().and_then(|ss| {
                        ss.into_iter()
                            .find(|s| s.network_id == v.network_id)
                            .map(|s| s.id)
                    });
                    port_response(v, subnet)
                })
                .collect(),
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
        Ok(value) => {
            let subnet = service.list_subnets(&token.project_id).ok().and_then(|ss| {
                ss.into_iter()
                    .find(|s| s.network_id == value.network_id)
                    .map(|s| s.id)
            });
            Json(PortEnvelope {
                port: port_response(value, subnet),
            })
            .into_response()
        }
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
    image: Option<IdReference>,
    flavor: Option<IdReference>,
    networks: Option<Vec<NetworkReference>>,
}
#[derive(serde::Deserialize)]
struct IdReference {
    id: String,
}
#[derive(serde::Deserialize)]
struct NetworkReference {
    uuid: Option<String>,
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
        ComputeError::Store(_) | ComputeError::Reconcile(_) | ComputeError::Provider(_) => {
            keystone_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "compute service is unavailable",
            )
        }
    }
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
    Json(FlavorListResponse {
        flavors: service.flavors().into_iter().map(flavor_response).collect(),
    })
    .into_response()
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
    match service.flavor(id) {
        Ok(flavor) => Json(FlavorEnvelope {
            flavor: flavor_response(flavor),
        })
        .into_response(),
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
    let Some(image) = body
        .server
        .image
        .and_then(|reference| (!reference.id.trim().is_empty()).then_some(reference.id))
    else {
        return keystone_error(StatusCode::BAD_REQUEST, "Bad Request", "image is required");
    };
    let Some(flavor) = body
        .server
        .flavor
        .and_then(|reference| reference.id.parse().ok())
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
    let idempotency = headers
        .get("x-openstack-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(&body.server.name)
        .to_owned();
    match service
        .create_server(
            &token.project_id,
            body.server.name,
            image,
            flavor,
            networks
                .into_iter()
                .filter_map(|network| network.uuid)
                .collect(),
            idempotency,
        )
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
