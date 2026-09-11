//! Native metering projection (#904).
//!
//! This is a *bounded, read-only* projection of canonical O3K metering
//! authority (ADR-0183, SPEC-0046). It exposes two things only:
//!
//! - the canonical meter catalog, which is public secret-free metadata;
//! - bounded usage for one scope, whose completeness is always explicit.
//!
//! It is never a billing/pricing surface, a raw-history export, or an
//! arbitrary time-series query engine. Instants are always rendered as
//! RFC3339 UTC strings so a client never has to guess the timezone, and a
//! usage number is never returned without a machine-readable completeness
//! status.

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
    ActionId, AuthorizationDecision, AuthorizationRequest, Clock, MAX_USAGE_METERS,
    MeterAggregation, MeterDefinition, MeterUnit, MeterUsage, MeterUsageReport, ResourceTarget,
    ResourceType, ScopeId, SystemClock, UsageGranularity, UsageQuery, UsageStatus,
};

/// Version served by this contract.
pub const METERING_VERSION: &str = "v1";

/// Default and maximum page sizes for the bounded definitions collection.
pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 200;

// ── Definitions ───────────────────────────────────────────────────────────

/// Secret-free public representation of one canonical meter definition.
///
/// Definitions describe semantics only. They never carry pricing, layout,
/// executable formulas, or provider identity.
#[derive(Debug, Clone, Serialize)]
pub struct MeterDefinitionView {
    pub key: String,
    pub owning_service: String,
    pub unit: MeterUnit,
    pub aggregation: MeterAggregation,
    pub resource_type: String,
    pub supported_granularities: Vec<&'static str>,
    pub tenant_visible: bool,
    pub operator_visible: bool,
    pub description: String,
    pub version: u32,
}

impl MeterDefinitionView {
    /// Projects a canonical [`MeterDefinition`] into its public wire shape.
    #[must_use]
    pub fn from_definition(definition: &MeterDefinition) -> Self {
        Self {
            key: definition.key.to_owned(),
            owning_service: definition.owning_service.to_owned(),
            unit: definition.unit,
            aggregation: definition.aggregation,
            resource_type: definition.resource_type.to_owned(),
            supported_granularities: definition
                .supported_granularities
                .iter()
                .map(|granularity| granularity.as_str())
                .collect(),
            tenant_visible: definition.tenant_visible,
            operator_visible: definition.operator_visible,
            description: definition.description.to_owned(),
            version: definition.version,
        }
    }
}

/// Bounded page over the canonical meter catalog.
#[derive(Debug, Clone, Serialize)]
pub struct MeterDefinitionsPage {
    pub definitions: Vec<MeterDefinitionView>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

/// Parsed query for the bounded definitions collection.
#[derive(Debug, Clone, Deserialize)]
pub struct DefinitionsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl DefinitionsQuery {
    /// Validates and bounds `limit`, returning the requested page size.
    ///
    /// SPEC-0046 §2.1 defines the accepted range as 1–200. A missing `limit`
    /// takes the default; `0` and anything above the maximum are rejected
    /// rather than silently repaired.
    #[allow(clippy::result_large_err)]
    pub fn page_size(&self) -> Result<usize, Response> {
        match self.limit {
            None => Ok(DEFAULT_PAGE_SIZE),
            Some(limit) if (1..=MAX_PAGE_SIZE).contains(&limit) => Ok(limit),
            Some(_) => Err(ProblemDetails::new(ErrorCode::BadRequest).into_response()),
        }
    }

