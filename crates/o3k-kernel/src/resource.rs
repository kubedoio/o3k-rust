use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{error::KernelError, scope::ScopeId};

/// Stable identifier of a cloud resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(String);

impl ResourceId {
    /// Creates and validates a new `ResourceId`.
    pub fn new(id: impl Into<String>) -> Result<Self, KernelError> {
        let s = id.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(KernelError::InvalidResourceId(
                "resource id must not be empty".to_string(),
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// Creates a `ResourceId` without validation.
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

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Service-namespaced resource type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceType {
    namespace: String,
    name: String,
}

impl ResourceType {
    /// Creates and validates a `ResourceType`.
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Result<Self, KernelError> {
        let ns = namespace.into();
        let n = name.into();
        if ns.trim().is_empty() || n.trim().is_empty() {
            return Err(KernelError::InvalidResourceType(
                "resource namespace and name must not be empty".to_string(),
            ));
        }
        Ok(Self {
            namespace: ns.trim().to_lowercase(),
            name: n.trim().to_lowercase(),
        })
    }

    /// Creates a `ResourceType` without validation.
    #[must_use]
    pub fn new_unchecked(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.name)
    }
}

/// Target of an operation or authorization request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum ResourceTarget {
    /// Collection target (e.g. creating a new resource in a scope, or listing resources in a scope).
    Collection {
        resource_type: ResourceType,
        owner_scope: Option<ScopeId>,
    },
    /// Instance target (e.g. reading, updating, or deleting a specific resource instance).
    Instance {
        resource_type: ResourceType,
        resource_id: ResourceId,
        owner_scope: Option<ScopeId>,
    },
}

impl ResourceTarget {
    /// Constructs a collection target.
    pub fn collection(resource_type: ResourceType, owner_scope: Option<ScopeId>) -> Self {
        Self::Collection {
            resource_type,
            owner_scope,
        }
    }

    /// Constructs an instance target.
    pub fn instance(
        resource_type: ResourceType,
        resource_id: ResourceId,
        owner_scope: Option<ScopeId>,
    ) -> Self {
        Self::Instance {
            resource_type,
            resource_id,
            owner_scope,
        }
    }

    /// Returns the target's `ResourceType`.
    #[must_use]
    pub fn resource_type(&self) -> &ResourceType {
        match self {
            Self::Collection { resource_type, .. } => resource_type,
            Self::Instance { resource_type, .. } => resource_type,
        }
    }

    /// Returns the target's `owner_scope`, if specified.
    #[must_use]
    pub fn owner_scope(&self) -> Option<&ScopeId> {
        match self {
            Self::Collection { owner_scope, .. } => owner_scope.as_ref(),
            Self::Instance { owner_scope, .. } => owner_scope.as_ref(),
        }
    }

    /// Returns the target's `resource_id` if it is an `Instance` target.
    #[must_use]
    pub fn resource_id(&self) -> Option<&ResourceId> {
        match self {
            Self::Collection { .. } => None,
            Self::Instance { resource_id, .. } => Some(resource_id),
        }
    }
}
