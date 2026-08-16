//! Glance-compatible image protocol adapter: create/list/show/download/
//! upload/delete handlers, wire models, and error mapping.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use o3k_image::{ImageError, ImageRecord};
use sha2::{Digest, Sha256};

use crate::{AppState, auth::require_auth_context, error::keystone_error};

#[derive(serde::Deserialize)]
pub(crate) struct CreateImageRequest {
    name: String,
    #[serde(default = "default_visibility")]
    visibility: String,
    container_format: String,
    disk_format: String,
}

#[derive(serde::Serialize)]
pub(crate) struct ImageResponse {
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
pub(crate) struct ImageListResponse {
    images: Vec<ImageResponse>,
}

pub(crate) fn default_visibility() -> String {
    "private".to_owned()
}

pub(crate) fn image_response(image: ImageRecord) -> ImageResponse {
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

pub(crate) fn image_error(error: ImageError) -> axum::response::Response {
    match error {
        ImageError::Unauthorized => keystone_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The request has not been authenticated.",
        ),
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
        ImageError::QuotaExceeded {
            ref key,
            limit,
            used,
            requested,
        } => {
            let message = format!(
                "Quota exceeded for {key}: limit {limit}, used {used}, requested {requested}"
            );
            if key.resource() == "bytes" {
                keystone_error(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large", message)
            } else {
                keystone_error(StatusCode::FORBIDDEN, "Forbidden", message)
            }
        }
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
        ImageError::FormatVerificationFailed => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "image content failed format verification",
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

pub(crate) async fn download_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.resolve_artifact(&auth, id).await {
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

pub(crate) async fn create_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<CreateImageRequest>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
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
            &auth,
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

pub(crate) async fn list_images(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.list(&auth).await {
        Ok(images) => Json(ImageListResponse {
            images: images.into_iter().map(image_response).collect(),
        })
        .into_response(),
        Err(error) => image_error(error),
    }
}

pub(crate) async fn show_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.get(&auth, id).await {
        Ok(image) => Json(image_response(image)).into_response(),
        Err(error) => image_error(error),
    }
}

pub(crate) async fn upload_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    body: Bytes,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
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
    match service.upload(&auth, id, &body).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_error(error),
    }
}

pub(crate) async fn delete_image(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Some(service) = &state.image else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "image service is not configured",
        );
    };
    match service.delete(&auth, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_error(error),
    }
}
