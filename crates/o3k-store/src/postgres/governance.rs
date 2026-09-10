use async_trait::async_trait;
use sqlx::Row;

use super::PostgresStore;
use crate::governance::{
    CreateAssignmentOutcome, GovernanceAssignmentFilter, GovernancePrincipalRecord,
    GovernanceReferences, GovernanceRepository, GovernanceRoleGrantRecord,
};
use crate::port::durable::bounded_fetch_limit;
use crate::{
    AuditEventRecord, KeystoneProjectRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord,
    OperatorAssignmentRecord, RepositoryPage, StoreError,
};

fn project_row(r: &sqlx::postgres::PgRow) -> Result<KeystoneProjectRecord, StoreError> {
    Ok(KeystoneProjectRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        domain_id: r.try_get("domain_id").map_err(StoreError::Database)?,
        name: r.try_get("name").map_err(StoreError::Database)?,
        description: r.try_get("description").map_err(StoreError::Database)?,
        enabled: r
            .try_get::<i32, _>("enabled")
            .map_err(StoreError::Database)?
            != 0,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
    })
}

fn principal_row(r: &sqlx::postgres::PgRow) -> Result<GovernancePrincipalRecord, StoreError> {
    Ok(GovernancePrincipalRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        domain_id: r.try_get("domain_id").map_err(StoreError::Database)?,
        name: r.try_get("name").map_err(StoreError::Database)?,
        email: r.try_get("email").map_err(StoreError::Database)?,
        enabled: r
            .try_get::<i32, _>("enabled")
            .map_err(StoreError::Database)?
            != 0,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
        service: r.try_get("is_service").map_err(StoreError::Database)?,
    })
}

fn role_row(r: &sqlx::postgres::PgRow) -> Result<KeystoneRoleRecord, StoreError> {
    Ok(KeystoneRoleRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        name: r.try_get("name").map_err(StoreError::Database)?,
        description: r.try_get("description").map_err(StoreError::Database)?,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
    })
}

fn assignment_row(r: &sqlx::postgres::PgRow) -> Result<KeystoneRoleAssignmentRecord, StoreError> {
    Ok(KeystoneRoleAssignmentRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        user_id: r.try_get("user_id").map_err(StoreError::Database)?,
        project_id: r.try_get("project_id").map_err(StoreError::Database)?,
        role_id: r.try_get("role_id").map_err(StoreError::Database)?,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
    })
}

fn operator_row(r: &sqlx::postgres::PgRow) -> Result<OperatorAssignmentRecord, StoreError> {
    Ok(OperatorAssignmentRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        user_id: r.try_get("user_id").map_err(StoreError::Database)?,
        profile: r.try_get("profile").map_err(StoreError::Database)?,
        enabled: r.try_get("enabled").map_err(StoreError::Database)?,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
        updated_at: r.try_get("updated_at").map_err(StoreError::Database)?,
    })
}

const PROJECTS_PAGE: &str = "SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2";
const PRINCIPALS_PAGE: &str = "SELECT u.id, u.domain_id, u.name, u.email, u.enabled, u.created_at, EXISTS(SELECT 1 FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = u.id AND r.name = 'service') AS is_service FROM keystone_users u WHERE ($1::text IS NULL OR u.id > $1) ORDER BY u.id ASC LIMIT $2";
const ROLES_PAGE: &str = "SELECT id, name, description, created_at FROM keystone_roles WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2";
const ASSIGNMENTS_PAGE: &str = "SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE ($1::text IS NULL OR id > $1) AND ($2::text IS NULL OR user_id = $2) AND ($3::text IS NULL OR project_id = $3) AND ($4::text IS NULL OR role_id = $4) ORDER BY id ASC LIMIT $5";
const OPERATOR_PAGE: &str = "SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2";

