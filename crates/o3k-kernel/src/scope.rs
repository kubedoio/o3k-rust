use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::KernelError;

/// Stable typed identifier for an ownership / security scope (e.g. project).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeId(String);

impl ScopeId {
    /// Creates and validates a new `ScopeId`.
    pub fn new(id: impl Into<String>) -> Result<Self, KernelError> {
        let s = id.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(KernelError::InvalidScopeId(
                "scope id must not be empty".to_string(),
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// Creates a `ScopeId` without validation.
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

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The profile/kind of an ownership scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Project,
    Domain,
    System,
}

/// Canonical ownership / security scope context in O3K.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipScope {
    id: ScopeId,
    kind: ScopeKind,
    name: Option<String>,
    domain_id: Option<String>,
}

impl OwnershipScope {
    /// Creates a new `OwnershipScope`.
    pub fn new(
        id: ScopeId,
        kind: ScopeKind,
        name: Option<String>,
        domain_id: Option<String>,
    ) -> Self {
        Self {
            id,
            kind,
            name,
            domain_id,
        }
    }

    /// Helper to create a standard project scope.
    pub fn project(id: ScopeId, name: Option<String>, domain_id: Option<String>) -> Self {
        Self::new(id, ScopeKind::Project, name, domain_id)
    }

    #[must_use]
    pub fn id(&self) -> &ScopeId {
        &self.id
    }

    #[must_use]
    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub fn domain_id(&self) -> Option<&str> {
        self.domain_id.as_deref()
    }
}
