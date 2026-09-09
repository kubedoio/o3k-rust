use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthorizationDecision, AuthorizationRequest, ResourceTarget, ResourceType, ScopeKind,
};
use serde::{Deserialize, Serialize};

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

#[derive(Debug, Clone, Serialize)]
pub struct MeterView {
    pub name: String,
    pub unit: &'static str,
    pub value: u64,
    pub complete: bool,
    pub as_of: String,
    pub authority: &'static str,
}

/// A machine-readable declaration of a meter O3K can actually produce for
/// the active deployment profile. Definitions describe semantics only; they
/// do not contain UI/layout or pricing metadata.
#[derive(Debug, Clone, Serialize)]
pub struct MeterDefinitionView {
    pub id: String,
    pub owning_service: String,
    pub unit: &'static str,
    pub aggregation: &'static str,
    pub applicability: &'static str,
    pub supported_granularities: Vec<&'static str>,
    pub tenant_visible: bool,
    pub status: &'static str,
}

/// A bounded historical aggregate over durable O3K metering events.  This is
/// deliberately an aggregate, not a raw event feed: callers cannot make the
/// service materialize an unbounded event collection.
#[derive(Debug, Clone, Serialize)]
pub struct MeterUsageView {
    pub meter_id: String,
    pub unit: String,
    pub total_quantity: u64,
    pub event_count: u64,
    pub effective_from: String,
    pub effective_to: String,
    pub complete: bool,
    pub authority: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterUsageQuery {
    pub meter_id: String,
    pub effective_from: String,
    pub effective_to: String,
    pub limit: Option<usize>,
}

fn valid_timestamp(value: &str) -> bool {
    // Keep the native crate free of a second date dependency while requiring
    // the canonical UTC form accepted by the metering store.
    value.len() >= 20
        && value.len() <= 64
        && value.ends_with('Z')
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || b"-:TZ.+".contains(&byte))
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

#[async_trait::async_trait]
pub trait MeterReader: Send + Sync {
    async fn list_definitions(&self) -> Result<Vec<MeterDefinitionView>, NativeReadError>;
    async fn read_project(&self, project_id: &str) -> Result<Vec<MeterView>, NativeReadError>;
    async fn read_usage(
        &self,
        project_id: &str,
        meter_id: &str,
        effective_from: &str,
        effective_to: &str,
        limit: usize,
    ) -> Result<MeterUsageView, NativeReadError>;
}

#[allow(clippy::result_large_err)]
fn authorize_metering(
    state: &NativeApiState,
    auth: &BearerAuth,
    request_id: &str,
) -> Result<(), Response> {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return Err(ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "metering authorization is not configured",
        )
        .with_request_id(request_id.to_owned())
        .into_response());
    };
    if auth.0.effective_scope().kind() != ScopeKind::Project {
        return Err(
            ProblemDetails::with_detail(ErrorCode::Forbidden, "project scope required")
                .with_request_id(request_id.to_owned())
                .into_response(),
        );
    }
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        action: ActionId::new_unchecked("metering", "Read"),
        resource_target: ResourceTarget::collection(
            ResourceType::new_unchecked("metering", "meter"),
            Some(auth.0.effective_scope().id().clone()),
        ),
    });
    if !matches!(decision, AuthorizationDecision::Allow) {
        return Err(ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "metering authorization denied",
        )
        .with_request_id(request_id.to_owned())
        .into_response());
    }
    Ok(())
}