    /// Decodes the opaque continuation cursor into the after-key value, or
    /// `None` for the first page. An invalid cursor is a client error, not an
    /// escalation: it only affects which page the caller sees.
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

/// Encodes an after-key continuation cursor. The value is opaque to clients.
#[must_use]
pub fn encode_cursor(after_id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(after_id.as_bytes())
}

// ── Usage ─────────────────────────────────────────────────────────────────

/// One bounded output bucket. `bucket_start` is RFC3339 UTC.
#[derive(Debug, Clone, Serialize)]
pub struct MeterUsageBucketView {
    pub bucket_start: String,
    pub bucket_width_ms: i64,
    /// Decimal quantity in the meter unit, three fractional digits.
    pub quantity: String,
}

/// Usage for exactly one meter over the requested period.
///
/// Every instant is an RFC3339 UTC string derived from the canonical
/// epoch-millisecond authority value. A client never receives a bare number
/// for a time and never has to guess the timezone.
#[derive(Debug, Clone, Serialize)]
pub struct MeterUsageResponse {
    pub scope: String,
    pub meter_key: String,
    pub unit: MeterUnit,
    pub aggregation: MeterAggregation,
    pub granularity: UsageGranularity,
    pub start: String,
    pub end: String,
    pub observed_through: String,
    pub authority_started_at: Option<String>,
    pub last_observed_at: Option<String>,
    pub status: UsageStatus,
    pub buckets: Vec<MeterUsageBucketView>,
    pub total: String,
}

/// Parsed usage query parameters.
///
/// `meter` is repeatable (`?meter=a&meter=b`). Axum's stock `Query` extractor
/// cannot decode a repeated key into a struct field through `serde_urlencoded`
/// (the second occurrence is a duplicate-field error), so `meter` is decoded
/// with a hand-written map visitor that collects occurrences in request order.
/// Every other key keeps its last occurrence, and an **unknown** key is a hard
/// decode error (→ 400) rather than being silently ignored, so a typo such as
/// `?scopes=project-b` cannot quietly return the caller's own scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQueryParams {
    /// Requested meter keys, in request order. Empty means "canonical default".
    pub meter: Vec<String>,
    /// Cross-scope selection. Honored only when it equals the caller's own
    /// scope or the caller satisfies `metering:ReadUsageAll`.
    pub scope: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub granularity: Option<String>,
    pub resource_id: Option<String>,
}

/// Accepted `UsageQueryParams` keys, reported to a caller that sends others.
const USAGE_QUERY_FIELDS: &[&str] = &[
    "meter",
    "scope",
    "start",
    "end",
    "granularity",
    "resource_id",
];

impl<'de> Deserialize<'de> for UsageQueryParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct UsageQueryVisitor;

        impl<'de> serde::de::Visitor<'de> for UsageQueryVisitor {
            type Value = UsageQueryParams;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a metering usage query string")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut params = UsageQueryParams::default();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<String>()?;
                    match key.as_str() {
                        "meter" => params.meter.push(value),
                        "scope" => params.scope = Some(value),
                        "start" => params.start = Some(value),
                        "end" => params.end = Some(value),
                        "granularity" => params.granularity = Some(value),
                        "resource_id" => params.resource_id = Some(value),
                        _ => {
                            return Err(<A::Error as serde::de::Error>::unknown_field(
                                &key,
                                USAGE_QUERY_FIELDS,
                            ));
                        }
                    }
                }
                Ok(params)
            }
        }

        deserializer.deserialize_map(UsageQueryVisitor)
    }
}

/// Renders an epoch-millisecond instant as RFC3339 UTC with millisecond
/// precision and a `Z` suffix. `None` means the durable value is not a
/// representable instant, which the caller reports as corrupt authority.
fn rfc3339_utc(unix_ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn optional_instant(unix_ms: Option<i64>) -> Result<Option<String>, MeteringError> {
    unix_ms
        .map(|value| rfc3339_utc(value).ok_or(MeteringError::Corrupt))
        .transpose()
}

// ── Port ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeteringError {
    /// A metering authority is not configured or currently unavailable.
    Unavailable,
    /// Durable metering state is corrupt or internally inconsistent.
    Corrupt,
    /// The request exceeds a bounded query limit.
    BoundsExceeded,
}

