//! Versioned native quota projection over the canonical Cloud Kernel quota port.

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, LimitKey, LimitValue,
    OwnershipScope, ResourceTarget, ResourceType, ScopeId,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct QuotaDimension {
    pub key: String,
    pub namespace: String,
    pub unit: String,
    pub scope: String,
    pub limit: LimitValue,
    pub usage: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaError {
    Invalid,
    StaleGeneration,
    NotFound,
    Forbidden,
    Unavailable,
    Corrupt,
    AuditUnavailable,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitRequest {
    pub limit: LimitValue,
    pub expected_generation: Option<u64>,
}

#[async_trait::async_trait]
pub trait QuotaReader: Send + Sync {
    async fn list(&self, scope: &OwnershipScope) -> Result<Vec<QuotaDimension>, QuotaError>;
    async fn set(
        &self,
        auth: &AuthContext,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
        expected_generation: Option<u64>,
    ) -> Result<QuotaDimension, QuotaError>;
    async fn clear(
        &self,
        auth: &AuthContext,
        scope: &OwnershipScope,
        key: &LimitKey,
        expected_generation: Option<u64>,
    ) -> Result<QuotaDimension, QuotaError>;
}

fn quota_error(error: QuotaError, request_id: &str) -> Response {
    let code = match error {
        QuotaError::Invalid => ErrorCode::BadRequest,
        QuotaError::StaleGeneration => ErrorCode::Conflict,
        QuotaError::NotFound => ErrorCode::ResourceNotFound,
        QuotaError::Forbidden => ErrorCode::Forbidden,
        QuotaError::Unavailable | QuotaError::AuditUnavailable => ErrorCode::NotAvailable,
        QuotaError::Corrupt => ErrorCode::InternalError,
    };
    ProblemDetails::new(code)
        .with_request_id(request_id.to_owned())
        .into_response()
}

fn authorize(
    state: &NativeApiState,
    auth: &AuthContext,
    action: ActionId,
    scope: &OwnershipScope,
) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("quota", "quota"),
                Some(scope.id().clone())
            ),
        }),
        AuthorizationDecision::Allow
    )
}

pub async fn list(auth: BearerAuth, State(state): State<NativeApiState>) -> Response {
    let Some(reader) = state.quota_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(
        &state,
        &auth.0,
        ActionId::new_unchecked("quota", "ReadQuota"),
        auth.0.effective_scope(),
    ) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    match reader.list(auth.0.effective_scope()).await {
        Ok(items) => {
            Json(serde_json::json!({"version":"v1","scope":auth.0.effective_scope(),"items":items}))
                .into_response()
        }
        Err(error) => quota_error(error, "quota-list"),
    }
}

pub async fn show(
    auth: BearerAuth,
    Path((namespace, dimension)): Path<(String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.quota_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if !authorize(
        &state,
        &auth.0,
        ActionId::new_unchecked("quota", "ReadQuota"),
        auth.0.effective_scope(),
    ) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let key = match LimitKey::new(&namespace, &dimension) {
        Ok(k) => k,
        Err(_) => return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response(),
    };
    match reader.list(auth.0.effective_scope()).await {
        Ok(items) => items
            .into_iter()
            .find(|item| item.namespace == key.namespace().as_str() && item.key == key.resource())
            .map(|item| Json(item).into_response())
            .unwrap_or_else(|| ProblemDetails::new(ErrorCode::ResourceNotFound).into_response()),
        Err(error) => quota_error(error, "quota-show"),
    }
}

pub async fn operator_list(
    auth: BearerAuth,
    Path(project): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(reader) = state.quota_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let scope = match ScopeId::new(project) {
        Ok(id) => OwnershipScope::project(id, None, None),
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    if !authorize(
        &state,
        &auth.0,
        ActionId::new_unchecked("quota", "ReadQuota"),
        &scope,
    ) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    match reader.list(&scope).await {
        Ok(items) => {
            Json(serde_json::json!({"version":"v1","scope":scope,"items":items})).into_response()
        }
        Err(error) => quota_error(error, "quota-operator-list"),
    }
}

pub async fn set(
    auth: BearerAuth,
    Path((project, namespace, dimension)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
    Json(body): Json<LimitRequest>,
) -> Response {
    mutate(auth, project, namespace, dimension, state, body, false).await
}

pub async fn clear(
    auth: BearerAuth,
    Path((project, namespace, dimension)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
    Json(body): Json<LimitRequest>,
) -> Response {
    mutate(
        auth,
        project,
        namespace,
        dimension,
        state,
        LimitRequest {
            limit: LimitValue::Unlimited,
            expected_generation: body.expected_generation,
        },
        true,
    )
    .await
}

async fn mutate(
    auth: BearerAuth,
    project: String,
    namespace: String,
    dimension: String,
    state: NativeApiState,
    body: LimitRequest,
    _clear: bool,
) -> Response {
    let Some(reader) = state.quota_reader.as_ref() else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let scope = match ScopeId::new(project) {
        Ok(id) => OwnershipScope::project(id, None, None),
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    if !authorize(
        &state,
        &auth.0,
        ActionId::new_unchecked("quota", "ManageQuota"),
        &scope,
    ) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let key = match LimitKey::new(&namespace, &dimension) {
        Ok(k) => k,
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    if body.expected_generation.is_none() {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    match reader
        .set(&auth.0, &scope, &key, body.limit, body.expected_generation)
        .await
    {
        Ok(item) => (axum::http::StatusCode::OK, Json(item)).into_response(),
        Err(error) => quota_error(error, "quota-mutation"),
    }
}
