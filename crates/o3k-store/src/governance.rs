use async_trait::async_trait;
use sqlx::Row;

use crate::port::durable::bounded_fetch_limit;
use crate::{
    AuditEventRecord, KeystoneProjectRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord,
    OperatorAssignmentRecord, RepositoryPage, SqliteStore, StoreError,
};

/// A bounded, password-free projection of a native IAM principal. The durable
/// user row also carries credentials; governance reads must never surface
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernancePrincipalRecord {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub service: bool,
}

/// The project/role names a principal is granted. Governance callers resolve
/// names for display; the durable assignment remains the authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceRoleGrantRecord {
    pub project_id: String,
    pub role_name: String,
}

/// Existence and enablement facts required before a governance mutation is
/// authorized. The durable store answers all five in one round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernanceReferences {
    pub principal_exists: bool,
    pub principal_enabled: bool,
    pub project_exists: bool,
    pub project_enabled: bool,
    pub role_exists: bool,
}

/// The outcome of an atomic assignment-plus-audit create. A replay of an
/// identical assignment is reported as `Existing` so the caller can answer
/// idempotently without a second mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateAssignmentOutcome {
    Created(KeystoneRoleAssignmentRecord),
    Existing(KeystoneRoleAssignmentRecord),
}

/// Server-side filters for the assignment collection. Every set field is
/// pushed into SQL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GovernanceAssignmentFilter {
    pub principal_id: Option<String>,
    pub project_id: Option<String>,
    pub role_id: Option<String>,
}

