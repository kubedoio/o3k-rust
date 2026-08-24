use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

use o3k_kernel::{
    AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, ServicePrincipal, UserPrincipal,
};
use o3k_store::{IdentityRepository, StoreError};

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
    fn user_by_id(&self, id: &str) -> Option<&SnapshotUser> {
        self.users.iter().find(|user| user.id == id)
    }

    fn user_by_name(&self, domain_id: &str, name: &str) -> Option<&SnapshotUser> {
        self.users
            .iter()
            .find(|user| user.domain_id == domain_id && user.name == name)
    }

    fn project_by_id(&self, id: &str) -> Option<&SnapshotProject> {
        self.projects.iter().find(|project| project.id == id)
    }

    fn project_by_name(&self, domain_id: &str, name: &str) -> Option<&SnapshotProject> {
        self.projects
            .iter()
            .find(|project| project.domain_id == domain_id && project.name == name)
    }

    fn domain_by_id(&self, id: &str) -> Option<&SnapshotDomain> {
        self.domains.iter().find(|domain| domain.id == id)
    }

    fn domain_by_name(&self, name: &str) -> Option<&SnapshotDomain> {
        self.domains.iter().find(|domain| domain.name == name)
    }

    fn role_names_for(&self, user_id: &str, project_id: &str) -> Vec<(String, String)> {
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
    const ITERATIONS: u32 = 120_000;
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

    fn derive_with_iterations(password: &str, iterations: u32) -> Result<Self, AuthError> {
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
fn pbkdf2_sha256(
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

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
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

/// Seeds the durable identity universe required by the hosted-service profile.
/// Idempotent: existing records are updated from configuration, assignments and
/// roles are inserted when missing. Passwords and secrets are never logged.
pub async fn seed_identity_defaults(
    store: &dyn IdentityRepository,
    config: &BootstrapConfig,
) -> Result<(), StoreError> {
    let now = now_rfc3339();
    let default_domain = "default".to_owned();

    store
        .insert_keystone_domain(&o3k_store::KeystoneDomainRecord {
            id: default_domain.clone(),
            name: "Default".to_owned(),
            description: Some("Default domain".to_owned()),
            enabled: true,
            created_at: now.clone(),
        })
        .await?;

    store
        .insert_keystone_project(&o3k_store::KeystoneProjectRecord {
            id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
            domain_id: default_domain.clone(),
            name: "admin".to_owned(),
            description: Some("TestLab bootstrap project".to_owned()),
            enabled: true,
            created_at: now.clone(),
        })
        .await?;
    store
        .insert_keystone_project(&o3k_store::KeystoneProjectRecord {
            id: "service-project".to_owned(),
            domain_id: default_domain.clone(),
            name: "service".to_owned(),
            description: Some("Service project for hosted services".to_owned()),
            enabled: true,
            created_at: now.clone(),
        })
        .await?;

    let iterations = if config.pbkdf2_iterations == 0 {
        PasswordHash::ITERATIONS
    } else {
        config.pbkdf2_iterations
    };
    let admin_hash =
        PasswordHash::derive_with_iterations(config.bootstrap_password.expose(), iterations)
            .map_err(store_auth_error)?;
    store
        .insert_keystone_user(&o3k_store::KeystoneUserRecord {
            id: "bootstrap-user".to_owned(),
            domain_id: default_domain.clone(),
            name: "admin".to_owned(),
            password_hash: admin_hash.encoded().to_owned(),
            email: None,
            enabled: true,
            created_at: now.clone(),
        })
        .await?;

    if let Some(cinder_password) = &config.cinder_password {
        let cinder_hash =
            PasswordHash::derive_with_iterations(cinder_password.expose(), iterations)
                .map_err(store_auth_error)?;
        store
            .insert_keystone_user(&o3k_store::KeystoneUserRecord {
                id: "cinder".to_owned(),
                domain_id: default_domain.clone(),
                name: "cinder".to_owned(),
                password_hash: cinder_hash.encoded().to_owned(),
                email: None,
                enabled: true,
                created_at: now.clone(),
            })
            .await?;
    }

    for seed in &config.extra_projects {
        store
            .insert_keystone_project(&o3k_store::KeystoneProjectRecord {
                id: seed.project_id.clone(),
                domain_id: default_domain.clone(),
                name: seed.project_name.clone(),
                description: Some("Isolated hosted-service test project".to_owned()),
                enabled: true,
                created_at: now.clone(),
            })
            .await?;
        let user_hash = PasswordHash::derive_with_iterations(seed.password.expose(), iterations)
            .map_err(store_auth_error)?;
        store
            .insert_keystone_user(&o3k_store::KeystoneUserRecord {
                id: seed.user_id.clone(),
                domain_id: default_domain.clone(),
                name: seed.user_name.clone(),
                password_hash: user_hash.encoded().to_owned(),
                email: None,
                enabled: true,
                created_at: now.clone(),
            })
            .await?;
    }

    for (id, name) in [
        ("admin", "admin"),
        ("member", "member"),
        ("service", "service"),
    ] {
        store
            .insert_keystone_role(&o3k_store::KeystoneRoleRecord {
                id: id.to_owned(),
                name: name.to_owned(),
                description: Some(format!("{name} role")),
                created_at: now.clone(),
            })
            .await?;
    }

    let mut assignments = vec![
        (
            "bootstrap-user",
            "eba29e2d-53de-461d-ae91-ede7402713cb",
            "admin",
        ),
        (
            "bootstrap-user",
            "eba29e2d-53de-461d-ae91-ede7402713cb",
            "member",
        ),
    ];
    if config.cinder_password.is_some() {
        assignments.extend([
            ("cinder", "service-project", "service"),
            ("cinder", "service-project", "admin"),
            ("cinder", "eba29e2d-53de-461d-ae91-ede7402713cb", "admin"),
            ("cinder", "eba29e2d-53de-461d-ae91-ede7402713cb", "service"),
        ]);
    }
    for seed in &config.extra_projects {
        assignments.extend([
            (seed.user_id.as_str(), seed.project_id.as_str(), "admin"),
            (seed.user_id.as_str(), seed.project_id.as_str(), "member"),
        ]);
        // The hosted-service profile's Cinder client acts in the caller's
        // project with the service identity's token, so the service user
        // must be assigned there exactly as it is in the bootstrap project.
        // Without this, Cinder rejects the service-scoped call for the
        // isolated tenant (Malformed request url) and the tenant cannot use
        // the hosted profile at all.
        if config.cinder_password.is_some() {
            assignments.extend([
                ("cinder", seed.project_id.as_str(), "admin"),
                ("cinder", seed.project_id.as_str(), "service"),
            ]);
        }
    }
    for (index, (user_id, project_id, role_id)) in assignments.into_iter().enumerate() {
        store
            .insert_keystone_role_assignment(&o3k_store::KeystoneRoleAssignmentRecord {
                id: format!("assignment-{index}"),
                user_id: user_id.to_owned(),
                project_id: project_id.to_owned(),
                role_id: role_id.to_owned(),
                created_at: now.clone(),
            })
            .await?;
    }

    store
        .insert_keystone_region(&o3k_store::KeystoneRegionRecord {
            id: "RegionOne".to_owned(),
            description: Some("Default region".to_owned()),
            parent_region_id: None,
            enabled: true,
            created_at: now.clone(),
        })
        .await?;

    let base = config.catalog_endpoint.trim_end_matches('/').to_owned();
    let cinder_url = config
        .cinder_endpoint
        .as_deref()
        .map(|endpoint| endpoint.trim_end_matches('/').to_owned());

    let mut services: Vec<(&str, &str, &str, String)> = vec![
        ("identity", "identity", "identity", format!("{base}/v3")),
        ("image", "image", "image", format!("{base}/v2")),
        ("network", "network", "network", format!("{base}/v2.0")),
        (
            "compute",
            "compute",
            "compute",
            format!("{base}/v2.1/{{project_id}}"),
        ),
        (
            "placement",
            "placement",
            "placement",
            format!("{base}/placement"),
        ),
    ];
    if let Some(cinder_url) = &cinder_url {
        services.push((
            "cinder",
            "cinder",
            "volumev3",
            format!("{cinder_url}/v3/{{project_id}}"),
        ));
    }

    for (id, name, service_type, url) in services {
        store
            .insert_keystone_service(&o3k_store::KeystoneServiceRecord {
                id: id.to_owned(),
                name: name.to_owned(),
                r#type: service_type.to_owned(),
                description: Some(format!("{name} service")),
                enabled: true,
                created_at: now.clone(),
            })
            .await?;
        // Keystone advertises each endpoint under the public, internal, and
        // admin interfaces pointing at the same URL for a single-node control
        // plane. Cinder's keystonemiddleware (keystoneauth1) negotiates the
        // catalog by interface and raises EndpointNotFound when the requested
        // interface (default internal) is absent, so all three are required.
        for interface in ["public", "internal", "admin"] {
            store
                .insert_keystone_endpoint(&o3k_store::KeystoneEndpointRecord {
                    id: format!("endpoint-{id}-{interface}"),
                    service_id: id.to_owned(),
                    interface: interface.to_owned(),
                    url: url.clone(),
                    region: "RegionOne".to_owned(),
                    enabled: true,
                    created_at: now.clone(),
                })
                .await?;
        }
    }

    Ok(())
}

fn store_auth_error(_: AuthError) -> StoreError {
    StoreError::Corrupt("password hashing failed".to_owned())
}

fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_time(seconds).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[derive(Clone)]
pub struct TokenService {
    snapshot: IdentitySnapshot,
    signing_key: Secret,
    token_ttl: Duration,
    catalog_endpoint: String,
    registry: Option<o3k_kernel::KernelRegistry>,
}

impl TokenService {
    /// Loads the durable identity snapshot from the store. The snapshot is
    /// authoritative for authentication, roles, and the catalog until the
    /// control plane restarts.
    pub async fn load(
        store: Arc<dyn IdentityRepository>,
        signing_key: Secret,
        token_ttl: Duration,
    ) -> Result<Self, AuthError> {
        let snapshot = load_snapshot(store.as_ref()).await?;
        Self::from_snapshot(snapshot, signing_key, token_ttl)
    }

    /// Builds a token service from an explicit identity snapshot. Used by
    /// tests and by the store-backed constructor.
    pub fn from_snapshot(
        snapshot: IdentitySnapshot,
        signing_key: Secret,
        token_ttl: Duration,
    ) -> Result<Self, AuthError> {
        if signing_key.expose().len() < 32 {
            return Err(AuthError::WeakSigningKey);
        }
        if token_ttl.is_zero() {
            return Err(AuthError::InvalidRequest);
        }
        if snapshot.domains.is_empty() {
            return Err(AuthError::IdentityUnavailable);
        }
        let catalog_endpoint = snapshot
            .endpoints
            .iter()
            .find_map(|ep| {
                let is_identity = snapshot
                    .services
                    .iter()
                    .any(|s| s.id == ep.service_id && s.service_type == "identity");
                if is_identity || ep.service_id == "identity" {
                    if let Some(pos) = ep.url.find("/v3") {
                        Some(ep.url[..pos].to_owned())
                    } else if let Some(pos) = ep.url.find("/v2") {
                        Some(ep.url[..pos].to_owned())
                    } else {
                        Some(ep.url.trim_end_matches('/').to_owned())
                    }
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "http://127.0.0.1:8080".to_owned());
        Ok(Self {
            snapshot,
            signing_key,
            token_ttl,
            catalog_endpoint,
            registry: None,
        })
    }

    /// Set the public base URL advertised in discovery responses.
    #[must_use]
    pub fn with_catalog_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.catalog_endpoint = endpoint.into().trim_end_matches('/').to_owned();
        self
    }

    /// Set the canonical Cloud Kernel registry used to project the service catalog.
    #[must_use]
    pub fn with_registry(mut self, registry: o3k_kernel::KernelRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    #[must_use]
    pub fn catalog_endpoint(&self) -> &str {
        &self.catalog_endpoint
    }

    #[must_use]
    pub fn snapshot(&self) -> &IdentitySnapshot {
        &self.snapshot
    }

    pub fn issue(
        &self,
        request: &TokenRequest,
        now: SystemTime,
    ) -> Result<(String, TokenResponse), AuthError> {
        let user_id = match request
            .auth
            .identity
            .methods
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["password"] => {
                let user_ref = &request
                    .auth
                    .identity
                    .password
                    .as_ref()
                    .ok_or(AuthError::InvalidRequest)?
                    .user;
                let user = self.resolve_user(user_ref)?;
                if !user.enabled {
                    return Err(AuthError::Unauthorized);
                }
                if !user.password_hash.verify(&user_ref.password) {
                    return Err(AuthError::Unauthorized);
                }
                user.id.clone()
            }
            // Keystone token re-authentication: a valid presented token is
            // exchanged for a freshly issued token (used by Cinder's Nova
            // client and service_auth, which re-authenticate with the
            // caller's token).
            ["token"] => {
                let token_id = &request
                    .auth
                    .identity
                    .token
                    .as_ref()
                    .ok_or(AuthError::InvalidRequest)?
                    .id;
                let verified = self.verify(token_id, now)?;
                let user = self
                    .snapshot
                    .user_by_id(&verified.user_id)
                    .ok_or(AuthError::InvalidToken)?;
                if !user.enabled {
                    return Err(AuthError::Unauthorized);
                }
                user.id.clone()
            }
            _ => return Err(AuthError::InvalidRequest),
        };

        let project_ref = request
            .auth
            .scope
            .as_ref()
            .and_then(|scope| scope.project.as_ref())
            .ok_or(AuthError::InvalidRequest)?;
        let project = self.resolve_project(project_ref)?;
        if !project.enabled {
            return Err(AuthError::Unauthorized);
        }

        let roles = self.snapshot.role_names_for(&user_id, &project.id);
        if roles.is_empty() {
            // Cross-project scoping fails closed before any token is issued.
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
                sub: user_id.clone(),
                project: project.id.clone(),
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
                token: self.details(&user_id, &project.id, &roles, issued_at, expires_at)?,
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

        let now = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::InvalidToken)?
            .as_secs();
        if now >= claims.expires {
            return Err(AuthError::ExpiredToken);
        }

        // Validation is fail-closed against the durable identity universe:
        // the subject user and scoped project must still exist and be enabled.
        let user = self.snapshot.user_by_id(&claims.sub);
        let project = self.snapshot.project_by_id(&claims.project);
        match (user, project) {
            (Some(user), Some(project)) if user.enabled && project.enabled => {}
            _ => return Err(AuthError::InvalidToken),
        }

        Ok(VerifiedToken {
            token_id: claims.token_id,
            user_id: claims.sub,
            project_id: claims.project,
            issued: claims.issued,
            expires: claims.expires,
        })
    }

    pub fn verify_details(&self, token: &str, now: SystemTime) -> Result<TokenResponse, AuthError> {
        let verified = self.verify(token, now)?;
        let roles = self
            .snapshot
            .role_names_for(&verified.user_id, &verified.project_id);
        let issued_at = format_time(verified.issued)?;
        let expires_at = format_time(verified.expires)?;
        Ok(TokenResponse {
            token: self.details(
                &verified.user_id,
                &verified.project_id,
                &roles,
                issued_at,
                expires_at,
            )?,
        })
    }

    pub fn auth_context(&self, token: &str, now: SystemTime) -> Result<AuthContext, AuthError> {
        let verified = self.verify(token, now)?;
        let user = self
            .snapshot
            .user_by_id(&verified.user_id)
            .ok_or(AuthError::InvalidToken)?;
        let project = self
            .snapshot
            .project_by_id(&verified.project_id)
            .ok_or(AuthError::InvalidToken)?;
        let domain = self
            .snapshot
            .domain_by_id(&project.domain_id)
            .ok_or(AuthError::InvalidToken)?;
        let role_names: Vec<String> = self
            .snapshot
            .role_names_for(&user.id, &project.id)
            .into_iter()
            .map(|(_id, name)| name)
            .collect();
        let principal_id = PrincipalId::new(&user.id).map_err(|_| AuthError::InvalidToken)?;
        let is_service = role_names.iter().any(|role| role == "service");
        let principal = if is_service {
            Principal::Service(ServicePrincipal::new(principal_id, &user.name, "service"))
        } else {
            Principal::User(UserPrincipal::new(
                principal_id,
                &user.name,
                Some(domain.id.clone()),
            ))
        };
        let scope_id = ScopeId::new(&project.id).map_err(|_| AuthError::InvalidToken)?;
        let scope = OwnershipScope::project(
            scope_id,
            Some(project.name.clone()),
            Some(domain.id.clone()),
        );
        let request_id = Uuid::now_v7().to_string();
        let audit_id = Uuid::now_v7().to_string();
        let service_principal = if is_service {
            Some(ServicePrincipal::new(
                PrincipalId::new(&user.id).map_err(|_| AuthError::InvalidToken)?,
                &user.name,
                "service",
            ))
        } else {
            None
        };
        Ok(AuthContext::new(
            principal,
            scope,
            role_names,
            verified.issued,
            verified.expires,
            audit_id,
            request_id,
            service_principal,
        ))
    }

    fn resolve_domain(
        &self,
        reference: Option<&DomainReference>,
    ) -> Result<SnapshotDomain, AuthError> {
        let Some(reference) = reference else {
            return self
                .snapshot
                .domain_by_name("Default")
                .or_else(|| self.snapshot.domain_by_id("default"))
                .cloned()
                .ok_or(AuthError::Unauthorized);
        };
        match (&reference.id, &reference.name) {
            (Some(id), None) => self
                .snapshot
                .domain_by_id(id)
                .cloned()
                .ok_or(AuthError::Unauthorized),
            (None, Some(name)) => self
                .snapshot
                .domain_by_name(name)
                .cloned()
                .ok_or(AuthError::Unauthorized),
            (Some(id), Some(name)) => self
                .snapshot
                .domain_by_id(id)
                .filter(|domain| domain.name == *name)
                .cloned()
                .ok_or(AuthError::Unauthorized),
            (None, None) => Err(AuthError::Unauthorized),
        }
    }

    fn resolve_user(&self, reference: &UserReference) -> Result<SnapshotUser, AuthError> {
        let domain = self.resolve_domain(reference.domain.as_ref())?;
        let user = match (&reference.id, &reference.name) {
            (Some(id), None) => self.snapshot.user_by_id(id),
            (None, Some(name)) => self.snapshot.user_by_name(domain.id.as_str(), name),
            (Some(id), Some(name)) => self
                .snapshot
                .user_by_id(id)
                .filter(|user| user.name == *name && user.domain_id == domain.id),
            (None, None) => None,
        };
        user.cloned()
            .ok_or(AuthError::Unauthorized)
            .and_then(|user| {
                if user.domain_id == domain.id {
                    Ok(user)
                } else {
                    Err(AuthError::Unauthorized)
                }
            })
    }

    fn resolve_project(&self, reference: &ProjectReference) -> Result<SnapshotProject, AuthError> {
        let domain = self.resolve_domain(reference.domain.as_ref())?;
        let project = match (&reference.id, &reference.name) {
            (Some(id), None) => self.snapshot.project_by_id(id),
            (None, Some(name)) => self.snapshot.project_by_name(domain.id.as_str(), name),
            (Some(id), Some(name)) => self
                .snapshot
                .project_by_id(id)
                .filter(|project| project.name == *name && project.domain_id == domain.id),
            (None, None) => None,
        };
        project
            .cloned()
            .ok_or(AuthError::Unauthorized)
            .and_then(|project| {
                if project.domain_id == domain.id {
                    Ok(project)
                } else {
                    Err(AuthError::Unauthorized)
                }
            })
    }

    fn details(
        &self,
        user_id: &str,
        project_id: &str,
        roles: &[(String, String)],
        issued_at: String,
        expires_at: String,
    ) -> Result<TokenDetails, AuthError> {
        let user = self
            .snapshot
            .user_by_id(user_id)
            .ok_or(AuthError::InvalidToken)?;
        let project = self
            .snapshot
            .project_by_id(project_id)
            .ok_or(AuthError::InvalidToken)?;
        let user_domain = self
            .snapshot
            .domain_by_id(&user.domain_id)
            .ok_or(AuthError::InvalidToken)?;
        let project_domain = self
            .snapshot
            .domain_by_id(&project.domain_id)
            .ok_or(AuthError::InvalidToken)?;

        let mut role_details: Vec<RoleDetails> = roles
            .iter()
            .map(|(id, name)| RoleDetails {
                id: id.clone(),
                name: name.clone(),
            })
            .collect();
        role_details.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(TokenDetails {
            expires_at,
            issued_at,
            methods: vec!["password".to_owned()],
            project: ProjectDetails {
                id: project.id.clone(),
                name: project.name.clone(),
                domain: DomainDetails {
                    id: project_domain.id.clone(),
                    name: project_domain.name.clone(),
                },
            },
            user: UserDetails {
                id: user.id.clone(),
                name: user.name.clone(),
                domain: DomainDetails {
                    id: user_domain.id.clone(),
                    name: user_domain.name.clone(),
                },
                password_expires_at: None,
            },
            roles: role_details,
            catalog: self.catalog(project_id),
        })
    }

    /// Builds the service catalog projected from the canonical Cloud Kernel registry.
    /// URLs are validated configuration and never derived from request
    /// headers. The `{project_id}` placeholder is substituted per token scope.
    fn catalog(&self, project_id: &str) -> Vec<ServiceDetails> {
        let cinder_url = self
            .snapshot
            .endpoints
            .iter()
            .find(|ep| ep.service_id == "cinder")
            .map(|ep| ep.url.replace("/{project_id}", "").replace("/v3", ""));

        let registry = self.registry.clone().unwrap_or_else(|| {
            o3k_kernel::KernelRegistry::standard(&self.catalog_endpoint, cinder_url.as_deref())
        });

        let enabled_services: std::collections::HashSet<&str> = self
            .snapshot
            .services
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.as_str())
            .collect();

        let projected = registry.project_keystone_catalog(project_id);
        projected
            .into_iter()
            .filter(|svc| enabled_services.is_empty() || enabled_services.contains(svc.id.as_str()))
            .map(|svc| ServiceDetails {
                name: svc.name,
                service_type: svc.service_type,
                id: svc.id,
                endpoints: svc
                    .endpoints
                    .into_iter()
                    .map(|ep| EndpointDetails {
                        url: ep.url,
                        interface: ep.interface,
                        region: ep.region,
                        region_id: ep.region_id,
                    })
                    .collect(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    pub token_id: String,
    pub user_id: String,
    pub project_id: String,
    pub issued: u64,
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

fn sign(key: &Secret, input: &[u8]) -> Result<String, AuthError> {
    let mut mac =
        HmacSha256::new_from_slice(key.expose().as_bytes()).map_err(|_| AuthError::InvalidToken)?;
    mac.update(input);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn format_time(seconds: u64) -> Result<String, AuthError> {
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_date(days)?;
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

// Howard Hinnant's proleptic Gregorian calendar conversion, adapted for
// non-negative Unix timestamps. This keeps token formatting on the standard
// library and avoids pulling a date parser into the authentication boundary.
fn civil_date(days_since_epoch: u64) -> Result<(i64, u64, u64), AuthError> {
    let days = i64::try_from(days_since_epoch).map_err(|_| AuthError::InvalidRequest)?;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    Ok((
        year,
        u64::try_from(month).map_err(|_| AuthError::InvalidRequest)?,
        u64::try_from(day).map_err(|_| AuthError::InvalidRequest)?,
    ))
}

async fn load_snapshot(store: &dyn IdentityRepository) -> Result<IdentitySnapshot, AuthError> {
    let map_error = |_: StoreError| AuthError::IdentityUnavailable;
    let domains = store
        .list_keystone_domains()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotDomain {
            id: record.id,
            name: record.name,
            enabled: record.enabled,
        })
        .collect();
    let projects = store
        .list_keystone_projects()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotProject {
            id: record.id,
            domain_id: record.domain_id,
            name: record.name,
            enabled: record.enabled,
        })
        .collect();
    let users = store
        .list_keystone_users()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotUser {
            id: record.id,
            domain_id: record.domain_id,
            name: record.name,
            password_hash: PasswordHash::from_encoded(record.password_hash),
            enabled: record.enabled,
        })
        .collect();
    let roles = store
        .list_keystone_roles()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotRole {
            id: record.id,
            name: record.name,
        })
        .collect();
    let assignments = store
        .list_keystone_role_assignments()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotAssignment {
            user_id: record.user_id,
            project_id: record.project_id,
            role_id: record.role_id,
        })
        .collect();
    let services = store
        .list_keystone_services()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotService {
            id: record.id,
            name: record.name,
            service_type: record.r#type,
            enabled: record.enabled,
        })
        .collect();
    let endpoints = store
        .list_keystone_endpoints()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotEndpoint {
            id: record.id,
            service_id: record.service_id,
            url: record.url,
            interface: record.interface,
            region: record.region,
            enabled: record.enabled,
        })
        .collect();
    let regions = store
        .list_keystone_regions()
        .await
        .map_err(map_error)?
        .into_iter()
        .map(|record| SnapshotRegion {
            id: record.id,
            enabled: record.enabled,
        })
        .collect();
    Ok(IdentitySnapshot {
        domains,
        projects,
        users,
        roles,
        assignments,
        services,
        endpoints,
        regions,
    })
}

impl PasswordHash {
    /// Parses a previously encoded hash without re-deriving it.
    fn from_encoded(encoded: String) -> Self {
        Self {
            encoded: Secret::new(encoded),
        }
    }
}

/// Shared test construction helpers for identity-backed integration tests.
pub mod testkit {
    use super::*;

    /// Builds a token service against a fresh in-memory durable store seeded
    /// with the deterministic bootstrap universe. Bootstrap and Cinder
    /// passwords are both `password`; the Cinder endpoint is the default
    /// 8776 port.
    pub async fn test_service(catalog_endpoint: &str) -> Result<TokenService, AuthError> {
        test_service_with_projects(catalog_endpoint, Vec::new()).await
    }

    /// Test-only identity service with explicitly seeded tenant users.
    pub async fn test_service_with_projects(
        catalog_endpoint: &str,
        extra_projects: Vec<ExtraProjectSeed>,
    ) -> Result<TokenService, AuthError> {
        let store = Arc::new(
            o3k_store::testkit::open_memory()
                .await
                .map_err(|_| AuthError::IdentityUnavailable)?,
        );
        seed_identity_defaults(
            store.as_ref(),
            &BootstrapConfig {
                catalog_endpoint: catalog_endpoint.to_owned(),
                bootstrap_password: Secret::new("password".to_owned()),
                cinder_password: Some(Secret::new("password".to_owned())),
                cinder_endpoint: Some("http://127.0.0.1:8776".to_owned()),
                pbkdf2_iterations: 1_000,
                extra_projects,
            },
        )
        .await
        .map_err(|_| AuthError::IdentityUnavailable)?;
        TokenService::load(
            store,
            Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
            Duration::from_secs(3600),
        )
        .await
        .map(|svc| svc.with_catalog_endpoint(catalog_endpoint))
    }

    /// A password authentication request for the bootstrap administrator in
    /// the bootstrap project.
    pub fn admin_request(password: &str) -> TokenRequest {
        TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["password".to_owned()],
                    token: None,
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

    /// A token re-authentication request exchanging an existing token for a
    /// freshly issued one (Keystone `methods: ["token"]`; used by Cinder's
    /// Nova client and service_auth).
    pub fn token_request(token: &str) -> TokenRequest {
        TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["token".to_owned()],
                    token: Some(TokenIdentity {
                        id: token.to_owned(),
                    }),
                    password: None,
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

    /// A password authentication request for the Cinder service user scoped
    /// to the bootstrap project.
    pub fn cinder_service_request(password: &str) -> TokenRequest {
        TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["password".to_owned()],
                    token: None,
                    password: Some(PasswordIdentity {
                        user: UserReference {
                            id: None,
                            name: Some("cinder".to_owned()),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_kernel::{PrincipalKind, ScopeKind};

    fn service_with_snapshot() -> Result<TokenService, AuthError> {
        let mut snapshot = IdentitySnapshot {
            domains: vec![SnapshotDomain {
                id: "default".to_owned(),
                name: "Default".to_owned(),
                enabled: true,
            }],
            projects: vec![
                SnapshotProject {
                    id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "admin".to_owned(),
                    enabled: true,
                },
                SnapshotProject {
                    id: "service-project".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "service".to_owned(),
                    enabled: true,
                },
                SnapshotProject {
                    id: "other-project".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "other".to_owned(),
                    enabled: true,
                },
            ],
            users: vec![
                SnapshotUser {
                    id: "bootstrap-user".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "admin".to_owned(),
                    password_hash: PasswordHash::derive_with_iterations_for_testing(
                        "password", 1_000,
                    )?,
                    enabled: true,
                },
                SnapshotUser {
                    id: "cinder".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "cinder".to_owned(),
                    password_hash: PasswordHash::derive_with_iterations_for_testing(
                        "password", 1_000,
                    )?,
                    enabled: true,
                },
                SnapshotUser {
                    id: "disabled-user".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "disabled".to_owned(),
                    password_hash: PasswordHash::derive_with_iterations_for_testing(
                        "password", 1_000,
                    )?,
                    enabled: false,
                },
            ],
            roles: vec![
                SnapshotRole {
                    id: "admin".to_owned(),
                    name: "admin".to_owned(),
                },
                SnapshotRole {
                    id: "member".to_owned(),
                    name: "member".to_owned(),
                },
                SnapshotRole {
                    id: "service".to_owned(),
                    name: "service".to_owned(),
                },
            ],
            assignments: vec![
                SnapshotAssignment {
                    user_id: "bootstrap-user".to_owned(),
                    project_id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                    role_id: "admin".to_owned(),
                },
                SnapshotAssignment {
                    user_id: "bootstrap-user".to_owned(),
                    project_id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                    role_id: "member".to_owned(),
                },
                SnapshotAssignment {
                    user_id: "cinder".to_owned(),
                    project_id: "service-project".to_owned(),
                    role_id: "service".to_owned(),
                },
                SnapshotAssignment {
                    user_id: "cinder".to_owned(),
                    project_id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                    role_id: "service".to_owned(),
                },
            ],
            services: vec![
                SnapshotService {
                    id: "identity".to_owned(),
                    name: "identity".to_owned(),
                    service_type: "identity".to_owned(),
                    enabled: true,
                },
                SnapshotService {
                    id: "cinder".to_owned(),
                    name: "cinder".to_owned(),
                    service_type: "volumev3".to_owned(),
                    enabled: true,
                },
            ],
            endpoints: vec![
                SnapshotEndpoint {
                    id: "endpoint-identity".to_owned(),
                    service_id: "identity".to_owned(),
                    url: "http://127.0.0.1:8080/v3".to_owned(),
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    enabled: true,
                },
                SnapshotEndpoint {
                    id: "endpoint-cinder".to_owned(),
                    service_id: "cinder".to_owned(),
                    url: "http://127.0.0.1:8776/v3/{project_id}".to_owned(),
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    enabled: true,
                },
            ],
            regions: vec![SnapshotRegion {
                id: "RegionOne".to_owned(),
                enabled: true,
            }],
        };
        // Deterministic ordering is required for response comparisons.
        snapshot.services.sort_by(|a, b| a.id.cmp(&b.id));
        snapshot.endpoints.sort_by(|a, b| a.id.cmp(&b.id));
        TokenService::from_snapshot(
            snapshot,
            Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
            Duration::from_secs(3600),
        )
    }

    fn admin_request() -> TokenRequest {
        testkit::admin_request("password")
    }

    fn admin_scoped_request(project_name: &str) -> TokenRequest {
        TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["password".to_owned()],
                    token: None,
                    password: Some(PasswordIdentity {
                        user: UserReference {
                            id: None,
                            name: Some("admin".to_owned()),
                            domain: None,
                            password: "password".to_owned(),
                        },
                    }),
                },
                scope: Some(Scope {
                    project: Some(ProjectReference {
                        id: None,
                        name: Some(project_name.to_owned()),
                        domain: None,
                    }),
                }),
            },
        }
    }

    #[test]
    fn password_scope_issues_and_verifies_token() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, response) = service.issue(&admin_request(), now)?;
        assert!(token.split('.').count() == 3);
        assert_eq!(
            response.token.project.id,
            "eba29e2d-53de-461d-ae91-ede7402713cb"
        );
        assert_eq!(response.token.user.id, "bootstrap-user");
        assert_eq!(service.verify(&token, now)?.user_id, "bootstrap-user");
        Ok(())
    }

    #[test]
    fn token_reauthentication_issues_a_fresh_token() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (presented, _) = service.issue(&admin_request(), now)?;
        let (reissued, response) = service.issue(&testkit::token_request(&presented), now)?;
        assert_ne!(reissued, presented);
        assert_eq!(response.token.user.id, "bootstrap-user");
        assert_eq!(
            response.token.project.id,
            "eba29e2d-53de-461d-ae91-ede7402713cb"
        );
        // The freshly issued token is valid and carries the same identity.
        assert_eq!(service.verify(&reissued, now)?.user_id, "bootstrap-user");
        Ok(())
    }

    #[test]
    fn token_reauthentication_rejects_invalid_tokens() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(matches!(
            service.issue(&testkit::token_request("not-a-real-token"), now),
            Err(AuthError::InvalidToken)
        ));
        // A missing token identity is an invalid request.
        let malformed = TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["token".to_owned()],
                    token: None,
                    password: None,
                },
                scope: Some(Scope {
                    project: Some(ProjectReference {
                        id: None,
                        name: Some("admin".to_owned()),
                        domain: None,
                    }),
                }),
            },
        };
        assert!(matches!(
            service.issue(&malformed, now),
            Err(AuthError::InvalidRequest)
        ));
        Ok(())
    }

    #[test]
    fn catalog_is_generated_from_durable_endpoint_records() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (_, response) = service.issue(&admin_request(), now)?;
        let urls: Vec<(String, String)> = response
            .token
            .catalog
            .iter()
            .map(|item| (item.service_type.clone(), item.endpoints[0].url.clone()))
            .collect();
        assert_eq!(
            urls,
            vec![
                ("identity".to_owned(), "http://127.0.0.1:8080/v3".to_owned()),
                (
                    "volumev3".to_owned(),
                    "http://127.0.0.1:8776/v3/eba29e2d-53de-461d-ae91-ede7402713cb".to_owned()
                ),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn catalog_advertises_identity_under_all_interfaces() -> Result<(), AuthError> {
        // Cinder's keystonemiddleware (keystoneauth1) negotiates the catalog by
        // interface and defaults to internal; if it is absent it raises
        // EndpointNotFound and every Cinder API request fails with HTTP 500.
        // This validates the production seed path (seed_identity_defaults),
        // which must advertise public, internal, and admin interfaces.
        let service = testkit::test_service("http://127.0.0.1:8080").await?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (_, response) = service.issue(&admin_request(), now)?;
        let identity = response
            .token
            .catalog
            .iter()
            .find(|item| item.service_type == "identity")
            .ok_or(AuthError::InvalidToken)?;
        let interfaces: Vec<&str> = identity
            .endpoints
            .iter()
            .map(|endpoint| endpoint.interface.as_str())
            .collect();
        for required in ["public", "internal", "admin"] {
            assert!(
                interfaces.contains(&required),
                "identity catalog must advertise the {required} interface; got {interfaces:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn disabled_user_cannot_authenticate() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let request = TokenRequest {
            auth: Auth {
                identity: Identity {
                    methods: vec!["password".to_owned()],
                    token: None,
                    password: Some(PasswordIdentity {
                        user: UserReference {
                            id: None,
                            name: Some("disabled".to_owned()),
                            domain: None,
                            password: "password".to_owned(),
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
        };
        assert!(matches!(
            service.issue(&request, UNIX_EPOCH),
            Err(AuthError::Unauthorized)
        ));
        Ok(())
    }

    #[test]
    fn invalid_password_is_not_accepted() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        assert!(matches!(
            service.issue(
                &TokenRequest {
                    auth: Auth {
                        identity: Identity {
                            methods: vec!["password".to_owned()],
                            token: None,
                            password: Some(PasswordIdentity {
                                user: UserReference {
                                    id: None,
                                    name: Some("admin".to_owned()),
                                    domain: None,
                                    password: "wrong".to_owned(),
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
                },
                UNIX_EPOCH,
            ),
            Err(AuthError::Unauthorized)
        ));
        Ok(())
    }

    #[test]
    fn cross_project_scoping_fails_closed() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        // The bootstrap user has no role in the "other" project.
        assert!(matches!(
            service.issue(&admin_scoped_request("other"), UNIX_EPOCH),
            Err(AuthError::Unauthorized)
        ));
        Ok(())
    }

    #[test]
    fn expired_token_is_rejected() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, _) = service.issue(&admin_request(), now)?;
        assert_eq!(
            service.verify(&token, now + Duration::from_secs(3600)),
            Err(AuthError::ExpiredToken)
        );
        Ok(())
    }

    #[test]
    fn malformed_and_tampered_tokens_are_rejected() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, _) = service.issue(&admin_request(), now)?;
        assert_eq!(
            service.verify("not-a-token", now),
            Err(AuthError::InvalidToken)
        );
        let mut parts: Vec<&str> = token.split('.').collect();
        parts[1] = "tampered";
        assert_eq!(
            service.verify(&parts.join("."), now),
            Err(AuthError::InvalidToken)
        );
        Ok(())
    }

    #[test]
    fn service_user_authentication_and_separation() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, response) = service.issue(&testkit::cinder_service_request("password"), now)?;
        assert_eq!(response.token.user.id, "cinder");
        assert_eq!(
            response.token.project.id,
            "eba29e2d-53de-461d-ae91-ede7402713cb"
        );
        let context = service.auth_context(&token, now)?;
        assert_eq!(context.principal().kind(), PrincipalKind::Service);
        assert_eq!(context.principal().id().as_str(), "cinder");
        assert_eq!(
            context.effective_scope().id().as_str(),
            "eba29e2d-53de-461d-ae91-ede7402713cb"
        );
        assert!(context.has_role("service"));
        assert!(!context.has_role("admin"));
        assert_eq!(context.issued_at(), 1000);
        assert!(!context.audit_id().is_empty());
        assert!(!context.request_id().is_empty());
        Ok(())
    }

    #[test]
    fn user_token_is_not_a_service_token() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, _) = service.issue(&admin_request(), now)?;
        let context = service.auth_context(&token, now)?;
        assert_eq!(context.principal().kind(), PrincipalKind::User);
        assert_eq!(context.principal().id().as_str(), "bootstrap-user");
        assert_eq!(
            context.effective_scope().id().as_str(),
            "eba29e2d-53de-461d-ae91-ede7402713cb"
        );
        assert!(context.has_role("admin"));
        assert_eq!(context.issued_at(), 1000);
        assert!(!context.audit_id().is_empty());
        assert!(!context.request_id().is_empty());
        Ok(())
    }

    #[test]
    fn keystone_token_mapping_to_kernel_auth_context() -> Result<(), AuthError> {
        let service = service_with_snapshot()?;
        let now = UNIX_EPOCH + Duration::from_secs(2_000);
        let (token_str, response) = service.issue(&admin_request(), now)?;

        let auth_ctx = service.auth_context(&token_str, now)?;

        // 1. User Principal
        assert_eq!(auth_ctx.principal().kind(), PrincipalKind::User);
        assert_eq!(auth_ctx.principal().id().as_str(), &response.token.user.id);
        assert_eq!(auth_ctx.principal().name(), &response.token.user.name);

        // 2. Ownership Scope
        assert_eq!(
            auth_ctx.effective_scope().id().as_str(),
            &response.token.project.id
        );
        assert_eq!(auth_ctx.effective_scope().kind(), ScopeKind::Project);

        // 3. Roles
        assert!(auth_ctx.has_role("admin"));

        // 4. Timestamps & Audit
        assert_eq!(auth_ctx.issued_at(), 2000);
        assert_eq!(auth_ctx.expires_at(), 2000 + 3600);
        assert!(!auth_ctx.audit_id().is_empty());
        assert!(!auth_ctx.request_id().is_empty());

        // 5. No raw token stored
        let serialized = serde_json::to_string(&auth_ctx).map_err(|_| AuthError::InvalidRequest)?;
        assert!(!serialized.contains(&token_str));

        Ok(())
    }

    #[test]
    fn secret_and_password_hash_display_are_redacted() -> Result<(), AuthError> {
        let hash = PasswordHash::derive("hunter2-password")?;
        assert!(!format!("{hash:?}").contains("hunter2"));
        assert!(!format!("{hash}").contains("hunter2"));
        assert!(format!("{hash:?}").contains("redacted"));
        let secret = Secret::new("super-secret".to_owned());
        assert!(!format!("{secret:?}").contains("super-secret"));
        assert!(!format!("{secret}").contains("super-secret"));
        Ok(())
    }

    #[test]
    fn password_hash_round_trip_and_wrong_password() -> Result<(), AuthError> {
        let hash = PasswordHash::derive("correct-password")?;
        assert!(hash.verify("correct-password"));
        assert!(!hash.verify("wrong-password"));
        Ok(())
    }

    #[test]
    fn pbkdf2_sha256_matches_rfc6070_vector() -> Result<(), AuthError> {
        // PBKDF2-HMAC-SHA256 vector: password "password", salt "salt",
        // 1 iteration, dkLen 32 (cross-checked with hashlib.pbkdf2_hmac).
        let derived = pbkdf2_sha256(b"password", b"salt", 1, 32)?;
        let expected: Vec<u8> = vec![
            0x12, 0x0f, 0xb6, 0xcf, 0xfc, 0xf8, 0xb3, 0x2c, 0x43, 0xe7, 0x22, 0x52, 0x56, 0xc4,
            0xf8, 0x37, 0xa8, 0x65, 0x48, 0xc9, 0x2c, 0xcc, 0x35, 0x48, 0x08, 0x05, 0x98, 0x7c,
            0xb7, 0x0b, 0xe1, 0x7b,
        ];
        assert_eq!(derived, expected);
        Ok(())
    }

    #[tokio::test]
    async fn endpoint_records_survive_reload_from_store() -> Result<(), AuthError> {
        let store = Arc::new(
            o3k_store::testkit::open_memory()
                .await
                .map_err(|_| AuthError::IdentityUnavailable)?,
        );
        seed_identity_defaults(
            store.as_ref(),
            &BootstrapConfig {
                catalog_endpoint: "http://127.0.0.1:18080".to_owned(),
                bootstrap_password: Secret::new("password".to_owned()),
                cinder_password: Some(Secret::new("password".to_owned())),
                cinder_endpoint: Some("http://127.0.0.1:8776".to_owned()),
                pbkdf2_iterations: 1_000,
                extra_projects: Vec::new(),
            },
        )
        .await
        .map_err(|_| AuthError::IdentityUnavailable)?;

        let first = TokenService::load(
            store.clone(),
            Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
            Duration::from_secs(3600),
        )
        .await?;
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let (token, _) = first.issue(&testkit::admin_request("password"), now)?;

        // Simulate restart: a fresh service loads the durable records again.
        let reloaded = TokenService::load(
            store,
            Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
            Duration::from_secs(3600),
        )
        .await?;
        assert_eq!(reloaded.snapshot().services.len(), 6);
        assert_eq!(reloaded.snapshot().users.len(), 2);
        assert!(reloaded.snapshot().endpoints.iter().any(|endpoint| {
            endpoint.service_id == "cinder"
                && endpoint.url == "http://127.0.0.1:8776/v3/{project_id}"
        }));
        // A token issued before restart validates after reload.
        let verified = reloaded.verify(&token, now)?;
        assert_eq!(verified.user_id, "bootstrap-user");
        Ok(())
    }

    #[test]
    fn weak_signing_key_is_rejected() -> Result<(), AuthError> {
        let snapshot = IdentitySnapshot {
            domains: vec![SnapshotDomain {
                id: "default".to_owned(),
                name: "Default".to_owned(),
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(matches!(
            TokenService::from_snapshot(
                snapshot,
                Secret::new("short".to_owned()),
                Duration::from_secs(3600),
            ),
            Err(AuthError::WeakSigningKey)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn extra_project_seed_assigns_the_cinder_service_identity() -> Result<(), AuthError> {
        let store = Arc::new(
            o3k_store::testkit::open_memory()
                .await
                .map_err(|_| AuthError::IdentityUnavailable)?,
        );
        let project_b = "8d1f3c4a-5b6e-4f2a-9c3d-1e2f3a4b5c6d";
        seed_identity_defaults(
            store.as_ref(),
            &BootstrapConfig {
                catalog_endpoint: "http://127.0.0.1:18080".to_owned(),
                bootstrap_password: Secret::new("password".to_owned()),
                cinder_password: Some(Secret::new("password".to_owned())),
                cinder_endpoint: Some("http://127.0.0.1:8776".to_owned()),
                pbkdf2_iterations: 1_000,
                extra_projects: vec![ExtraProjectSeed {
                    project_id: project_b.to_owned(),
                    project_name: "tenant-b".to_owned(),
                    user_id: "a7c2e9d1-4f3b-4c8e-9d2a-3b4c5d6e7f8a".to_owned(),
                    user_name: "tenant-b-user".to_owned(),
                    password: Secret::new("tenant-b-password".to_owned()),
                }],
            },
        )
        .await
        .map_err(|_| AuthError::IdentityUnavailable)?;

        // The isolated tenant's own user holds admin+member there.
        let tenant_roles = store
            .list_user_role_names("a7c2e9d1-4f3b-4c8e-9d2a-3b4c5d6e7f8a", project_b)
            .await
            .map_err(|_| AuthError::IdentityUnavailable)?;
        assert!(tenant_roles.contains(&"admin".to_owned()));
        assert!(tenant_roles.contains(&"member".to_owned()));
        assert!(!tenant_roles.contains(&"service".to_owned()));

        // The hosted-profile Cinder service identity must be able to act in
        // the isolated project (the client scopes its service token to the
        // caller's project); without these assignments real Cinder rejects
        // the service call with Malformed request url.
        let service_roles = store
            .list_user_role_names("cinder", project_b)
            .await
            .map_err(|_| AuthError::IdentityUnavailable)?;
        assert!(service_roles.contains(&"admin".to_owned()));
        assert!(service_roles.contains(&"service".to_owned()));
        Ok(())
    }
}
