//! Shared token validation and kernel AuthContext conversion helpers.

use std::time::SystemTime;

use axum::http::StatusCode;
use o3k_kernel::AuthContext;

use crate::{AppState, error::keystone_error};

// Axum handlers consume the concrete response directly; boxing this error would
// add conversions across every OpenStack adapter without changing behavior.
#[allow(clippy::result_large_err)]
pub(crate) fn require_auth_context(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<AuthContext, axum::response::Response> {
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
    service.auth_context(token, SystemTime::now()).map_err(|_| {
        keystone_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The request has not been authenticated.",
        )
    })
}