#[async_trait]
impl GovernanceRepository for PostgresStore {
    async fn list_governance_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneProjectRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query(PROJECTS_PAGE)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, project_row)
    }

    async fn get_governance_project(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.as_ref().map(project_row).transpose()
    }

    async fn list_governance_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<GovernancePrincipalRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query(PRINCIPALS_PAGE)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, principal_row)
    }

    async fn get_governance_principal(
        &self,
        id: &str,
    ) -> Result<Option<GovernancePrincipalRecord>, StoreError> {
        let row = sqlx::query("SELECT u.id, u.domain_id, u.name, u.email, u.enabled, u.created_at, EXISTS(SELECT 1 FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = u.id AND r.name = 'service') AS is_service FROM keystone_users u WHERE u.id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.as_ref().map(principal_row).transpose()
    }

    async fn list_governance_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query(ROLES_PAGE)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, role_row)
    }

    async fn get_governance_role(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.as_ref().map(role_row).transpose()
    }

    async fn list_governance_assignments_page(
        &self,
        filter: &GovernanceAssignmentFilter,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleAssignmentRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query(ASSIGNMENTS_PAGE)
            .bind(after_id)
            .bind(&filter.principal_id)
            .bind(&filter.project_id)
            .bind(&filter.role_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, assignment_row)
    }

    async fn get_governance_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError> {
        let row = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.as_ref().map(assignment_row).transpose()
    }

    async fn get_governance_role_grants(
        &self,
        principal_id: &str,
    ) -> Result<Vec<GovernanceRoleGrantRecord>, StoreError> {
        let rows = sqlx::query("SELECT ra.project_id, r.name FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = $1 ORDER BY ra.project_id ASC, r.name ASC")
            .bind(principal_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter()
            .map(|r| {
                Ok(GovernanceRoleGrantRecord {
                    project_id: r.try_get("project_id").map_err(StoreError::Database)?,
                    role_name: r.try_get("name").map_err(StoreError::Database)?,
                })
            })
            .collect()
    }

    async fn list_governance_operator_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<OperatorAssignmentRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query(OPERATOR_PAGE)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, operator_row)
    }

    async fn get_governance_operator_assignment(
        &self,
        id: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError> {
        let row = sqlx::query("SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.as_ref().map(operator_row).transpose()
    }

    async fn get_governance_operator_assignment_by_owner(
        &self,
        user_id: &str,
        profile: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError> {
        let row = sqlx::query("SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE user_id = $1 AND profile = $2")
            .bind(user_id)
            .bind(profile)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.as_ref().map(operator_row).transpose()
    }

    async fn check_governance_references(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<GovernanceReferences, StoreError> {
        let row = sqlx::query("SELECT (SELECT COUNT(*) FROM keystone_users WHERE id = $1), (SELECT COUNT(*) FROM keystone_users WHERE id = $2 AND enabled = 1), (SELECT COUNT(*) FROM keystone_projects WHERE id = $3), (SELECT COUNT(*) FROM keystone_projects WHERE id = $4 AND enabled = 1), (SELECT COUNT(*) FROM keystone_roles WHERE id = $5)")
            .bind(principal_id)
            .bind(principal_id)
            .bind(project_id)
            .bind(project_id)
            .bind(role_id)
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let principal_exists: i64 = row.try_get(0).map_err(StoreError::Database)?;
        let principal_enabled: i64 = row.try_get(1).map_err(StoreError::Database)?;
        let project_exists: i64 = row.try_get(2).map_err(StoreError::Database)?;
        let project_enabled: i64 = row.try_get(3).map_err(StoreError::Database)?;
        let role_exists: i64 = row.try_get(4).map_err(StoreError::Database)?;
        Ok(GovernanceReferences {
            principal_exists: principal_exists > 0,
            principal_enabled: principal_enabled > 0,
            project_exists: project_exists > 0,
            project_enabled: project_enabled > 0,
            role_exists: role_exists > 0,
        })
    }

    async fn create_role_assignment_with_audit(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<CreateAssignmentOutcome, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let inserted = sqlx::query("INSERT INTO keystone_role_assignments (id, user_id, project_id, role_id, created_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_id, project_id, role_id) DO NOTHING")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .bind(&assignment.created_at)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if inserted.rows_affected() == 1 {
            insert_required_audit(&mut tx, audit).await?;
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(CreateAssignmentOutcome::Created(assignment.clone()));
        }
        tx.rollback().await.map_err(StoreError::Database)?;
        let existing = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE user_id = $1 AND project_id = $2 AND role_id = $3")
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or_else(|| {
                StoreError::Corrupt("role assignment conflict row disappeared".to_owned())
            })?;
        Ok(CreateAssignmentOutcome::Existing(assignment_row(
            &existing,
        )?))
    }

    async fn delete_role_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let deleted = sqlx::query("DELETE FROM keystone_role_assignments WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if deleted.rows_affected() != 1 {
            tx.rollback().await.map_err(StoreError::Database)?;
            return Ok(false);
        }
        insert_required_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(true)
    }

    async fn create_operator_assignment_with_audit(
        &self,
        assignment: &OperatorAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let inserted = sqlx::query("INSERT INTO operator_assignments (id, user_id, profile, enabled, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_id, profile) DO NOTHING")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.profile)
            .bind(assignment.enabled)
            .bind(&assignment.created_at)
            .bind(&assignment.updated_at)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if inserted.rows_affected() != 1 {
            tx.rollback().await.map_err(StoreError::Database)?;
            return Ok(false);
        }
        insert_required_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(true)
    }

    async fn delete_operator_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let deleted = sqlx::query("DELETE FROM operator_assignments WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if deleted.rows_affected() != 1 {
            tx.rollback().await.map_err(StoreError::Database)?;
            return Ok(false);
        }
        insert_required_audit(&mut tx, audit).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(true)
    }
}

fn page<T>(
    rows: Vec<sqlx::postgres::PgRow>,
    limit: usize,
    map: fn(&sqlx::postgres::PgRow) -> Result<T, StoreError>,
) -> Result<RepositoryPage<T>, StoreError> {
    let has_more = rows.len() > limit;
    let continuation_key = has_more.then(|| rows[limit - 1].get("id"));
    let items = rows
        .iter()
        .take(limit)
        .map(map)
        .collect::<Result<Vec<_>, _>>()?;
    RepositoryPage::new(items, has_more, continuation_key, limit)
}

async fn insert_required_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    audit: &AuditEventRecord,
) -> Result<(), StoreError> {
    let result = sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT (event_id) DO NOTHING")
        .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.principal_kind).bind(&audit.effective_scope).bind(&audit.service).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(&audit.operation_id).bind(&audit.outcome).bind(&audit.reason_category)
        .execute(&mut **tx).await.map_err(StoreError::Database)?;
    if result.rows_affected() != 1 {
        return Err(StoreError::AuditEventConflict);
    }
    Ok(())
}