/// Returns only meters backed by canonical O3K lifecycle authority. Provider
/// telemetry, pricing and historical estimates are intentionally unadvertised.
pub async fn list_definitions(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize_metering(&state, &auth, &request_id.0) {
        return response;
    }
    let Some(reader) = state.meter_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "metering service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader.list_definitions().await {
        Ok(definitions) => (axum::http::StatusCode::OK, Json(definitions)).into_response(),
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "meter visibility denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn list_meters(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize_metering(&state, &auth, &request_id.0) {
        return response;
    }
    let Some(reader) = state.meter_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "metering service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader
        .read_project(auth.0.effective_scope().id().as_str())
        .await
    {
        Ok(meters) => (axum::http::StatusCode::OK, Json(meters)).into_response(),
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "meter visibility denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn usage(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<MeterUsageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize_metering(&state, &auth, &request_id.0) {
        return response;
    }
    let limit = query.limit.unwrap_or(10_000);
    if !(1..=10_000).contains(&limit)
        || query.meter_id.is_empty()
        || query.meter_id.len() > 128
        || !valid_timestamp(&query.effective_from)
        || !valid_timestamp(&query.effective_to)
        || query.effective_from >= query.effective_to
    {
        return ProblemDetails::with_detail(
            ErrorCode::BadRequest,
            "invalid bounded metering interval or limit",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    let Some(reader) = state.meter_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "metering service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader
        .read_usage(
            auth.0.effective_scope().id().as_str(),
            &query.meter_id,
            &query.effective_from,
            &query.effective_to,
            limit,
        )
        .await
    {
        Ok(value) => (axum::http::StatusCode::OK, Json(value)).into_response(),
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "meter visibility denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod contract_tests {
    use super::*;

    #[test]
    fn metering_projection_matches_versioned_schema_and_contains_no_secrets() {
        let payload = serde_json::to_value(vec![MeterView {
            name: "compute:server_count".into(),
            unit: "count",
            value: 2,
            complete: true,
            as_of: "2026-01-01T00:00:00Z".into(),
            authority: "o3k-resource-lifecycle",
        }])
        .unwrap();
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-metering-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
        assert!(!payload[0].as_object().unwrap().contains_key("password"));
        assert!(!payload[0].as_object().unwrap().contains_key("token"));
    }

    #[test]
    fn meter_definition_is_machine_safe_and_declares_snapshot_semantics() {
        let value = serde_json::to_value(MeterDefinitionView {
            id: "compute:server_count".into(),
            owning_service: "compute".into(),
            unit: "count",
            aggregation: "gauge",
            applicability: "project",
            supported_granularities: vec!["instant"],
            tenant_visible: true,
            status: "available",
        })
        .unwrap();
        assert_eq!(value["aggregation"], "gauge");
        assert_eq!(value["supported_granularities"][0], "instant");
        assert!(!value.as_object().unwrap().contains_key("price"));
        assert!(!value.as_object().unwrap().contains_key("provider_endpoint"));

        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-meter-definition-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&value).is_ok());
        let lifecycle = serde_json::json!({
            "id": "compute:server:lifecycle_created",
            "owning_service": "compute",
            "unit": "event",
            "aggregation": "sum",
            "applicability": "project",
            "supported_granularities": ["bounded_interval"],
            "tenant_visible": true,
            "status": "available"
        });
        assert!(validator.validate(&lifecycle).is_ok());
    }

    #[test]
    fn historical_usage_projection_is_secret_safe() {
        let value = serde_json::to_value(MeterUsageView {
            meter_id: "compute:server_count".into(),
            unit: "count".into(),
            total_quantity: 3,
            event_count: 2,
            effective_from: "2026-01-01T00:00:00Z".into(),
            effective_to: "2026-01-02T00:00:00Z".into(),
            complete: true,
            authority: "o3k-metering-events",
        })
        .unwrap();
        assert!(!value.as_object().unwrap().contains_key("provider_id"));
        assert!(!value.as_object().unwrap().contains_key("price"));
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-meter-usage-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&value).is_ok());
    }

    #[test]
    fn timestamp_validation_rejects_malformed_or_oversized_values() {
        assert!(valid_timestamp("2026-01-01T00:00:00Z"));
        assert!(!valid_timestamp("2026-01-01T00:00:00"));
        assert!(!valid_timestamp("2026-99-99T99:99:99Z"));
        assert!(!valid_timestamp("not-a-timestamp"));
        assert!(!valid_timestamp(&format!(
            "2026-01-01T00:00:00{}Z",
            ".1".repeat(40)
        )));
    }
}
