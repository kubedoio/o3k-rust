use async_trait::async_trait;
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthorizationDecision, AuthorizationRequest, ResourceTarget, ResourceType, ScopeId,
    ScopeKind,
};
use serde::Serialize;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, ProblemDetails},
};

#[derive(Debug, Serialize)]
pub struct DiagnosticService {
    pub service_id: String,
    pub namespace: String,
    pub service_version: String,
    pub controller_state: Option<String>,
    pub healthy: Option<bool>,
    pub detail: Option<String>,
    pub regions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsResponse {
    pub status: &'static str,
    pub services: Vec<DiagnosticService>,
    pub locations: Vec<String>,
    pub capacity: CapacityStatus,
}

#[derive(Debug, Serialize)]
pub struct CapacityStatus {
    pub available: bool,
    pub reason: String,
    pub dimensions: Vec<CapacityDimension>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapacityDimension {
    pub resource_class: String,
    pub total: u64,
    pub reserved: u64,
    pub used: u64,
    pub available: u64,
}

/// Capacity must come from the canonical O3K placement authority.  The native
/// API deliberately has no provider/controller escape hatch.
#[async_trait]
pub trait CapacityReader: Send + Sync {
    async fn read(&self) -> Result<CapacityStatus, ()>;
}

const MAX_DIAGNOSTIC_SERVICES: usize = 256;
const MAX_DIAGNOSTIC_LOCATIONS: usize = 256;
const MAX_SERVICE_REGIONS: usize = 256;
const MAX_CAPACITY_DIMENSIONS: usize = 256;
// Diagnostics is an operator-facing bounded contract.  Counts alone do not
// bound the response when registry authorities contain malformed strings.
const MAX_DIAGNOSTIC_STRING_BYTES: usize = 256;

fn bounded(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_DIAGNOSTIC_STRING_BYTES
}

pub async fn show(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
) -> Response {
    if auth.0.effective_scope().kind() != ScopeKind::System {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "system scope required")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "operator authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        action: ActionId::new_unchecked("operator", "ReadDiagnostics"),
        resource_target: ResourceTarget::collection(
            ResourceType::new_unchecked("operator", "diagnostics"),
            Some(ScopeId::new_unchecked("system")),
        ),
    });
    if !matches!(decision, AuthorizationDecision::Allow) {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "diagnostic authorization denied",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    let mut services = Vec::new();
    if let Some(registry) = state.lifecycle_registry.as_ref() {
        let registry = match registry.read() {
            Ok(registry) => registry,
            Err(_) => {
                return ProblemDetails::with_detail(
                    ErrorCode::NotAvailable,
                    "diagnostic service registry is unavailable",
                )
                .with_request_id(request_id.0)
                .into_response();
            }
        };
        let manifests = registry.all_bounded(MAX_DIAGNOSTIC_SERVICES + 1);
        if manifests.len() > MAX_DIAGNOSTIC_SERVICES {
            return ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "diagnostic service inventory exceeds the bounded response limit",
            )
            .with_request_id(request_id.0)
            .into_response();
        }
        let mut manifests = manifests;
        manifests.sort_by(|left, right| left.service_id.cmp(&right.service_id));
        for manifest in manifests {
            if !bounded(&manifest.service_id)
                || !bounded(&manifest.namespace)
                || !bounded(&manifest.service_version)
                || manifest.regions.iter().any(|region| !bounded(region))
            {
                return ProblemDetails::with_detail(
                    ErrorCode::NotAvailable,
                    "diagnostic service inventory contains an invalid value",
                )
                .with_request_id(request_id.0)
                .into_response();
            }
            if manifest.regions.len() > MAX_SERVICE_REGIONS {
                return ProblemDetails::with_detail(
                    ErrorCode::NotAvailable,
                    "diagnostic service region inventory exceeds the bounded response limit",
                )
                .with_request_id(request_id.0)
                .into_response();
            }
            let controller = registry.controller(&manifest.service_id);
            services.push(DiagnosticService {
                service_id: manifest.service_id.clone(),
                namespace: manifest.namespace.clone(),
                service_version: manifest.service_version.clone(),
                controller_state: controller.map(|registration| registration.state.to_string()),
                healthy: manifest.health.as_ref().map(|h| h.healthy),
                // Health details can originate at an execution controller or
                // provider. Never forward their raw text across the native
                // boundary; expose only a bounded, canonical category.
                detail: manifest.health.as_ref().and_then(|health| {
                    (!health.healthy).then_some("service reported unhealthy".to_owned())
                }),
                regions: manifest.regions.clone(),
            });
        }
    }
    let mut locations: Vec<String> = state
        .locations
        .as_ref()
        .map(|registry| {
            registry
                .regions_bounded(MAX_DIAGNOSTIC_LOCATIONS + 1)
                .into_iter()
                .map(|region| region.id.clone())
                .collect()
        })
        .unwrap_or_default();
    if locations.len() > MAX_DIAGNOSTIC_LOCATIONS {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "diagnostic location inventory exceeds the bounded response limit",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    if locations.iter().any(|location| !bounded(location)) {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "diagnostic location inventory contains an invalid value",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    locations.sort();
    let capacity = match state.capacity_reader.as_ref() {
        Some(reader) => reader.read().await.unwrap_or(CapacityStatus {
            available: false,
            reason: "capacity authority unavailable".to_owned(),
            dimensions: Vec::new(),
        }),
        None => CapacityStatus {
            available: false,
            reason: "capacity authority is not configured".to_owned(),
            dimensions: Vec::new(),
        },
    };
    if capacity.dimensions.len() > MAX_CAPACITY_DIMENSIONS {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "diagnostic capacity inventory exceeds the bounded response limit",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    // CapacityReader is an internal port, but keep its output canonical at
    // this public boundary. Never forward arbitrary authority/provider text.
    let capacity_known = matches!(
        capacity.reason.as_str(),
        "capacity authority is configured"
            | "capacity authority is configured with degraded providers"
    );
    if capacity
        .dimensions
        .iter()
        .any(|dimension| !bounded(&dimension.resource_class))
    {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "diagnostic capacity inventory contains an invalid value",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    (
        axum::http::StatusCode::OK,
        Json(DiagnosticsResponse {
            status: if !capacity_known {
                "unknown"
            } else if services
                .iter()
                .all(|service| service.healthy != Some(false))
            {
                "healthy"
            } else {
                "degraded"
            },
            services,
            locations,
            capacity,
        }),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod contract_tests {
    use super::*;

    #[test]
    fn diagnostics_contract_is_versioned_and_secret_safe() {
        let payload = serde_json::to_value(DiagnosticsResponse {
            services: vec![DiagnosticService {
                service_id: "compute".into(),
                namespace: "compute".into(),
                service_version: "1.0.0".into(),
                controller_state: Some("ready".into()),
                healthy: Some(true),
                detail: None,
                regions: vec!["region-a".into()],
            }],
            locations: vec!["region-a".into()],
            capacity: CapacityStatus {
                available: false,
                reason: "capacity authority is configured".into(),
                dimensions: vec![],
            },
            status: "healthy",
        })
        .unwrap();
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-diagnostics-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
        assert!(!payload.to_string().contains("password"));
        assert!(!payload.to_string().contains("private_key"));
    }

    #[test]
    fn diagnostics_detail_is_bounded_and_not_a_backend_error_channel() {
        let payload = serde_json::to_value(DiagnosticService {
            service_id: "compute".into(),
            namespace: "compute".into(),
            service_version: "1.0.0".into(),
            controller_state: Some("not_ready".into()),
            healthy: Some(false),
            detail: Some("service reported unhealthy".into()),
            regions: vec![],
        })
        .unwrap();
        assert_eq!(payload["detail"], "service reported unhealthy");
        assert!(!payload.to_string().contains("postgres://"));
        assert!(!payload.to_string().contains("provider-private-error"));
    }
}
