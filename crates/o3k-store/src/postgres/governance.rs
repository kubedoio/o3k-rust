use async_trait::async_trait;
use sqlx::Row;

use super::helpers::map_pg_error;
use crate::{
    GovernanceRepository, KeystoneProjectRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord,
    KeystoneUserRecord, PostgresStore, StoreError,
};

#[async_trait]
impl GovernanceRepository for PostgresStore {
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
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "{}\n{}\n{}",
                request.owner_scope, request.action, request.key
            ))
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if let Some(row) = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3")
            .bind(&request.owner_scope)
            .bind(&request.action)
            .bind(&request.key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
        {
            let fp: String = row.get("fingerprint");
            let existing = uuid::Uuid::parse_str(&row.get::<String, _>("operation_id"))
                .map_err(StoreError::InvalidUuid)?;
            if fp != request.fingerprint {
                tx.commit().await.map_err(StoreError::Database)?;
                return Ok(crate::IdempotencyReservation::Conflict);
            } else if existing != operation.id {
                return Err(StoreError::Corrupt("governance replay operation mismatch".into()));
            }
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(crate::IdempotencyReservation::ExistingEquivalent(existing));
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keystone_role_assignments WHERE id=$1 AND user_id=$2 AND project_id=$3 AND role_id=$4")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if exists != 1 {
            return Err(StoreError::ResourceNotFound);
        }
        sqlx::query("INSERT INTO operations (id,resource_id,kind,state,provider_operation_id,error_category,error_message) VALUES ($1,$2,$3,$4,$5,$6,$7)").bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind).bind(operation.state.as_str()).bind(&operation.provider_operation_id).bind(&operation.error_category).bind(&operation.error_message).execute(&mut *tx).await.map_err(map_pg_error)?;
        sqlx::query("INSERT INTO canonical_operation_metadata (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,created_at,started_at,finished_at,error,request_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)").bind(canonical.id.to_string()).bind(&canonical.service).bind(&canonical.action).bind(&canonical.actor).bind(&canonical.owner_scope).bind(&canonical.resource_type).bind(&canonical.resource_id).bind(i32::try_from(canonical.attempt).map_err(|_| StoreError::Corrupt("operation attempt overflow".into()))?).bind(&canonical.created_at).bind(&canonical.started_at).bind(&canonical.finished_at).bind(&canonical.error).bind(&canonical.request_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
        sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES ($1,$2,$3,$4,$5)").bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint).bind(request.operation_id.to_string()).execute(&mut *tx).await.map_err(map_pg_error)?;
        sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)").bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(audit.operation_id.map(|id| id.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json).execute(&mut *tx).await.map_err(map_pg_error)?;
        sqlx::query("DELETE FROM keystone_role_assignments WHERE id=$1 AND user_id=$2 AND project_id=$3 AND role_id=$4").bind(&assignment.id).bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(crate::IdempotencyReservation::Created(operation.id))
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
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "{}\n{}\n{}",
                request.owner_scope, request.action, request.key
            ))
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if let Some(row) = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3").bind(&request.owner_scope).bind(&request.action).bind(&request.key).fetch_optional(&mut *tx).await.map_err(StoreError::Database)? {
            let fingerprint: String = sqlx::Row::get(&row, "fingerprint");
            let existing = uuid::Uuid::parse_str(&sqlx::Row::get::<String, _>(&row, "operation_id")).map_err(StoreError::InvalidUuid)?;
            if fingerprint != request.fingerprint { tx.commit().await.map_err(StoreError::Database)?; return Ok(crate::IdempotencyReservation::Conflict); }
            if existing != operation.id { return Err(StoreError::Corrupt("governance replay operation mismatch".into())); }
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(crate::IdempotencyReservation::ExistingEquivalent(existing));
        }
        sqlx::query("INSERT INTO keystone_role_assignments (id,user_id,project_id,role_id,created_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(user_id,project_id,role_id) DO NOTHING").bind(&assignment.id).bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).bind(&assignment.created_at).execute(&mut *tx).await.map_err(crate::postgres::helpers::map_pg_error)?;
        let actual: String = sqlx::query_scalar("SELECT id FROM keystone_role_assignments WHERE user_id=$1 AND project_id=$2 AND role_id=$3").bind(&assignment.user_id).bind(&assignment.project_id).bind(&assignment.role_id).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
        if actual != assignment.id {
            return Err(StoreError::IdempotencyConflict);
        }
        sqlx::query("INSERT INTO operations (id,resource_id,kind,state,provider_operation_id,error_category,error_message) VALUES ($1,$2,$3,$4,$5,$6,$7)").bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind).bind(operation.state.as_str()).bind(&operation.provider_operation_id).bind(&operation.error_category).bind(&operation.error_message).execute(&mut *tx).await.map_err(crate::postgres::helpers::map_pg_error)?;
        sqlx::query("INSERT INTO canonical_operation_metadata (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,created_at,started_at,finished_at,error,request_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)").bind(canonical.id.to_string()).bind(&canonical.service).bind(&canonical.action).bind(&canonical.actor).bind(&canonical.owner_scope).bind(&canonical.resource_type).bind(&canonical.resource_id).bind(i32::try_from(canonical.attempt).map_err(|_| StoreError::Corrupt("operation attempt overflow".into()))?).bind(&canonical.created_at).bind(&canonical.started_at).bind(&canonical.finished_at).bind(&canonical.error).bind(&canonical.request_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
        sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES ($1,$2,$3,$4,$5)").bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint).bind(request.operation_id.to_string()).execute(&mut *tx).await.map_err(crate::postgres::helpers::map_pg_error)?;
        sqlx::query("INSERT INTO audit_events (event_id,timestamp,request_id,audit_id,principal_id,effective_scope,service_namespace,action,resource_type,resource_id,owner_scope,operation_id,outcome,reason_category,event_json) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)").bind(&audit.event_id).bind(&audit.timestamp).bind(&audit.request_id).bind(&audit.audit_id).bind(&audit.principal_id).bind(&audit.effective_scope).bind(&audit.service_namespace).bind(&audit.action).bind(&audit.resource_type).bind(&audit.resource_id).bind(&audit.owner_scope).bind(audit.operation_id.map(|id| id.to_string())).bind(&audit.outcome).bind(&audit.reason_category).bind(&audit.event_json).execute(&mut *tx).await.map_err(crate::postgres::helpers::map_pg_error)?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(crate::IdempotencyReservation::Created(operation.id))
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
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id, project_id, role_id) DO NOTHING",
        )
        .bind(&assignment.id)
        .bind(&assignment.user_id)
        .bind(&assignment.project_id)
        .bind(&assignment.role_id)
        .bind(&assignment.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        sqlx::query(
            "SELECT id, user_id, project_id, role_id, created_at
             FROM keystone_role_assignments
             WHERE user_id = $1 AND project_id = $2 AND role_id = $3",
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
        sqlx::query("SELECT id,user_id,project_id,role_id,created_at FROM keystone_role_assignments WHERE id=$1").bind(id).fetch_optional(&self.pool).await.map_err(StoreError::Database).map(|r| r.map(|row| KeystoneRoleAssignmentRecord { id: row.get("id"), user_id: row.get("user_id"), project_id: row.get("project_id"), role_id: row.get("role_id"), created_at: row.get("created_at") }))
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
            "DELETE FROM keystone_role_assignments WHERE user_id = $1 AND project_id = $2 AND role_id = $3",
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
        sqlx::query("SELECT id, domain_id, name, email, enabled, created_at FROM keystone_users WHERE id = $1")
            .bind(id).fetch_optional(&self.pool).await.map_err(StoreError::Database).map(|row| row.map(|row| KeystoneUserRecord { id: row.get("id"), domain_id: row.get("domain_id"), name: row.get("name"), password_hash: String::new(), email: row.get("email"), enabled: row.get::<i32, _>("enabled") != 0, created_at: row.get("created_at") }))
    }

    async fn get_project(&self, id: &str) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = $1")
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
             FROM keystone_projects WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2",
        )
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
             FROM keystone_users WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2",
        )
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
        let rows = sqlx::query("SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2")
            .bind(after_id).bind(limit).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
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
             WHERE ($1::text IS NULL OR id > $1) ORDER BY id ASC LIMIT $2",
        )
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
