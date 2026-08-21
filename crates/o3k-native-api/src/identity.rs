//! Native identity and context endpoints.
//!
//! ADR-0173 requires a native IAM surface:
//! ```text
//! POST /o3k/v1/identity/tokens
//! GET  /o3k/v1/identity/me
//! ```
//!
//! These are adapters over O3K IAM, not a second identity system.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::NativeApiState;

/// Current context/identity response — initially a stub that will be wired
/// to O3K IAM in P12.4+.
#[derive(Serialize)]
pub struct CurrentContext {
    pub authenticated: bool,
    pub principal_id: Option<String>,
    pub principal_kind: Option<String>,
    pub effective_scope: Option<String>,
}

/// Returns the current authentication context, or an unauthenticated response.
pub async fn current_context(_state: State<NativeApiState>) -> impl IntoResponse {
    let ctx = CurrentContext {
        authenticated: false,
        principal_id: None,
        principal_kind: None,
        effective_scope: None,
    };
    (StatusCode::OK, Json(ctx))
}
