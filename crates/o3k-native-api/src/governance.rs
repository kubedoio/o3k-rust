//! Canonical, system-authorized native IAM governance reads.

use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, ResourceTarget,
    ResourceType, ScopeId, ScopeKind,
};
use serde::{Deserialize, Serialize};

use crate::{
    NativeApiState,
    auth::{BearerAuth, RequestId},
    error::{ErrorCode, NativeReadError, ProblemDetails},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAssignmentCreateRequest {
    pub principal_id: String,
    pub project_id: String,
    pub role_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleAssignmentMutationResponse {
    pub assignment: RoleAssignmentView,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceMutationError {
    Validation,
    Conflict,
    Unavailable,
    Internal,
}

#[async_trait::async_trait]
pub trait GovernanceMutator: Send + Sync {
    async fn create_role_assignment(
        &self,
        auth: &AuthContext,
        request: RoleAssignmentCreateRequest,
        idempotency_key: &str,
    ) -> Result<RoleAssignmentMutationResponse, GovernanceMutationError>;
    async fn delete_role_assignment(
        &self,
        auth: &AuthContext,
        id: &str,
        idempotency_key: &str,
    ) -> Result<RoleAssignmentMutationResponse, GovernanceMutationError>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrincipalView {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleAssignmentView {
    pub id: String,
    pub principal_id: String,
    pub project_id: String,
    pub role_id: String,
    pub created_at: String,
}

#[async_trait::async_trait]
pub trait GovernanceReader: Send + Sync {
    async fn show_project(
        &self,
        auth: &AuthContext,
        id: &str,
    ) -> Result<ProjectView, NativeReadError>;
    async fn list_projects_page(
        &self,
        auth: &AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectView>, NativeReadError>;
    async fn list_principals_page(
        &self,
        auth: &AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PrincipalView>, NativeReadError>;
    async fn show_principal(
        &self,
        auth: &AuthContext,
        id: &str,
    ) -> Result<PrincipalView, NativeReadError>;
    async fn list_role_assignments_page(
        &self,
        auth: &AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RoleAssignmentView>, NativeReadError>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectListResponse {
    pub items: Vec<ProjectView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PrincipalListResponse {
    pub items: Vec<PrincipalView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RoleAssignmentListResponse {
    pub items: Vec<RoleAssignmentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[allow(clippy::result_large_err)]
fn authorize(
    state: &NativeApiState,
    auth: &AuthContext,
    action_name: &str,
    resource_type: &str,
) -> Result<(), Response> {
    if auth.effective_scope().kind() != ScopeKind::System {
        return Err(
            ProblemDetails::with_detail(ErrorCode::Forbidden, "system scope required")
                .into_response(),
        );
    }
    let Some(authorizer) = state.authorizer.as_ref() else {
        return Err(ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "governance authorization is not configured",
        )
        .into_response());
    };
    let decision = authorizer.authorize(&AuthorizationRequest {
        auth_context: auth,
        action: ActionId::new_unchecked("operator", action_name),
        resource_target: ResourceTarget::collection(
            ResourceType::new_unchecked("iam", resource_type),
            Some(ScopeId::new_unchecked("system")),
        ),
    });
    match decision {
        AuthorizationDecision::Allow => Ok(()),
        AuthorizationDecision::Deny { .. } => Err(ProblemDetails::with_detail(
            ErrorCode::Forbidden,
            "governance authorization denied",
        )
        .into_response()),
    }
}

pub async fn list_projects(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "ListProjects", "project") {
        return response;
    }
    let Some(reader) = state.governance_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let scope = auth.0.effective_scope().id().as_str().to_owned();
    let after = match query.cursor {
        Some(cursor) => match state
            .cursor_config
            .decode_cursor(&cursor, &scope, "iam:project", "")
        {
            Ok(payload) => Some(payload.last_id),
            Err(_) => {
                return ProblemDetails::bad_request("invalid project cursor")
                    .with_request_id(request_id.0)
                    .into_response();
            }
        },
        None => None,
    };
    match reader
        .list_projects_page(&auth.0, after.as_deref(), limit + 1)
        .await
    {
        Ok(mut items) => {
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|item| {
                    state
                        .cursor_config
                        .encode_cursor(&crate::pagination::CursorPayload {
                            last_id: item.id.clone(),
                            scope_id: scope.clone(),
                            resource_type: "iam:project".into(),
                            query_hash: String::new(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(ProjectListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "governance authorization denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn show_project(
    auth: BearerAuth,
    request_id: RequestId,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "ReadProject", "project") {
        return response;
    }
    let Some(reader) = state.governance_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader.show_project(&auth.0, &id).await {
        Ok(project) => (axum::http::StatusCode::OK, Json(project)).into_response(),
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(Some(&id))
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn list_principals(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "ListPrincipals", "principal") {
        return response;
    }
    let Some(reader) = state.governance_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let scope = auth.0.effective_scope().id().as_str().to_owned();
    let after = match query.cursor {
        Some(cursor) => {
            match state
                .cursor_config
                .decode_cursor(&cursor, &scope, "iam:principal", "")
            {
                Ok(payload) => Some(payload.last_id),
                Err(_) => {
                    return ProblemDetails::bad_request("invalid principal cursor")
                        .with_request_id(request_id.0)
                        .into_response();
                }
            }
        }
        None => None,
    };
    match reader
        .list_principals_page(&auth.0, after.as_deref(), limit + 1)
        .await
    {
        Ok(mut items) => {
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|item| {
                    state
                        .cursor_config
                        .encode_cursor(&crate::pagination::CursorPayload {
                            last_id: item.id.clone(),
                            scope_id: scope.clone(),
                            resource_type: "iam:principal".into(),
                            query_hash: String::new(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(PrincipalListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "governance authorization denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn show_principal(
    auth: BearerAuth,
    request_id: RequestId,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "ReadPrincipal", "principal") {
        return response;
    }
    let Some(reader) = state.governance_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match reader.show_principal(&auth.0, &id).await {
        Ok(principal) => (axum::http::StatusCode::OK, Json(principal)).into_response(),
        Err(NativeReadError::NotFound | NativeReadError::Forbidden) => {
            ProblemDetails::not_found(Some(&id))
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn list_role_assignments(
    auth: BearerAuth,
    request_id: RequestId,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "ListRoleAssignments", "role_assignment") {
        return response;
    }
    let Some(reader) = state.governance_reader.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    let limit = crate::pagination::parse_page_size(query.limit.as_deref());
    let scope = auth.0.effective_scope().id().as_str().to_owned();
    let resource_type = "iam:role_assignment";
    let after = match query.cursor {
        Some(cursor) => match state
            .cursor_config
            .decode_cursor(&cursor, &scope, resource_type, "")
        {
            Ok(payload) => Some(payload.last_id),
            Err(_) => {
                return ProblemDetails::bad_request("invalid role-assignment cursor")
                    .with_request_id(request_id.0)
                    .into_response();
            }
        },
        None => None,
    };
    match reader
        .list_role_assignments_page(&auth.0, after.as_deref(), limit + 1)
        .await
    {
        Ok(mut items) => {
            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                items.last().map(|item| {
                    state
                        .cursor_config
                        .encode_cursor(&crate::pagination::CursorPayload {
                            last_id: item.id.clone(),
                            scope_id: scope.clone(),
                            resource_type: resource_type.into(),
                            query_hash: String::new(),
                            version: 1,
                        })
                })
            } else {
                None
            };
            (
                axum::http::StatusCode::OK,
                Json(RoleAssignmentListResponse { items, next_cursor }),
            )
                .into_response()
        }
        Err(NativeReadError::Forbidden | NativeReadError::NotFound) => {
            ProblemDetails::with_detail(ErrorCode::Forbidden, "governance authorization denied")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(NativeReadError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn create_role_assignment(
    auth: BearerAuth,
    request_id: RequestId,
    headers: axum::http::HeaderMap,
    State(state): State<NativeApiState>,
    Json(request): Json<RoleAssignmentCreateRequest>,
) -> Response {
    // Role assignment governance has one canonical mutation action.  Keep
    // create and delete behind `operator:AssignRole`; using operation-shaped
    // action names here would be denied by the kernel policy registry and
    // would make the advertised native mutation unusable.
    if let Err(response) = authorize(&state, &auth.0, "AssignRole", "role_assignment") {
        return response;
    }
    let key = match headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        Some(value) if !value.is_empty() && value.len() <= 128 => value,
        _ => {
            return ProblemDetails::bad_request(
                "idempotency-key is required and must be 1..128 bytes",
            )
            .with_request_id(request_id.0)
            .into_response();
        }
    };
    if request.principal_id.trim().is_empty()
        || request.project_id.trim().is_empty()
        || request.role_id.trim().is_empty()
        || request.principal_id.len() > 256
        || request.project_id.len() > 256
        || request.role_id.len() > 256
    {
        return ProblemDetails::bad_request("invalid role-assignment identifiers")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(mutator) = state.governance_mutator.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match mutator.create_role_assignment(&auth.0, request, key).await {
        Ok(value) => (axum::http::StatusCode::ACCEPTED, Json(value)).into_response(),
        Err(GovernanceMutationError::Validation) => {
            ProblemDetails::bad_request("invalid role-assignment request")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(GovernanceMutationError::Conflict) => ProblemDetails::with_detail(
            ErrorCode::Conflict,
            "idempotency key conflicts with an existing request",
        )
        .with_request_id(request_id.0)
        .into_response(),
        Err(GovernanceMutationError::Unavailable) => ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is unavailable",
        )
        .with_request_id(request_id.0)
        .into_response(),
        Err(GovernanceMutationError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

pub async fn delete_role_assignment(
    auth: BearerAuth,
    request_id: RequestId,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if let Err(response) = authorize(&state, &auth.0, "AssignRole", "role_assignment") {
        return response;
    }
    let key = match headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        Some(v) if !v.is_empty() && v.len() <= 128 => v,
        _ => {
            return ProblemDetails::bad_request(
                "idempotency-key is required and must be 1..128 bytes",
            )
            .with_request_id(request_id.0)
            .into_response();
        }
    };
    if id.trim().is_empty() || id.len() > 256 {
        return ProblemDetails::bad_request("invalid role-assignment identifier")
            .with_request_id(request_id.0)
            .into_response();
    }
    let Some(mutator) = state.governance_mutator.as_ref() else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is not configured",
        )
        .with_request_id(request_id.0)
        .into_response();
    };
    match mutator.delete_role_assignment(&auth.0, &id, key).await {
        Ok(v) => (axum::http::StatusCode::ACCEPTED, Json(v)).into_response(),
        Err(GovernanceMutationError::Validation) => {
            ProblemDetails::bad_request("invalid role-assignment request")
                .with_request_id(request_id.0)
                .into_response()
        }
        Err(GovernanceMutationError::Conflict) => ProblemDetails::with_detail(
            ErrorCode::Conflict,
            "idempotency key conflicts with an existing request",
        )
        .with_request_id(request_id.0)
        .into_response(),
        Err(GovernanceMutationError::Unavailable) => ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "governance service is unavailable",
        )
        .with_request_id(request_id.0)
        .into_response(),
        Err(GovernanceMutationError::Internal) => ProblemDetails::internal()
            .with_request_id(request_id.0)
            .into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod contract_tests {
    use super::{
        PrincipalListResponse, PrincipalView, ProjectListResponse, ProjectView,
        RoleAssignmentCreateRequest, RoleAssignmentListResponse, RoleAssignmentView,
    };
    #[test]
    fn governance_project_contract_is_versioned_and_secret_safe() {
        let payload = serde_json::json!({
            "items": [{
                "id": "project-a",
                "domain_id": "domain-a",
                "name": "Project A",
                "description": null,
                "enabled": true,
                "created_at": "2026-01-01T00:00:00Z"
            }]
        });
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-governance-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
        assert!(!payload.to_string().contains("password"));
        assert!(!payload.to_string().contains("token"));
    }

    #[test]
    fn governance_all_list_variants_match_the_published_contract() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-governance-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let project = ProjectView {
            id: "project-a".into(),
            domain_id: "domain-a".into(),
            name: "Project A".into(),
            description: None,
            enabled: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let principal = PrincipalView {
            id: "principal-a".into(),
            domain_id: "domain-a".into(),
            name: "Principal A".into(),
            email: None,
            enabled: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let assignment = RoleAssignmentView {
            id: "assignment-a".into(),
            principal_id: "principal-a".into(),
            project_id: "project-a".into(),
            role_id: "member".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        for value in [
            serde_json::to_value(ProjectListResponse {
                items: vec![project.clone()],
                next_cursor: Some("cursor-a".into()),
            })
            .unwrap(),
            serde_json::to_value(PrincipalListResponse {
                items: vec![principal],
                next_cursor: None,
            })
            .unwrap(),
            serde_json::to_value(RoleAssignmentListResponse {
                items: vec![assignment],
                next_cursor: Some("cursor-b".into()),
            })
            .unwrap(),
        ] {
            assert!(
                validator.validate(&value).is_ok(),
                "governance drift: {value}"
            );
            assert!(!value.to_string().contains("password"));
            assert!(!value.to_string().contains("token"));
        }
    }

    #[test]
    fn role_assignment_mutation_contract_is_versioned_and_secret_safe() {
        let payload = serde_json::json!({
            "assignment": {
                "id": "00000000-0000-0000-0000-000000000001",
                "principal_id": "principal-a",
                "project_id": "project-a",
                "role_id": "member",
                "created_at": "2026-01-01T00:00:00Z"
            },
            "operation_id": "00000000-0000-0000-0000-000000000002"
        });
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-governance-mutation-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
        assert!(!payload.to_string().contains("password"));
        assert!(!payload.to_string().contains("token"));
        assert!(!payload.to_string().contains("provider"));
    }

    #[test]
    fn role_assignment_request_rejects_unknown_fields() {
        let result = serde_json::from_str::<RoleAssignmentCreateRequest>(
            r#"{"principal_id":"p","project_id":"project-a","role_id":"member","secret":"nope"}"#,
        );
        assert!(result.is_err());
    }
}