fn metering_error(error: MeteringError, request_id: &str) -> Response {
    let code = match error {
        MeteringError::Unavailable => ErrorCode::NotAvailable,
        MeteringError::Corrupt => ErrorCode::InternalError,
        MeteringError::BoundsExceeded => ErrorCode::BadRequest,
    };
    ProblemDetails::new(code)
        .with_request_id(request_id.to_owned())
        .into_response()
}

/// Port implemented by the production `o3kd` composition.
///
/// Implementations must project canonical O3K metering authority only. They
/// must never invent historical usage, expose provider credentials or raw
/// provider payloads, or return a usage number without completeness status.
#[async_trait::async_trait]
pub trait MeteringReader: Send + Sync {
    /// Bounded page of the meters this authority can actually produce, ordered
    /// by `key`. A meter that is not producible for the active profile is
    /// omitted here and rejected by [`MeteringReader::usage`].
    async fn definitions(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<MeterDefinitionsPage, MeteringError>;

    /// Bounded usage aggregation for exactly the requested scope and meters.
    async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, MeteringError>;
}

// ── Authorization ─────────────────────────────────────────────────────────

fn authorize_read_definitions(state: &NativeApiState, auth: &o3k_kernel::AuthContext) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: ActionId::new_unchecked("metering", "ReadDefinitions"),
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("metering", "definitions"),
                Some(ScopeId::new_unchecked("system")),
            ),
        }),
        AuthorizationDecision::Allow
    )
}

/// Authorizes a usage read against the caller's *effective* scope.
fn authorize_read_usage(state: &NativeApiState, auth: &o3k_kernel::AuthContext) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: ActionId::new_unchecked("metering", "ReadUsage"),
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("metering", "usage"),
                Some(auth.effective_scope().id().clone()),
            ),
        }),
        AuthorizationDecision::Allow
    )
}

/// Authorizes reading usage for `target_scope`, which the caller has already
/// established differs from its own effective scope. This is the discoverable
/// system/operator capability (`metering:ReadUsageAll`), not ad-hoc handler
/// logic: System scope plus the `operator` role is required.
fn authorize_read_usage_all(
    state: &NativeApiState,
    auth: &o3k_kernel::AuthContext,
    target_scope: &str,
) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: ActionId::new_unchecked("metering", "ReadUsageAll"),
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("metering", "usage"),
                Some(ScopeId::new_unchecked(target_scope)),
            ),
        }),
        AuthorizationDecision::Allow
    )
}

fn forbidden(request_id: &str) -> Response {
    ProblemDetails::new(ErrorCode::Forbidden)
        .with_request_id(request_id.to_owned())
        .into_response()
}

// ── Handlers ──────────────────────────────────────────────────────────────

pub async fn definitions(
    auth: BearerAuth,
    Query(query): Query<DefinitionsQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    // Authorization precedes the authority-presence check so an unauthorized
    // caller learns nothing about configuration (matches diagnostics.rs).
    if !authorize_read_definitions(&state, &auth.0) {
        return forbidden(auth.0.request_id());
    }
    let Some(reader) = state.metering_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let limit = match query.page_size() {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let after = match query.after_id() {
        Ok(after) => after,
        Err(response) => return response,
    };
    match reader.definitions(limit, after.as_deref()).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => metering_error(error, auth.0.request_id()),
    }
}

