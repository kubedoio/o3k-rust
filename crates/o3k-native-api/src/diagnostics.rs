//! Native Operator diagnostics projection (#903).
//!
//! This is a *bounded, read-only* projection of canonical O3K service,
//! provider, location and placement-capacity authority. It is explicitly
//! system/operator-authorized and never becomes a shell gateway, provider
//! administration surface, or arbitrary RPC/log passthrough.
//!
//! Freshness is a first-class property: every source class carries an
//! observation timestamp and a small, stable status vocabulary so a stale
//! or never-observed component is never reported as `healthy`.

use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};
use o3k_kernel::{
    ActionId, AuthorizationDecision, AuthorizationRequest, ResourceTarget, ResourceType, ScopeId,
};

/// Version served by this contract.
pub const DIAGNOSTICS_VERSION: &str = "v1";

/// Default and maximum page sizes for bounded diagnostic collections.
pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 200;

/// Maximum number of distinct capacity resource classes the projection will
/// accept. The storage aggregate already fails closed above this bound.
pub const MAX_CAPACITY_CLASSES: usize = 64;

/// Stable status vocabulary. Distinct by design: `unknown` (never observed)
/// and `stale` (last observation older than its freshness threshold) must
/// never collapse into `healthy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Healthy,
    Degraded,
    Unavailable,
    Stale,
    Unknown,
}

impl DiagnosticStatus {
    /// Returns a stable snake_case wire value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for DiagnosticStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bounded, secret-safe reason category. Provider exception text, connection
/// strings and private topology are never forwarded into these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticReason {
    /// The authority is not configured, so no observation is possible.
    NotConfigured,
    /// The component has never produced an observation in this process.
    NeverObserved,
    /// The last observation is older than the source freshness threshold.
    ObservationStale,
    /// An external component stopped heartbeating.
    HeartbeatLost,
    /// Operator-disabled.
    AdministrativelyDisabled,
    /// Placed in a draining state.
    Draining,
    /// Readiness dependency is not satisfied.
    ReadinessFailed,
    /// Protocol/manifest version is incompatible.
    ProtocolIncompatible,
    /// A required dependency is unavailable.
    DependencyUnavailable,
    /// The component explicitly reported unhealthy.
    ReportedUnhealthy,
    /// The capacity authority could not be read.
    CapacitySourceUnavailable,
    /// This dimension/class is not authoritative and is not claimed.
    Unsupported,
}

impl DiagnosticReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::NeverObserved => "never_observed",
            Self::ObservationStale => "observation_stale",
            Self::HeartbeatLost => "heartbeat_lost",
            Self::AdministrativelyDisabled => "administratively_disabled",
            Self::Draining => "draining",
            Self::ReadinessFailed => "readiness_failed",
            Self::ProtocolIncompatible => "protocol_incompatible",
            Self::DependencyUnavailable => "dependency_unavailable",
            Self::ReportedUnhealthy => "reported_unhealthy",
            Self::CapacitySourceUnavailable => "capacity_source_unavailable",
            Self::Unsupported => "unsupported",
        }
    }
}

