//! Service-neutral native resource envelope: common metadata, service-owned
//! spec/status, and top-level resource representation.
//!
//! ADR-0173 / SPEC-0030 define the architectural contract.
//! This module provides the canonical Rust types for the v1 envelope.
//!
//! Architectural invariants:
//! - The Cloud Kernel owns common identity/ownership/generation semantics.
//! - Each service owns its `spec` and `status` schema.
//! - No giant universal enum of every service's business fields.

use serde::{Deserialize, Serialize};

use crate::resource::{ResourceId, ResourceType};
use crate::scope::OwnershipScope;

/// Common metadata shared by every first-class native resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMeta {
    /// Stable canonical resource identifier.
    pub id: ResourceId,
    /// Durable ownership/security scope (project, domain, or system).
    pub owner_scope: OwnershipScope,
    /// Monotonic generation counter incremented on each desired-state mutation.
    #[serde(default)]
    pub generation: i64,
    /// RFC3339 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC3339 last-update timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Region identifier, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Availability domain, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_domain: Option<String>,
    /// Optional free-form labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// Optional free-form annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::HashMap<String, String>>,
}

impl ResourceMeta {
    /// Creates a new `ResourceMeta` with the minimum required fields.
    pub fn new(id: ResourceId, owner_scope: OwnershipScope) -> Self {
        Self {
            id,
            owner_scope,
            generation: 0,
            created_at: None,
            updated_at: None,
            region: None,
            availability_domain: None,
            labels: None,
            annotations: None,
        }
    }
}

/// Service-neutral native resource envelope.
///
/// `spec` and `status` are opaque JSON values owned by the declaring service.
/// The Cloud Kernel interprets only the common metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEnvelope {
    /// API version string (e.g. `"o3k.io/v1"`).
    pub api_version: String,
    /// Service-namespaced resource type (e.g. `"compute:server"`).
    pub kind: ResourceType,
    /// Common resource metadata.
    pub metadata: ResourceMeta,
    /// Service-owned desired state.
    pub spec: serde_json::Value,
    /// Service-owned observed/reported state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
}

impl ResourceEnvelope {
    /// Creates a new `ResourceEnvelope`.
    pub fn new(
        api_version: &str,
        kind: ResourceType,
        metadata: ResourceMeta,
        spec: serde_json::Value,
        status: Option<serde_json::Value>,
    ) -> Self {
        Self {
            api_version: api_version.to_owned(),
            kind,
            metadata,
            spec,
            status,
        }
    }
}
