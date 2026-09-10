use async_trait::async_trait;

use super::O3kStore;
use crate::governance::{
    CreateAssignmentOutcome, GovernanceAssignmentFilter, GovernancePrincipalRecord,
    GovernanceReferences, GovernanceRepository, GovernanceRoleGrantRecord,
};
use crate::{
    AuditEventRecord, KeystoneProjectRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord,
    OperatorAssignmentRecord, RepositoryPage, StoreError,
};

#[async_trait]
impl GovernanceRepository for O3kStore {
    async fn list_governance_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_governance_projects_page(after_id, limit).await,
            Self::Postgres(s) => s.list_governance_projects_page(after_id, limit).await,
        }
    }

    async fn get_governance_project(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_project(id).await,
            Self::Postgres(s) => s.get_governance_project(id).await,
        }
    }

    async fn list_governance_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<GovernancePrincipalRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_governance_principals_page(after_id, limit).await,
            Self::Postgres(s) => s.list_governance_principals_page(after_id, limit).await,
        }
    }

    async fn get_governance_principal(
        &self,
        id: &str,
    ) -> Result<Option<GovernancePrincipalRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_principal(id).await,
            Self::Postgres(s) => s.get_governance_principal(id).await,
        }
    }

    async fn list_governance_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_governance_roles_page(after_id, limit).await,
            Self::Postgres(s) => s.list_governance_roles_page(after_id, limit).await,
        }
    }

    async fn get_governance_role(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_role(id).await,
            Self::Postgres(s) => s.get_governance_role(id).await,
        }
    }

    async fn list_governance_assignments_page(
        &self,
        filter: &GovernanceAssignmentFilter,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.list_governance_assignments_page(filter, after_id, limit)
                    .await
            }
            Self::Postgres(s) => {
                s.list_governance_assignments_page(filter, after_id, limit)
                    .await
            }
        }
    }

    async fn get_governance_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_assignment(id).await,
            Self::Postgres(s) => s.get_governance_assignment(id).await,
        }
    }

    async fn get_governance_role_grants(
        &self,
        principal_id: &str,
    ) -> Result<Vec<GovernanceRoleGrantRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_role_grants(principal_id).await,
            Self::Postgres(s) => s.get_governance_role_grants(principal_id).await,
        }
    }

    async fn list_governance_operator_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<OperatorAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.list_governance_operator_assignments_page(after_id, limit)
                    .await
            }
            Self::Postgres(s) => {
                s.list_governance_operator_assignments_page(after_id, limit)
                    .await
            }
        }
    }

    async fn get_governance_operator_assignment(
        &self,
        id: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_governance_operator_assignment(id).await,
            Self::Postgres(s) => s.get_governance_operator_assignment(id).await,
        }
    }

    async fn get_governance_operator_assignment_by_owner(
        &self,
        user_id: &str,
        profile: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_governance_operator_assignment_by_owner(user_id, profile)
                    .await
            }
            Self::Postgres(s) => {
                s.get_governance_operator_assignment_by_owner(user_id, profile)
                    .await
            }
        }
    }

    async fn check_governance_references(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<GovernanceReferences, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.check_governance_references(principal_id, project_id, role_id)
                    .await
            }
            Self::Postgres(s) => {
                s.check_governance_references(principal_id, project_id, role_id)
                    .await
            }
        }
    }

    async fn create_role_assignment_with_audit(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<CreateAssignmentOutcome, StoreError> {
        match self {
            Self::Sqlite(s) => s.create_role_assignment_with_audit(assignment, audit).await,
            Self::Postgres(s) => s.create_role_assignment_with_audit(assignment, audit).await,
        }
    }

    async fn delete_role_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_role_assignment_with_audit(id, audit).await,
            Self::Postgres(s) => s.delete_role_assignment_with_audit(id, audit).await,
        }
    }

    async fn create_operator_assignment_with_audit(
        &self,
        assignment: &OperatorAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.create_operator_assignment_with_audit(assignment, audit)
                    .await
            }
            Self::Postgres(s) => {
                s.create_operator_assignment_with_audit(assignment, audit)
                    .await
            }
        }
    }

    async fn delete_operator_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_operator_assignment_with_audit(id, audit).await,
            Self::Postgres(s) => s.delete_operator_assignment_with_audit(id, audit).await,
        }
    }
}
