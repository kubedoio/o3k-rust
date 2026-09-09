use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    GovernanceRepository, KeystoneProjectRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord,
    KeystoneUserRecord, SqliteStore, StoreError,
};

#[async_trait]
impl GovernanceRepository for SqliteStore {
    async fn mutate_role_assignment_removal(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
        audit: &crate::AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, StoreError> {
        if operation.id != request.operation_id
            || operation.resource_id.to_string() != assignment.id
        {
            return Err(StoreError::Corrupt(
                "governance operation identity mismatch".into(),
            ));
        }
        crate::validate_canonical_idempotent_operation_identity(operation, canonical, request)?;
        let mut c = self.pool.acquire().await.map_err(StoreError::Database)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *c)
            .await
            .map_err(StoreError::Database)?;
        let result: Result<crate::IdempotencyReservation, StoreError> = async {
            if let Some(row) = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=? AND action=? AND idempotency_key=?").bind(&request.owner_scope).bind(&request.action).bind(&request.key).fetch_optional(&mut *c).await.map_err(StoreError::Database)? {
                let fp: String = row.get("fingerprint"); let existing = uuid::Uuid::parse_str(&row.get::<String,_>("operation_id")).map_err(StoreError::InvalidUuid)?;
                if fp != request.fingerprint { return Ok(crate::IdempotencyReservation::Conflict); }
                if existing != operation.id { return Err(StoreError::Corrupt("governance replay operation mismatch".into())); }
                return Ok(crate::IdempotencyReservation::ExistingEquivalent(existing));
            }
            let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keystone_role_assignments WHERE id=? AND user_id=? AND project_id=? AND role_id=?").bind(&assignment.id).bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).fetch_one(&mut *c).await.map_err(StoreError::Database)?;
            if exists != 1 { return Err(StoreError::ResourceNotFound); }
            sqlx::query("INSERT INTO operations (id,resource_id,kind,state,provider_operation_id,error_category,error_message) VALUES (?,?,?,?,?,?,?)").bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind).bind(operation.state.as_str()).bind(&operation.provider_operation_id).bind(&operation.error_category).bind(&operation.error_message).execute(&mut *c).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO canonical_operation_metadata (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,created_at,started_at,finished_at,error,request_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)").bind(canonical.id.to_string()).bind(&canonical.service).bind(&canonical.action).bind(&canonical.actor).bind(&canonical.owner_scope).bind(&canonical.resource_type).bind(&canonical.resource_id).bind(i64::from(canonical.attempt)).bind(&canonical.created_at).bind(&canonical.started_at).bind(&canonical.finished_at).bind(&canonical.error).bind(&canonical.request_id).execute(&mut *c).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES (?,?,?,?,?)").bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint).bind(request.operation_id.to_string()).execute(&mut *c).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)").bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(audit.operation_id.map(|id| id.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json).execute(&mut *c).await.map_err(StoreError::Database)?;
            sqlx::query("DELETE FROM keystone_role_assignments WHERE id=? AND user_id=? AND project_id=? AND role_id=?")
                .bind(&assignment.id).bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id)
                .execute(&mut *c).await.map_err(StoreError::Database)?;
            Ok(crate::IdempotencyReservation::Created(operation.id))
        }.await;
        match result {
            Ok(v) => {
                sqlx::query("COMMIT")
                    .execute(&mut *c)
                    .await
                    .map_err(StoreError::Database)?;
                Ok(v)
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *c).await;
                Err(e)
            }
        }
    }
    async fn mutate_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
        audit: &crate::AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, StoreError> {
        if operation.id != request.operation_id
            || operation.resource_id.to_string() != assignment.id
        {
            return Err(StoreError::Corrupt(
                "governance operation identity mismatch".into(),
            ));
        }
        crate::validate_canonical_idempotent_operation_identity(operation, canonical, request)?;
        let mut connection = self.pool.acquire().await.map_err(StoreError::Database)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(StoreError::Database)?;
        let result: Result<crate::IdempotencyReservation, StoreError> = async {
            if let Some(row) = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=? AND action=? AND idempotency_key=?")
                .bind(&request.owner_scope).bind(&request.action).bind(&request.key)
                .fetch_optional(&mut *connection).await.map_err(StoreError::Database)? {
                let fingerprint: String = row.get("fingerprint");
                let existing = Uuid::parse_str(&row.get::<String, _>("operation_id")).map_err(StoreError::InvalidUuid)?;
                if fingerprint != request.fingerprint { return Ok(crate::IdempotencyReservation::Conflict); }
                if existing != operation.id { return Err(StoreError::Corrupt("governance replay operation mismatch".into())); }
                return Ok(crate::IdempotencyReservation::ExistingEquivalent(existing));
            }
            sqlx::query("INSERT INTO keystone_role_assignments (id,user_id,project_id,role_id,created_at) VALUES (?,?,?,?,?) ON CONFLICT(user_id,project_id,role_id) DO NOTHING")
                .bind(&assignment.id).bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).bind(&assignment.created_at)
                .execute(&mut *connection).await.map_err(StoreError::Database)?;
            let actual: String = sqlx::query_scalar("SELECT id FROM keystone_role_assignments WHERE user_id=? AND project_id=? AND role_id=?")
                .bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).fetch_one(&mut *connection).await.map_err(StoreError::Database)?;
            if actual != assignment.id { return Err(StoreError::IdempotencyConflict); }
            sqlx::query("INSERT INTO operations (id,resource_id,kind,state,provider_operation_id,error_category,error_message) VALUES (?,?,?,?,?,?,?)")
                .bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind).bind(operation.state.as_str()).bind(&operation.provider_operation_id).bind(&operation.error_category).bind(&operation.error_message)
                .execute(&mut *connection).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO canonical_operation_metadata (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,created_at,started_at,finished_at,error,request_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(canonical.id.to_string()).bind(&canonical.service).bind(&canonical.action).bind(&canonical.actor).bind(&canonical.owner_scope).bind(&canonical.resource_type).bind(&canonical.resource_id).bind(i64::from(canonical.attempt)).bind(&canonical.created_at).bind(&canonical.started_at).bind(&canonical.finished_at).bind(&canonical.error).bind(&canonical.request_id)
                .execute(&mut *connection).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES (?,?,?,?,?)")
                .bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint).bind(request.operation_id.to_string()).execute(&mut *connection).await.map_err(StoreError::Database)?;
            sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(audit.operation_id.map(|id| id.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json).execute(&mut *connection).await.map_err(StoreError::Database)?;
            Ok(crate::IdempotencyReservation::Created(operation.id))
        }.await;
        match result {
            Ok(value) => {
                sqlx::query("COMMIT")
                    .execute(&mut *connection)
                    .await
                    .map_err(StoreError::Database)?;
                Ok(value)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }
    async fn ensure_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<KeystoneRoleAssignmentRecord, StoreError> {
        if assignment.id.trim().is_empty()
            || assignment.user_id.trim().is_empty()
            || assignment.project_id.trim().is_empty()
            || assignment.role_id.trim().is_empty()
        {
            return Err(StoreError::Corrupt(
                "role assignment identifiers must not be empty".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO keystone_role_assignments
             (id, user_id, project_id, role_id, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(user_id, project_id, role_id) DO NOTHING",
        )
        .bind(&assignment.id)
        .bind(&assignment.user_id)
        .bind(&assignment.project_id)
        .bind(&assignment.role_id)
        .bind(&assignment.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        sqlx::query(
            "SELECT id, user_id, project_id, role_id, created_at
             FROM keystone_role_assignments
             WHERE user_id = ? AND project_id = ? AND role_id = ?",
        )
        .bind(&assignment.user_id)
        .bind(&assignment.project_id)
        .bind(&assignment.role_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?
        .map(|row| KeystoneRoleAssignmentRecord {
            id: row.get("id"),
            user_id: row.get("user_id"),
            project_id: row.get("project_id"),
            role_id: row.get("role_id"),
            created_at: row.get("created_at"),
        })
        .ok_or(StoreError::ResourceNotFound)
    }
    async fn get_role_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError> {
        sqlx::query("SELECT id,user_id,project_id,role_id,created_at FROM keystone_role_assignments WHERE id=?").bind(id).fetch_optional(&self.pool).await.map_err(StoreError::Database).map(|r| r.map(|row| KeystoneRoleAssignmentRecord { id: row.get("id"), user_id: row.get("user_id"), project_id: row.get("project_id"), role_id: row.get("role_id"), created_at: row.get("created_at") }))
    }

    async fn remove_role_assignment(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<bool, StoreError> {
        if principal_id.is_empty() || project_id.is_empty() || role_id.is_empty() {
            return Err(StoreError::Corrupt(
                "role assignment identifiers must not be empty".into(),
            ));
        }
        let result = sqlx::query(
            "DELETE FROM keystone_role_assignments WHERE user_id = ? AND project_id = ? AND role_id = ?",
        )
        .bind(principal_id)
        .bind(project_id)
        .bind(role_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(result.rows_affected() == 1)
    }

    async fn get_principal(&self, id: &str) -> Result<Option<KeystoneUserRecord>, StoreError> {
        sqlx::query("SELECT id, domain_id, name, email, enabled, created_at FROM keystone_users WHERE id = ?")
            .bind(id).fetch_optional(&self.pool).await.map_err(StoreError::Database).map(|row| row.map(|row| KeystoneUserRecord { id: row.get("id"), domain_id: row.get("domain_id"), name: row.get("name"), password_hash: String::new(), email: row.get("email"), enabled: row.get::<i32, _>("enabled") != 0, created_at: row.get("created_at") }))
    }

    async fn get_project(&self, id: &str) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = ?")
            .bind(id).fetch_optional(&self.pool).await.map_err(StoreError::Database).map(|row| row.map(|row| KeystoneProjectRecord { id: row.get("id"), domain_id: row.get("domain_id"), name: row.get("name"), description: row.get("description"), enabled: row.get::<i32, _>("enabled") != 0, created_at: row.get("created_at") }))
    }

    async fn list_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("project page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "project page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT id, domain_id, name, description, enabled, created_at
             FROM keystone_projects WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?",
        )
        .bind(after_id)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|row| KeystoneProjectRecord {
                id: row.get("id"),
                domain_id: row.get("domain_id"),
                name: row.get("name"),
                description: row.get("description"),
                enabled: row.get::<i32, _>("enabled") != 0,
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn list_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::KeystoneUserRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("principal page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "principal page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT id, domain_id, name, email, enabled, created_at
             FROM keystone_users WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?",
        )
        .bind(after_id)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|row| crate::KeystoneUserRecord {
                id: row.get("id"),
                domain_id: row.get("domain_id"),
                name: row.get("name"),
                password_hash: String::new(),
                email: row.get("email"),
                enabled: row.get::<i32, _>("enabled") != 0,
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn list_role_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::KeystoneRoleAssignmentRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("assignment page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "assignment page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?")
            .bind(after_id).bind(after_id).bind(limit).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|row| crate::KeystoneRoleAssignmentRecord {
                id: row.get("id"),
                user_id: row.get("user_id"),
                project_id: row.get("project_id"),
                role_id: row.get("role_id"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn list_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StoreError::Corrupt("role page limit overflow".into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::Corrupt(
                "role page limit outside 1..=1000".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles
             WHERE (? IS NULL OR id > ?) ORDER BY id ASC LIMIT ?",
        )
        .bind(after_id)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|row| KeystoneRoleRecord {
                id: row.get("id"),
                name: row.get("name"),
                description: row.get("description"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn project_governance_page_is_bounded_and_cursor_ordered() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        store
            .insert_keystone_domain(&crate::KeystoneDomainRecord {
                id: "default".into(),
                name: "Default".into(),
                description: None,
                enabled: true,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .await?;
        for (id, name) in [("project-a", "A"), ("project-b", "B"), ("project-c", "C")] {
            store
                .insert_keystone_project(&KeystoneProjectRecord {
                    id: id.into(),
                    domain_id: "default".into(),
                    name: name.into(),
                    description: None,
                    enabled: true,
                    created_at: "2026-01-01T00:00:00Z".into(),
                })
                .await?;
        }
        assert_eq!(store.list_projects_page(None, 2).await?.len(), 2);
        assert_eq!(
            store.list_projects_page(Some("project-a"), 2).await?[0].id,
            "project-b"
        );
        assert!(store.list_projects_page(None, 1001).await.is_err());
        for (id, name) in [("role-a", "reader"), ("role-b", "operator")] {
            store
                .insert_keystone_role(&crate::KeystoneRoleRecord {
                    id: id.into(),
                    name: name.into(),
                    description: None,
                    created_at: "2026-01-01T00:00:00Z".into(),
                })
                .await?;
        }
        assert_eq!(store.list_roles_page(None, 1).await?.len(), 1);
        assert_eq!(
            store.list_roles_page(Some("role-a"), 1).await?[0].id,
            "role-b"
        );
        assert!(store.list_roles_page(None, 1001).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn ensure_role_assignment_converges_to_canonical_row() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let now = "2026-01-01T00:00:00Z";
        store
            .insert_keystone_domain(&crate::KeystoneDomainRecord {
                id: "default".into(),
                name: "Default".into(),
                description: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_project(&KeystoneProjectRecord {
                id: "project-a".into(),
                domain_id: "default".into(),
                name: "A".into(),
                description: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_user(&crate::KeystoneUserRecord {
                id: "user-a".into(),
                domain_id: "default".into(),
                name: "A".into(),
                password_hash: "redacted-test-hash".into(),
                email: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_role(&KeystoneRoleRecord {
                id: "role-reader".into(),
                name: "reader".into(),
                description: None,
                created_at: now.into(),
            })
            .await?;
        let first = KeystoneRoleAssignmentRecord {
            id: "assignment-canonical".into(),
            user_id: "user-a".into(),
            project_id: "project-a".into(),
            role_id: "role-reader".into(),
            created_at: now.into(),
        };
        let returned = store.ensure_role_assignment(&first).await?;
        assert_eq!(returned, first);
        let retry = store
            .ensure_role_assignment(&KeystoneRoleAssignmentRecord {
                id: "different-retry-id".into(),
                ..first.clone()
            })
            .await?;
        assert_eq!(retry, first);
        assert_eq!(store.list_role_assignments_page(None, 10).await?.len(), 1);
        assert!(
            store
                .remove_role_assignment("user-a", "project-a", "role-reader")
                .await?
        );
        assert!(
            !store
                .remove_role_assignment("user-a", "project-a", "role-reader")
                .await?
        );
        assert!(
            store
                .ensure_role_assignment(&KeystoneRoleAssignmentRecord {
                    id: "bad".into(),
                    user_id: "".into(),
                    project_id: "project-a".into(),
                    role_id: "role-reader".into(),
                    created_at: now.into(),
                })
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn role_assignment_mutation_is_atomic_and_replay_safe() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let now = "2026-01-01T00:00:00Z";
        store
            .insert_keystone_domain(&crate::KeystoneDomainRecord {
                id: "d".into(),
                name: "D".into(),
                description: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_project(&KeystoneProjectRecord {
                id: "p".into(),
                domain_id: "d".into(),
                name: "P".into(),
                description: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_user(&crate::KeystoneUserRecord {
                id: "u".into(),
                domain_id: "d".into(),
                name: "U".into(),
                password_hash: "hash".into(),
                email: None,
                enabled: true,
                created_at: now.into(),
            })
            .await?;
        store
            .insert_keystone_role(&KeystoneRoleRecord {
                id: "r".into(),
                name: "reader".into(),
                description: None,
                created_at: now.into(),
            })
            .await?;
        let assignment_id = uuid::Uuid::new_v4();
        let assignment = KeystoneRoleAssignmentRecord {
            id: assignment_id.to_string(),
            user_id: "u".into(),
            project_id: "p".into(),
            role_id: "r".into(),
            created_at: now.into(),
        };
        let operation = crate::OperationRecord {
            id: uuid::Uuid::new_v4(),
            resource_id: assignment_id,
            kind: "iam:assign_role".into(),
            state: crate::OperationState::Succeeded,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let canonical = crate::CanonicalOperationRecord {
            id: operation.id,
            service: "iam".into(),
            action: "iam:AssignRole".into(),
            actor: "u".into(),
            owner_scope: "p".into(),
            resource_type: "iam:role_assignment".into(),
            resource_id: Some(assignment_id.to_string()),
            state: crate::OperationState::Succeeded,
            attempt: 1,
            created_at: now.into(),
            started_at: None,
            finished_at: Some(now.into()),
            error: None,
            request_id: Some("req".into()),
        };
        let request = crate::IdempotencyReservationRequest::from_semantics(
            "p",
            "iam:AssignRole",
            "key-1",
            "iam:role_assignment",
            Some(&assignment_id.to_string()),
            &serde_json::json!({"principal":"u","project":"p","role":"r"}),
            operation.id,
        )?;
        let audit = crate::AuditEventRecord {
            event_id: "audit-1".into(),
            timestamp: now.into(),
            request_id: "req".into(),
            audit_id: "audit".into(),
            principal_id: "u".into(),
            effective_scope: "system".into(),
            service_namespace: "iam".into(),
            action: "iam:AssignRole".into(),
            resource_type: Some("iam:role_assignment".into()),
            resource_id: Some(assignment_id.to_string()),
            owner_scope: Some("p".into()),
            operation_id: Some(operation.id),
            outcome: "succeeded".into(),
            reason_category: None,
            event_json: "{\"safe\":true}".into(),
        };
        assert_eq!(
            store
                .mutate_role_assignment(&assignment, &operation, &canonical, &request, &audit)
                .await?,
            crate::IdempotencyReservation::Created(operation.id)
        );
        assert_eq!(store.list_role_assignments_page(None, 10).await?.len(), 1);
        assert_eq!(
            store
                .mutate_role_assignment(&assignment, &operation, &canonical, &request, &audit)
                .await?,
            crate::IdempotencyReservation::ExistingEquivalent(operation.id)
        );
        assert_eq!(store.list_role_assignments_page(None, 10).await?.len(), 1);
        let conflict = crate::IdempotencyReservationRequest::from_semantics(
            "p",
            "iam:AssignRole",
            "key-1",
            "iam:role_assignment",
            Some(&assignment_id.to_string()),
            &serde_json::json!({"principal":"u","project":"p","role":"different"}),
            operation.id,
        )?;
        assert_eq!(
            store
                .mutate_role_assignment(&assignment, &operation, &canonical, &conflict, &audit)
                .await?,
            crate::IdempotencyReservation::Conflict
        );
        Ok(())
    }
}
