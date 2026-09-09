use chrono::Utc;
use std::sync::Arc;
use uuid::Uuid;

use o3k_native_api::{
    error::NativeReadError,
    governance::{
        GovernanceMutationError, RoleAssignmentCreateRequest, RoleAssignmentMutationResponse,
    },
    governance::{PrincipalView, ProjectView, RoleAssignmentView},
};
use o3k_store::{GovernanceRepository, O3kStore};

pub struct GovernanceReaderAdapter {
    pub store: Arc<O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::governance::GovernanceReader for GovernanceReaderAdapter {
    async fn show_principal(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<PrincipalView, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .get_principal(id)
            .await
            .map_err(|error| {
                tracing::error!(%error, "native principal governance show failed");
                NativeReadError::Internal
            })?
            .map(|principal| PrincipalView {
                id: principal.id,
                domain_id: principal.domain_id,
                name: principal.name,
                email: principal.email,
                enabled: principal.enabled,
                created_at: principal.created_at,
            })
            .ok_or(NativeReadError::NotFound)
    }

    async fn list_role_assignments_page(
        &self,
        auth: &o3k_kernel::AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RoleAssignmentView>, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .list_role_assignments_page(after_id, limit)
            .await
            .map_err(|error| {
                tracing::error!(%error, "native role assignment governance list failed");
                NativeReadError::Internal
            })
            .map(|assignments| {
                assignments
                    .into_iter()
                    .map(|assignment| RoleAssignmentView {
                        id: assignment.id,
                        principal_id: assignment.user_id,
                        project_id: assignment.project_id,
                        role_id: assignment.role_id,
                        created_at: assignment.created_at,
                    })
                    .collect()
            })
    }

    async fn list_principals_page(
        &self,
        auth: &o3k_kernel::AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PrincipalView>, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .list_principals_page(after_id, limit)
            .await
            .map_err(|error| {
                tracing::error!(%error, "native principal governance list failed");
                NativeReadError::Internal
            })
            .map(|principals| {
                principals
                    .into_iter()
                    .map(|principal| PrincipalView {
                        id: principal.id,
                        domain_id: principal.domain_id,
                        name: principal.name,
                        email: principal.email,
                        enabled: principal.enabled,
                        created_at: principal.created_at,
                    })
                    .collect()
            })
    }

    async fn show_project(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<ProjectView, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .get_project(id)
            .await
            .map_err(|error| {
                tracing::error!(%error, "native project governance show failed");
                NativeReadError::Internal
            })?
            .map(|project| ProjectView {
                id: project.id,
                domain_id: project.domain_id,
                name: project.name,
                description: project.description,
                enabled: project.enabled,
                created_at: project.created_at,
            })
            .ok_or(NativeReadError::NotFound)
    }

    async fn list_projects_page(
        &self,
        auth: &o3k_kernel::AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectView>, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .list_projects_page(after_id, limit)
            .await
            .map_err(|error| {
                tracing::error!(%error, "native project governance list failed");
                NativeReadError::Internal
            })
            .map(|projects| {
                projects
                    .into_iter()
                    .map(|project| ProjectView {
                        id: project.id,
                        domain_id: project.domain_id,
                        name: project.name,
                        description: project.description,
                        enabled: project.enabled,
                        created_at: project.created_at,
                    })
                    .collect()
            })
    }
}

#[async_trait::async_trait]
impl o3k_native_api::governance::GovernanceMutator for GovernanceReaderAdapter {
    async fn delete_role_assignment(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        idempotency_key: &str,
    ) -> Result<RoleAssignmentMutationResponse, GovernanceMutationError> {
        // Keep the authority boundary defensive even when this adapter is
        // invoked outside the HTTP router.  IAM governance is system scoped;
        // a project AuthContext must never be able to mutate assignments by
        // reaching the repository adapter directly.
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(GovernanceMutationError::Validation);
        }
        let assignment = self
            .store
            .get_role_assignment(id)
            .await
            .map_err(|_| GovernanceMutationError::Unavailable)?
            .ok_or(GovernanceMutationError::Validation)?;
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "iam:AssignRole:{}:{}:{}",
                auth.effective_scope().id(),
                id,
                idempotency_key
            )
            .as_bytes(),
        );
        let timestamp = Utc::now().to_rfc3339();
        let operation = o3k_store::OperationRecord {
            id: operation_id,
            resource_id: Uuid::parse_str(id).map_err(|_| GovernanceMutationError::Validation)?,
            kind: "iam:role-assignment:delete".into(),
            state: o3k_store::OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let kernel = o3k_kernel::Operation::new(
            operation_id,
            "iam",
            o3k_kernel::ActionId::new_unchecked("operator", "AssignRole"),
            auth.principal().id().to_string(),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(assignment.project_id.clone()),
                None,
                None,
            ),
            o3k_kernel::ResourceType::new_unchecked("iam", "role_assignment"),
            Some(o3k_kernel::ResourceId::new_unchecked(id.to_owned())),
            Some(auth.request_id().to_owned()),
        );
        let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(&kernel)
            .map_err(|_| GovernanceMutationError::Internal)?;
        let body = serde_json::json!({"assignment_id":id,"project_id":assignment.project_id,"principal_id":assignment.user_id,"role_id":assignment.role_id});
        let req = o3k_store::IdempotencyReservationRequest::from_semantics(
            &assignment.project_id,
            "iam:AssignRole",
            idempotency_key,
            "iam:role_assignment",
            Some(id),
            &body,
            operation_id,
        )
        .map_err(|_| GovernanceMutationError::Validation)?;
        let audit = o3k_store::AuditEventRecord {
            event_id: Uuid::new_v4().to_string(),
            timestamp: timestamp.clone(),
            request_id: auth.request_id().to_owned(),
            audit_id: auth.audit_id().to_owned(),
            principal_id: auth.principal().id().to_string(),
            effective_scope: auth.effective_scope().id().as_str().to_owned(),
            service_namespace: "iam".into(),
            action: "iam:AssignRole".into(),
            resource_type: Some("iam:role_assignment".into()),
            resource_id: Some(id.to_owned()),
            owner_scope: Some(assignment.project_id.clone()),
            operation_id: Some(operation_id),
            outcome: "succeeded".into(),
            reason_category: None,
            event_json: serde_json::json!({"event":"role_assignment.delete","resource_id":id})
                .to_string(),
        };
        self.store
            .mutate_role_assignment_removal(&assignment, &operation, &canonical, &req, &audit)
            .await
            .map_err(|e| match e {
                o3k_store::StoreError::IdempotencyConflict => GovernanceMutationError::Conflict,
                o3k_store::StoreError::ResourceNotFound => GovernanceMutationError::Validation,
                o3k_store::StoreError::Database(_) => GovernanceMutationError::Unavailable,
                _ => GovernanceMutationError::Internal,
            })?;
        Ok(RoleAssignmentMutationResponse {
            assignment: RoleAssignmentView {
                id: assignment.id,
                principal_id: assignment.user_id,
                project_id: assignment.project_id,
                role_id: assignment.role_id,
                created_at: assignment.created_at,
            },
            operation_id: operation_id.to_string(),
        })
    }
    async fn create_role_assignment(
        &self,
        auth: &o3k_kernel::AuthContext,
        request: RoleAssignmentCreateRequest,
        idempotency_key: &str,
    ) -> Result<RoleAssignmentMutationResponse, GovernanceMutationError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::System {
            return Err(GovernanceMutationError::Validation);
        }
        let assignment_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "{}:{}:{}",
                request.principal_id, request.project_id, request.role_id
            )
            .as_bytes(),
        );
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "iam:AssignRole:{}:{}:{}",
                auth.effective_scope().id(),
                request.project_id,
                idempotency_key
            )
            .as_bytes(),
        );
        let timestamp = Utc::now().to_rfc3339();
        let assignment = o3k_store::KeystoneRoleAssignmentRecord {
            id: assignment_id.to_string(),
            user_id: request.principal_id.clone(),
            project_id: request.project_id.clone(),
            role_id: request.role_id.clone(),
            created_at: timestamp.clone(),
        };
        let operation = o3k_store::OperationRecord {
            id: operation_id,
            resource_id: assignment_id,
            kind: "iam:role-assignment:create".into(),
            state: o3k_store::OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let kernel_operation = o3k_kernel::Operation::new(
            operation_id,
            "iam",
            o3k_kernel::ActionId::new_unchecked("operator", "AssignRole"),
            auth.principal().id().to_string(),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(request.project_id.clone()),
                None,
                None,
            ),
            o3k_kernel::ResourceType::new_unchecked("iam", "role_assignment"),
            Some(o3k_kernel::ResourceId::new_unchecked(
                assignment_id.to_string(),
            )),
            Some(auth.request_id().to_owned()),
        );
        let canonical =
            o3k_store::CanonicalOperationRecord::from_kernel_operation(&kernel_operation)
                .map_err(|_| GovernanceMutationError::Internal)?;
        let body = serde_json::json!({
            "principal_id": request.principal_id,
            "project_id": request.project_id,
            "role_id": request.role_id,
        });
        let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
            request.project_id.as_str(),
            "iam:AssignRole",
            idempotency_key,
            "iam:role_assignment",
            Some(&assignment_id.to_string()),
            &body,
            operation_id,
        )
        .map_err(|_| GovernanceMutationError::Validation)?;
        let audit = o3k_store::AuditEventRecord {
            event_id: Uuid::new_v4().to_string(),
            timestamp: timestamp.clone(),
            request_id: auth.request_id().to_owned(),
            audit_id: auth.audit_id().to_owned(),
            principal_id: auth.principal().id().to_string(),
            effective_scope: auth.effective_scope().id().as_str().to_owned(),
            service_namespace: "iam".into(),
            action: "iam:AssignRole".into(),
            resource_type: Some("iam:role_assignment".into()),
            resource_id: Some(assignment_id.to_string()),
            owner_scope: Some(request.project_id.clone()),
            operation_id: Some(operation_id),
            // The mutation is durably accepted before the 202 response; the
            // durable operation remains pending and its later lifecycle is
            // represented separately.  Audit outcomes use the versioned
            // vocabulary, not the internal operation state spelling.
            outcome: "succeeded".into(),
            reason_category: None,
            event_json:
                serde_json::json!({"event":"role_assignment.create","resource_id":assignment_id})
                    .to_string(),
        };
        self.store
            .mutate_role_assignment(&assignment, &operation, &canonical, &identity, &audit)
            .await
            .map_err(|error| match error {
                o3k_store::StoreError::IdempotencyConflict => GovernanceMutationError::Conflict,
                o3k_store::StoreError::Database(_) => GovernanceMutationError::Unavailable,
                _ => GovernanceMutationError::Internal,
            })?;
        Ok(RoleAssignmentMutationResponse {
            assignment: RoleAssignmentView {
                id: assignment.id,
                principal_id: assignment.user_id,
                project_id: assignment.project_id,
                role_id: assignment.role_id,
                created_at: assignment.created_at,
            },
            operation_id: operation_id.to_string(),
        })
    }
}
