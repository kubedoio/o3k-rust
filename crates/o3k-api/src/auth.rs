//! Shared token and project-scope validation helpers used by the
//! service protocol adapters.

use std::time::SystemTime;

use axum::http::StatusCode;

use crate::{AppState, error::keystone_error};

// Axum handlers consume the concrete response directly; boxing this error would
// add conversions across every OpenStack adapter without changing behavior.
#[allow(clippy::result_large_err)]
pub(crate) fn require_token(
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
// The 404 contract and message are shared with compute's resource lookup;
// this helper is generic but intentionally keeps compute's public message.
#[allow(clippy::result_large_err)]
pub(crate) fn project_token(
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
