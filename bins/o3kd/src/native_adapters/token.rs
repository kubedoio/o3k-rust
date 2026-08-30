use std::sync::Arc;
use std::time::SystemTime;

use o3k_native_api::{
    auth::{NativeCredentialV1, NativeTokenRequestV1, TokenIssuer},
    error::ProblemDetails,
};

/// Store-backed canonical operation visibility adapter. Historical operation
/// rows without P12.4 metadata fail closed rather than being reconstructed
/// with fabricated ownership or action fields.
pub struct TokenIssuerAdapter {
    pub service: Arc<o3k_identity::TokenService>,
}

#[async_trait::async_trait]
impl TokenIssuer for TokenIssuerAdapter {
    async fn issue_native(
        &self,
        request: &NativeTokenRequestV1,
    ) -> Result<(String, serde_json::Value), ProblemDetails> {
        let credential = request
            .auth
            .credential()
            .map_err(ProblemDetails::bad_request)?;
        let (methods, password, token) = match credential {
            NativeCredentialV1::Password { user_id, password } => (
                vec!["password".to_owned()],
                Some(o3k_identity::PasswordIdentity {
                    user: o3k_identity::UserReference {
                        id: Some(user_id),
                        name: None,
                        domain: None,
                        password,
                    },
                }),
                None,
            ),
            NativeCredentialV1::Token { token } => (
                vec!["token".to_owned()],
                None,
                Some(o3k_identity::TokenIdentity { id: token }),
            ),
        };
        // Build a Keystone-compatible TokenRequest from native request
        let token_req = o3k_identity::TokenRequest {
            auth: o3k_identity::Auth {
                identity: o3k_identity::Identity {
                    methods,
                    password,
                    token,
                },
                scope: request
                    .auth
                    .project_id
                    .as_ref()
                    .map(|pid| o3k_identity::Scope {
                        project: Some(o3k_identity::ProjectReference {
                            id: Some(pid.clone()),
                            name: None,
                            domain: None,
                        }),
                    }),
            },
        };

        match self.service.issue(&token_req, SystemTime::now()) {
            Ok((token, response)) => match serde_json::to_value(response) {
                Ok(val) => Ok((token, val)),
                Err(_) => Err(ProblemDetails::internal()),
            },
            Err(_) => Err(ProblemDetails::unauthorized()),
        }
    }

    async fn auth_context(&self, token: &str) -> Result<o3k_kernel::AuthContext, ProblemDetails> {
        self.service
            .auth_context(token, SystemTime::now())
            .map_err(|_| ProblemDetails::unauthorized())
    }
}

// ── ServerReader ──────────────────────────────────────────────────────────