pub async fn usage(
    auth: BearerAuth,
    Query(query): Query<UsageQueryParams>,
    State(state): State<NativeApiState>,
) -> Response {
    // Authorization precedes every other check so an unauthorized caller
    // learns nothing about configuration or parameter validity.
    if !authorize_read_usage(&state, &auth.0) {
        return forbidden(auth.0.request_id());
    }

    // Scope selection. `metering:ReadUsage` was authorized above against the
    // caller's own effective scope. Supplying an explicit `scope` equal to that
    // scope is a no-op convenience; a genuinely different scope additionally
    // requires `metering:ReadUsageAll` (System scope plus the operator role),
    // never an ad-hoc role-name check. A caller that fails it is denied
    // outright and never silently downgraded to its own scope.
    let caller_scope_id = auth.0.effective_scope().id().as_str().to_owned();
    let target_scope = match query.scope.as_deref() {
        Some(scope) if scope == caller_scope_id.as_str() => scope.to_owned(),
        Some(scope) => {
            if !authorize_read_usage_all(&state, &auth.0, scope) {
                return forbidden(auth.0.request_id());
            }
            scope.to_owned()
        }
        None => caller_scope_id,
    };

    let Some(reader) = state.metering_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };

    // When the caller does not name meters, use the *producible* set from the
    // authority itself (the same set `definitions` advertises) rather than the
    // static catalog, which would request meters the active profile cannot
    // produce. The list is deliberately not truncated: an over-large producible
    // set fails the bound check below instead of being silently narrowed.
    let meter_keys = if query.meter.is_empty() {
        match reader.definitions(MAX_PAGE_SIZE, None).await {
            Ok(page) => page
                .definitions
                .into_iter()
                .map(|definition| definition.key)
                .collect(),
            Err(error) => return metering_error(error, auth.0.request_id()),
        }
    } else {
        query.meter.clone()
    };
    if meter_keys.is_empty() || meter_keys.len() > MAX_USAGE_METERS {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }

    let Some(start_ms) = parse_instant_ms(query.start.as_deref()) else {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    };
    let Some(end_ms) = parse_instant_ms(query.end.as_deref()) else {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    };

    let granularity = match query.granularity.as_deref() {
        None | Some("hour") => UsageGranularity::Hour,
        Some("day") => UsageGranularity::Day,
        Some(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };

    // Alignment is required, not silently repaired: the client must state the
    // exact bucket boundaries it expects so the response cannot be misread.
    let width = granularity.width_ms();
    if start_ms % width != 0 || end_ms % width != 0 {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }

    let usage_query = UsageQuery {
        scope: target_scope,
        meter_keys: meter_keys.clone(),
        start_ms,
        end_ms,
        granularity,
        resource_id: query.resource_id.clone(),
        evaluated_at_ms: SystemClock.now_unix_ms(),
    };
    if usage_query.validate().is_err() {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }

    let report = match reader.usage(&usage_query).await {
        Ok(report) => report,
        Err(error) => return metering_error(error, auth.0.request_id()),
    };

    let mut responses = Vec::with_capacity(meter_keys.len());
    for key in &meter_keys {
        let Some(definition) = o3k_kernel::meter_definition(key) else {
            return metering_error(MeteringError::Corrupt, auth.0.request_id());
        };
        let Some(found) = report.meters.iter().find(|usage| usage.meter_key == *key) else {
            return metering_error(MeteringError::Corrupt, auth.0.request_id());
        };
        match usage_response(&report, found, definition.aggregation) {
            Ok(response) => responses.push(response),
            Err(error) => return metering_error(error, auth.0.request_id()),
        }
    }
    Json(responses).into_response()
}

fn parse_instant_ms(value: Option<&str>) -> Option<i64> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|instant| instant.timestamp_millis())
}

