//! Native identity and context endpoints.
//!
//! ADR-0173 requires a native IAM surface:
//! ```text
//! POST /o3k/v1/identity/tokens
//! GET  /o3k/v1/identity/me
//! ```
//!
//! These are adapters over O3K IAM, not a second identity system.

use std::time::SystemTime;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};

// ── POST /o3k/v1/identity/tokens ──────────────────────────────────────────

/// Native token issuance endpoint.
pub async fn issue_token(
    State(state): State<NativeApiState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(ref issuer) = state.token_issuer else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "IAM is not configured",
            )),
        )
            .into_response();
    };

    match issuer.issue(&body, SystemTime::now()).await {
        Ok((token, response_body)) => {
            let native_response = serde_json::json!({
                "token": {
                    "id": token,
                    "expires_at": response_body["token"]["expires_at"],
                    "issued_at": response_body["token"]["issued_at"],
                    "project": response_body["token"]["project"],
                    "user": response_body["token"]["user"],
                }
            });
            (StatusCode::CREATED, Json(native_response)).into_response()
        }
        Err(pd) => {
            let status = StatusCode::from_u16(pd.status).unwrap_or(StatusCode::UNAUTHORIZED);
            (status, Json(pd)).into_response()
        }
    }
}

// ── GET /o3k/v1/identity/me ───────────────────────────────────────────────

/// Current authentication context for the bearer token.
#[derive(Serialize)]
pub struct CurrentContext {
    pub authenticated: bool,
    pub principal_id: Option<String>,
    pub principal_kind: Option<String>,
    pub principal_name: Option<String>,
    pub effective_scope_id: Option<String>,
    pub effective_scope_kind: Option<String>,
}

/// Returns the current authentication context from the bearer token.
pub async fn current_context(
    auth: Result<BearerAuth, (StatusCode, Json<ProblemDetails>)>,
) -> Json<CurrentContext> {
    match auth {
        Ok(bearer) => {
            let ctx = bearer.0;
            Json(CurrentContext {
                authenticated: true,
                principal_id: Some(ctx.principal().id().to_string()),
                principal_kind: Some(format!("{:?}", ctx.principal().kind()).to_lowercase()),
                principal_name: Some(ctx.principal().name().to_owned()),
                effective_scope_id: Some(ctx.effective_scope().id().to_string()),
                effective_scope_kind: Some(ctx.effective_scope().kind().as_str().to_owned()),
            })
        }
        Err(_) => Json(CurrentContext {
            authenticated: false,
            principal_id: None,
            principal_kind: None,
            principal_name: None,
            effective_scope_id: None,
            effective_scope_kind: None,
        }),
    }
}
