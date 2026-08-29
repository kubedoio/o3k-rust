use super::*;

#[async_trait]
impl IdentityRepository for O3kStore {
    async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_domain(domain).await,
            Self::Postgres(s) => s.insert_keystone_domain(domain).await,
        }
    }

    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_domains().await,
            Self::Postgres(s) => s.list_keystone_domains().await,
        }
    }

    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_project(project).await,
            Self::Postgres(s) => s.insert_keystone_project(project).await,
        }
    }

    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_projects().await,
            Self::Postgres(s) => s.list_keystone_projects().await,
        }
    }

    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_user(user).await,
            Self::Postgres(s) => s.insert_keystone_user(user).await,
        }
    }

    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_users().await,
            Self::Postgres(s) => s.list_keystone_users().await,
        }
    }

    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_role(role).await,
            Self::Postgres(s) => s.insert_keystone_role(role).await,
        }
    }

    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_roles().await,
            Self::Postgres(s) => s.list_keystone_roles().await,
        }
    }

    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_role_assignment(assignment).await,
            Self::Postgres(s) => s.insert_keystone_role_assignment(assignment).await,
        }
    }

    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_role_assignments().await,
            Self::Postgres(s) => s.list_keystone_role_assignments().await,
        }
    }

    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_service(service).await,
            Self::Postgres(s) => s.insert_keystone_service(service).await,
        }
    }

    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_services().await,
            Self::Postgres(s) => s.list_keystone_services().await,
        }
    }

    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_endpoint(endpoint).await,
            Self::Postgres(s) => s.insert_keystone_endpoint(endpoint).await,
        }
    }

    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_endpoints().await,
            Self::Postgres(s) => s.list_keystone_endpoints().await,
        }
    }

    async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_region(region).await,
            Self::Postgres(s) => s.insert_keystone_region(region).await,
        }
    }

    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_regions().await,
            Self::Postgres(s) => s.list_keystone_regions().await,
        }
    }
}
#[async_trait]
impl KeypairRepository for O3kStore {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keypair(keypair).await,
            Self::Postgres(s) => s.insert_keypair(keypair).await,
        }
    }

    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keypairs(user_id, project_id).await,
            Self::Postgres(s) => s.list_keypairs(user_id, project_id).await,
        }
    }

    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_keypair(user_id, project_id, name).await,
            Self::Postgres(s) => s.get_keypair(user_id, project_id, name).await,
        }
    }

    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_keypair(user_id, project_id, name).await,
            Self::Postgres(s) => s.delete_keypair(user_id, project_id, name).await,
        }
    }

    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.attach_server_keypair(server_id, keypair_id).await,
            Self::Postgres(s) => s.attach_server_keypair(server_id, keypair_id).await,
        }
    }

    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.detach_server_keypair(server_id).await,
            Self::Postgres(s) => s.detach_server_keypair(server_id).await,
        }
    }

    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_server_keypair_name(server_id).await,
            Self::Postgres(s) => s.get_server_keypair_name(server_id).await,
        }
    }
}
