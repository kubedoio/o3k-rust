//! System/operator-authorized native IAM governance API.
//!
//! This surface exposes bounded, canonical administration over the durable O3K
//! IAM authority (`keystone_projects`, `keystone_users`, `keystone_roles`,
//! `keystone_role_assignments`, `operator_assignments`). External IdP identity
//! is never cloud authorization truth here: every route requires an explicit
//! system-scoped `governance:*` action and the durable `operator` role.
//!
//! No credential material (password hashes, tokens, private keys, service
//! secrets) is representable in these DTOs. See SPEC-0044.
use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::RepositoryPage,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, ResourceTarget,
    ResourceType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The canonical governance action namespace. All three actions are
/// system-scoped and additionally require the durable `operator` role.
pub const ACTION_READ: &str = "ReadGovernance";
pub const ACTION_MANAGE_ASSIGNMENT: &str = "ManageAssignment";
pub const ACTION_MANAGE_OPERATOR_ASSIGNMENT: &str = "ManageOperatorAssignment";

/// The only operator profile the canonical token path currently honours.
pub const OPERATOR_PROFILE: &str = "operator-console";

/// Maximum accepted length for a canonical identifier supplied by a caller.
const MAX_IDENTIFIER_LENGTH: usize = 256;

// ── Public DTOs ───────────────────────────────────────────────────────────

/// Canonical project metadata. Fields map 1:1 to durable state; no
/// organization/account/billing hierarchy is invented here.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

/// Canonical principal metadata. `kind` distinguishes human and service
/// principals; `password_hash` is structurally absent.
#[derive(Debug, Clone, Serialize)]
pub struct PrincipalView {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

/// Canonical role identity. Role names are display identity only; callers must
/// not infer permissions from them (see [`CapabilityView`]).
#[derive(Debug, Clone, Serialize)]
pub struct RoleView {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
}

/// A canonical project membership / role assignment.
#[derive(Debug, Clone, Serialize)]
pub struct AssignmentView {
    pub id: String,
    pub principal_id: String,
    pub project_id: String,
    pub role_id: String,
    pub created_at: String,
}

/// A canonical durable operator (system) assignment.
#[derive(Debug, Clone, Serialize)]
pub struct OperatorAssignmentView {
    pub id: String,
    pub principal_id: String,
    pub profile: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A canonical authorization action projection. This is the server-side
/// contract clients use instead of inferring permissions from role names.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityView {
    pub action: String,
    pub resource_type: String,
    pub required_roles: Vec<String>,
    pub require_ownership: bool,
}

// ── Requests ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
    pub project_id: Option<String>,
    pub principal_id: Option<String>,
    pub role_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentCreateRequest {
    pub principal_id: String,
    pub project_id: String,
    pub role_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAssignmentCreateRequest {
    pub principal_id: String,
    #[serde(default)]
    pub profile: Option<String>,
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_LENGTH
        && !value.bytes().any(|byte| byte == 0)
}

impl AssignmentCreateRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if !valid_identifier(&self.principal_id) {
            return Err("principal_id is empty or malformed");
        }
        if !valid_identifier(&self.project_id) {
            return Err("project_id is empty or malformed");
        }
        if !valid_identifier(&self.role_id) {
            return Err("role_id is empty or malformed");
        }
        Ok(())
    }
}

impl OperatorAssignmentCreateRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if !valid_identifier(&self.principal_id) {
            return Err("principal_id is empty or malformed");
        }
        if self
            .profile
            .as_deref()
            .is_some_and(|profile| profile != OPERATOR_PROFILE)
        {
            return Err("unsupported operator profile");
        }
        Ok(())
    }
}

// ── Errors ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceError {
    /// Durable storage is unavailable or degraded.
    Unavailable,
    /// The requested record does not exist.
    NotFound,
    /// A referenced principal/project/role does not exist or is disabled.
    InvalidReference,
    /// The request is malformed before it reaches storage.
    InvalidRequest,
    /// A requested write conflicts with durable state.
    Conflict,
    /// The repository returned an invalid page (internal fault).
    InvalidPage,
    /// Durable state is corrupt or an internal invariant failed.
    Internal,
}