// ── Summary ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsSummary {
    pub version: String,
    pub evaluated_at_unix_ms: i64,
    pub status: DiagnosticStatus,
    pub counts: StatusCounts,
    pub control_plane: Option<ControlPlaneStatus>,
    pub locations: LocationDiagnostics,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StatusCounts {
    pub services: ComponentCounts,
    pub providers: ComponentCounts,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ComponentCounts {
    pub total: u64,
    pub healthy: u64,
    pub degraded: u64,
    pub unavailable: u64,
    pub stale: u64,
    pub unknown: u64,
}

impl ComponentCounts {
    /// Computes the aggregate status of a component class from its counts.
    ///
    /// Rules (documented in SPEC-0045):
    /// - nothing observed       -> `unknown`;
    /// - all healthy            -> `healthy`;
    /// - any unavailable        -> `degraded` unless *all* are unavailable/unknown;
    /// - any stale              -> `degraded`;
    /// - any degraded           -> `degraded`;
    /// - otherwise              -> `unknown`.
    #[must_use]
    pub fn aggregate(self) -> DiagnosticStatus {
        if self.total == 0 {
            return DiagnosticStatus::Unknown;
        }
        if self.healthy == self.total {
            return DiagnosticStatus::Healthy;
        }
        if self.unavailable == self.total {
            return DiagnosticStatus::Unavailable;
        }
        if self.unavailable > 0 || self.stale > 0 || self.degraded > 0 {
            return DiagnosticStatus::Degraded;
        }
        DiagnosticStatus::Unknown
    }
}

/// Control-plane process liveness, sourced from the durable coordination
/// lease authority (`controller_sessions`). This is the only durable,
/// timestamped platform liveness signal and is distinct from the aggregate
/// component status.
#[derive(Debug, Clone, Serialize)]
pub struct ControlPlaneStatus {
    pub status: DiagnosticStatus,
    pub active_sessions: u64,
    pub observed_at_unix_ms: Option<i64>,
    pub reason: Option<DiagnosticReason>,
}

/// Location topology from the canonical #887 registry. Locations carry
/// identity only; there is no region/AZ liveness authority, so no health
/// status is fabricated for them.
#[derive(Debug, Clone, Serialize)]
pub struct LocationDiagnostics {
    pub configured: bool,
    pub regions: u64,
    pub availability_domains: u64,
}

// ── Services ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ServiceDiagnostics {
    pub service_id: String,
    pub namespace: String,
    pub service_version: String,
    pub ownership: String,
    pub lifecycle_state: String,
    pub status: DiagnosticStatus,
    pub observed_at_unix_ms: Option<i64>,
    pub reason: Option<DiagnosticReason>,
    pub controller: Option<ControllerDiagnostics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControllerDiagnostics {
    pub mode: String,
    pub protocol: String,
    pub protocol_version: String,
    pub healthy: bool,
    pub session_generation: Option<u64>,
}

// ── Providers ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDiagnostics {
    pub provider_id: String,
    pub state: String,
    pub availability: String,
    pub status: DiagnosticStatus,
    pub observed_at_unix_ms: Option<i64>,
    pub reason: Option<DiagnosticReason>,
    pub capacity: Vec<ProviderCapacityDimension>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderCapacityDimension {
    pub resource_class: String,
    pub total: u64,
    pub reserved: u64,
    pub allocated: u64,
    pub available: u64,
}

// ── Capacity ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CapacityDiagnostics {
    pub version: String,
    pub status: DiagnosticStatus,
    pub observed_at_unix_ms: Option<i64>,
    pub reason: Option<DiagnosticReason>,
    pub providers_enabled: u64,
    pub providers_draining: u64,
    pub providers_unavailable: u64,
    pub providers_deleted: u64,
    pub dimensions: Vec<CapacityDimension>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacityDimension {
    pub resource_class: String,
    pub unit: String,
    pub allocatable: u64,
    pub reserved: u64,
    pub allocated: u64,
    pub available: u64,
}

impl CapacityDimension {
    /// Derives `available` with saturating arithmetic so overflow can never
    /// wrap into an impossible value. A negative remainder (drifted or corrupt
    /// durable state) surfaces as `0` and is reported as degraded by the
    /// caller, never as a negative capacity.
    #[must_use]
    pub fn with_available(mut self) -> Self {
        self.available = self
            .allocatable
            .saturating_sub(self.reserved)
            .saturating_sub(self.allocated);
        self
    }
}

// ── Bounded page ──────────────────────────────────────────────────────────

/// Opaque-cursor page over a bounded diagnostic collection.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsPage<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

/// Parsed query for bounded diagnostic collections.
#[derive(Debug, Clone, Deserialize)]
pub struct DiagnosticsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl DiagnosticsQuery {
    /// Validates and bounds `limit`, returning the requested page size.
    #[allow(clippy::result_large_err)]
    pub fn page_size(&self) -> Result<usize, Response> {
        match self.limit {
            None => Ok(DEFAULT_PAGE_SIZE),
            Some(0) => Ok(DEFAULT_PAGE_SIZE),
            Some(limit) if limit <= MAX_PAGE_SIZE => Ok(limit),
            Some(_) => Err(ProblemDetails::new(ErrorCode::BadRequest).into_response()),
        }
    }

    /// Decodes the opaque continuation cursor into the after-id key, or
    /// `None` for the first page. Invalid cursors are a client error, not an
    /// escalation: they only affect which page the caller sees.
    #[allow(clippy::result_large_err)]
    pub fn after_id(&self) -> Result<Option<String>, Response> {
        let Some(cursor) = self.cursor.as_deref() else {
            return Ok(None);
        };
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(cursor.as_bytes())
            .map_err(|_| ProblemDetails::new(ErrorCode::InvalidCursor).into_response())?;
        let id = String::from_utf8(decoded)
            .map_err(|_| ProblemDetails::new(ErrorCode::InvalidCursor).into_response())?;
        if id.is_empty() || id.len() > 512 {
            return Err(ProblemDetails::new(ErrorCode::InvalidCursor).into_response());
        }
        Ok(Some(id))
    }
}

/// Encodes an after-id continuation cursor. The value is opaque to clients.
pub fn encode_cursor(after_id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(after_id.as_bytes())
}

// ── Port ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsError {
    /// A diagnostic authority is not configured or currently unavailable.
    Unavailable,
    /// Durable diagnostic/capacity state is corrupt.
    Corrupt,
}

