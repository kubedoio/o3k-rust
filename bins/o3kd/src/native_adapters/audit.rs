use std::sync::Arc;

use o3k_native_api::{
    audit::{AuditEventView, AuditFilters},
    error::NativeReadError,
};
use o3k_store::{AuditEventFilters, AuditEventRecord, AuditRepository};

pub struct AuditReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

fn public_reason_category(reason: Option<String>) -> Option<String> {
    // Older rows may predate the kernel's category normalization.  Never
    // project a persisted free-form value: provider/SQL error text can be
    // present in legacy data even though new writes are category-only.
    reason.map(|value| match value.as_str() {
        "unauthorized"
        | "validation failed"
        | "not found"
        | "conflict"
        | "unavailable"
        | "provider timeout"
        | "provider failed"
        | "operation failed"
        | "idempotency conflict"
        | "stale generation"
        | "quota exceeded" => value,
        _ => "operation failed".to_owned(),
    })
}

fn public_outcome(outcome: String) -> String {
    // Audit rows can outlive a server version.  Do not trust legacy/free-form
    // outcome values at the public boundary: older writers could persist
    // provider text here, which would turn a harmless list/show into an
    // information disclosure channel.
    match outcome.as_str() {
        "allowed" | "denied" | "succeeded" | "failed" | "unknown_outcome" => outcome,
        _ => "failed".to_owned(),
    }
}

fn view(record: AuditEventRecord) -> AuditEventView {
    AuditEventView {
        event_id: record.event_id,
        timestamp: record.timestamp,
        request_id: record.request_id,
        audit_id: record.audit_id,
        principal_id: record.principal_id,
        effective_scope: record.effective_scope,
        service_namespace: record.service_namespace,
        action: record.action,
        resource_type: record.resource_type,
        resource_id: record.resource_id,
        owner_scope: record.owner_scope,
        operation_id: record.operation_id.map(|id| id.to_string()),
        outcome: public_outcome(record.outcome),
        reason_category: public_reason_category(record.reason_category),
    }
}

#[async_trait::async_trait]
impl o3k_native_api::audit::AuditReader for AuditReaderAdapter {
    async fn show_audit_event(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<AuditEventView, NativeReadError> {
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::Project
            && auth.effective_scope().kind() != o3k_kernel::ScopeKind::System
        {
            return Err(NativeReadError::Forbidden);
        }
        let event = if auth.effective_scope().kind() == o3k_kernel::ScopeKind::System {
            self.store.get_audit_event_system(id).await
        } else {
            self.store
                .get_audit_event(auth.effective_scope().id().as_str(), id)
                .await
        }
        .map_err(|error| {
            tracing::error!(%error, "native audit show failed");
            NativeReadError::Internal
        })?;
        event.map(view).ok_or(NativeReadError::NotFound)
    }

    async fn list_audit_events_page(
        &self,
        auth: &o3k_kernel::AuthContext,
        after_id: Option<&str>,
        limit: usize,
        filters: &AuditFilters,
    ) -> Result<Vec<AuditEventView>, NativeReadError> {
        if auth.effective_scope().kind() == o3k_kernel::ScopeKind::System {
            return self
                .store
                .list_audit_events_system_page(
                    filters.scope.as_deref(),
                    after_id,
                    limit,
                    &AuditEventFilters {
                        event_id: filters.event_id.clone(),
                        timestamp_from: filters.timestamp_from.clone(),
                        timestamp_to: filters.timestamp_to.clone(),
                        principal_id: filters.principal_id.clone(),
                        service_namespace: filters.service.clone(),
                        action: filters.action.clone(),
                        outcome: filters.outcome.clone(),
                        resource_type: filters.resource_type.clone(),
                        resource_id: filters.resource_id.clone(),
                        operation_id: filters.operation_id.clone(),
                        request_id: filters.request_id.clone(),
                        audit_id: filters.audit_id.clone(),
                    },
                )
                .await
                .map_err(|error| {
                    tracing::error!(%error, "native system audit list failed");
                    NativeReadError::Internal
                })
                .map(|events| events.into_iter().map(view).collect());
        }
        if auth.effective_scope().kind() != o3k_kernel::ScopeKind::Project {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .list_audit_events_page(
                auth.effective_scope().id().as_str(),
                after_id,
                limit,
                &AuditEventFilters {
                    event_id: filters.event_id.clone(),
                    timestamp_from: filters.timestamp_from.clone(),
                    timestamp_to: filters.timestamp_to.clone(),
                    principal_id: filters.principal_id.clone(),
                    service_namespace: filters.service.clone(),
                    action: filters.action.clone(),
                    outcome: filters.outcome.clone(),
                    resource_type: filters.resource_type.clone(),
                    resource_id: filters.resource_id.clone(),
                    operation_id: filters.operation_id.clone(),
                    request_id: filters.request_id.clone(),
                    audit_id: filters.audit_id.clone(),
                },
            )
            .await
            .map_err(|error| {
                tracing::error!(%error, "native audit list failed");
                NativeReadError::Internal
            })
            .map(|events| events.into_iter().map(view).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{public_outcome, public_reason_category};

    #[test]
    fn legacy_free_form_reason_is_projected_as_safe_category() {
        let projected = public_reason_category(Some(
            "provider password=super-secret /var/lib/controller/db.sqlite".to_owned(),
        ));
        assert_eq!(projected.as_deref(), Some("operation failed"));
    }

    #[test]
    fn canonical_reason_categories_are_preserved() {
        assert_eq!(
            public_reason_category(Some("provider timeout".to_owned())).as_deref(),
            Some("provider timeout")
        );
    }

    #[test]
    fn legacy_free_form_outcome_is_not_projected() {
        assert_eq!(
            public_outcome("provider password=super-secret".to_owned()),
            "failed"
        );
    }

    #[test]
    fn canonical_outcomes_are_preserved() {
        assert_eq!(
            public_outcome("unknown_outcome".to_owned()),
            "unknown_outcome"
        );
    }
}
