use crate::{AuditEventFilters, AuditEventRecord, AuditRepository, O3kStore, StoreError};
use async_trait::async_trait;

#[async_trait]
impl AuditRepository for O3kStore {
    async fn append_audit_event(&self, event: &AuditEventRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.append_audit_event(event).await,
            Self::Postgres(s) => s.append_audit_event(event).await,
        }
    }
    async fn prune_audit_events_before(
        &self,
        timestamp: &str,
        limit: usize,
    ) -> Result<usize, StoreError> {
        match self {
            Self::Sqlite(s) => s.prune_audit_events_before(timestamp, limit).await,
            Self::Postgres(s) => s.prune_audit_events_before(timestamp, limit).await,
        }
    }
    async fn list_audit_events_page(
        &self,
        scope: &str,
        after: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_audit_events_page(scope, after, limit, filters).await,
            Self::Postgres(s) => s.list_audit_events_page(scope, after, limit, filters).await,
        }
    }
    async fn list_audit_events_system_page(
        &self,
        scope: Option<&str>,
        after: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.list_audit_events_system_page(scope, after, limit, filters)
                    .await
            }
            Self::Postgres(s) => {
                s.list_audit_events_system_page(scope, after, limit, filters)
                    .await
            }
        }
    }
    async fn get_audit_event(
        &self,
        scope: &str,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_audit_event(scope, event_id).await,
            Self::Postgres(s) => s.get_audit_event(scope, event_id).await,
        }
    }
    async fn get_audit_event_system(
        &self,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_audit_event_system(event_id).await,
            Self::Postgres(s) => s.get_audit_event_system(event_id).await,
        }
    }
}
