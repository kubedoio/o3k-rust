use std::sync::Arc;

use o3k_kernel::{AuditEvent, AuditQuery, AuthContext, DurableAuditRepository};
use o3k_native_api::pagination::RepositoryPage;

/// Production native-Audit adapter backed by the same durable store used by
/// mutation services. No in-memory history or provider logs are consulted.
pub struct AuditReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::audit::AuditReader for AuditReaderAdapter {
    async fn list_page(
        &self,
        _auth: &AuthContext,
        query: AuditQuery,
    ) -> Result<RepositoryPage<AuditEvent>, String> {
        let limit = query.limit;
        let page = self
            .store
            .page(&query)
            .await
            .map_err(|_| "audit unavailable".to_owned())?;
        RepositoryPage::new(page.events, page.has_more, page.continuation_key, limit)
            .map_err(|_| "invalid audit page".to_owned())
    }

    async fn show(&self, auth: &AuthContext, id: &str) -> Result<AuditEvent, String> {
        let query = AuditQuery {
            scope: auth.effective_scope().clone(),
            after_event_id: None,
            limit: 1,
            service: None,
            action: None,
            outcome: None,
            resource_type: None,
            resource_id: None,
            operation_id: None,
            principal_id: None,
            request_id: None,
            audit_id: Some(id.to_owned()),
            from_timestamp: None,
            until_timestamp: None,
        };
        let page = self
            .store
            .page(&query)
            .await
            .map_err(|_| "audit unavailable".to_owned())?;
        page.events
            .into_iter()
            .next()
            .ok_or_else(|| "not found".to_owned())
    }
}
