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
use o3k_identity::{AuthError, TokenRequest, TokenService};
use o3k_image::{ImageError, ImageRecord, ImageService};
use serde::Serialize;

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
