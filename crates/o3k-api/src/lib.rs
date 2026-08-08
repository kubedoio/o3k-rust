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
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use o3k_compute::{ComputeError, ComputeService, Flavor, Server};
use o3k_compute_agent::NodeRegistry;
use o3k_console::{ConsoleError, ConsoleService};
use o3k_domain::{ServerId, ServerState};
use o3k_identity::{AuthError, TokenRequest, TokenService};
use o3k_image::{ImageError, ImageRecord, ImageService};
use o3k_network::{NetworkError, NetworkRecord, NetworkService, PortRecord, SubnetRecord};
use o3k_provider::{ConfigDriveRequest, InstanceAction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;
use uuid::Uuid;

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

async fn placement_discovery() -> impl IntoResponse {
    Json(serde_json::json!({
        "versions": [
            {
                "id": "v1.0",
                "status": "CURRENT",
                "min_version": "1.0",
                "max_version": "1.28",
                "links": [
                    {
                        "rel": "self",
                        "href": "/placement/"
                    }
                ]
            }
        ]
    }))
}

fn parse_microversion(ver: &str) -> Result<(u32, u32), ()> {
    let parts: Vec<&str> = ver.split('.').collect();
    if parts.len() == 2
        && let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
    {
        return Ok((major, minor));
    }
    Err(())
}

/// Whether the caller negotiated Nova microversion 2.89 for this request.
/// Mirrors the parsing in `microversion_middleware`: the caller may use
/// `OpenStack-API-Version: compute 2.89` or `X-OpenStack-Nova-API-Version:
/// 2.89`. The operation-scoped 2.89 profile is GET-only on the volume
/// attachment routes; this helper is used to select the 2.89 response shape.
fn requested_compute_289(headers: &HeaderMap) -> bool {
    let os_api_ver = headers
        .get("OpenStack-API-Version")
        .and_then(|h| h.to_str().ok());
    let nova_api_ver = headers
        .get("X-OpenStack-Nova-API-Version")
        .and_then(|h| h.to_str().ok());
    let mut compute_version: Option<&str> = None;
    if let Some(val) = os_api_ver {
        for part in val.split(',') {
            let tokens: Vec<&str> = part.split_whitespace().collect();
            if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("compute") {
                compute_version = Some(tokens[1]);
                break;
            }
        }
    }
    if compute_version.is_none()
        && let Some(val) = nova_api_ver
    {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            compute_version = Some(trimmed);
        }
    }
    compute_version == Some("2.89")
}

