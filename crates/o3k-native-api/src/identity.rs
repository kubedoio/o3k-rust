//! Native identity and context endpoints.
//!
//! ADR-0173 requires:
//! ```text
//! POST /o3k/v1/identity/tokens  — native token issuance
//! GET  /o3k/v1/identity/me      — current auth context (requires bearer)
//! ```
//!
//! These are adapters over O3K IAM, not a second identity system.

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthorizationDecision, AuthorizationRequest, ResourceTarget, ResourceType, ScopeId,
};
use serde::Serialize;

use crate::{
    NativeApiState,
    auth::{BearerAuth, NativeTokenRequestV1, RequestId},
    error::{ErrorCode, ProblemDetails},
};

// ── POST /o3k/v1/identity/tokens ──────────────────────────────────────────

/// Native token issuance endpoint.
///
/// Accepts `NativeTokenRequestV1` and produces a bearer token
/// usable against the native API. Same canonical O3K IAM as the
/// Keystone-compatible path.
pub async fn issue_token(
    State(state): State<NativeApiState>,
    request_id: RequestId,
    body: Result<Json<NativeTokenRequestV1>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return ProblemDetails::bad_request("invalid native credential request")
                .with_request_id(request_id.0)
                .into_response();
        }
    };
    let Some(ref issuer) = state.token_issuer else {
        return ProblemDetails::with_detail(ErrorCode::NotAvailable, "IAM is not configured")
            .with_request_id(request_id.0)
            .into_response();
    };

    match issuer.issue_native(&body).await {
        Ok((token, _response_body)) => {
            let native_response = serde_json::json!({
                "token": {
                    "id": token,
                    "expires_at": _response_body["token"]["expires_at"],
                    "issued_at": _response_body["token"]["issued_at"],
                    "project": _response_body["token"]["project"],
                    "user": _response_body["token"]["user"],
                }
            });
            (StatusCode::CREATED, Json(native_response)).into_response()
        }
        Err(pd) => pd.with_request_id(request_id.0).into_response(),
    }
}

// ── GET /o3k/v1/identity/me ───────────────────────────────────────────────

/// Current authentication context.
#[derive(Serialize)]
pub struct CurrentContext {
    pub authenticated: bool,
    pub principal_id: String,
    pub principal_kind: String,
    pub principal_name: String,
    pub effective_scope_id: String,
    pub effective_scope_kind: String,
}

/// Returns the current authentication context.
///
/// Requires a valid Bearer token. Missing/invalid bearer → 401.
pub async fn current_context(auth: BearerAuth, _request_id: RequestId) -> Json<CurrentContext> {
    let ctx = auth.0;
    Json(CurrentContext {
        authenticated: true,
        principal_id: ctx.principal().id().to_string(),
        principal_kind: format!("{:?}", ctx.principal().kind()).to_lowercase(),
        principal_name: ctx.principal().name().to_owned(),
        effective_scope_id: ctx.effective_scope().id().to_string(),
        effective_scope_kind: ctx.effective_scope().kind().as_str().to_owned(),
    })
}

/// Bounded operator-console probe. The route is intentionally server-
/// authorized and does not expose shell, host, or data-plane controls.
pub async fn operator_profile(
    State(state): State<NativeApiState>,
    auth: BearerAuth,
    request_id: RequestId,
) -> Response {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operator authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let target = ResourceTarget::collection(
        ResourceType::new_unchecked("operator", "profile"),
        Some(ScopeId::new_unchecked("system")),
    );
    match authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        action: ActionId::new_unchecked("operator", "ReadProfile"),
        resource_target: target,
    }) {
        AuthorizationDecision::Allow => Json(serde_json::json!({
            "profile": "operator-console",
            "scope": "system",
            "principal_id": auth.0.principal().id().to_string(),
            "audit_id": auth.0.audit_id(),
        }))
        .into_response(),
        AuthorizationDecision::Deny { .. } => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "operator authorization denied")
                .with_request_id(request_id.0)
                .into_response()
        }
    }
}
