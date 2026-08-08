//! Keystone-compatible identity protocol adapter: token issue, validate,
//! and check handlers.

use std::time::SystemTime;

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use o3k_identity::{AuthError, TokenRequest};

use crate::{AppState, error::keystone_error};

pub(crate) async fn issue_token(
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

pub(crate) async fn validate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
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

pub(crate) async fn check_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
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
