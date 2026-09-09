use async_trait::async_trait;

use crate::{
    GovernanceRepository, KeystoneProjectRecord, KeystoneRoleRecord, KeystoneUserRecord, O3kStore,
    StoreError,
};

#[async_trait]
impl GovernanceRepository for O3kStore {
    async fn mutate_role_assignment_removal(
        &self,
        a: &crate::KeystoneRoleAssignmentRecord,
        o: &crate::OperationRecord,
        c: &crate::CanonicalOperationRecord,
        r: &crate::IdempotencyReservationRequest,
        audit: &crate::AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, crate::StoreError> {
        match self {
            Self::Sqlite(s) => s.mutate_role_assignment_removal(a, o, c, r, audit).await,
            Self::Postgres(s) => s.mutate_role_assignment_removal(a, o, c, r, audit).await,
        }
    }
    async fn mutate_role_assignment(
        &self,
        assignment: &crate::KeystoneRoleAssignmentRecord,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
        audit: &crate::AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, crate::StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .mutate_role_assignment(assignment, operation, canonical, request, audit)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .mutate_role_assignment(assignment, operation, canonical, request, audit)
                    .await
            }
        }
    }
    async fn ensure_role_assignment(
        &self,
        assignment: &crate::KeystoneRoleAssignmentRecord,
    ) -> Result<crate::KeystoneRoleAssignmentRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.ensure_role_assignment(assignment).await,
            Self::Postgres(store) => store.ensure_role_assignment(assignment).await,
        }
    }
    async fn get_role_assignment(
        &self,
        id: &str,
    ) -> Result<Option<crate::KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_role_assignment(id).await,
            Self::Postgres(s) => s.get_role_assignment(id).await,
        }
    }

    async fn remove_role_assignment(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .remove_role_assignment(principal_id, project_id, role_id)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .remove_role_assignment(principal_id, project_id, role_id)
                    .await
            }
        }
    }

    async fn get_principal(&self, id: &str) -> Result<Option<KeystoneUserRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_principal(id).await,
            Self::Postgres(store) => store.get_principal(id).await,
        }
    }

    async fn get_project(&self, id: &str) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_project(id).await,
            Self::Postgres(store) => store.get_project(id).await,
        }
    }

    async fn list_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_projects_page(after_id, limit).await,
            Self::Postgres(store) => store.list_projects_page(after_id, limit).await,
        }
    }

    async fn list_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_principals_page(after_id, limit).await,
            Self::Postgres(store) => store.list_principals_page(after_id, limit).await,
        }
    }

    async fn list_role_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_role_assignments_page(after_id, limit).await,
            Self::Postgres(store) => store.list_role_assignments_page(after_id, limit).await,
        }
    }

    async fn list_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_roles_page(after_id, limit).await,
            Self::Postgres(store) => store.list_roles_page(after_id, limit).await,
        }
    }
}
