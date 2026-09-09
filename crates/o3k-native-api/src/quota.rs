//! Authoritative native quota/usage projection backed by Cloud Kernel enforcement state.

use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthorizationDecision, AuthorizationRequest, LimitKey, LimitValue, OwnershipScope,
    ResourceTarget, ResourceType,
};
use serde::Serialize;

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};
use o3k_kernel::ScopeKind;

#[derive(Debug, Clone, Serialize)]
pub struct QuotaDimensionView {
    pub namespace: String,
    pub resource: String,
    pub limit: Option<u64>,
    pub unlimited: bool,
    pub in_use: u64,
    pub reserved: u64,
    pub total_consumed: u64,
}

#[async_trait::async_trait]
pub trait QuotaReader: Send + Sync {
    async fn read_scope(
        &self,
        scope: &OwnershipScope,
    ) -> Result<Vec<QuotaDimensionView>, NativeReadError>;
}

pub async fn list_quota(
    auth: BearerAuth,
    request_id: RequestId,
    State(state): State<NativeApiState>,
) -> Response {
    // Quota dimensions in v1 are tenant/project enforcement state.  Do not
    // reinterpret an operator or system scope as a project id: the durable
    // repository's project-owned usage queries would otherwise return a
    // misleading empty projection (and could reveal scope confusion).
    if auth.0.effective_scope().kind() != ScopeKind::Project {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "project scope required for quota visibility",
        )
        .with_request_id(request_id.0)
        .into_response();
    }
    let Some(authorizer) = state.authorizer.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "quota authorization is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: &auth.0,
        action: ActionId::new_unchecked("quota", "Read"),
        resource_target: ResourceTarget::collection(
            ResourceType::new_unchecked("quota", "dimension"),
            Some(auth.0.effective_scope().id().clone()),
        ),
    });
    if !matches!(decision, AuthorizationDecision::Allow) {
        return ProblemDetails::with_detail(ErrorCode::Forbidden, "quota authorization denied")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(reader) = state.quota_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "quota service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader.read_scope(auth.0.effective_scope()).await {
        Ok(items) => (axum::http::StatusCode::OK, Json(items)).into_response(),
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "quota visibility denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub fn view(key: &LimitKey, limit: LimitValue, usage: &o3k_kernel::Usage) -> QuotaDimensionView {
    let (limit, unlimited) = match limit {
        LimitValue::Unlimited => (None, true),
        LimitValue::Maximum(value) => (Some(value), false),
    };
    QuotaDimensionView {
        namespace: key.namespace().to_string(),
        resource: key.resource().to_owned(),
        limit,
        unlimited,
        in_use: usage.in_use,
        reserved: usage.reserved,
        total_consumed: usage.total_consumed(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod contract_tests {
    use super::*;

    #[test]
    fn quota_projection_matches_schema_and_contains_no_secrets() {
        let key = LimitKey::compute_servers();
        let scope =
            OwnershipScope::project(o3k_kernel::ScopeId::new_unchecked("project-a"), None, None);
        let usage = o3k_kernel::Usage::new(scope, key.clone(), 2, 1);
        let payload = serde_json::Value::Array(vec![
            serde_json::to_value(view(&key, LimitValue::Maximum(10), &usage)).unwrap(),
            serde_json::to_value(view(&key, LimitValue::Unlimited, &usage)).unwrap(),
        ]);
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-quota-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
        assert_eq!(payload[0]["total_consumed"], 3);
        assert_eq!(payload[1]["unlimited"], true);
        assert!(!payload[0].as_object().unwrap().contains_key("password"));
    }
}
