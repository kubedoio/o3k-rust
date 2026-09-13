use async_trait::async_trait;
use o3k_kernel::{
    AuditEvent, FailureDomain, KernelError, TopologyBinding, TopologySnapshot, TopologyStore,
};

use super::O3kStore;

#[async_trait]
impl TopologyStore for O3kStore {
    async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
        match self {
            Self::Sqlite(store) => store.load_snapshot().await,
            Self::Postgres(store) => store.load_snapshot().await,
        }
    }

    async fn insert_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.insert_region(region_id, audit).await,
            Self::Postgres(store) => store.insert_region(region_id, audit).await,
        }
    }

    async fn delete_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.delete_region(region_id, audit).await,
            Self::Postgres(store) => store.delete_region(region_id, audit).await,
        }
    }

    async fn insert_availability_domain(
        &self,
        region_id: &str,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .insert_availability_domain(region_id, az_id, audit)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .insert_availability_domain(region_id, az_id, audit)
                    .await
            }
        }
    }

    async fn delete_availability_domain(
        &self,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.delete_availability_domain(az_id, audit).await,
            Self::Postgres(store) => store.delete_availability_domain(az_id, audit).await,
        }
    }

    async fn insert_failure_domain(
        &self,
        domain: &FailureDomain,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.insert_failure_domain(domain, audit).await,
            Self::Postgres(store) => store.insert_failure_domain(domain, audit).await,
        }
    }

    async fn update_failure_domain(
        &self,
        domain: &FailureDomain,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .update_failure_domain(domain, expected_generation, audit)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .update_failure_domain(domain, expected_generation, audit)
                    .await
            }
        }
    }

    async fn delete_failure_domain(
        &self,
        domain_id: &str,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .delete_failure_domain(domain_id, expected_generation, audit)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .delete_failure_domain(domain_id, expected_generation, audit)
                    .await
            }
        }
    }

    async fn insert_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.insert_binding(binding, audit).await,
            Self::Postgres(store) => store.insert_binding(binding, audit).await,
        }
    }

    async fn delete_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.delete_binding(binding, audit).await,
            Self::Postgres(store) => store.delete_binding(binding, audit).await,
        }
    }

    async fn record_audit(&self, audit: &AuditEvent) -> Result<(), KernelError> {
        match self {
            Self::Sqlite(store) => store.record_audit(audit).await,
            Self::Postgres(store) => store.record_audit(audit).await,
        }
    }

    async fn list_failure_domains(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FailureDomain>, KernelError> {
        match self {
            Self::Sqlite(store) => store.list_failure_domains(after_id, limit).await,
            Self::Postgres(store) => store.list_failure_domains(after_id, limit).await,
        }
    }

    async fn list_bindings(
        &self,
        after: Option<&o3k_kernel::BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError> {
        match self {
            Self::Sqlite(store) => store.list_bindings(after, limit).await,
            Self::Postgres(store) => store.list_bindings(after, limit).await,
        }
    }

    async fn list_bindings_of(
        &self,
        failure_domain: &str,
        after: Option<&o3k_kernel::BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError> {
        match self {
            Self::Sqlite(store) => store.list_bindings_of(failure_domain, after, limit).await,
            Self::Postgres(store) => store.list_bindings_of(failure_domain, after, limit).await,
        }
    }
}
