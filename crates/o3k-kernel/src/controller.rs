//! Controller protocol contract — the interface between O3K Cloud Kernel and
//! external/resource controllers.
//!
//! ADR-0174 / SPEC-0031 define the controller model. This module provides the
//! minimal Rust contract types: the `Controller` trait, session/registration
//! types, and protocol version negotiation.
//!
//! Architectural rules:
//! - The Cloud Kernel calls into the controller through this interface.
//! - Controllers have authenticated service identity distinct from tenant users.
//! - Delegated work preserves original actor and scope.
//! - The protocol is language-neutral (gRPC/protobuf direction); this Rust
//!   interface is the canonical runtime contract for in-process and SDK usage.
//!
//! Implementation note: the gRPC transport layer is intentionally deferred.
//! This module defines the logical contract only.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::ActionId;
use crate::resource::{ResourceId, ResourceType};
use crate::scope::OwnershipScope;

/// Controller protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const V1: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

    /// Creates a new protocol version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns true if this version is compatible with the given supported range.
    #[must_use]
    pub fn is_compatible(&self, supported: &ProtocolVersion) -> bool {
        self.major == supported.major && self.minor <= supported.minor
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Controller session identity — authenticates one controller instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerSession {
    /// Controller identity (bound to service_id).
    pub controller_id: String,
    /// Monotonic session generation for replay/staleness detection.
    pub session_generation: u64,
    /// Negotiated protocol version.
    pub protocol_version: ProtocolVersion,
    /// RFC3339 session start time.
    pub started_at: String,
}

/// Reconcile request — sent by the control plane to a controller for a
/// specific resource operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileRequest {
    /// Operation ID for idempotency and tracking.
    pub operation_id: Uuid,
    /// Target resource type.
    pub resource_type: ResourceType,
    /// Target resource ID.
    pub resource_id: ResourceId,
    /// Current resource generation.
    pub generation: i64,
    /// Desired spec (service-owned JSON).
    pub spec: serde_json::Value,
    /// Current status (service-owned JSON).
    pub status: Option<serde_json::Value>,
    /// Delegate context for cross-service action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<DelegationContext>,
}

/// Delegation context — preserves original actor and scope when a service
/// performs work on behalf of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationContext {
    /// Original user principal identity.
    pub original_actor: String,
    /// Original ownership/security scope.
    pub original_scope: OwnershipScope,
    /// Calling service principal identity.
    pub calling_service: String,
    /// Parent action that triggered the delegation.
    pub parent_action: ActionId,
    /// Allowed delegated action.
    pub delegated_action: ActionId,
    /// Request correlation ID.
    pub request_id: String,
}

/// Outcome of a controller reconcile operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// Resource is now in the desired state.
    Succeeded,
    /// Controller has accepted the work but it is not yet complete.
    Accepted {
        /// Human-readable detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Operation failed but may be retried.
    Retryable {
        /// Error description.
        error: String,
    },
    /// Operation terminally failed.
    Failed {
        /// Error description.
        error: String,
    },
    /// Outcome is unknown (timeout, lost connection).
    Unknown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Controller health state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerHealth {
    /// Whether the controller is healthy and can accept work.
    pub healthy: bool,
    /// Human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Protocol version the controller supports.
    pub protocol_version: ProtocolVersion,
}

/// Controller capability summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerCapabilities {
    /// Supported protocol version range.
    pub protocol_version: ProtocolVersion,
    /// Resource types this controller can reconcile.
    pub resource_types: Vec<String>,
    /// Actions this controller can perform.
    pub actions: Vec<String>,
}

/// The controller interface — the canonical contract between O3K Cloud Kernel
/// and an external resource controller.
///
/// This trait is the Rust-side representation. The production transport is
/// gRPC/protobuf (deferred); this trait exists for in-process testing and
/// as the normative architecture contract.
#[async_trait::async_trait]
pub trait Controller: Send + Sync {
    /// Health check.
    async fn health(&self) -> ControllerHealth;
    /// Capability advertisement.
    async fn capabilities(&self) -> ControllerCapabilities;
    /// Reconcile a resource to its desired state.
    async fn reconcile(&self, request: ReconcileRequest) -> ReconcileOutcome;
    /// Observe a resource (read-only, no side effects).
    async fn observe(&self, resource_type: ResourceType, resource_id: ResourceId) -> ReconcileOutcome;
    /// Delete a resource.
    async fn delete(&self, resource_type: ResourceType, resource_id: ResourceId) -> ReconcileOutcome;
}

// ── Registration ───────────────────────────────────────────────────────────

/// The life cycle state of a registered controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerState {
    /// Service declared but controller not yet registered.
    Declared,
    /// Controller session active and healthy.
    Ready,
    /// Controller reports healthy but not ready for work.
    NotReady,
    /// Controller protocol or manifest version is incompatible.
    Incompatible,
    /// Operator-disabled.
    Disabled,
}

/// A registered controller binding, linking a service manifest to an
/// authenticated controller session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerRegistration {
    /// Service ID this controller is registered for.
    pub service_id: String,
    /// Service namespace.
    pub namespace: String,
    /// Current controller session, if registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ControllerSession>,
    /// Current lifecycle state.
    pub state: ControllerState,
    /// Last known health.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<ControllerHealth>,
}

impl ControllerRegistration {
    /// Creates a new declared registration.
    #[must_use]
    pub fn declared(service_id: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            namespace: namespace.into(),
            session: None,
            state: ControllerState::Declared,
            health: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_compatibility() {
        let v1 = ProtocolVersion::V1;
        assert!(v1.is_compatible(&ProtocolVersion::V1));
        assert!(ProtocolVersion::new(1, 0).is_compatible(&ProtocolVersion::new(1, 5)));
        assert!(!ProtocolVersion::new(2, 0).is_compatible(&ProtocolVersion::V1));
    }

    #[test]
    fn reconcile_outcome_serialization() {
        let outcome = ReconcileOutcome::Succeeded;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, r#"{"status":"succeeded"}"#);
    }

    #[test]
    fn controller_registration_declared() {
        let reg = ControllerRegistration::declared("database-example", "database");
        assert_eq!(reg.state, ControllerState::Declared);
        assert!(reg.session.is_none());
    }
}