async fn microversion_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();

    if path.starts_with("/v2.1") {
        if path == "/v2.1" || path == "/v2.1/" {
            return next.run(req).await;
        }

        let headers = req.headers();
        let os_api_ver = headers
            .get("OpenStack-API-Version")
            .and_then(|h| h.to_str().ok());
        let nova_api_ver = headers
            .get("X-OpenStack-Nova-API-Version")
            .and_then(|h| h.to_str().ok());

        let mut compute_version: Option<&str> = None;
        let mut malformed = false;

        if let Some(val) = os_api_ver {
            for part in val.split(',') {
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("compute") {
                    compute_version = Some(tokens[1]);
                    break;
                } else if tokens.len() != 2
                    && !part.trim().is_empty()
                    && part.trim().to_lowercase().contains("compute")
                {
                    malformed = true;
                }
            }
        }

        if compute_version.is_none()
            && !malformed
            && let Some(val) = nova_api_ver
        {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                compute_version = Some(trimmed);
            }
        }

        if malformed {
            let body = serde_json::json!({
                "badRequest": {
                    "code": 400,
                    "message": "Invalid microversion header format."
                }
            });
            let mut resp = (StatusCode::BAD_REQUEST, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            resp.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
            resp.headers_mut().insert(
                "Vary",
                HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
            );
            return resp;
        }

        let is_attachment_route = path.contains("/os-volume_attachments");
        // The operation-scoped 2.89 profile is GET-only on the volume
        // attachment list/show operations that Cinder's attachment-delete
        // guard (bug #2004555) requires. Every other 2.89 request is rejected.
        let is_allowed_289 = is_attachment_route
            && req.method() == axum::http::Method::GET
            && compute_version == Some("2.89");

        if let Some(ver) = compute_version
            && ver != "2.1"
            && !is_allowed_289
        {
            let body = serde_json::json!({
                "computeFault": {
                    "code": 406,
                    "message": format!(
                        "Version {ver} is not supported by the API. Minimum supported version is 2.1 and maximum supported version is 2.1."
                    )
                }
            });
            let mut resp = (StatusCode::NOT_ACCEPTABLE, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            resp.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
            resp.headers_mut().insert(
                "Vary",
                HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
            );
            return resp;
        }

        let mut response = next.run(req).await;
        if is_allowed_289 {
            response.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.89"),
            );
            response.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.89"),
            );
        } else {
            response.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            response.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
        }
        response.headers_mut().insert(
            "Vary",
            HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
        );
        return response;
    }

    if path.starts_with("/placement") {
        if path == "/placement" || path == "/placement/" {
            return next.run(req).await;
        }

        let headers = req.headers();
        let os_api_ver = headers
            .get("OpenStack-API-Version")
            .and_then(|h| h.to_str().ok());

        let mut placement_version: Option<&str> = None;
        let mut malformed = false;

        if let Some(val) = os_api_ver {
            for part in val.split(',') {
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("placement") {
                    placement_version = Some(tokens[1]);
                    break;
                } else if tokens.len() != 2
                    && !part.trim().is_empty()
                    && part.trim().to_lowercase().contains("placement")
                {
                    malformed = true;
                }
            }
        }

        if malformed {
            let body = serde_json::json!({
                "error": {
                    "code": 400,
                    "message": "Invalid microversion header format."
                }
            });
            let mut resp = (StatusCode::BAD_REQUEST, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("placement 1.0"),
            );
            resp.headers_mut()
                .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
            return resp;
        }

        let mut negotiated = "1.0".to_string();
        if let Some(ver) = placement_version {
            let is_valid = if ver.eq_ignore_ascii_case("latest") {
                negotiated = "1.28".to_string();
                true
            } else if let Ok((major, minor)) = parse_microversion(ver) {
                if major == 1 && minor <= 28 {
                    negotiated = ver.to_string();
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !is_valid {
                let body = serde_json::json!({
                    "error": {
                        "code": 406,
                        "message": format!(
                            "Version {ver} is not supported by Placement API. Minimum supported version is 1.0 and maximum supported version is 1.28."
                        )
                    }
                });
                let mut resp = (StatusCode::NOT_ACCEPTABLE, Json(body)).into_response();
                resp.headers_mut().insert(
                    "OpenStack-API-Version",
                    HeaderValue::from_static("placement 1.28"),
                );
                resp.headers_mut()
                    .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
                return resp;
            }
        }

        let mut response = next.run(req).await;
        if let Ok(header_val) = HeaderValue::from_str(&format!("placement {negotiated}")) {
            response
                .headers_mut()
                .insert("OpenStack-API-Version", header_val);
        }
        response
            .headers_mut()
            .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
        return response;
    }

    next.run(req).await
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
        Err(AuthError::IdentityUnavailable) => keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "identity is not configured",
        ),
    }
}

async fn validate_token(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(service) = state.identity else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "identity is not configured",
        );
    };

    let token = headers
        .get("x-subject-token")
        .or_else(|| headers.get("x-auth-token"))
        .and_then(|v| v.to_str().ok());

    let Some(token) = token else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "X-Subject-Token header is required",
        );
    };

    match service.verify_details(token, SystemTime::now()) {
        Ok(response) => {
            let Ok(subject_token) = HeaderValue::from_str(token) else {
                return keystone_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "token could not be encoded",
                );
            };
            (
                StatusCode::OK,
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
        Err(_) => keystone_error(StatusCode::NOT_FOUND, "Not Found", "Could not find token"),
    }
}