fn usage_response(
    report: &MeterUsageReport,
    usage: &MeterUsage,
    aggregation: MeterAggregation,
) -> Result<MeterUsageResponse, MeteringError> {
    let mut buckets = Vec::with_capacity(usage.buckets.len());
    for bucket in &usage.buckets {
        buckets.push(MeterUsageBucketView {
            bucket_start: rfc3339_utc(bucket.bucket_start_ms).ok_or(MeteringError::Corrupt)?,
            bucket_width_ms: bucket.bucket_width_ms,
            quantity: bucket.quantity.clone(),
        });
    }
    Ok(MeterUsageResponse {
        scope: report.scope.clone(),
        meter_key: usage.meter_key.clone(),
        unit: usage.unit,
        aggregation,
        granularity: usage.granularity,
        start: rfc3339_utc(report.start_ms).ok_or(MeteringError::Corrupt)?,
        end: rfc3339_utc(report.end_ms).ok_or(MeteringError::Corrupt)?,
        observed_through: rfc3339_utc(report.observed_through_ms).ok_or(MeteringError::Corrupt)?,
        authority_started_at: optional_instant(report.authority_started_at_ms)?,
        last_observed_at: optional_instant(report.last_observed_at_ms)?,
        status: usage.status,
        buckets,
        total: usage.total.clone(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::{METER_CATALOG, UsageBucket};

    fn sample_usage() -> MeterUsage {
        MeterUsage {
            meter_key: "compute:instance_seconds".to_owned(),
            unit: MeterUnit::InstanceSecond,
            granularity: UsageGranularity::Hour,
            status: UsageStatus::Complete,
            buckets: vec![UsageBucket {
                bucket_start_ms: 1_699_999_200_000,
                bucket_width_ms: 3_600_000,
                quantity: "1.500".to_owned(),
            }],
            total: "1.500".to_owned(),
        }
    }

    fn sample_report() -> MeterUsageReport {
        MeterUsageReport {
            scope: "project-a".to_owned(),
            start_ms: 1_699_999_200_000,
            end_ms: 1_700_002_800_000,
            observed_through_ms: 1_700_002_800_000,
            authority_started_at_ms: Some(1_699_000_000_000),
            last_observed_at_ms: None,
            meters: vec![sample_usage()],
        }
    }

    #[test]
    fn definition_view_carries_no_secret_and_matches_the_catalog() {
        let definition = &METER_CATALOG[0];
        let value = serde_json::to_value(MeterDefinitionView::from_definition(definition)).unwrap();
        assert_eq!(value["key"], definition.key);
        assert_eq!(value["unit"], "instance_second");
        assert_eq!(value["aggregation"], "integral");
        assert_eq!(value["supported_granularities"][0], "hour");
        assert_eq!(value["version"], definition.version);
        let wire = serde_json::to_string(&value).unwrap();
        for secret in [
            "password",
            "secret",
            "token",
            "postgres://",
            "private_key",
            "price",
            "cost",
        ] {
            assert!(
                !wire.contains(secret),
                "{secret:?} leaked into a definition"
            );
        }
    }

    #[test]
    fn definitions_page_size_is_bounded_and_cursor_round_trips() {
        let default = DefinitionsQuery {
            limit: None,
            cursor: None,
        };
        assert_eq!(default.page_size().ok(), Some(DEFAULT_PAGE_SIZE));

        // SPEC-0046 §2.1: `limit` is 1–200. Zero is a client error, not a
        // silent fallback to the default.
        let zero = DefinitionsQuery {
            limit: Some(0),
            cursor: None,
        };
        assert!(zero.page_size().is_err());

        let min = DefinitionsQuery {
            limit: Some(1),
            cursor: None,
        };
        assert_eq!(min.page_size().ok(), Some(1));

        let max = DefinitionsQuery {
            limit: Some(MAX_PAGE_SIZE),
            cursor: None,
        };
        assert_eq!(max.page_size().ok(), Some(MAX_PAGE_SIZE));

        let over = DefinitionsQuery {
            limit: Some(MAX_PAGE_SIZE + 1),
            cursor: None,
        };
        assert!(over.page_size().is_err());

        let cursor = encode_cursor("compute:instance_seconds");
        let decoded = DefinitionsQuery {
            limit: None,
            cursor: Some(cursor),
        };
        assert_eq!(
            decoded.after_id().ok().flatten().as_deref(),
            Some("compute:instance_seconds")
        );

        let invalid = DefinitionsQuery {
            limit: None,
            cursor: Some("not base64!!".to_owned()),
        };
        assert!(invalid.after_id().is_err());
    }

    #[test]
    fn usage_response_renders_every_instant_as_rfc3339_utc() {
        let response = usage_response(
            &sample_report(),
            &sample_usage(),
            MeterAggregation::Integral,
        )
        .unwrap();
        let value = serde_json::to_value(&response).unwrap();
        for field in [
            "start",
            "end",
            "observed_through",
            "authority_started_at",
            "bucket_start",
        ] {
            let rendered = if field == "bucket_start" {
                value["buckets"][0]["bucket_start"].clone()
            } else if field == "authority_started_at" {
                value["authority_started_at"].clone()
            } else {
                value[field].clone()
            };
            let text = rendered.as_str().unwrap();
            assert!(text.ends_with('Z'), "{field} is not UTC: {text}");
            assert!(
                chrono::DateTime::parse_from_rfc3339(text).is_ok(),
                "{field} is not RFC3339: {text}"
            );
        }
        // A never-observed instant stays null rather than becoming 1970-01-01.
        assert!(value["last_observed_at"].is_null());
        assert_eq!(value["status"], "complete");
        assert_eq!(value["scope"], "project-a");
    }

    #[test]
    fn usage_statuses_are_never_ambiguous() {
        let mut statuses = Vec::new();
        for status in [
            UsageStatus::Complete,
            UsageStatus::Partial,
            UsageStatus::Unavailable,
        ] {
            statuses.push(status.as_str().to_owned());
        }
        assert_eq!(statuses, vec!["complete", "partial", "unavailable"]);
    }

    #[test]
    fn meter_usage_error_maps_to_stable_http_statuses() {
        assert_eq!(
            metering_error(MeteringError::Unavailable, "req").status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            metering_error(MeteringError::Corrupt, "req").status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            metering_error(MeteringError::BoundsExceeded, "req").status(),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn usage_query_params_decode_known_keys_and_reject_unknown_keys() {
        // Repeated `meter` keys are exercised end-to-end through axum's query
        // extractor in `crates/o3k-api/tests/native_metering_routes.rs`; JSON
        // cannot represent duplicate object keys, so this unit test covers the
        // scalar keys and unknown-key rejection of the same map visitor.
        let params: UsageQueryParams = serde_json::from_value(serde_json::json!({
            "meter": "compute:instance_seconds",
            "scope": "project-a",
            "start": "2023-11-14T22:00:00Z",
            "end": "2023-11-14T23:00:00Z",
            "granularity": "day",
            "resource_id": "server-1"
        }))
        .unwrap();
        assert_eq!(params.meter, vec!["compute:instance_seconds".to_owned()]);
        assert_eq!(params.scope.as_deref(), Some("project-a"));
        assert_eq!(params.granularity.as_deref(), Some("day"));
        assert_eq!(params.resource_id.as_deref(), Some("server-1"));

        let empty: UsageQueryParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(empty.meter.is_empty());
        assert!(empty.scope.is_none());

        // A typo such as `scopes` must not silently degrade to the caller's
        // own scope: it is a hard decode error (→ 400 at the HTTP boundary).
        let typo = serde_json::from_value::<UsageQueryParams>(serde_json::json!({
            "scopes": "project-b",
            "start": "2023-11-14T22:00:00Z",
            "end": "2023-11-14T23:00:00Z"
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn align_helpers_reject_unaligned_instants() {
        assert_eq!(
            parse_instant_ms(Some("2024-01-01T00:00:00Z")),
            Some(1_704_067_200_000)
        );
        assert!(parse_instant_ms(Some("not-a-time")).is_none());
        assert!(parse_instant_ms(None).is_none());
    }

    /// Builds a validator that checks an instance against one `definitions`
    /// entry of the native metering contract schema while carrying the full
    /// `definitions` table.
    fn definition_validator(definition: &str) -> jsonschema::Validator {
        let full: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-metering-v1.schema.json"
        )))
        .unwrap();
        let mut wrapper = serde_json::json!({ "$ref": format!("#/definitions/{definition}") });
        wrapper["definitions"] = full["definitions"].clone();
        jsonschema::validator_for(&wrapper).unwrap()
    }

    /// Validates a single serialized DTO against one `definitions` entry.
    fn validate_against_definition(payload: &serde_json::Value, definition: &str) {
        let validator = definition_validator(definition);
        assert!(
            validator.validate(payload).is_ok(),
            "payload does not match definition {definition:?}"
        );
    }

    /// Exact public key set of each DTO. A future field addition is the
    /// realistic leak vector, so it must fail these tests, not slip through.
    fn object_keys(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn usage_response_json_keys_are_exactly_the_contract() {
        let response = usage_response(
            &sample_report(),
            &sample_usage(),
            MeterAggregation::Integral,
        )
        .unwrap();
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            object_keys(&value),
            std::collections::BTreeSet::from([
                "scope".to_owned(),
                "meter_key".to_owned(),
                "unit".to_owned(),
                "aggregation".to_owned(),
                "granularity".to_owned(),
                "start".to_owned(),
                "end".to_owned(),
                "observed_through".to_owned(),
                "authority_started_at".to_owned(),
                "last_observed_at".to_owned(),
                "status".to_owned(),
                "buckets".to_owned(),
                "total".to_owned(),
            ])
        );
        assert_eq!(
            object_keys(&value["buckets"][0]),
            std::collections::BTreeSet::from([
                "bucket_start".to_owned(),
                "bucket_width_ms".to_owned(),
                "quantity".to_owned(),
            ])
        );
    }

    #[test]
    fn definition_json_keys_are_exactly_the_contract() {
        let definition = MeterDefinitionView::from_definition(&METER_CATALOG[0]);
        let value = serde_json::to_value(&definition).unwrap();
        assert_eq!(
            object_keys(&value),
            std::collections::BTreeSet::from([
                "key".to_owned(),
                "owning_service".to_owned(),
                "unit".to_owned(),
                "aggregation".to_owned(),
                "resource_type".to_owned(),
                "supported_granularities".to_owned(),
                "tenant_visible".to_owned(),
                "operator_visible".to_owned(),
                "description".to_owned(),
                "version".to_owned(),
            ])
        );

        let page = MeterDefinitionsPage {
            definitions: vec![definition],
            has_more: false,
            next_cursor: None,
        };
        assert_eq!(
            object_keys(&serde_json::to_value(&page).unwrap()),
            std::collections::BTreeSet::from([
                "definitions".to_owned(),
                "has_more".to_owned(),
                "next_cursor".to_owned(),
            ])
        );
    }

    #[test]
    fn malformed_instants_fail_the_metering_contract_schema() {
        let response = usage_response(
            &sample_report(),
            &sample_usage(),
            MeterAggregation::Integral,
        )
        .unwrap();
        let validator = definition_validator("meterUsage");

        for field in ["start", "end", "observed_through"] {
            let mut value = serde_json::to_value(&response).unwrap();
            value[field] = serde_json::json!("2023-11-14 22:00:00");
            assert!(
                validator.validate(&value).is_err(),
                "schema accepted a malformed {field}"
            );
        }

        // Nullable instants must accept null but reject a malformed non-null
        // value instead of treating it as a free-form string.
        let mut value = serde_json::to_value(&response).unwrap();
        value["authority_started_at"] = serde_json::json!("not-a-time");
        assert!(validator.validate(&value).is_err());
        let mut value = serde_json::to_value(&response).unwrap();
        value["last_observed_at"] = serde_json::Value::Null;
        assert!(validator.validate(&value).is_ok());

        let mut value = serde_json::to_value(&response).unwrap();
        value["buckets"][0]["bucket_start"] = serde_json::json!("2023-11-14");
        assert!(validator.validate(&value).is_err());
    }

    #[test]
    fn dtos_validate_against_the_metering_contract_schema() {
        let definition = MeterDefinitionView::from_definition(&METER_CATALOG[0]);
        validate_against_definition(
            &serde_json::to_value(&definition).unwrap(),
            "meterDefinition",
        );

        let page = MeterDefinitionsPage {
            definitions: vec![definition],
            has_more: true,
            next_cursor: Some(encode_cursor("compute:instance_seconds")),
        };
        validate_against_definition(
            &serde_json::to_value(&page).unwrap(),
            "meterDefinitionsPage",
        );

        let response = usage_response(
            &sample_report(),
            &sample_usage(),
            MeterAggregation::Integral,
        )
        .unwrap();
        validate_against_definition(&serde_json::to_value(&response).unwrap(), "meterUsage");
        validate_against_definition(
            &serde_json::to_value(&response.buckets[0]).unwrap(),
            "usageBucket",
        );

        // The top-level shape is the array-of-meter-usage response.
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-metering-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        validator
            .validate(&serde_json::to_value(vec![&response]).unwrap())
            .unwrap();
    }

    /// Fake authority that reports each completeness status and every reader
    /// error variant, so the DTO round-trip is exercised without a store.
    struct FakeReader;

    #[async_trait::async_trait]
    impl MeteringReader for FakeReader {
        async fn definitions(
            &self,
            _limit: usize,
            _after: Option<&str>,
        ) -> Result<MeterDefinitionsPage, MeteringError> {
            Ok(MeterDefinitionsPage {
                definitions: vec![MeterDefinitionView::from_definition(&METER_CATALOG[0])],
                has_more: false,
                next_cursor: None,
            })
        }

        async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, MeteringError> {
            let mut report = sample_report();
            report.scope = query.scope.clone();
            match query.scope.as_str() {
                "complete" => report.meters[0].status = UsageStatus::Complete,
                "partial" => report.meters[0].status = UsageStatus::Partial,
                "unavailable" => report.meters[0].status = UsageStatus::Unavailable,
                "corrupt" => return Err(MeteringError::Corrupt),
                "bounds" => return Err(MeteringError::BoundsExceeded),
                _ => return Err(MeteringError::Unavailable),
            }
            Ok(report)
        }
    }

    fn query_for_scope(scope: &str) -> UsageQuery {
        UsageQuery {
            scope: scope.to_owned(),
            meter_keys: vec!["compute:instance_seconds".to_owned()],
            start_ms: 1_699_999_200_000,
            end_ms: 1_700_002_800_000,
            granularity: UsageGranularity::Hour,
            resource_id: None,
            evaluated_at_ms: 1_700_002_800_000,
        }
    }

    #[tokio::test]
    async fn metering_reader_fake_reports_each_status() {
        let reader = FakeReader;
        for (scope, expected) in [
            ("complete", "complete"),
            ("partial", "partial"),
            ("unavailable", "unavailable"),
        ] {
            let report = reader.usage(&query_for_scope(scope)).await.unwrap();
            assert_eq!(report.meters[0].status.as_str(), expected);
            let response =
                usage_response(&report, &report.meters[0], MeterAggregation::Integral).unwrap();
            assert_eq!(serde_json::to_value(&response).unwrap()["status"], expected);
        }

        assert_eq!(
            reader.usage(&query_for_scope("corrupt")).await.unwrap_err(),
            MeteringError::Corrupt
        );
        assert_eq!(
            reader.usage(&query_for_scope("bounds")).await.unwrap_err(),
            MeteringError::BoundsExceeded
        );
        assert_eq!(
            reader.usage(&query_for_scope("other")).await.unwrap_err(),
            MeteringError::Unavailable
        );
    }

    #[tokio::test]
    async fn definitions_page_serializes_without_secrets() {
        let page = FakeReader.definitions(50, None).await.unwrap();
        let wire = serde_json::to_string(&page).unwrap();
        for secret in ["password", "secret", "token", "postgres://"] {
            assert!(!wire.contains(secret), "{secret:?} leaked into a page");
        }
    }
}