fn diagnostics_error(error: DiagnosticsError, request_id: &str) -> Response {
    let code = match error {
        DiagnosticsError::Unavailable => ErrorCode::NotAvailable,
        DiagnosticsError::Corrupt => ErrorCode::InternalError,
    };
    ProblemDetails::new(code)
        .with_request_id(request_id.to_owned())
        .into_response()
}

/// Port implemented by the production `o3kd` composition.
///
/// Implementations must project canonical authority only. They must never
/// expose provider credentials, connection strings, node identities, or raw
/// provider/controller error text, and must never materialize unbounded
/// provider/service collections.
#[async_trait::async_trait]
pub trait DiagnosticsReader: Send + Sync {
    async fn summary(&self) -> Result<DiagnosticsSummary, DiagnosticsError>;
    async fn services(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<DiagnosticsPage<ServiceDiagnostics>, DiagnosticsError>;
    async fn providers(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<DiagnosticsPage<ProviderDiagnostics>, DiagnosticsError>;
    async fn capacity(&self) -> Result<CapacityDiagnostics, DiagnosticsError>;
}

// ── Authorization ─────────────────────────────────────────────────────────

fn authorize(state: &NativeApiState, auth: &o3k_kernel::AuthContext) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: ActionId::new_unchecked("operator", "ReadDiagnostics"),
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("operator", "diagnostics"),
                Some(ScopeId::new_unchecked("system")),
            ),
        }),
        AuthorizationDecision::Allow
    )
}

// ── Handlers ──────────────────────────────────────────────────────────────

