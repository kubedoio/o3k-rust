use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::KernelError;

/// Stable typed identifier for an authenticated principal.
///
/// Principal IDs distinguish durable identifiers from human display names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrincipalId(String);

impl PrincipalId {
    /// Creates and validates a new `PrincipalId`.
    pub fn new(id: impl Into<String>) -> Result<Self, KernelError> {
        let s = id.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(KernelError::InvalidPrincipalId(
                "principal id must not be empty".to_string(),
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// Creates a `PrincipalId` without validation.
    #[must_use]
    pub fn new_unchecked(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Exposes the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind/class of principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    User,
    Service,
}

/// An authenticated human or external user principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrincipal {
    id: PrincipalId,
    name: String,
    domain_id: Option<String>,
}

impl UserPrincipal {
    /// Constructs a validated `UserPrincipal`.
    pub fn new(id: PrincipalId, name: impl Into<String>, domain_id: Option<String>) -> Self {
        Self {
            id,
            name: name.into(),
            domain_id,
        }
    }

    #[must_use]
    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn domain_id(&self) -> Option<&str> {
        self.domain_id.as_deref()
    }
}

/// An authenticated service-to-service principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePrincipal {
    id: PrincipalId,
    name: String,
    service_type: String,
}

impl ServicePrincipal {
    /// Constructs a validated `ServicePrincipal`.
    pub fn new(id: PrincipalId, name: impl Into<String>, service_type: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            service_type: service_type.into(),
        }
    }

    #[must_use]
    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn service_type(&self) -> &str {
        &self.service_type
    }
}

/// Canonical principal representation in O3K.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    User(UserPrincipal),
    Service(ServicePrincipal),
}

impl Principal {
    /// Returns the stable `PrincipalId`.
    #[must_use]
    pub fn id(&self) -> &PrincipalId {
        match self {
            Self::User(u) => u.id(),
            Self::Service(s) => s.id(),
        }
    }

    /// Returns the display/login name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::User(u) => u.name(),
            Self::Service(s) => s.name(),
        }
    }

    /// Returns the `PrincipalKind`.
    #[must_use]
    pub fn kind(&self) -> PrincipalKind {
        match self {
            Self::User(_) => PrincipalKind::User,
            Self::Service(_) => PrincipalKind::Service,
        }
    }
}