async fn check_token(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(service) = state.identity else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let token = headers
        .get("x-subject-token")
        .or_else(|| headers.get("x-auth-token"))
        .and_then(|v| v.to_str().ok());

    let Some(token) = token else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    match service.verify(token, SystemTime::now()) {
        Ok(_) => {
            let Ok(subject_token) = HeaderValue::from_str(token) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            (
                StatusCode::OK,
                [
                    (
                        header::HeaderName::from_static("x-subject-token"),
                        subject_token,
                    ),
                    (header::VARY, HeaderValue::from_static("X-Auth-Token")),
                ],
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
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

// Axum handlers consume the concrete response directly; boxing this error would
// add conversions across every OpenStack adapter without changing behavior.
#[allow(clippy::result_large_err)]
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
        ImageError::Storage(_) | ImageError::CorruptMetadata(_) | ImageError::Store(_) => {
            keystone_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "image storage is unavailable",
            )
        }
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
    match service.resolve_artifact(&token.project_id, id).await {
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
    match service
        .create(
            &token.project_id,
            request.name,
            request.visibility,
            request.container_format,
            request.disk_format,
        )
        .await
    {
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
    match service.list(&token.project_id).await {
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
    match service.get(&token.project_id, id).await {
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
    match service.upload(&token.project_id, id, &body).await {
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
    match service.delete(&token.project_id, id).await {
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
        fixed_ips: value
            .subnet_id
            .map(|subnet_id| FixedIpResponse {
                subnet_id: subnet_id.to_string(),
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
        NetworkError::Store(_) | NetworkError::CorruptMetadata(_) => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "network storage is unavailable",
        ),
    }
}

#[allow(clippy::result_large_err)]
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
    match service
        .create_network(&token.project_id, body.network.name)
        .await
    {
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
    match service.list_networks(&token.project_id).await {
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
    match service.get_network(&token.project_id, id).await {
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
    match service.delete_network(&token.project_id, id).await {
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
    match service
        .create_subnet(
            &token.project_id,
            body.subnet.network_id,
            body.subnet.name,
            body.subnet.cidr,
            body.subnet.gateway_ip,
            pool.map(|v| v.start),
            pool.map(|v| v.end),
        )
        .await
    {
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
    match service.list_subnets(&token.project_id).await {
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
    match service.get_subnet(&token.project_id, id).await {
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
    match service.delete_subnet(&token.project_id, id).await {
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
    match service
        .create_port(&token.project_id, body.port.network_id, body.port.name)
        .await
    {
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
    match service.list_ports(&token.project_id).await {
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
    match service.get_port(&token.project_id, id).await {
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
    match service.delete_port(&token.project_id, id).await {
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
    config_drive: Option<bool>,
    user_data: Option<String>,
    vendor_data: Option<String>,
    ssh_public_key: Option<String>,
    key_name: Option<String>,
}

fn config_drive_ssh_public_key(
    explicit: Option<String>,
    keypair: Option<String>,
) -> Result<String, &'static str> {
    explicit
        .or(keypair)
        .filter(|value| !value.trim().is_empty())
        .ok_or("key_name or ssh_public_key is required when config_drive is enabled")
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
    config_drive: bool,
    // Nova servers always carry a metadata object; public clients (for
    // example openstackclient 6.6 `_prep_server_detail`) pop it
    // unconditionally. O3K does not model server metadata yet, so the
    // representation is always the empty object.
    metadata: serde_json::Value,
    // Nova's extended server attribute reporting the selected compute host.
    // O3K projects the durable scheduler placement provider identity, never a
    // display name; null only when no placement decision was recorded.
    #[serde(rename = "OS-EXT-SRV-ATTR:host")]
    host: Option<String>,
}
#[derive(Serialize)]
struct IdResponse {
    id: String,
}

/// Projects the canonical server lifecycle state into the Nova status string
/// of the current response shape. Nova/OpenStack status strings live here,
/// in the API crate; the canonical domain model and the persisted values are
/// separate projections owned by `o3k-domain` and `o3k-store`.
fn nova_status(state: ServerState) -> &'static str {
    match state {
        ServerState::Requested => "REQUESTED",
        ServerState::Building => "BUILD",
        ServerState::Active => "ACTIVE",
        ServerState::Stopping => "STOPPING",
        ServerState::Stopped => "SHUTOFF",
        ServerState::Starting => "STARTING",
        ServerState::Rebooting => "REBOOTING",
        ServerState::Deleting => "DELETING",
        ServerState::Deleted => "DELETED",
        ServerState::Error => "ERROR",
    }
}

async fn server_response(
    server: Server,
    network_service: Option<&NetworkService>,
) -> ServerResponse {
    let mut addresses = serde_json::Map::new();
    if let Some(network_service) = network_service {
        for port_id in &server.network_ids {
            let Ok(port_id) = port_id.parse::<uuid::Uuid>() else {
                continue;
            };
            let Ok(port) = network_service.get_port(&server.project_id, port_id).await else {
                continue;
            };
            let address_list = addresses
                .entry(port.network_id.to_string())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(address_list) = address_list.as_array_mut() {
                address_list.push(serde_json::json!({
                    "version": 4,
                    "addr": port.fixed_ip.to_string(),
                    "OS-EXT-IPS:type": "fixed"
                }));
            }
        }
    }
    ServerResponse {
        id: server.id.to_string(),
        name: server.name,
        status: nova_status(server.state).to_owned(),
        tenant_id: server.project_id.clone(),
        project_id: server.project_id,
        image: IdResponse {
            id: server.image_id,
        },
        flavor: IdResponse {
            id: server.flavor_id.to_string(),
        },
        addresses: serde_json::Value::Object(addresses),
        key_name: server.key_name,
        config_drive: server.config_drive,
        metadata: serde_json::Value::Object(serde_json::Map::new()),
        host: server.host,
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
        ComputeError::Unavailable => keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service is unavailable",
        ),
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

#[allow(clippy::result_large_err)]
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

#[allow(clippy::result_large_err)]
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
    let config_drive = if body.server.config_drive == Some(true) {
        let keypair_public_key = if body.server.ssh_public_key.is_none() {
            if let Some(key_name) = body.server.key_name.as_deref() {
                match service
                    .show_keypair(&token.user_id, &token.project_id, key_name)
                    .await
                {
                    Ok(keypair) => Some(keypair.public_key),
                    Err(error) => return compute_error(error),
                }
            } else {
                None
            }
        } else {
            None
        };
        let ssh_public_key = match config_drive_ssh_public_key(
            body.server.ssh_public_key.clone(),
            keypair_public_key,
        ) {
            Ok(value) => value,
            Err(message) => return keystone_error(StatusCode::BAD_REQUEST, "Bad Request", message),
        };
        Some(ConfigDriveRequest {
            user_data: body
                .server
                .user_data
                .clone()
                .unwrap_or_default()
                .into_bytes(),
            vendor_data: body.server.vendor_data.clone().map(String::into_bytes),
            ssh_public_key,
        })
    } else {
        None
    };
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
        .and_then(|reference| reference.parse().ok())
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
        || networks.iter().any(|network| {
            network
                .port
                .as_deref()
                .or(network.uuid.as_deref())
                .is_none_or(str::is_empty)
        })
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
        match image_service.get(&token.project_id, image_id).await {
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
            if let Err(error) = network_service.get_port(&token.project_id, port_id).await {
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
            config_drive,
            idempotency_key: idempotency,
        })
        .await
    {
        Ok(server) => (
            StatusCode::ACCEPTED,
            Json(ServerEnvelope {
                server: server_response(server, state.network.as_deref()).await,
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
        Ok(servers) => {
            let mut server_responses = Vec::with_capacity(servers.len());
            for server in servers {
                server_responses.push(server_response(server, state.network.as_deref()).await);
            }
            Json(ServerListResponse {
                servers: server_responses,
            })
            .into_response()
        }
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
    match service
        .show_server(&token.project_id, ServerId::from_uuid(id))
        .await
    {
        Ok(server) => Json(ServerEnvelope {
            server: server_response(server, state.network.as_deref()).await,
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
    match service
        .delete_server(&token.project_id, ServerId::from_uuid(id))
        .await
    {
        Ok(()) => {
            if let Some(console) = state.console.as_ref()
                && let Err(error) = console.cleanup(id)
            {
                tracing::warn!(%error, server_id = %id, "deleted server console cleanup failed");
                return keystone_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "server console cleanup failed",
                );
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
            if let Err(error) = service
                .show_server(&token.project_id, ServerId::from_uuid(id))
                .await
            {
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
            if should_query_live_console(offset)
                && let Some(registry) = state.agent_registry.as_ref()
            {
                match service
                    .placement_provider_id(&token.project_id, ServerId::from_uuid(id))
                    .await
                {
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
                        if let Ok(op_uuid) = uuid::Uuid::parse_str(&operation_id) {
                            let _ = registry.persist_pending_command(&command, op_uuid).await;
                        }
                        let dispatch_started = std::time::Instant::now();
                        tracing::info!(
                            server_id = %id,
                            agent_id = %agent_id,
                            %operation_id,
                            offset,
                            length,
                            "console dispatch start"
                        );
                        let observation = match registry
                            .dispatch_command_and_wait(command, CONSOLE_AGENT_DISPATCH_TIMEOUT)
                            .await
                        {
                            Ok(observation) => {
                                tracing::info!(
                                    server_id = %id,
                                    %operation_id,
                                    console_bytes = observation.console_log_bytes.len(),
                                    console_offset = observation.console_log_offset,
                                    complete = observation.console_log_complete,
                                    truncated = observation.console_log_truncated,
                                    elapsed_ms = dispatch_started.elapsed().as_millis(),
                                    "console dispatch observation received"
                                );
                                observation
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    server_id = %id,
                                    %operation_id,
                                    elapsed_ms = dispatch_started.elapsed().as_millis(),
                                    "agent console query failed"
                                );
                                if let Some(response) =
                                    cached_console_response(console, id, offset, length)
                                {
                                    tracing::info!(
                                        server_id = %id,
                                        %operation_id,
                                        "console dispatch fell back to cached output"
                                    );
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
                            tracing::warn!(
                                %error,
                                server_id = %id,
                                %operation_id,
                                "agent console observation persistence failed"
                            );
                        }
                        if let Some(response) = cached_console_response(console, id, offset, length)
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
            return match console.read_from(id, offset, length) {
                Ok(chunk) => (
                    StatusCode::OK,
                    Json(serde_json::json!({"output": String::from_utf8_lossy(&chunk.bytes)})),
                )
                    .into_response(),
                Err(ConsoleError::NotFound) => {
                    tracing::info!(
                        server_id = %id,
                        offset,
                        "no persisted console output; returning empty response"
                    );
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
    match service
        .action(&token.project_id, ServerId::from_uuid(id), action)
        .await
    {
        Ok(server) => (
            StatusCode::ACCEPTED,
            Json(ServerEnvelope {
                server: server_response(server, state.network.as_deref()).await,
            }),
        )
            .into_response(),
        Err(error) => compute_error(error),
    }
}

#[derive(Debug, Deserialize)]
struct VolumeAttachmentRequest {
    #[serde(rename = "volumeAttachment", alias = "volume_attachment")]
    volume_attachment: VolumeAttachmentRequestPayload,
}

#[derive(Debug, Deserialize)]
struct VolumeAttachmentRequestPayload {
    #[serde(rename = "volumeId", alias = "volume_id")]
    volume_id: String,
    device: Option<String>,
    tag: Option<String>,
    #[serde(default, alias = "delete_on_termination")]
    delete_on_termination: bool,
}

#[derive(Debug, Serialize)]
struct VolumeAttachmentResponse {
    #[serde(rename = "volumeAttachment")]
    volume_attachment: VolumeAttachmentDetails,
}

#[derive(Debug, Serialize)]
struct VolumeAttachmentsResponse {
    #[serde(rename = "volumeAttachments")]
    volume_attachments: Vec<VolumeAttachmentDetails>,
}

#[derive(Debug, Serialize)]
struct VolumeAttachmentDetails {
    /// Legacy `id` field, emitted only at microversion 2.1 (and below).
    /// Upstream Nova removed it at 2.89 in favor of `attachment_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "attachment_id")]
    attachment_id: String,
    /// `attachmentId` (camel) is a legacy O3K alias emitted at 2.1 only; it is
    /// not part of the upstream 2.89 field set.
    #[serde(skip_serializing_if = "Option::is_none", rename = "attachmentId")]
    attachment_id_camel: Option<String>,
    #[serde(rename = "bdm_uuid")]
    bdm_uuid: String,
    #[serde(rename = "serverId")]
    server_id: String,
    #[serde(rename = "volumeId")]
    volume_id: String,
    device: String,
    tag: Option<String>,
    delete_on_termination: bool,
}

fn map_volume_attachment(
    record: o3k_store::VolumeAttachmentRecord,
    at_289: bool,
) -> VolumeAttachmentDetails {
    let attachment_id = record
        .cinder_attachment_id
        .clone()
        .unwrap_or_else(|| record.id.to_string());
    VolumeAttachmentDetails {
        id: if at_289 {
            None
        } else {
            Some(attachment_id.clone())
        },
        attachment_id: attachment_id.clone(),
        attachment_id_camel: if at_289 { None } else { Some(attachment_id) },
        bdm_uuid: record.id.to_string(),
        server_id: record.server_id.to_string(),
        volume_id: record.volume_id.to_string(),
        device: record.device,
        tag: record.tag,
        delete_on_termination: record.delete_on_termination,
    }
}

async fn attach_volume(
    State(state): State<AppState>,
    Path((project_id, server_id)): Path<(String, String)>,
    Json(request): Json<VolumeAttachmentRequest>,
) -> impl IntoResponse {
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Ok(volume_uuid) = Uuid::parse_str(&request.volume_attachment.volume_id) else {
        return compute_error(ComputeError::InvalidRequest).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .attach_volume(
            &project_id,
            ServerId::from_uuid(server_uuid),
            volume_uuid,
            request.volume_attachment.device,
            request.volume_attachment.tag,
            request.volume_attachment.delete_on_termination,
        )
        .await
    {
        Ok(record) => (
            StatusCode::OK,
            Json(VolumeAttachmentResponse {
                volume_attachment: map_volume_attachment(record, false),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error).into_response(),
    }
}

async fn list_volume_attachments(
    State(state): State<AppState>,
    Path((project_id, server_id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
        .await
    {
        Ok(records) => {
            let at_289 = requested_compute_289(&headers);
            (
                StatusCode::OK,
                Json(VolumeAttachmentsResponse {
                    volume_attachments: records
                        .into_iter()
                        .filter(|r| r.status == "attached")
                        .map(|record| map_volume_attachment(record, at_289))
                        .collect(),
                }),
            )
                .into_response()
        }
        Err(error) => compute_error(error).into_response(),
    }
}

async fn show_volume_attachment(
    State(state): State<AppState>,
    Path((project_id, server_id, attachment_id)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };
    let at_289 = requested_compute_289(&headers);

    if let Ok(records) = compute
        .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
        .await
    {
        for record in records {
            if record.status == "attached"
                && (record.id.to_string() == attachment_id
                    || record.volume_id.to_string() == attachment_id
                    || record.cinder_attachment_id.as_deref() == Some(&attachment_id))
            {
                return (
                    StatusCode::OK,
                    Json(VolumeAttachmentResponse {
                        volume_attachment: map_volume_attachment(record, at_289),
                    }),
                )
                    .into_response();
            }
        }
    }

    compute_error(ComputeError::NotFound).into_response()
}

async fn delete_volume_attachment(
    State(state): State<AppState>,
    Path((project_id, server_id, attachment_id)): Path<(String, String, String)>,
) -> impl IntoResponse {
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    let target_uuid = if let Ok(uuid) = Uuid::parse_str(&attachment_id) {
        if compute
            .get_volume_attachment(&project_id, ServerId::from_uuid(server_uuid), uuid)
            .await
            .is_ok()
        {
            Some(uuid)
        } else {
            None
        }
    } else {
        None
    };

    let target_uuid = match target_uuid {
        Some(uuid) => uuid,
        None => {
            if let Ok(records) = compute
                .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
                .await
            {
                let found = records.into_iter().find(|r| {
                    r.volume_id.to_string() == attachment_id
                        || r.cinder_attachment_id.as_deref() == Some(&attachment_id)
                });
                match found {
                    Some(r) => r.id,
                    None => return compute_error(ComputeError::NotFound).into_response(),
                }
            } else {
                return compute_error(ComputeError::NotFound).into_response();
            }
        }
    };

    match compute
        .detach_volume(&project_id, ServerId::from_uuid(server_uuid), target_uuid)
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => compute_error(error).into_response(),
    }
}

#[cfg(test)]
mod tests {

    use super::{
        CONSOLE_AGENT_DISPATCH_TIMEOUT, Server, ServerId, ServerState, server_response,
        should_query_live_console,
    };
    use std::time::Duration;
    use uuid::Uuid;

    #[test]
    fn live_console_queries_are_limited_to_the_snapshot_offset() {
        assert!(should_query_live_console(0));
        assert!(!should_query_live_console(1));
        assert!(!should_query_live_console(u64::MAX));
    }

    #[test]
    fn live_console_agent_budget_matches_public_request_budget() {
        assert_eq!(CONSOLE_AGENT_DISPATCH_TIMEOUT, Duration::from_secs(15));
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
        assert_eq!(parsed.config_drive, None);
        assert!(
            matches!(parsed.image, Some(super::IdReference::String(value)) if value == "image-id")
        );
        assert!(
            matches!(parsed.flavor, Some(super::IdReference::String(value)) if value == "550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            parsed
                .networks
                .as_ref()
                .and_then(|items| items[0].port.as_deref()),
            Some("550e8400-e29b-41d4-a716-446655440001")
        );
        Ok(())
    }

    #[test]
    fn config_drive_key_name_falls_back_to_the_project_keypair() {
        assert_eq!(
            super::config_drive_ssh_public_key(
                None,
                Some("ssh-ed25519 AAAA generated-by-keypair".to_owned())
            )
            .as_deref(),
            Ok("ssh-ed25519 AAAA generated-by-keypair")
        );
        assert!(super::config_drive_ssh_public_key(None, None).is_err());
    }

    #[tokio::test]
    async fn server_response_reports_requested_config_drive_without_exposing_payload()
    -> Result<(), serde_json::Error> {
        let server = Server {
            id: ServerId::from_uuid(Uuid::nil()),
            name: "server".to_owned(),
            project_id: "project".to_owned(),
            flavor_id: Uuid::nil(),
            image_id: "image".to_owned(),
            state: ServerState::Active,
            key_name: Some("key".to_owned()),
            config_drive: true,
            network_ids: Vec::new(),
            host: None,
        };
        let response = server_response(server, None).await;
        let value = serde_json::to_value(response)?;
        assert_eq!(value.get("config_drive"), Some(&serde_json::json!(true)));
        assert!(value.get("user_data").is_none());
        assert!(value.get("ssh_public_key").is_none());
        assert_eq!(
            value.get("OS-EXT-SRV-ATTR:host"),
            Some(&serde_json::Value::Null)
        );
        Ok(())
    }

    #[tokio::test]
    async fn server_response_reports_the_durable_placement_host() -> Result<(), serde_json::Error> {
        let server = Server {
            id: ServerId::from_uuid(Uuid::nil()),
            name: "server".to_owned(),
            project_id: "project".to_owned(),
            flavor_id: Uuid::nil(),
            image_id: "image".to_owned(),
            state: ServerState::Active,
            key_name: None,
            config_drive: false,
            network_ids: Vec::new(),
            host: Some("node-a".to_owned()),
        };
        let response = server_response(server, None).await;
        let value = serde_json::to_value(response)?;
        assert_eq!(
            value.get("OS-EXT-SRV-ATTR:host"),
            Some(&serde_json::json!("node-a"))
        );
        Ok(())
    }

    #[test]
    fn canonical_server_states_project_to_the_nova_response_shape() {
        let expected = [
            (ServerState::Requested, "REQUESTED"),
            (ServerState::Building, "BUILD"),
            (ServerState::Active, "ACTIVE"),
            (ServerState::Stopping, "STOPPING"),
            (ServerState::Stopped, "SHUTOFF"),
            (ServerState::Starting, "STARTING"),
            (ServerState::Rebooting, "REBOOTING"),
            (ServerState::Deleting, "DELETING"),
            (ServerState::Deleted, "DELETED"),
            (ServerState::Error, "ERROR"),
        ];
        assert_eq!(expected.len(), 10);
        for (state, status) in expected {
            assert_eq!(
                super::nova_status(state),
                status,
                "{state:?} must project to {status}"
            );
        }
    }

    #[tokio::test]
    async fn server_response_projects_the_canonical_state_as_status()
    -> Result<(), serde_json::Error> {
        let server = Server {
            id: ServerId::from_uuid(Uuid::nil()),
            name: "server".to_owned(),
            project_id: "project".to_owned(),
            flavor_id: Uuid::nil(),
            image_id: "image".to_owned(),
            state: ServerState::Stopped,
            key_name: None,
            config_drive: false,
            network_ids: Vec::new(),
            host: None,
        };
        let value = serde_json::to_value(server_response(server, None).await)?;
        assert_eq!(value.get("status"), Some(&serde_json::json!("SHUTOFF")));
        Ok(())
    }
}
