use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Deserialize)]
pub struct TokenRequest {
    pub auth: Auth,
}

#[derive(Deserialize)]
pub struct Auth {
    pub identity: Identity,
    pub scope: Option<Scope>,
}

#[derive(Deserialize)]
pub struct Identity {
    pub methods: Vec<String>,
    pub password: Option<PasswordIdentity>,
}

#[derive(Deserialize)]
pub struct PasswordIdentity {
    pub user: UserReference,
}

#[derive(Deserialize)]
pub struct UserReference {
    pub id: Option<String>,
    pub name: Option<String>,
    pub domain: Option<DomainReference>,
    pub password: String,
}

#[derive(Deserialize)]
pub struct DomainReference {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct Scope {
    pub project: Option<ProjectReference>,
}

#[derive(Deserialize)]
pub struct ProjectReference {
    pub id: Option<String>,
    pub name: Option<String>,
    pub domain: Option<DomainReference>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub token: TokenDetails,
}

#[derive(Debug, Serialize)]
pub struct TokenDetails {
    pub expires_at: String,
    pub issued_at: String,
    pub methods: Vec<String>,
    pub project: ProjectDetails,
    pub user: UserDetails,
    pub roles: Vec<RoleDetails>,
    pub catalog: Vec<ServiceDetails>,
}

#[derive(Debug, Serialize)]
pub struct ProjectDetails {
    pub id: String,
    pub name: String,
    pub domain: DomainDetails,
}

#[derive(Debug, Serialize)]
pub struct UserDetails {
    pub id: String,
    pub name: String,
    pub domain: DomainDetails,
    pub password_expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DomainDetails {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct RoleDetails {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ServiceDetails {
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: String,
    pub id: String,
    pub endpoints: Vec<EndpointDetails>,
}

#[derive(Debug, Serialize)]
pub struct EndpointDetails {
    pub url: String,
    pub interface: String,
    pub region: String,
    pub region_id: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("invalid authentication request")]
    InvalidRequest,
    #[error("authentication failed")]
    Unauthorized,
    #[error("token is invalid")]
    InvalidToken,
    #[error("token has expired")]
    ExpiredToken,
    #[error("token signing key must be at least 32 bytes")]
    WeakSigningKey,
}

#[derive(Clone)]
pub struct TokenService {
    user_id: String,
    user_name: String,
    password: Secret,
    project_id: String,
    project_name: String,
    signing_key: Secret,
    token_ttl: Duration,
}

impl TokenService {
    pub fn new(
        user_id: String,
        user_name: String,
        password: Secret,
        project_id: String,
        project_name: String,
        signing_key: Secret,
        token_ttl: Duration,
    ) -> Result<Self, AuthError> {
        if signing_key.expose().len() < 32 {
            return Err(AuthError::WeakSigningKey);
        }
        if token_ttl.is_zero() {
            return Err(AuthError::InvalidRequest);
        }
        Ok(Self {
            user_id,
            user_name,
            password,
            project_id,
            project_name,
            signing_key,
            token_ttl,
        })
    }

    pub fn issue(
        &self,
        request: &TokenRequest,
        now: SystemTime,
    ) -> Result<(String, TokenResponse), AuthError> {
        let user = &request
            .auth
            .identity
            .password
            .as_ref()
            .ok_or(AuthError::InvalidRequest)?
            .user;
        if !valid_domain(user.domain.as_ref()) {
            return Err(AuthError::InvalidRequest);
        }
        if request.auth.identity.methods != ["password"] {
            return Err(AuthError::InvalidRequest);
        }
        if user.password != self.password.expose()
            || !matches_reference(
                user.id.as_deref(),
                user.name.as_deref(),
                &self.user_id,
                &self.user_name,
            )
        {
            return Err(AuthError::Unauthorized);
        }
        let project = request
            .auth
            .scope
            .as_ref()
            .and_then(|scope| scope.project.as_ref())
            .ok_or(AuthError::InvalidRequest)?;
        if !valid_domain(project.domain.as_ref()) {
            return Err(AuthError::InvalidRequest);
        }
        if !matches_reference(
            project.id.as_deref(),
            project.name.as_deref(),
            &self.project_id,
            &self.project_name,
        ) {
            return Err(AuthError::Unauthorized);
        }

        let issued = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::InvalidRequest)?
            .as_secs();
        let expires = issued.saturating_add(self.token_ttl.as_secs());
        let token_id = Uuid::now_v7().to_string();
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Claims {
                sub: self.user_id.clone(),
                project: self.project_id.clone(),
                issued,
                expires,
                token_id,
            })
            .map_err(|_| AuthError::InvalidRequest)?,
        );
        let signing_input = format!("{header}.{payload}");
        let signature = sign(&self.signing_key, signing_input.as_bytes())?;
        let token = format!("{signing_input}.{signature}");
        let issued_at = format_time(issued)?;
        let expires_at = format_time(expires)?;
        Ok((
            token,
            TokenResponse {
                token: self.details(issued_at, expires_at),
            },
        ))
    }

