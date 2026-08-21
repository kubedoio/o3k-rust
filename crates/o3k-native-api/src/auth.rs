//! Native API authentication and authorization helpers.
//!
//! Defines the `TokenIssuer` trait that the native identity endpoints
//! consume, and the `require_auth` extractor for protected handlers.
//! The concrete implementation is wired in `o3kd` main.rs where the
//! actual `TokenService` is available.

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use o3k_kernel::AuthContext;

use crate::{
    NativeApiState,
    error::{ErrorCode, ProblemDetails},
};

// ── TokenIssuer trait ──────────────────────────────────────────────────────

/// Lightweight IAM port used by the native identity endpoints.
///
/// The concrete implementation wraps `o3k_identity::TokenService` and is
/// wired at the composition root (`o3kd` main.rs).
#[async_trait::async_trait]
pub trait TokenIssuer: Send + Sync {
    /// Issues a token from a Keystone-compatible JSON token request.
    /// Returns `(subject_token_string, full_response_body)` on success.
    async fn issue(
        &self,
        request: &serde_json::Value,
        now: std::time::SystemTime,
    ) -> Result<(String, serde_json::Value), ProblemDetails>;

    /// Validates a bearer token and returns the canonical AuthContext.
    async fn auth_context(&self, token: &str) -> Result<AuthContext, ProblemDetails>;
}

// ── Bearer auth extractor ──────────────────────────────────────────────────

/// Extractor that validates a bearer token and provides the canonical
/// `AuthContext`.
///
/// Returns `ProblemDetails` directly (as an error response) when the
/// token is missing or invalid.
#[derive(Debug, Clone)]
pub struct BearerAuth(pub AuthContext);

impl<S> FromRequestParts<S> for BearerAuth
where
    S: Send + Sync,
    NativeApiState: FromRef<S>,
{
    type Rejection = (StatusCode, axum::Json<ProblemDetails>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let api_state = NativeApiState::from_ref(state);

        let Some(ref issuer) = api_state.token_issuer else {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(ProblemDetails::with_detail(
                    ErrorCode::NotAvailable,
                    "IAM is not configured",
                )),
            ));
        };

        let header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(ProblemDetails::unauthorized()),
                )
            })?;

        let token = header.strip_prefix("Bearer ").ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(ProblemDetails::unauthorized()),
            )
        })?;

        let ctx = issuer
            .auth_context(token)
            .await
            .map_err(|pd| (StatusCode::UNAUTHORIZED, axum::Json(pd)))?;

        Ok(BearerAuth(ctx))
    }
}
