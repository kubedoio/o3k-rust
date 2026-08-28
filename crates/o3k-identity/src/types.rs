//! Identity domain types: auth, tokens, projects, users, services, passwords.

use std::fmt;

use base64::{
    Engine as _,
    engine::general_purpose::STANDARD as BASE64,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// A value that must never be logged or formatted with its contents.
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
    pub token: Option<TokenIdentity>,
}

#[derive(Deserialize)]
pub struct TokenIdentity {
    pub id: String,
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

/// Durable domain models for identity resources loaded from the store. IDs are
/// stable and independent from display names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDomain {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotProject {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotUser {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub password_hash: PasswordHash,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRole {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAssignment {
    pub user_id: String,
    pub project_id: String,
    pub role_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotService {
    pub id: String,
    pub name: String,
    pub service_type: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEndpoint {
    pub id: String,
    pub service_id: String,
    pub url: String,
    pub interface: String,
    pub region: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRegion {
    pub id: String,
    pub enabled: bool,
}

/// Immutable identity universe loaded from the durable store. `TokenService`
/// authenticates and validates against this snapshot; restarting the control
/// plane reloads it from the durable records, so identity state survives
/// restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentitySnapshot {
    pub domains: Vec<SnapshotDomain>,
    pub projects: Vec<SnapshotProject>,
    pub users: Vec<SnapshotUser>,
    pub roles: Vec<SnapshotRole>,
    pub assignments: Vec<SnapshotAssignment>,
    pub services: Vec<SnapshotService>,
    pub endpoints: Vec<SnapshotEndpoint>,
    pub regions: Vec<SnapshotRegion>,
}

impl IdentitySnapshot {
    pub(crate) fn user_by_id(&self, id: &str) -> Option<&SnapshotUser> {
        self.users.iter().find(|user| user.id == id)
    }

    pub(crate) fn user_by_name(&self, domain_id: &str, name: &str) -> Option<&SnapshotUser> {
        self.users
            .iter()
            .find(|user| user.domain_id == domain_id && user.name == name)
    }

    pub(crate) fn project_by_id(&self, id: &str) -> Option<&SnapshotProject> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub(crate) fn project_by_name(&self, domain_id: &str, name: &str) -> Option<&SnapshotProject> {
        self.projects
            .iter()
            .find(|project| project.domain_id == domain_id && project.name == name)
    }

    pub(crate) fn domain_by_id(&self, id: &str) -> Option<&SnapshotDomain> {
        self.domains.iter().find(|domain| domain.id == id)
    }

    pub(crate) fn domain_by_name(&self, name: &str) -> Option<&SnapshotDomain> {
        self.domains.iter().find(|domain| domain.name == name)
    }

    pub(crate) fn role_names_for(&self, user_id: &str, project_id: &str) -> Vec<(String, String)> {
        let mut roles: Vec<(String, String)> = self
            .assignments
            .iter()
            .filter(|assignment| {
                assignment.user_id == user_id && assignment.project_id == project_id
            })
            .filter_map(|assignment| {
                self.roles
                    .iter()
                    .find(|role| role.id == assignment.role_id)
                    .map(|role| (role.id.clone(), role.name.clone()))
            })
            .collect();
        roles.sort();
        roles.dedup();
        roles
    }
}

/// PBKDF2-HMAC-SHA256 password hash stored as a self-describing string:
/// `pbkdf2-sha256$<iterations>$<salt_b64>$<dk_b64>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordHash {
    encoded: Secret,
}

impl PasswordHash {
pub(crate) const ITERATIONS: u32 = 100_000;
    const DK_LEN: usize = 32;

    pub fn derive(password: &str) -> Result<Self, AuthError> {
        Self::derive_with_iterations(password, Self::ITERATIONS)
    }

    /// Derives a hash with an explicit iteration count. Tests and constrained
    /// environments may use lower counts; the encoded hash records the count
    /// actually used.
    pub fn derive_with_iterations_for_testing(
        password: &str,
        iterations: u32,
    ) -> Result<Self, AuthError> {
        Self::derive_with_iterations(password, iterations)
    }

