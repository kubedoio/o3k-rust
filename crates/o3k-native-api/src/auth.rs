//! Native API authentication and authorization helpers.
//!
//! Defines:
//! - `NativeTokenRequestV1` — native IAM request DTO
//! - `TokenIssuer` trait — port for token issuance/validation
//! - `BearerAuth` extractor — 401 on failure (never swallowed)

use axum::{
    extract::{FromRef, FromRequestParts},
    http::request::Parts,
    response::{IntoResponse, Response},
};
use o3k_kernel::AuthContext;
use serde::{Deserialize, Serialize};

use crate::{NativeApiState, error::ProblemDetails};

// ── Native Token Request DTO ──────────────────────────────────────────────

/// Native token request (SPEC-0030 §5).
///
/// This is the canonical native IAM request, separate from the
/// Keystone-compatible `TokenRequest`. Both map to the same O3K IAM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeTokenRequestV1 {
    pub auth: NativeAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeAuth {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<NativePasswordCredentials>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Validated native credential.  The wire DTO remains deliberately separate
/// from Keystone's request shape; callers must pass through this validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCredentialV1 {
    Password { user_id: String, password: String },
    Token { token: String },
}

impl NativeAuth {
    pub fn credential(&self) -> Result<NativeCredentialV1, &'static str> {
        match (
            self.method.as_str(),
            self.password.as_ref(),
            self.token.as_ref(),
        ) {
            ("password", Some(password), None)
                if !password.user_id.is_empty() && !password.password.is_empty() =>
            {
                Ok(NativeCredentialV1::Password {
                    user_id: password.user_id.clone(),
                    password: password.password.clone(),
                })
            }
            ("token", None, Some(token)) if !token.is_empty() => Ok(NativeCredentialV1::Token {
                token: token.clone(),
            }),
            ("password" | "token", _, _) => Err("credential does not match method"),
            _ => Err("unknown native credential method"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativePasswordCredentials {
    pub user_id: String,
    pub password: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(
        method: &str,
        password: Option<NativePasswordCredentials>,
        token: Option<&str>,
    ) -> NativeAuth {
        NativeAuth {
            method: method.to_owned(),
            password,
            token: token.map(str::to_owned),
            project_id: None,
        }
    }

    #[test]
    fn credentials_are_strictly_discriminated() {
        assert!(matches!(
            auth(
                "password",
                Some(NativePasswordCredentials {
                    user_id: "u".into(),
                    password: "p".into()
                }),
                None
            )
            .credential(),
            Ok(NativeCredentialV1::Password { .. })
        ));
        assert!(matches!(
            auth("token", None, Some("t")).credential(),
            Ok(NativeCredentialV1::Token { .. })
        ));
        assert!(auth("password", None, Some("t")).credential().is_err());
        assert!(
            auth(
                "token",
                Some(NativePasswordCredentials {
                    user_id: "u".into(),
                    password: "p".into()
                }),
                None
            )
            .credential()
            .is_err()
        );
        assert!(auth("other", None, None).credential().is_err());
    }
}

// ── TokenIssuer trait ──────────────────────────────────────────────────────

/// Lightweight IAM port used by the native identity endpoints.
#[async_trait::async_trait]
pub trait TokenIssuer: Send + Sync {
    /// Issues a token from a native token request.
    async fn issue_native(
        &self,
        request: &NativeTokenRequestV1,
    ) -> Result<(String, serde_json::Value), ProblemDetails>;

    /// Validates a bearer token and returns the canonical AuthContext.
    async fn auth_context(&self, token: &str) -> Result<AuthContext, ProblemDetails>;
}

// ── Bearer auth extractor ──────────────────────────────────────────────────

/// Extractor that validates a bearer token and provides the canonical
/// `AuthContext`.
///
/// Missing/malformed/invalid bearer → 401 application/problem+json.
/// Never returns 200 with authenticated=false.
#[derive(Debug, Clone)]
pub struct BearerAuth(pub AuthContext);

impl<S> FromRequestParts<S> for BearerAuth
where
    S: Send + Sync,
    NativeApiState: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ProblemDetails::unauthorized().into_response())?;

        // Missing credentials are always an authentication failure.  Check
        // this before optional service configuration to avoid leaking a 503.
        let api_state = NativeApiState::from_ref(state);
        let Some(ref issuer) = api_state.token_issuer else {
            return Err(ProblemDetails::with_detail(
                crate::error::ErrorCode::NotAvailable,
                "IAM is not configured",
            )
            .into_response());
        };

        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ProblemDetails::unauthorized().into_response())?;

        let ctx = issuer
            .auth_context(token)
            .await
            .map_err(|_| ProblemDetails::unauthorized().into_response())?;

        Ok(BearerAuth(ctx))
    }
}
