use serde::{Deserialize, Serialize};

use crate::{
    principal::{Principal, ServicePrincipal},
    scope::OwnershipScope,
};

/// Canonical O3K Cloud Kernel authorization and authentication context.
///
/// This context is produced once at the protocol/compatibility ingress boundary
/// (e.g. Keystone adapter) from verified credentials and is consumed by all
/// downstream application services and the kernel authorizer.
///
/// Security contract:
/// - Raw token strings (`X-Auth-Token`, `token_id`, passwords) MUST NOT be stored here.
/// - Carries durable `Principal`, `OwnershipScope`, roles, timestamps, `audit_id`, and `request_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthContext {
    principal: Principal,
    effective_scope: OwnershipScope,
    roles: Vec<String>,
    issued_at: u64,
    expires_at: u64,
    audit_id: String,
    request_id: String,
    service_principal: Option<ServicePrincipal>,
}

impl AuthContext {
    /// Creates a new canonical `AuthContext`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        principal: Principal,
        effective_scope: OwnershipScope,
        roles: Vec<String>,
        issued_at: u64,
        expires_at: u64,
        audit_id: impl Into<String>,
        request_id: impl Into<String>,
        service_principal: Option<ServicePrincipal>,
    ) -> Self {
        Self {
            principal,
            effective_scope,
            roles,
            issued_at,
            expires_at,
            audit_id: audit_id.into(),
            request_id: request_id.into(),
            service_principal,
        }
    }

    /// Returns the authenticated primary principal.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Returns the active/effective ownership scope.
    #[must_use]
    pub fn effective_scope(&self) -> &OwnershipScope {
        &self.effective_scope
    }

    /// Returns the compatibility role inputs.
    #[must_use]
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// Checks if a given role name is present.
    #[must_use]
    pub fn has_role(&self, role_name: &str) -> bool {
        self.roles.iter().any(|r| r.eq_ignore_ascii_case(role_name))
    }

    /// Returns the token issuance timestamp (UNIX epoch seconds).
    #[must_use]
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    /// Returns the token expiration timestamp (UNIX epoch seconds).
    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Returns the audit identity.
    #[must_use]
    pub fn audit_id(&self) -> &str {
        &self.audit_id
    }

    /// Returns the request correlation identity.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the optional service principal if this was a service-delegated context.
    #[must_use]
    pub fn service_principal(&self) -> Option<&ServicePrincipal> {
        self.service_principal.as_ref()
    }
}