fn governance_error(error: GovernanceError, request_id: &str) -> Response {
    let code = match error {
        GovernanceError::Unavailable => ErrorCode::NotAvailable,
        GovernanceError::NotFound => ErrorCode::ResourceNotFound,
        GovernanceError::InvalidReference => ErrorCode::BadRequest,
        GovernanceError::InvalidRequest => ErrorCode::BadRequest,
        GovernanceError::Conflict => ErrorCode::Conflict,
        GovernanceError::InvalidPage => ErrorCode::InternalError,
        GovernanceError::Internal => ErrorCode::InternalError,
    };
    let problem = match error {
        GovernanceError::InvalidReference => ProblemDetails::with_detail(
            code,
            "referenced principal, project, or role is unavailable",
        ),
        _ => ProblemDetails::new(code),
    };
    problem
        .with_request_id(request_id.to_owned())
        .into_response()
}

// ── Reader port ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssignmentFilter {
    pub project_id: Option<String>,
    pub principal_id: Option<String>,
    pub role_id: Option<String>,
}

#[async_trait::async_trait]
pub trait GovernanceReader: Send + Sync {
    async fn list_projects(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<ProjectView>, GovernanceError>;
    async fn show_project(&self, id: &str) -> Result<ProjectView, GovernanceError>;

    async fn list_principals(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<PrincipalView>, GovernanceError>;
    async fn show_principal(&self, id: &str) -> Result<PrincipalView, GovernanceError>;

    async fn list_roles(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<RoleView>, GovernanceError>;

    async fn list_assignments(
        &self,
        filter: &AssignmentFilter,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<AssignmentView>, GovernanceError>;

    async fn list_operator_assignments(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<OperatorAssignmentView>, GovernanceError>;

    /// Idempotently creates the canonical assignment. An equivalent existing
    /// assignment is returned unchanged and creates no additional authority.
    async fn create_assignment(
        &self,
        auth: &AuthContext,
        request: &AssignmentCreateRequest,
    ) -> Result<AssignmentView, GovernanceError>;

    /// Removes the canonical assignment. Returns `NotFound` when absent.
    async fn delete_assignment(&self, auth: &AuthContext, id: &str) -> Result<(), GovernanceError>;

    /// Idempotently creates the canonical operator assignment.
    async fn create_operator_assignment(
        &self,
        auth: &AuthContext,
        principal_id: &str,
        profile: &str,
    ) -> Result<OperatorAssignmentView, GovernanceError>;

    /// Removes the canonical operator assignment. Returns `NotFound` when absent.
    async fn delete_operator_assignment(
        &self,
        auth: &AuthContext,
        id: &str,
    ) -> Result<(), GovernanceError>;
}

// ── Handler helpers ───────────────────────────────────────────────────────

fn authorize(state: &NativeApiState, auth: &AuthContext, action: &str) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: ActionId::new_unchecked("governance", action),
            resource_target: ResourceTarget::collection(
                ResourceType::new_unchecked("governance", "governance"),
                Some(auth.effective_scope().id().clone()),
            ),
        }),
        AuthorizationDecision::Allow
    )
}

fn query_identity(prefix: &str, parts: &[Option<&str>]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update([0]);
    for part in parts {
        hasher.update(part.unwrap_or("").as_bytes());
        hasher.update([0]);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn unavailable() -> Response {
    ProblemDetails::new(ErrorCode::NotAvailable).into_response()
}

fn forbidden() -> Response {
    ProblemDetails::new(ErrorCode::Forbidden).into_response()
}

/// Runs the shared bounded-collection handler flow: authorize, validate the
/// cursor against the effective scope and filter identity, read, complete page.
async fn read_page<T, F, Fut>(
    state: &NativeApiState,
    auth: &AuthContext,
    raw_limit: Option<&str>,
    raw_cursor: Option<&str>,
    resource_type: &str,
    query_identity: &str,
    read: F,
) -> Response
where
    T: Serialize,
    F: FnOnce(Option<String>, usize) -> Fut,
    Fut: std::future::Future<Output = Result<RepositoryPage<T>, GovernanceError>>,
{
    let scope_id = auth.effective_scope().id().as_str().to_owned();
    let validated = match state.cursor_config.validate_query_with_identity(
        raw_limit,
        raw_cursor,
        &scope_id,
        resource_type,
        query_identity,
    ) {
        Ok(validated) => validated,
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let page = match read(
        validated.continuation_key().map(str::to_owned),
        validated.limit(),
    )
    .await
    {
        Ok(page) => page,
        Err(error) => return governance_error(error, auth.request_id()),
    };
    match state.cursor_config.complete_page(&validated, page) {
        Ok(page) => Json(page).into_response(),
        Err(_) => ProblemDetails::new(ErrorCode::InternalError).into_response(),
    }
}

fn show_result<T: Serialize>(result: Result<T, GovernanceError>, request_id: &str) -> Response {
    match result {
        Ok(view) => Json(view).into_response(),
        Err(error) => governance_error(error, request_id),
    }
}

// ── Handlers: canonical collections ───────────────────────────────────────

pub async fn list_projects(
    auth: BearerAuth,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    let identity = query_identity("o3k/governance-project/v1", &[]);
    read_page(
        &state,
        &auth.0,
        query.limit.as_deref(),
        query.cursor.as_deref(),
        "governance_project",
        &identity,
        move |after, limit| async move { reader.list_projects(after.as_deref(), limit).await },
    )
    .await
}

pub async fn show_project(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if !valid_identifier(&id) {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    show_result(reader.show_project(&id).await, auth.0.request_id())
}

pub async fn list_principals(
    auth: BearerAuth,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    let identity = query_identity("o3k/governance-principal/v1", &[]);
    read_page(
        &state,
        &auth.0,
        query.limit.as_deref(),
        query.cursor.as_deref(),
        "governance_principal",
        &identity,
        move |after, limit| async move { reader.list_principals(after.as_deref(), limit).await },
    )
    .await
}

pub async fn show_principal(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if !valid_identifier(&id) {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    show_result(reader.show_principal(&id).await, auth.0.request_id())
}

pub async fn list_roles(
    auth: BearerAuth,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    let identity = query_identity("o3k/governance-role/v1", &[]);
    read_page(
        &state,
        &auth.0,
        query.limit.as_deref(),
        query.cursor.as_deref(),
        "governance_role",
        &identity,
        move |after, limit| async move { reader.list_roles(after.as_deref(), limit).await },
    )
    .await
}

/// Returns the canonical authorization action inventory so clients never have
/// to infer permissions from role display names.
pub async fn list_capabilities(auth: BearerAuth, State(state): State<NativeApiState>) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(authorizer) = state.authorizer.as_ref() else {
        return unavailable();
    };
    let items = authorizer
        .capabilities()
        .into_iter()
        .map(|policy| CapabilityView {
            action: policy.action.as_str(),
            resource_type: policy.expected_resource_type.to_string(),
            required_roles: policy.required_roles,
            require_ownership: policy.require_ownership,
        })
        .collect::<Vec<_>>();
    Json(CapabilitiesResponse { items }).into_response()
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesResponse {
    pub items: Vec<CapabilityView>,
}

pub async fn list_assignments(
    auth: BearerAuth,
    Query(query): Query<AssignmentQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if query
        .project_id
        .as_deref()
        .is_some_and(|v| !valid_identifier(v))
        || query
            .principal_id
            .as_deref()
            .is_some_and(|v| !valid_identifier(v))
        || query
            .role_id
            .as_deref()
            .is_some_and(|v| !valid_identifier(v))
    {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    let filter = AssignmentFilter {
        project_id: query.project_id.clone(),
        principal_id: query.principal_id.clone(),
        role_id: query.role_id.clone(),
    };
    // The cursor is bound to the effective filter set so it cannot be replayed
    // with different predicates or a different authorization scope.
    let identity = query_identity(
        "o3k/governance-assignment/v1",
        &[
            query.project_id.as_deref(),
            query.principal_id.as_deref(),
            query.role_id.as_deref(),
        ],
    );
    read_page(
        &state,
        &auth.0,
        query.limit.as_deref(),
        query.cursor.as_deref(),
        "governance_assignment",
        &identity,
        move |after, limit| async move {
            reader
                .list_assignments(&filter, after.as_deref(), limit)
                .await
        },
    )
    .await
}

pub async fn list_operator_assignments(
    auth: BearerAuth,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_READ) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    let identity = query_identity("o3k/governance-operator-assignment/v1", &[]);
    read_page(
        &state,
        &auth.0,
        query.limit.as_deref(),
        query.cursor.as_deref(),
        "governance_operator_assignment",
        &identity,
        move |after, limit| async move {
            reader
                .list_operator_assignments(after.as_deref(), limit)
                .await
        },
    )
    .await
}

// ── Handlers: mutations ───────────────────────────────────────────────────

pub async fn create_assignment(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Json(request): Json<AssignmentCreateRequest>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_MANAGE_ASSIGNMENT) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if let Err(detail) = request.validate() {
        return ProblemDetails::with_detail(ErrorCode::BadRequest, detail)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    match reader.create_assignment(&auth.0, &request).await {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(error) => governance_error(error, auth.0.request_id()),
    }
}

pub async fn delete_assignment(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_MANAGE_ASSIGNMENT) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if !valid_identifier(&id) {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    match reader.delete_assignment(&auth.0, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => governance_error(error, auth.0.request_id()),
    }
}

pub async fn create_operator_assignment(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Json(request): Json<OperatorAssignmentCreateRequest>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_MANAGE_OPERATOR_ASSIGNMENT) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if let Err(detail) = request.validate() {
        return ProblemDetails::with_detail(ErrorCode::BadRequest, detail)
            .with_request_id(auth.0.request_id().to_owned())
            .into_response();
    }
    let profile = request.profile.as_deref().unwrap_or(OPERATOR_PROFILE);
    match reader
        .create_operator_assignment(&auth.0, &request.principal_id, profile)
        .await
    {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(error) => governance_error(error, auth.0.request_id()),
    }
}

pub async fn delete_operator_assignment(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, ACTION_MANAGE_OPERATOR_ASSIGNMENT) {
        return forbidden();
    }
    let Some(reader) = state.governance_reader.as_ref().cloned() else {
        return unavailable();
    };
    if !valid_identifier(&id) {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    }
    match reader.delete_operator_assignment(&auth.0, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => governance_error(error, auth.0.request_id()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{NativeApiState, auth, error, pagination::CursorConfig, router};
    use o3k_kernel::{
        OwnershipScope, Principal, PrincipalId, ScopeId, ScopeKind, StaticAuthorizer, UserPrincipal,
    };
    use std::sync::Arc;

    #[derive(Clone)]
    struct TestIssuer(AuthContext);

    #[async_trait::async_trait]
    impl auth::TokenIssuer for TestIssuer {
        async fn issue_native(
            &self,
            _request: &auth::NativeTokenRequestV1,
        ) -> Result<(String, serde_json::Value), error::ProblemDetails> {
            Err(error::ProblemDetails::unauthorized())
        }

        async fn auth_context(&self, _token: &str) -> Result<AuthContext, error::ProblemDetails> {
            Ok(self.0.clone())
        }
    }

    fn context(system: bool) -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked("operator-1"),
                "operator",
                None,
            )),
            OwnershipScope::new(
                ScopeId::new_unchecked(if system { "system" } else { "project-a" }),
                if system {
                    ScopeKind::System
                } else {
                    ScopeKind::Project
                },
                None,
                None,
            ),
            vec!["operator".to_owned()],
            1,
            2,
            "audit-test",
            "request-test",
            None,
        )
    }

    struct FakeReader {
        references_valid: bool,
    }

    impl FakeReader {
        fn page<T>(&self, item: T, limit: usize) -> RepositoryPage<T> {
            // Report a continuation so the handler must round-trip a cursor.
            RepositoryPage::new(vec![item], true, Some("k1".to_owned()), limit)
                .expect("valid repository page")
        }
    }

    #[async_trait::async_trait]
    impl GovernanceReader for FakeReader {
        async fn list_projects(
            &self,
            _after: Option<&str>,
            limit: usize,
        ) -> Result<RepositoryPage<ProjectView>, GovernanceError> {
            Ok(self.page(
                ProjectView {
                    id: "proj-1".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "project-one".to_owned(),
                    description: None,
                    enabled: true,
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
                limit,
            ))
        }

        async fn show_project(&self, id: &str) -> Result<ProjectView, GovernanceError> {
            if id == "missing" {
                return Err(GovernanceError::NotFound);
            }
            Ok(ProjectView {
                id: id.to_owned(),
                domain_id: "default".to_owned(),
                name: "project-one".to_owned(),
                description: None,
                enabled: true,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            })
        }

        async fn list_principals(
            &self,
            _after: Option<&str>,
            limit: usize,
        ) -> Result<RepositoryPage<PrincipalView>, GovernanceError> {
            Ok(self.page(
                PrincipalView {
                    id: "user-1".to_owned(),
                    domain_id: "default".to_owned(),
                    name: "alice".to_owned(),
                    kind: "user".to_owned(),
                    email: None,
                    enabled: true,
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
                limit,
            ))
        }

        async fn show_principal(&self, id: &str) -> Result<PrincipalView, GovernanceError> {
            Ok(PrincipalView {
                id: id.to_owned(),
                domain_id: "default".to_owned(),
                name: "alice".to_owned(),
                kind: "user".to_owned(),
                email: None,
                enabled: true,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            })
        }

        async fn list_roles(
            &self,
            _after: Option<&str>,
            limit: usize,
        ) -> Result<RepositoryPage<RoleView>, GovernanceError> {
            Ok(self.page(
                RoleView {
                    id: "role-1".to_owned(),
                    name: "member".to_owned(),
                    description: None,
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
                limit,
            ))
        }

        async fn list_assignments(
            &self,
            _filter: &AssignmentFilter,
            _after: Option<&str>,
            limit: usize,
        ) -> Result<RepositoryPage<AssignmentView>, GovernanceError> {
            Ok(self.page(
                AssignmentView {
                    id: "assign-1".to_owned(),
                    principal_id: "user-1".to_owned(),
                    project_id: "proj-1".to_owned(),
                    role_id: "role-1".to_owned(),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
                limit,
            ))
        }

        async fn list_operator_assignments(
            &self,
            _after: Option<&str>,
            limit: usize,
        ) -> Result<RepositoryPage<OperatorAssignmentView>, GovernanceError> {
            Ok(self.page(
                OperatorAssignmentView {
                    id: "op-1".to_owned(),
                    principal_id: "user-1".to_owned(),
                    profile: OPERATOR_PROFILE.to_owned(),
                    enabled: true,
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                    updated_at: "2026-01-01T00:00:00Z".to_owned(),
                },
                limit,
            ))
        }

        async fn create_assignment(
            &self,
            _auth: &AuthContext,
            request: &AssignmentCreateRequest,
        ) -> Result<AssignmentView, GovernanceError> {
            if !self.references_valid {
                return Err(GovernanceError::InvalidReference);
            }
            Ok(AssignmentView {
                id: "assign-new".to_owned(),
                principal_id: request.principal_id.clone(),
                project_id: request.project_id.clone(),
                role_id: request.role_id.clone(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            })
        }

        async fn delete_assignment(
            &self,
            _auth: &AuthContext,
            id: &str,
        ) -> Result<(), GovernanceError> {
            if id == "missing" {
                return Err(GovernanceError::NotFound);
            }
            Ok(())
        }

        async fn create_operator_assignment(
            &self,
            _auth: &AuthContext,
            principal_id: &str,
            profile: &str,
        ) -> Result<OperatorAssignmentView, GovernanceError> {
            Ok(OperatorAssignmentView {
                id: "op-new".to_owned(),
                principal_id: principal_id.to_owned(),
                profile: profile.to_owned(),
                enabled: true,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
            })
        }

        async fn delete_operator_assignment(
            &self,
            _auth: &AuthContext,
            _id: &str,
        ) -> Result<(), GovernanceError> {
            Ok(())
        }
    }

    fn app(system: bool, references_valid: bool) -> axum::Router {
        let state = NativeApiState::new(
            None,
            CursorConfig::new(vec![9u8; 32]).unwrap(),
            Some(Arc::new(TestIssuer(context(system)))),
            None,
            None,
            None,
        )
        .unwrap()
        .with_governance_reader(Arc::new(FakeReader { references_valid }))
        .with_authorizer(Arc::new(StaticAuthorizer::standard()));
        router(state)
    }

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::builder()
            .uri(uri)
            .header("authorization", "Bearer test")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    async fn post(
        app: axum::Router,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", "Bearer test")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn governance_reads_require_system_operator_scope() {
        // A project-scoped caller that holds an `operator` role string is denied.
        let (status, _) = get(app(false, true), "/operator/governance/projects").await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A system-scoped operator is allowed.
        let (status, body) = get(app(true, true), "/operator/governance/projects").await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_governance_schema(&body);
    }

    #[tokio::test]
    async fn governance_collections_match_contract() {
        for uri in [
            "/operator/governance/projects",
            "/operator/governance/principals",
            "/operator/governance/roles",
            "/operator/governance/assignments",
            "/operator/governance/operator-assignments",
        ] {
            let (status, body) = get(app(true, true), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(body["has_more"], serde_json::json!(true), "{uri}");
            assert!(body["next_cursor"].is_string(), "{uri}");
            crate::assert_governance_schema(&body);
        }
    }

    #[tokio::test]
    async fn governance_capabilities_expose_canonical_actions() {
        let (status, body) = get(app(true, true), "/operator/governance/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_governance_schema(&body);
        let actions: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["action"].as_str().unwrap())
            .collect();
        assert!(actions.contains(&"governance:ReadGovernance"));
        assert!(actions.contains(&"governance:ManageAssignment"));
        assert!(actions.contains(&"governance:ManageOperatorAssignment"));
    }

    #[tokio::test]
    async fn governance_cursor_is_bound_to_effective_filter() {
        let (status, body) = get(
            app(true, true),
            "/operator/governance/assignments?project_id=proj-1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let cursor = body["next_cursor"].as_str().unwrap().to_owned();

        // Replaying the cursor under a different filter set must be rejected.
        let uri = format!("/operator/governance/assignments?project_id=proj-2&cursor={cursor}");
        let (status, _) = get(app(true, true), &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn governance_mutation_requires_manage_action_and_valid_reference() {
        let body = serde_json::json!({
            "principal_id": "user-1",
            "project_id": "proj-1",
            "role_id": "role-1"
        });
        // A project-scoped caller cannot mutate global IAM.
        let (status, _) = post(
            app(false, true),
            "/operator/governance/assignments",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // An invalid reference is rejected before any durable write.
        let (status, _) = post(
            app(true, false),
            "/operator/governance/assignments",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A valid system-scoped create succeeds.
        let (status, response) =
            post(app(true, true), "/operator/governance/assignments", body).await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_governance_schema(&response);
    }

    #[tokio::test]
    async fn governance_rejects_unknown_body_fields() {
        let (status, _) = post(
            app(true, true),
            "/operator/governance/assignments",
            serde_json::json!({
                "principal_id": "user-1",
                "project_id": "proj-1",
                "role_id": "role-1",
                "system": true
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
