//! Canonical Cloud Kernel audit event model, outcome vocabulary, and sink port.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    action::ActionId,
    auth_context::AuthContext,
    authorization::AuthorizationDecision,
    principal::{PrincipalId, PrincipalKind, ServicePrincipal},
    registry::ServiceNamespace,
    resource::{ResourceId, ResourceType},
    scope::OwnershipScope,
};

/// Strongly typed audit event identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(String);

impl EventId {
    /// Generates a new random / time-ordered `EventId` (UUID v7).
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// Creates an `EventId` from an existing string representation.
    #[must_use]
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Standardized outcome vocabulary for Cloud Kernel audit events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditOutcome {
    /// Operation was authorized.
    Allowed,
    /// Operation was denied by authorization.
    Denied,
    /// Operation mutation or read completed successfully.
    Succeeded,
    /// Operation failed with an error.
    Failed,
    /// Provider execution outcome is unknown (timeout or stale state).
    UnknownOutcome,
}

impl fmt::Display for AuditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allowed => write!(f, "allowed"),
            Self::Denied => write!(f, "denied"),
            Self::Succeeded => write!(f, "succeeded"),
            Self::Failed => write!(f, "failed"),
            Self::UnknownOutcome => write!(f, "unknown_outcome"),
        }
    }
}

/// Canonical, secret-safe audit event recorded by Cloud Kernel services.
///
/// This struct guarantees that secrets (passwords, tokens, keys, CHAP credentials,
/// user-data payloads) are structurally absent and never logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event identifier.
    pub event_id: EventId,
    /// RFC3339 formatted event timestamp.
    pub timestamp: String,
    /// Transport / API correlation request ID.
    pub request_id: String,
    /// Security audit trace correlation ID.
    pub audit_id: String,
    /// Calling principal identity.
    pub principal_id: PrincipalId,
    /// Calling principal kind (User or Service).
    pub principal_kind: PrincipalKind,
    /// Effective project/domain security scope.
    pub effective_scope: OwnershipScope,
    /// Target service domain namespace.
    pub service_namespace: ServiceNamespace,
    /// Canonical action executed.
    pub action: ActionId,
    /// Target resource type, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<ResourceType>,
    /// Target resource identifier, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<ResourceId>,
    /// Target resource owner scope, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_scope: Option<OwnershipScope>,
    /// Authorization decision outcome, if relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_decision: Option<AuthorizationDecision>,
    /// Correlated durable operation ID, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
    /// Outcome of this action / phase.
    pub outcome: AuditOutcome,
    /// Normalized failure / denial reason category (bounded string, no raw secrets/errors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_category: Option<String>,
    /// Authenticated service principal for cross-service delegated work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_principal: Option<ServicePrincipal>,
}

impl AuditEvent {
    /// Constructs an `AuditEvent` from an `AuthContext` and action details.
    #[must_use]
    pub fn from_auth(
        auth: &AuthContext,
        service_namespace: ServiceNamespace,
        action: ActionId,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            event_id: EventId::new(),
            timestamp: now_rfc3339(),
            request_id: auth.request_id().to_owned(),
            audit_id: auth.audit_id().to_owned(),
            principal_id: auth.principal().id().clone(),
            principal_kind: auth.principal().kind(),
            effective_scope: auth.effective_scope().clone(),
            service_namespace,
            action,
            resource_type: None,
            resource_id: None,
            owner_scope: None,
            authorization_decision: None,
            operation_id: None,
            outcome,
            reason_category: None,
            service_principal: auth.service_principal().cloned(),
        }
    }

    #[must_use]
    pub fn with_resource(
        mut self,
        resource_type: ResourceType,
        resource_id: Option<ResourceId>,
        owner_scope: Option<OwnershipScope>,
    ) -> Self {
        self.resource_type = Some(resource_type);
        self.resource_id = resource_id;
        self.owner_scope = owner_scope;
        self
    }

    #[must_use]
    pub fn with_decision(mut self, decision: AuthorizationDecision) -> Self {
        self.authorization_decision = Some(decision);
        self
    }

    #[must_use]
    pub fn with_operation(mut self, operation_id: Uuid) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason_category = Some(reason.into());
        self
    }
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AuditEvent[{}] action={} outcome={} principal={} scope={} audit_id={}",
            self.event_id,
            self.action,
            self.outcome,
            self.principal_id,
            self.effective_scope,
            self.audit_id
        )
    }
}

/// Sink port for recording canonical Cloud Kernel audit events.
pub trait AuditSink: Send + Sync {
    /// Records a canonical audit event. Implementations must be fail-safe and bounded.
    fn record(&self, event: &AuditEvent);
}

/// Audit sink that forwards recorded events to a closure or function.
pub struct FnAuditSink<F: Fn(&AuditEvent) + Send + Sync>(pub F);

impl<F: Fn(&AuditEvent) + Send + Sync> AuditSink for FnAuditSink<F> {
    fn record(&self, event: &AuditEvent) {
        (self.0)(event);
    }
}

/// In-memory audit sink for testing and verification.
#[derive(Debug, Clone, Default)]
pub struct MemoryAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl MemoryAuditSink {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.events.lock() {
            guard.clear();
        }
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&self, event: &AuditEvent) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(event.clone());
        }
    }
}

/// No-op audit sink for unit tests or environments with disabled audit tracing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditSink;

impl AuditSink for NoopAuditSink {
    fn record(&self, _event: &AuditEvent) {}
}

pub(crate) fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_time(seconds)
}

fn format_time(seconds: u64) -> String {
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date(days_since_epoch: u64) -> (i64, u64, u64) {
    let z = days_since_epoch as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