/// Narrow repository port for bounded native IAM governance reads and atomic
/// (assignment + required audit) mutations.
#[async_trait]
pub trait GovernanceRepository: Send + Sync {
    async fn list_governance_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneProjectRecord>, StoreError>;
    async fn get_governance_project(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError>;
    async fn list_governance_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<GovernancePrincipalRecord>, StoreError>;
    async fn get_governance_principal(
        &self,
        id: &str,
    ) -> Result<Option<GovernancePrincipalRecord>, StoreError>;
    async fn list_governance_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleRecord>, StoreError>;
    async fn get_governance_role(&self, id: &str)
    -> Result<Option<KeystoneRoleRecord>, StoreError>;
    async fn list_governance_assignments_page(
        &self,
        filter: &GovernanceAssignmentFilter,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneRoleAssignmentRecord>, StoreError>;
    async fn get_governance_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError>;
    async fn get_governance_role_grants(
        &self,
        principal_id: &str,
    ) -> Result<Vec<GovernanceRoleGrantRecord>, StoreError>;
    async fn list_governance_operator_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<OperatorAssignmentRecord>, StoreError>;
    async fn get_governance_operator_assignment(
        &self,
        id: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError>;
    async fn get_governance_operator_assignment_by_owner(
        &self,
        user_id: &str,
        profile: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError>;
    async fn check_governance_references(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<GovernanceReferences, StoreError>;
    async fn create_role_assignment_with_audit(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<CreateAssignmentOutcome, StoreError>;
    async fn delete_role_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError>;
    async fn create_operator_assignment_with_audit(
        &self,
        assignment: &OperatorAssignmentRecord,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError>;
    async fn delete_operator_assignment_with_audit(
        &self,
        id: &str,
        audit: &AuditEventRecord,
    ) -> Result<bool, StoreError>;
}

fn project_row(r: &sqlx::sqlite::SqliteRow) -> Result<KeystoneProjectRecord, StoreError> {
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

fn principal_row(r: &sqlx::sqlite::SqliteRow) -> Result<GovernancePrincipalRecord, StoreError> {
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
        service: r
            .try_get::<i32, _>("is_service")
            .map_err(StoreError::Database)?
            != 0,
    })
}

fn role_row(r: &sqlx::sqlite::SqliteRow) -> Result<KeystoneRoleRecord, StoreError> {
    Ok(KeystoneRoleRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        name: r.try_get("name").map_err(StoreError::Database)?,
        description: r.try_get("description").map_err(StoreError::Database)?,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
    })
}

fn assignment_row(r: &sqlx::sqlite::SqliteRow) -> Result<KeystoneRoleAssignmentRecord, StoreError> {
    Ok(KeystoneRoleAssignmentRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        user_id: r.try_get("user_id").map_err(StoreError::Database)?,
        project_id: r.try_get("project_id").map_err(StoreError::Database)?,
        role_id: r.try_get("role_id").map_err(StoreError::Database)?,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
    })
}

fn operator_row(r: &sqlx::sqlite::SqliteRow) -> Result<OperatorAssignmentRecord, StoreError> {
    Ok(OperatorAssignmentRecord {
        id: r.try_get("id").map_err(StoreError::Database)?,
        user_id: r.try_get("user_id").map_err(StoreError::Database)?,
        profile: r.try_get("profile").map_err(StoreError::Database)?,
        enabled: r
            .try_get::<i32, _>("enabled")
            .map_err(StoreError::Database)?
            != 0,
        created_at: r.try_get("created_at").map_err(StoreError::Database)?,
        updated_at: r.try_get("updated_at").map_err(StoreError::Database)?,
    })
}

#[async_trait]
impl GovernanceRepository for SqliteStore {
    async fn list_governance_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<RepositoryPage<KeystoneProjectRecord>, StoreError> {
        let n = bounded_fetch_limit(limit)?;
        let rows = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?")
            .bind(after_id)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, "id", project_row)
    }

    async fn get_governance_project(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = ?")
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
        let rows = sqlx::query("SELECT u.id, u.domain_id, u.name, u.email, u.enabled, u.created_at, EXISTS(SELECT 1 FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = u.id AND r.name = 'service') AS is_service FROM keystone_users u WHERE (? IS NULL OR u.id > ?) ORDER BY u.id ASC LIMIT ?")
            .bind(after_id)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, "id", principal_row)
    }

    async fn get_governance_principal(
        &self,
        id: &str,
    ) -> Result<Option<GovernancePrincipalRecord>, StoreError> {
        let row = sqlx::query("SELECT u.id, u.domain_id, u.name, u.email, u.enabled, u.created_at, EXISTS(SELECT 1 FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = u.id AND r.name = 'service') AS is_service FROM keystone_users u WHERE u.id = ?")
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
        let rows = sqlx::query("SELECT id, name, description, created_at FROM keystone_roles WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?")
            .bind(after_id)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, "id", role_row)
    }

    async fn get_governance_role(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles WHERE id = ?",
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
        let rows = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE (? IS NULL OR id > ?) AND (? IS NULL OR user_id = ?) AND (? IS NULL OR project_id = ?) AND (? IS NULL OR role_id = ?) ORDER BY id ASC LIMIT ?")
            .bind(after_id)
            .bind(after_id)
            .bind(&filter.principal_id)
            .bind(&filter.principal_id)
            .bind(&filter.project_id)
            .bind(&filter.project_id)
            .bind(&filter.role_id)
            .bind(&filter.role_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, "id", assignment_row)
    }

    async fn get_governance_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError> {
        let row = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE id = ?")
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
        let rows = sqlx::query("SELECT ra.project_id, r.name FROM keystone_role_assignments ra JOIN keystone_roles r ON r.id = ra.role_id WHERE ra.user_id = ? ORDER BY ra.project_id ASC, r.name ASC")
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
        let rows = sqlx::query("SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?")
            .bind(after_id)
            .bind(after_id)
            .bind(n)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        page(rows, limit, "id", operator_row)
    }

    async fn get_governance_operator_assignment(
        &self,
        id: &str,
    ) -> Result<Option<OperatorAssignmentRecord>, StoreError> {
        let row = sqlx::query("SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE id = ?")
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
        let row = sqlx::query("SELECT id, user_id, profile, enabled, created_at, updated_at FROM operator_assignments WHERE user_id = ? AND profile = ?")
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
        let row = sqlx::query("SELECT (SELECT COUNT(*) FROM keystone_users WHERE id = ?), (SELECT COUNT(*) FROM keystone_users WHERE id = ? AND enabled = 1), (SELECT COUNT(*) FROM keystone_projects WHERE id = ?), (SELECT COUNT(*) FROM keystone_projects WHERE id = ? AND enabled = 1), (SELECT COUNT(*) FROM keystone_roles WHERE id = ?)")
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
        let inserted = sqlx::query("INSERT INTO keystone_role_assignments (id, user_id, project_id, role_id, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(user_id, project_id, role_id) DO NOTHING")
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
        let existing = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE user_id = ? AND project_id = ? AND role_id = ?")
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or_else(|| {
                // The unique-constraint conflicted row vanished between the
                // rolled-back insert and this read: a create/delete race.
                // Surface a deterministic conflict so the caller retries.
                StoreError::ResourceAlreadyExists
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
        let deleted = sqlx::query("DELETE FROM keystone_role_assignments WHERE id = ?")
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
        let inserted = sqlx::query("INSERT INTO operator_assignments (id, user_id, profile, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(user_id, profile) DO NOTHING")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.profile)
            .bind(if assignment.enabled { 1 } else { 0 })
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
        let deleted = sqlx::query("DELETE FROM operator_assignments WHERE id = ?")
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

/// Builds a validated bounded page from a look-ahead fetch. `rows` holds at
/// most `limit + 1` rows; the continuation key is the primary key of the last
/// kept row, never the look-ahead row.
fn page<T>(
    rows: Vec<sqlx::sqlite::SqliteRow>,
    limit: usize,
    key_column: &str,
    map: fn(&sqlx::sqlite::SqliteRow) -> Result<T, StoreError>,
) -> Result<RepositoryPage<T>, StoreError> {
    let has_more = rows.len() > limit;
    let continuation_key = has_more.then(|| rows[limit - 1].get(key_column));
    let items = rows
        .iter()
        .take(limit)
        .map(map)
        .collect::<Result<Vec<_>, _>>()?;
    RepositoryPage::new(items, has_more, continuation_key, limit)
}

async fn insert_required_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    audit: &AuditEventRecord,
) -> Result<(), StoreError> {
    let result = sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,principal_kind,effective_scope,service,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(event_id) DO NOTHING")
        .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.principal_kind).bind(&audit.effective_scope).bind(&audit.service).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(&audit.operation_id).bind(&audit.outcome).bind(&audit.reason_category)
        .execute(&mut **tx).await.map_err(StoreError::Database)?;
    if result.rows_affected() != 1 {
        return Err(StoreError::AuditEventConflict);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeystoneDomainRecord, KeystoneUserRecord};

    const NOW: &str = "2026-01-01T00:00:00Z";

    async fn seed_domain(store: &SqliteStore) -> Result<(), StoreError> {
        store
            .insert_keystone_domain(&KeystoneDomainRecord {
                id: "default".to_owned(),
                name: "Default".to_owned(),
                description: None,
                enabled: true,
                created_at: NOW.to_owned(),
            })
            .await
    }

    async fn seed_user(store: &SqliteStore, id: &str, enabled: bool) -> Result<(), StoreError> {
        store
            .insert_keystone_user(&KeystoneUserRecord {
                id: id.to_owned(),
                domain_id: "default".to_owned(),
                name: format!("name-{id}"),
                password_hash: "pbkdf2_sha256$1$test".to_owned(),
                email: Some(format!("{id}@example.test")),
                enabled,
                created_at: NOW.to_owned(),
            })
            .await
    }

    async fn seed_project(store: &SqliteStore, id: &str, enabled: bool) -> Result<(), StoreError> {
        store
            .insert_keystone_project(&KeystoneProjectRecord {
                id: id.to_owned(),
                domain_id: "default".to_owned(),
                name: format!("name-{id}"),
                description: None,
                enabled,
                created_at: NOW.to_owned(),
            })
            .await
    }

    async fn seed_role(store: &SqliteStore, id: &str) -> Result<(), StoreError> {
        store
            .insert_keystone_role(&KeystoneRoleRecord {
                id: id.to_owned(),
                name: format!("name-{id}"),
                description: None,
                created_at: NOW.to_owned(),
            })
            .await
    }

    fn assignment(id: &str, user: &str, project: &str, role: &str) -> KeystoneRoleAssignmentRecord {
        KeystoneRoleAssignmentRecord {
            id: id.to_owned(),
            user_id: user.to_owned(),
            project_id: project.to_owned(),
            role_id: role.to_owned(),
            created_at: NOW.to_owned(),
        }
    }

    fn audit_event(tag: &str) -> AuditEventRecord {
        AuditEventRecord {
            event_id: format!("gov-audit-{tag}-{}", uuid::Uuid::now_v7()),
            timestamp: NOW.to_owned(),
            request_id: "gov-request".to_owned(),
            audit_id: "gov-audit".to_owned(),
            principal_id: "operator".to_owned(),
            principal_kind: "user".to_owned(),
            effective_scope: "system".to_owned(),
            service: "identity".to_owned(),
            action: "identity:CreateRoleAssignment".to_owned(),
            resource_type: Some("identity:role_assignment".to_owned()),
            resource_id: None,
            owner_scope: None,
            operation_id: None,
            outcome: "succeeded".to_owned(),
            reason_category: None,
        }
    }

    async fn audit_count(store: &SqliteStore) -> Result<i64, StoreError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
            .fetch_one(&store.pool)
            .await
            .map_err(StoreError::Database)
    }

    #[tokio::test]
    async fn governance_collections_page_without_overlap() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        for i in 1..=3 {
            seed_project(&store, &format!("proj-{i}"), true).await?;
            seed_role(&store, &format!("role-{i}")).await?;
        }

        let first = store.list_governance_projects_page(None, 2).await?;
        assert_eq!(
            first
                .items
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["proj-1", "proj-2"]
        );
        assert!(first.has_more);
        assert_eq!(first.continuation_key.as_deref(), Some("proj-2"));

        let second = store
            .list_governance_projects_page(first.continuation_key.as_deref(), 2)
            .await?;
        assert_eq!(
            second
                .items
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["proj-3"]
        );
        assert!(!second.has_more);
        assert_eq!(second.continuation_key, None);

        let roles = store.list_governance_roles_page(None, 2).await?;
        assert_eq!(
            roles
                .items
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["role-1", "role-2"]
        );
        assert!(roles.has_more);

        assert!(store.list_governance_projects_page(None, 0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn governance_references_report_missing_and_disabled() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-on", true).await?;
        seed_user(&store, "user-off", false).await?;
        seed_project(&store, "proj-on", true).await?;
        seed_project(&store, "proj-off", false).await?;
        seed_role(&store, "role-1").await?;

        let missing = store
            .check_governance_references("missing-user", "proj-on", "role-1")
            .await?;
        assert_eq!(
            missing,
            GovernanceReferences {
                principal_exists: false,
                principal_enabled: false,
                project_exists: true,
                project_enabled: true,
                role_exists: true,
            }
        );

        let enabled = store
            .check_governance_references("user-on", "proj-on", "role-1")
            .await?;
        assert!(enabled.principal_exists && enabled.principal_enabled);
        assert!(enabled.project_exists && enabled.project_enabled);
        assert!(enabled.role_exists);

        let disabled_principal = store
            .check_governance_references("user-off", "proj-on", "role-1")
            .await?;
        assert!(disabled_principal.principal_exists);
        assert!(!disabled_principal.principal_enabled);

        let disabled_project = store
            .check_governance_references("user-on", "proj-off", "role-1")
            .await?;
        assert!(disabled_project.project_exists);
        assert!(!disabled_project.project_enabled);

        let no_role = store
            .check_governance_references("user-on", "proj-on", "missing-role")
            .await?;
        assert!(!no_role.role_exists);
        Ok(())
    }

    #[tokio::test]
    async fn governance_principal_projection_hides_credentials() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_project(&store, "proj-1", true).await?;
        store
            .insert_keystone_role(&KeystoneRoleRecord {
                id: "role-service".to_owned(),
                name: "service".to_owned(),
                description: None,
                created_at: NOW.to_owned(),
            })
            .await?;

        let plain = store
            .get_governance_principal("user-1")
            .await?
            .ok_or_else(|| StoreError::Corrupt("principal missing".to_owned()))?;
        assert!(!plain.service);
        assert_eq!(plain.email.as_deref(), Some("user-1@example.test"));

        assert_eq!(store.get_governance_principal("service-user").await?, None);

        store
            .insert_keystone_user(&KeystoneUserRecord {
                id: "service-user".to_owned(),
                domain_id: "default".to_owned(),
                name: "service-user".to_owned(),
                password_hash: "pbkdf2_sha256$1$test".to_owned(),
                email: None,
                enabled: true,
                created_at: NOW.to_owned(),
            })
            .await?;
        let service_id = "service-user".to_owned();
        store
            .create_role_assignment_with_audit(
                &assignment("svc-assign", &service_id, "proj-1", "role-service"),
                &audit_event("service"),
            )
            .await?;
        let projected = store
            .get_governance_principal(&service_id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("service principal missing".to_owned()))?;
        assert!(projected.service);
        Ok(())
    }

    #[tokio::test]
    async fn governance_role_assignment_create_is_idempotent_with_required_audit()
    -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_project(&store, "proj-1", true).await?;
        seed_role(&store, "role-1").await?;

        let created = store
            .create_role_assignment_with_audit(
                &assignment("assign-1", "user-1", "proj-1", "role-1"),
                &audit_event("create"),
            )
            .await?;
        assert_eq!(
            created,
            CreateAssignmentOutcome::Created(assignment("assign-1", "user-1", "proj-1", "role-1"))
        );

        // A replay with a fresh id but the same durable triple converges on the
        // original row and does not write a second audit event.
        let replay = store
            .create_role_assignment_with_audit(
                &assignment("assign-2", "user-1", "proj-1", "role-1"),
                &audit_event("replay"),
            )
            .await?;
        let existing = match replay {
            CreateAssignmentOutcome::Existing(existing) => existing,
            CreateAssignmentOutcome::Created(_) => {
                return Err(StoreError::Corrupt(
                    "replayed assignment was reported as created".to_owned(),
                ));
            }
        };
        assert_eq!(existing.id, "assign-1");
        assert_eq!(audit_count(&store).await?, 1);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keystone_role_assignments")
            .fetch_one(&store.pool)
            .await
            .map_err(StoreError::Database)?;
        assert_eq!(rows, 1);
        Ok(())
    }

