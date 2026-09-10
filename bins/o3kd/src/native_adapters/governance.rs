//! Production native IAM governance adapter over canonical durable O3K IAM
//! authority. Every mutation builds its required durable audit event and
//! commits it in the same store transaction as the domain change.
use std::sync::Arc;

use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuthContext, OwnershipScope, ResourceId, ResourceType,
    ScopeId, ServiceNamespace,
};
use o3k_native_api::{
    governance::{
        AssignmentCreateRequest, AssignmentFilter, AssignmentView, GovernanceError,
        GovernanceReader, OperatorAssignmentView, PrincipalView, ProjectView, RoleView,
    },
    pagination::RepositoryPage,
};
use o3k_store::{
    AuditEventRecord, CreateAssignmentOutcome, GovernanceAssignmentFilter,
    GovernancePrincipalRecord, GovernanceRepository, IdentityRepository, KeystoneProjectRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, OperatorAssignmentRecord,
    RepositoryPage as StorePage, StoreError,
};

const SERVICE: &str = "governance";
const RESOURCE: &str = "governance";

pub struct GovernanceReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    /// Canonical identity service. When present, successful governance
    /// mutations reload the identity snapshot so grant/revoke converge without
    /// a process restart.
    pub identity: Option<Arc<o3k_identity::TokenService>>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn map_store(error: StoreError) -> GovernanceError {
    match error {
        StoreError::ResourceNotFound => GovernanceError::NotFound,
        StoreError::ResourceAlreadyExists => GovernanceError::Conflict,
        StoreError::Corrupt(_) => GovernanceError::Internal,
        _ => GovernanceError::Unavailable,
    }
}

fn map_page<S, T>(
    page: StorePage<S>,
    limit: usize,
    map: impl Fn(S) -> T,
) -> Result<RepositoryPage<T>, GovernanceError> {
    RepositoryPage::new(
        page.items.into_iter().map(map).collect(),
        page.has_more,
        page.continuation_key,
        limit,
    )
    .map_err(|_| GovernanceError::InvalidPage)
}

fn project_view(record: KeystoneProjectRecord) -> ProjectView {
    ProjectView {
        id: record.id,
        domain_id: record.domain_id,
        name: record.name,
        description: record.description,
        enabled: record.enabled,
        created_at: record.created_at,
    }
}

fn principal_view(record: GovernancePrincipalRecord) -> PrincipalView {
    PrincipalView {
        id: record.id,
        domain_id: record.domain_id,
        name: record.name,
        kind: if record.service { "service" } else { "user" }.to_owned(),
        email: record.email,
        enabled: record.enabled,
        created_at: record.created_at,
    }
}

fn role_view(record: KeystoneRoleRecord) -> RoleView {
    RoleView {
        id: record.id,
        name: record.name,
        description: record.description,
        created_at: record.created_at,
    }
}

pub(crate) fn assignment_view(record: KeystoneRoleAssignmentRecord) -> AssignmentView {
    AssignmentView {
        id: record.id,
        principal_id: record.user_id,
        project_id: record.project_id,
        role_id: record.role_id,
        created_at: record.created_at,
    }
}