    pub(crate) fn derive_with_iterations(password: &str, iterations: u32) -> Result<Self, AuthError> {
        if iterations < 1 {
            return Err(AuthError::InvalidRequest);
        }
        let salt = Uuid::new_v4().as_bytes().to_vec();
        Ok(Self {
            encoded: Secret::new(Self::encode(password.as_bytes(), &salt, iterations)?),
        })
    }

    fn encode(password: &[u8], salt: &[u8], iterations: u32) -> Result<String, AuthError> {
        let derived = pbkdf2_sha256(password, salt, iterations, Self::DK_LEN)?;
        Ok(format!(
            "pbkdf2-sha256${iterations}${}${}",
            BASE64.encode(salt),
            BASE64.encode(derived)
        ))
    }

    pub fn encoded(&self) -> &str {
        self.encoded.expose()
    }

    pub fn verify(&self, password: &str) -> bool {
        let parts: Vec<&str> = self.encoded.expose().split('$').collect();
        if parts.len() != 4 || parts[0] != "pbkdf2-sha256" {
            return false;
        }
        let Ok(iterations) = parts[1].parse::<u32>() else {
            return false;
        };
        let Ok(salt) = BASE64.decode(parts[2]) else {
            return false;
        };
        let Ok(expected) = BASE64.decode(parts[3]) else {
            return false;
        };
        let Ok(derived) = pbkdf2_sha256(password.as_bytes(), &salt, iterations, expected.len())
        else {
            return false;
        };
        constant_time_eq(&derived, &expected)
    }
}

impl fmt::Display for PasswordHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// PKCS#5 PBKDF2 with HMAC-SHA256 PRF (RFC 8018).
pub(crate) fn pbkdf2_sha256(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    dk_len: usize,
) -> Result<Vec<u8>, AuthError> {
    if iterations < 1 || dk_len == 0 {
        return Err(AuthError::InvalidRequest);
    }
    let mut out = Vec::with_capacity(dk_len);
    let mut block_index: u32 = 1;
    while out.len() < dk_len {
        let mut mac =
            HmacSha256::new_from_slice(password).map_err(|_| AuthError::InvalidRequest)?;
        mac.update(salt);
        mac.update(&block_index.to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut t = u;
        for _ in 1..iterations {
            let mut mac =
                HmacSha256::new_from_slice(password).map_err(|_| AuthError::InvalidRequest)?;
            mac.update(&u);
            u = mac.finalize().into_bytes();
            for (t_byte, u_byte) in t.iter_mut().zip(u.iter()) {
                *t_byte ^= *u_byte;
            }
        }
        out.extend_from_slice(&t);
        block_index = block_index.saturating_add(1);
        if block_index == 0 {
            return Err(AuthError::InvalidRequest);
        }
    }
    out.truncate(dk_len);
    Ok(out)
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (l, r) in left.iter().zip(right.iter()) {
        diff |= l ^ r;
    }
    diff == 0
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
    #[error("identity state is not available")]
    IdentityUnavailable,
}

/// Configuration for the deterministic TestLab bootstrap identity universe.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// Public base URL advertised in the durable service catalog.
    pub catalog_endpoint: String,
    /// Bootstrap administrator password.
    pub bootstrap_password: Secret,
    /// Cinder service-user password. When absent, the Cinder service user is
    /// not created.
    pub cinder_password: Option<Secret>,
    /// External Cinder API base URL. When present, a durable `volumev3`
    /// service and endpoint are registered.
    pub cinder_endpoint: Option<String>,
    /// PBKDF2 iteration count. Zero selects the production default.
    pub pbkdf2_iterations: u32,
    /// Optional additional isolated project/user pairs seeded alongside the
    /// bootstrap universe. Intentionally empty by default: the hosted-service
    /// protected runner sets this to prove cross-tenant isolation with a
    /// second fully independent project. Not an identity administration API.
    pub extra_projects: Vec<ExtraProjectSeed>,
}

/// A fully independent project with exactly one user and role assignments
/// inside that project. The seed is idempotent like the rest of the bootstrap
/// universe, and passwords are never logged.
#[derive(Debug, Clone)]
pub struct ExtraProjectSeed {
    /// Durable project UUID (also used as the URL path project).
    pub project_id: String,
    /// Human project name.
    pub project_name: String,
    /// Durable user UUID.
    pub user_id: String,
    /// Human user name.
    pub user_name: String,
    /// User password.
    pub password: Secret,
}