    #[tokio::test]
    async fn governance_create_rolls_back_when_required_audit_fails() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_project(&store, "proj-1", true).await?;
        seed_role(&store, "role-1").await?;

        sqlx::query(
            "CREATE TRIGGER governance_test_fail_audit BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END",
        )
        .execute(&store.pool)
        .await
        .map_err(StoreError::Database)?;

        let failed = store
            .create_role_assignment_with_audit(
                &assignment("assign-1", "user-1", "proj-1", "role-1"),
                &audit_event("fail"),
            )
            .await;
        assert!(matches!(failed, Err(StoreError::Database(_))));
        assert_eq!(store.get_governance_assignment("assign-1").await?, None);
        assert_eq!(audit_count(&store).await?, 0);

        sqlx::query("DROP TRIGGER governance_test_fail_audit")
            .execute(&store.pool)
            .await
            .map_err(StoreError::Database)?;
        let recovered = store
            .create_role_assignment_with_audit(
                &assignment("assign-1", "user-1", "proj-1", "role-1"),
                &audit_event("recovered"),
            )
            .await?;
        assert!(matches!(recovered, CreateAssignmentOutcome::Created(_)));
        assert_eq!(audit_count(&store).await?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn governance_delete_rolls_back_when_required_audit_fails() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_project(&store, "proj-1", true).await?;
        seed_role(&store, "role-1").await?;

        let absent = store
            .delete_role_assignment_with_audit("missing", &audit_event("absent"))
            .await?;
        assert!(!absent);

        store
            .create_role_assignment_with_audit(
                &assignment("assign-1", "user-1", "proj-1", "role-1"),
                &audit_event("create"),
            )
            .await?;

        sqlx::query(
            "CREATE TRIGGER governance_test_fail_delete_audit BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END",
        )
        .execute(&store.pool)
        .await
        .map_err(StoreError::Database)?;
        let failed = store
            .delete_role_assignment_with_audit("assign-1", &audit_event("fail-delete"))
            .await;
        assert!(matches!(failed, Err(StoreError::Database(_))));
        assert!(store.get_governance_assignment("assign-1").await?.is_some());
        assert_eq!(audit_count(&store).await?, 1);

        sqlx::query("DROP TRIGGER governance_test_fail_delete_audit")
            .execute(&store.pool)
            .await
            .map_err(StoreError::Database)?;
        let deleted = store
            .delete_role_assignment_with_audit("assign-1", &audit_event("delete"))
            .await?;
        assert!(deleted);
        assert_eq!(store.get_governance_assignment("assign-1").await?, None);
        assert_eq!(audit_count(&store).await?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn governance_assignment_filter_and_role_grants() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_user(&store, "user-2", true).await?;
        seed_project(&store, "proj-1", true).await?;
        seed_project(&store, "proj-2", true).await?;
        seed_role(&store, "role-a").await?;
        seed_role(&store, "role-b").await?;

        for (id, user, project, role) in [
            ("a-1", "user-1", "proj-1", "role-a"),
            ("a-2", "user-1", "proj-2", "role-b"),
            ("a-3", "user-2", "proj-1", "role-a"),
        ] {
            store
                .create_role_assignment_with_audit(
                    &assignment(id, user, project, role),
                    &audit_event(id),
                )
                .await?;
        }

        let filter = GovernanceAssignmentFilter {
            principal_id: Some("user-1".to_owned()),
            project_id: None,
            role_id: Some("role-b".to_owned()),
        };
        let filtered = store
            .list_governance_assignments_page(&filter, None, 10)
            .await?;
        assert_eq!(
            filtered
                .items
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["a-2"]
        );
        assert!(!filtered.has_more);

        let grants = store.get_governance_role_grants("user-1").await?;
        assert_eq!(
            grants,
            vec![
                GovernanceRoleGrantRecord {
                    project_id: "proj-1".to_owned(),
                    role_name: "name-role-a".to_owned(),
                },
                GovernanceRoleGrantRecord {
                    project_id: "proj-2".to_owned(),
                    role_name: "name-role-b".to_owned(),
                },
            ]
        );
        assert_eq!(store.get_governance_role_grants("user-2").await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn governance_operator_assignment_is_mutually_audited() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;

        let operator = OperatorAssignmentRecord {
            id: "operator-1".to_owned(),
            user_id: "user-1".to_owned(),
            profile: "operator".to_owned(),
            enabled: true,
            created_at: NOW.to_owned(),
            updated_at: NOW.to_owned(),
        };
        assert!(
            store
                .create_operator_assignment_with_audit(&operator, &audit_event("operator"))
                .await?
        );
        let duplicate = OperatorAssignmentRecord {
            id: "operator-2".to_owned(),
            ..operator.clone()
        };
        assert!(
            !store
                .create_operator_assignment_with_audit(&duplicate, &audit_event("operator-dup"))
                .await?
        );
        assert_eq!(
            store
                .get_governance_operator_assignment("operator-1")
                .await?,
            Some(operator)
        );
        assert_eq!(audit_count(&store).await?, 1);
        assert!(
            store
                .delete_operator_assignment_with_audit("operator-1", &audit_event("operator-del"))
                .await?
        );
        assert_eq!(
            store
                .get_governance_operator_assignment("operator-1")
                .await?,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn governance_concurrent_identical_assignments_converge()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::path::PathBuf::from(format!(
            "/tmp/o3k-governance-race-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = crate::testkit::open_file(&path).await?;
        seed_domain(&store).await?;
        seed_user(&store, "user-1", true).await?;
        seed_project(&store, "proj-1", true).await?;
        seed_role(&store, "role-1").await?;

        let first = store.clone();
        let second = store.clone();
        let task_a = tokio::spawn(async move {
            first
                .create_role_assignment_with_audit(
                    &assignment("assign-a", "user-1", "proj-1", "role-1"),
                    &audit_event("race-a"),
                )
                .await
        });
        let task_b = tokio::spawn(async move {
            second
                .create_role_assignment_with_audit(
                    &assignment("assign-b", "user-1", "proj-1", "role-1"),
                    &audit_event("race-b"),
                )
                .await
        });
        let (a, b) = tokio::join!(task_a, task_b);
        let a = a?;
        let b = b?;
        let created = [&a, &b]
            .iter()
            .filter(|outcome| matches!(outcome, Ok(CreateAssignmentOutcome::Created(_))))
            .count();
        let existing = [&a, &b]
            .iter()
            .filter(|outcome| matches!(outcome, Ok(CreateAssignmentOutcome::Existing(_))))
            .count();
        assert_eq!(
            (created, existing),
            (1, 1),
            "concurrent creates must converge to one durable row: {a:?} / {b:?}"
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keystone_role_assignments")
            .fetch_one(&store.pool)
            .await?;
        assert_eq!(rows, 1);
        assert_eq!(audit_count(&store).await?, 1);
        drop(store);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}