pub(crate) fn operator_assignment_view(record: OperatorAssignmentRecord) -> OperatorAssignmentView {
    OperatorAssignmentView {
        id: record.id,
        principal_id: record.user_id,
        profile: record.profile,
        enabled: record.enabled,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

impl GovernanceReaderAdapter {
    /// Reloads the identity snapshot after a governance mutation so the change
    /// converges without a restart. A reload failure after a committed mutation
    /// is security-relevant (a revoke could silently remain effective), so it
    /// is surfaced to the caller instead of being swallowed.
    async fn reload_identity(&self) -> Result<(), GovernanceError> {
        let Some(identity) = self.identity.as_ref() else {
            return Ok(());
        };
        let store: Arc<dyn IdentityRepository> = self.store.clone();
        identity.reload(store).await.map_err(|error| {
            tracing::warn!(%error, "identity snapshot reload after governance mutation failed");
            GovernanceError::Unavailable
        })
    }
}

#[async_trait::async_trait]
impl GovernanceReader for GovernanceReaderAdapter {
    async fn list_projects(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<ProjectView>, GovernanceError> {
        let page = self
            .store
            .list_governance_projects_page(after_id, limit)
            .await
            .map_err(map_store)?;
        map_page(page, limit, project_view)
    }

    async fn show_project(&self, id: &str) -> Result<ProjectView, GovernanceError> {
        self.store
            .get_governance_project(id)
            .await
            .map_err(map_store)?
            .map(project_view)
            .ok_or(GovernanceError::NotFound)
    }

    async fn list_principals(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<PrincipalView>, GovernanceError> {
        let page = self
            .store
            .list_governance_principals_page(after_id, limit)
            .await
            .map_err(map_store)?;
        map_page(page, limit, principal_view)
    }

    async fn show_principal(&self, id: &str) -> Result<PrincipalView, GovernanceError> {
        self.store
            .get_governance_principal(id)
            .await
            .map_err(map_store)?
            .map(principal_view)
            .ok_or(GovernanceError::NotFound)
    }

    async fn list_roles(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<RoleView>, GovernanceError> {
        let page = self
            .store
            .list_governance_roles_page(after_id, limit)
            .await
            .map_err(map_store)?;
        map_page(page, limit, role_view)
    }

    async fn list_assignments(
        &self,
        filter: &AssignmentFilter,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<AssignmentView>, GovernanceError> {
        let filter = GovernanceAssignmentFilter {
            principal_id: filter.principal_id.clone(),
            project_id: filter.project_id.clone(),
            role_id: filter.role_id.clone(),
        };
        let page = self
            .store
            .list_governance_assignments_page(&filter, after_id, limit)
            .await
            .map_err(map_store)?;
        map_page(page, limit, assignment_view)
    }

    async fn list_operator_assignments(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<OperatorAssignmentView>, GovernanceError> {
        let page = self
            .store
            .list_governance_operator_assignments_page(after_id, limit)
            .await
            .map_err(map_store)?;
        map_page(page, limit, operator_assignment_view)
    }

    async fn create_assignment(
        &self,
        auth: &AuthContext,
        request: &AssignmentCreateRequest,
    ) -> Result<AssignmentView, GovernanceError> {
        // Existence of the references is enforced by durable foreign keys; the
        // enabled check here is a best-effort precondition. A principal or
        // project disabled concurrently still fails token issuance downstream,
        // so the race cannot mint usable cloud authority.
        let references = self
            .store
            .check_governance_references(
                &request.principal_id,
                &request.project_id,
                &request.role_id,
            )
            .await
            .map_err(map_store)?;
        if !references.principal_exists
            || !references.principal_enabled
            || !references.project_exists
            || !references.project_enabled
            || !references.role_exists
        {
            return Err(GovernanceError::InvalidReference);
        }
        let assignment = KeystoneRoleAssignmentRecord {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: request.principal_id.clone(),
            project_id: request.project_id.clone(),
            role_id: request.role_id.clone(),
            created_at: now_rfc3339(),
        };
        let owner_scope = OwnershipScope::project(
            ScopeId::new_unchecked(request.project_id.clone()),
            None,
            None,
        );
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked(SERVICE.to_owned()),
            ActionId::new_unchecked(SERVICE, "ManageAssignment"),
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new_unchecked(SERVICE, RESOURCE),
            Some(ResourceId::new_unchecked(assignment.id.clone())),
            Some(owner_scope),
        )
        .with_reason(format!("role={}", request.role_id));
        let outcome = self
            .store
            .create_role_assignment_with_audit(
                &assignment,
                &AuditEventRecord::from_kernel_event(&event),
            )
            .await
            .map_err(map_store)?;
        let record = match outcome {
            CreateAssignmentOutcome::Created(record) => {
                // Only an actual durable change requires an identity reload.
                self.reload_identity().await?;
                record
            }
            CreateAssignmentOutcome::Existing(record) => record,
        };
        Ok(assignment_view(record))
    }

    async fn delete_assignment(&self, auth: &AuthContext, id: &str) -> Result<(), GovernanceError> {
        let existing = self
            .store
            .get_governance_assignment(id)
            .await
            .map_err(map_store)?
            .ok_or(GovernanceError::NotFound)?;
        let owner_scope = OwnershipScope::project(
            ScopeId::new_unchecked(existing.project_id.clone()),
            None,
            None,
        );
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked(SERVICE.to_owned()),
            ActionId::new_unchecked(SERVICE, "ManageAssignment"),
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new_unchecked(SERVICE, RESOURCE),
            Some(ResourceId::new_unchecked(existing.id.clone())),
            Some(owner_scope),
        )
        .with_reason(format!("role={}", existing.role_id));
        let removed = self
            .store
            .delete_role_assignment_with_audit(id, &AuditEventRecord::from_kernel_event(&event))
            .await
            .map_err(map_store)?;
        if !removed {
            return Err(GovernanceError::NotFound);
        }
        self.reload_identity().await?;
        Ok(())
    }

    async fn create_operator_assignment(
        &self,
        auth: &AuthContext,
        principal_id: &str,
        profile: &str,
    ) -> Result<OperatorAssignmentView, GovernanceError> {
        let principal = self
            .store
            .get_governance_principal(principal_id)
            .await
            .map_err(map_store)?;
        match principal {
            Some(principal) if principal.enabled => {}
            _ => return Err(GovernanceError::InvalidReference),
        }
        let timestamp = now_rfc3339();
        let assignment = OperatorAssignmentRecord {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: principal_id.to_owned(),
            profile: profile.to_owned(),
            enabled: true,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked(SERVICE.to_owned()),
            ActionId::new_unchecked(SERVICE, "ManageOperatorAssignment"),
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new_unchecked(SERVICE, RESOURCE),
            Some(ResourceId::new_unchecked(assignment.id.clone())),
            Some(auth.effective_scope().clone()),
        )
        .with_reason(format!("profile={profile}"));
        let created = self
            .store
            .create_operator_assignment_with_audit(
                &assignment,
                &AuditEventRecord::from_kernel_event(&event),
            )
            .await
            .map_err(map_store)?;
        if created {
            // Only an actual durable change requires an identity reload.
            self.reload_identity().await?;
            return Ok(operator_assignment_view(assignment));
        }
        // Idempotent replay: return the already-canonical durable assignment.
        self.store
            .get_governance_operator_assignment_by_owner(principal_id, profile)
            .await
            .map_err(map_store)?
            .map(operator_assignment_view)
            .ok_or(GovernanceError::Internal)
    }

    async fn delete_operator_assignment(
        &self,
        auth: &AuthContext,
        id: &str,
    ) -> Result<(), GovernanceError> {
        let existing = self
            .store
            .get_governance_operator_assignment(id)
            .await
            .map_err(map_store)?
            .ok_or(GovernanceError::NotFound)?;
        let event = AuditEvent::from_auth(
            auth,
            ServiceNamespace::new_unchecked(SERVICE.to_owned()),
            ActionId::new_unchecked(SERVICE, "ManageOperatorAssignment"),
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new_unchecked(SERVICE, RESOURCE),
            Some(ResourceId::new_unchecked(existing.id.clone())),
            Some(auth.effective_scope().clone()),
        )
        .with_reason(format!("profile={}", existing.profile));
        let removed = self
            .store
            .delete_operator_assignment_with_audit(id, &AuditEventRecord::from_kernel_event(&event))
            .await
            .map_err(map_store)?;
        if !removed {
            return Err(GovernanceError::NotFound);
        }
        self.reload_identity().await?;
        Ok(())
    }
}