    pub fn verify(&self, token: &str, now: SystemTime) -> Result<VerifiedToken, AuthError> {
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(AuthError::InvalidToken);
        };
        if token.len() > 4096 {
            return Err(AuthError::InvalidToken);
        }
        let decoded_header = URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| AuthError::InvalidToken)?;
        let parsed_header: Header =
            serde_json::from_slice(&decoded_header).map_err(|_| AuthError::InvalidToken)?;
        if parsed_header.alg != "HS256" || parsed_header.typ != "JWT" {
            return Err(AuthError::InvalidToken);
        }
        let signing_input = format!("{header}.{payload}");
        let expected = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| AuthError::InvalidToken)?;
        let mut mac = HmacSha256::new_from_slice(self.signing_key.expose().as_bytes())
            .map_err(|_| AuthError::InvalidToken)?;
        mac.update(signing_input.as_bytes());
        mac.verify_slice(&expected)
            .map_err(|_| AuthError::InvalidToken)?;
        let claims: Claims = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| AuthError::InvalidToken)?,
        )
        .map_err(|_| AuthError::InvalidToken)?;
        if claims.sub != self.user_id || claims.project != self.project_id {
            return Err(AuthError::InvalidToken);
        }
        let now = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::InvalidToken)?
            .as_secs();
        if now >= claims.expires {
            return Err(AuthError::ExpiredToken);
        }
        Ok(VerifiedToken {
            user_id: claims.sub,
            project_id: claims.project,
            expires: claims.expires,
        })
    }

    fn details(&self, issued_at: String, expires_at: String) -> TokenDetails {
        TokenDetails {
            expires_at,
            issued_at,
            methods: vec!["password".to_owned()],
            project: ProjectDetails {
                id: self.project_id.clone(),
                name: self.project_name.clone(),
                domain: default_domain(),
            },
            user: UserDetails {
                id: self.user_id.clone(),
                name: self.user_name.clone(),
                domain: default_domain(),
                password_expires_at: None,
            },
            roles: vec![RoleDetails {
                id: "member".to_owned(),
                name: "member".to_owned(),
            }],
            catalog: vec![service(
                "identity",
                "identity",
                "identity",
                "http://127.0.0.1:8080/v3",
            )],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    pub user_id: String,
    pub project_id: String,
    pub expires: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    project: String,
    issued: u64,
    expires: u64,
    token_id: String,
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    typ: String,
}

fn matches_reference(
    id: Option<&str>,
    name: Option<&str>,
    expected_id: &str,
    expected_name: &str,
) -> bool {
    match (id, name) {
        (Some(id), Some(name)) => id == expected_id && name == expected_name,
        (Some(id), None) => id == expected_id,
        (None, Some(name)) => name == expected_name,
        (None, None) => false,
    }
}

fn valid_domain(domain: Option<&DomainReference>) -> bool {
    domain.is_none_or(|domain| {
        matches_reference(
            domain.id.as_deref(),
            domain.name.as_deref(),
            "default",
            "Default",
        )
    })
}

fn sign(key: &Secret, input: &[u8]) -> Result<String, AuthError> {
    let mut mac =
        HmacSha256::new_from_slice(key.expose().as_bytes()).map_err(|_| AuthError::InvalidToken)?;
    mac.update(input);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn format_time(seconds: u64) -> Result<String, AuthError> {
    OffsetDateTime::from_unix_timestamp(
        i64::try_from(seconds).map_err(|_| AuthError::InvalidRequest)?,
    )
    .map_err(|_| AuthError::InvalidRequest)?
    .format(&Rfc3339)
    .map_err(|_| AuthError::InvalidRequest)
}

fn default_domain() -> DomainDetails {
    DomainDetails {
        id: "default".to_owned(),
        name: "Default".to_owned(),
    }
}

fn service(name: &str, service_type: &str, id: &str, url: &str) -> ServiceDetails {
    ServiceDetails {
        name: name.to_owned(),
        service_type: service_type.to_owned(),
        id: id.to_owned(),
        endpoints: vec![EndpointDetails {
            url: url.to_owned(),
            interface: "public".to_owned(),
            region: "RegionOne".to_owned(),
            region_id: "RegionOne".to_owned(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Result<TokenService, AuthError> {
        TokenService::new(
            "user-1".to_owned(),
            "admin".to_owned(),
            Secret::new("password".to_owned()),
            "project-1".to_owned(),
            "admin".to_owned(),
            Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
            Duration::from_secs(3600),
        )
    }

    fn request(password: &str) -> TokenRequest {
        TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["password".to_owned()],
                    password: Some(PasswordIdentity {
                        user: UserReference {
                            id: None,
                            name: Some("admin".to_owned()),
                            domain: None,
                            password: password.to_owned(),
                        },
                    }),
                },
                scope: Some(Scope {
                    project: Some(ProjectReference {
                        id: None,
                        name: Some("admin".to_owned()),
                        domain: None,
                    }),
                }),
            },
        }
    }

    #[test]
    fn password_scope_issues_and_verifies_token() -> Result<(), AuthError> {
        let service = service()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, response) = service.issue(&request("password"), now)?;
        assert!(token.split('.').count() == 3);
        assert_eq!(response.token.project.id, "project-1");
        assert_eq!(service.verify(&token, now)?.user_id, "user-1");
        Ok(())
    }

    #[test]
    fn invalid_password_is_not_accepted() -> Result<(), AuthError> {
        let service = service()?;
        assert!(matches!(
            service.issue(&request("wrong"), UNIX_EPOCH),
            Err(AuthError::Unauthorized)
        ));
        Ok(())
    }

    #[test]
    fn expired_token_is_rejected() -> Result<(), AuthError> {
        let service = service()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, _) = service.issue(&request("password"), now)?;
        assert_eq!(
            service.verify(&token, now + Duration::from_secs(3600)),
            Err(AuthError::ExpiredToken)
        );
        Ok(())
    }
}