pub async fn summary(auth: BearerAuth, State(state): State<NativeApiState>) -> Response {
    let Some(reader) = state.diagnostics_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(&state, &auth.0) {
        return ProblemDetails::new(ErrorCode::Forbidden)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    match reader.summary().await {
        Ok(summary) => Json(summary).into_response(),
        Err(error) => diagnostics_error(error, auth.0.request_id()),
    }
}

pub async fn services(
    auth: BearerAuth,
    Query(query): Query<DiagnosticsQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.diagnostics_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(&state, &auth.0) {
        return ProblemDetails::new(ErrorCode::Forbidden)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    let limit = match query.page_size() {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let after = match query.after_id() {
        Ok(after) => after,
        Err(response) => return response,
    };
    match reader.services(limit, after.as_deref()).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => diagnostics_error(error, auth.0.request_id()),
    }
}

pub async fn providers(
    auth: BearerAuth,
    Query(query): Query<DiagnosticsQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.diagnostics_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(&state, &auth.0) {
        return ProblemDetails::new(ErrorCode::Forbidden)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    let limit = match query.page_size() {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let after = match query.after_id() {
        Ok(after) => after,
        Err(response) => return response,
    };
    match reader.providers(limit, after.as_deref()).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => diagnostics_error(error, auth.0.request_id()),
    }
}

pub async fn capacity(auth: BearerAuth, State(state): State<NativeApiState>) -> Response {
    let Some(reader) = state.diagnostics_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(&state, &auth.0) {
        return ProblemDetails::new(ErrorCode::Forbidden)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    match reader.capacity().await {
        Ok(capacity) => Json(capacity).into_response(),
        Err(error) => diagnostics_error(error, auth.0.request_id()),
    }
}

/// Groups per-class capacity into a canonical ordering and drops nothing.
pub fn sort_dimensions(dimensions: &mut [CapacityDimension]) {
    dimensions.sort_by(|left, right| left.resource_class.cmp(&right.resource_class));
}

/// Validates that a bounded capacity projection stays within the accepted
/// class bound. Used by adapters that aggregate placement authority.
#[must_use]
pub fn validate_capacity_class_count(count: usize) -> bool {
    count <= MAX_CAPACITY_CLASSES
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn status_vocabulary_is_stable_and_distinct() {
        assert_eq!(DiagnosticStatus::Healthy.as_str(), "healthy");
        assert_eq!(DiagnosticStatus::Degraded.as_str(), "degraded");
        assert_eq!(DiagnosticStatus::Unavailable.as_str(), "unavailable");
        assert_eq!(DiagnosticStatus::Stale.as_str(), "stale");
        assert_eq!(DiagnosticStatus::Unknown.as_str(), "unknown");
        // The four "not healthy" states must all differ from healthy.
        let mut set = std::collections::HashSet::new();
        for status in [
            DiagnosticStatus::Healthy,
            DiagnosticStatus::Degraded,
            DiagnosticStatus::Unavailable,
            DiagnosticStatus::Stale,
            DiagnosticStatus::Unknown,
        ] {
            set.insert(status.as_str());
        }
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn aggregate_never_collapses_unknown_into_healthy() {
        // Nothing observed -> unknown, not healthy.
        assert_eq!(
            ComponentCounts::default().aggregate(),
            DiagnosticStatus::Unknown
        );
    }

    #[test]
    fn aggregate_all_healthy_is_healthy() {
        let counts = ComponentCounts {
            total: 3,
            healthy: 3,
            ..ComponentCounts::default()
        };
        assert_eq!(counts.aggregate(), DiagnosticStatus::Healthy);
    }

    #[test]
    fn aggregate_partial_failure_is_degraded_not_healthy() {
        // One unavailable among healthy -> degraded.
        let counts = ComponentCounts {
            total: 3,
            healthy: 2,
            unavailable: 1,
            ..ComponentCounts::default()
        };
        assert_eq!(counts.aggregate(), DiagnosticStatus::Degraded);
    }

    #[test]
    fn aggregate_all_unavailable_is_unavailable() {
        let counts = ComponentCounts {
            total: 2,
            unavailable: 2,
            ..ComponentCounts::default()
        };
        assert_eq!(counts.aggregate(), DiagnosticStatus::Unavailable);
    }

    #[test]
    fn aggregate_stale_is_degraded_not_healthy() {
        let counts = ComponentCounts {
            total: 1,
            stale: 1,
            ..ComponentCounts::default()
        };
        assert_eq!(counts.aggregate(), DiagnosticStatus::Degraded);
    }

    #[test]
    fn capacity_available_is_saturating() {
        let dimension = CapacityDimension {
            resource_class: "VCPU".to_owned(),
            unit: "count".to_owned(),
            allocatable: 10,
            reserved: 2,
            allocated: 5,
            available: 0,
        }
        .with_available();
        assert_eq!(dimension.available, 3);

        // Overflow/negative remainder clamps to zero, never wraps.
        let overflow = CapacityDimension {
            resource_class: "VCPU".to_owned(),
            unit: "count".to_owned(),
            allocatable: 2,
            reserved: 5,
            allocated: 9,
            available: 0,
        }
        .with_available();
        assert_eq!(overflow.available, 0);
    }

    #[test]
    fn cursor_round_trips_and_rejects_invalid() {
        let encoded = encode_cursor("provider-42");
        let query = DiagnosticsQuery {
            limit: None,
            cursor: Some(encoded),
        };
        assert_eq!(
            query.after_id().ok().flatten().as_deref(),
            Some("provider-42")
        );

        let bad = DiagnosticsQuery {
            limit: None,
            cursor: Some("not base64!!".to_owned()),
        };
        assert!(bad.after_id().is_err());
    }

    #[test]
    fn page_size_is_bounded() {
        let ok = DiagnosticsQuery {
            limit: Some(200),
            cursor: None,
        };
        assert_eq!(ok.page_size().ok(), Some(200));
        let too_large = DiagnosticsQuery {
            limit: Some(201),
            cursor: None,
        };
        assert!(too_large.page_size().is_err());
    }
}
