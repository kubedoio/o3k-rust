//! Token service: issue, verify, snapshot, bootstrap, signing.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use uuid::Uuid;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use o3k_kernel::{
    AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, ServicePrincipal, UserPrincipal,
};
use o3k_store::{IdentityRepository, StoreError};

use crate::types::*;

type HmacSha256 = Hmac<Sha256>;


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
        // The standard Gophercloud image client appends `/v2/` to the
        // catalog service root when constructing its ResourceBase.
        ("image", "image", "image", format!("{base}/")),
        // The pinned Gophercloud NewNetworkV2 client appends `/v2.0/` to
        // this catalog service root when constructing ResourceBase.
        ("network", "network", "network", format!("{base}/")),
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

pub(crate) fn store_auth_error(_: AuthError) -> StoreError {
    StoreError::Corrupt("password hashing failed".to_owned())
}

pub(crate) fn now_rfc3339() -> String {
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

pub(crate) fn sign(key: &Secret, input: &[u8]) -> Result<String, AuthError> {
    let mut mac =
        HmacSha256::new_from_slice(key.expose().as_bytes()).map_err(|_| AuthError::InvalidToken)?;
    mac.update(input);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub(crate) fn format_time(seconds: u64) -> Result<String, AuthError> {
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
pub(crate) fn civil_date(days_since_epoch: u64) -> Result<(i64, u64, u64), AuthError> {
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
