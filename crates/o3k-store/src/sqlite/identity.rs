use super::*;
use async_trait::async_trait;

impl SqliteStore {
    pub async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_domains (id, name, description, enabled, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description, enabled=excluded.enabled")
            .bind(&domain.id)
            .bind(&domain.name)
            .bind(&domain.description)
            .bind(if domain.enabled { 1 } else { 0 })
            .bind(&domain.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_domain_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneDomainRecord>, StoreError> {
        let row = sqlx::query("SELECT id, name, description, enabled, created_at FROM keystone_domains WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneDomainRecord {
            id: r.get("id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_projects (id, domain_id, name, description, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description, enabled=excluded.enabled")
            .bind(&project.id)
            .bind(&project.domain_id)
            .bind(&project.name)
            .bind(&project.description)
            .bind(if project.enabled { 1 } else { 0 })
            .bind(&project.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_project_by_name(
        &self,
        domain_id: &str,
        name: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE domain_id = ? AND name = ?")
            .bind(domain_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneProjectRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_users (id, domain_id, name, password_hash, email, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET password_hash=excluded.password_hash, enabled=excluded.enabled")
            .bind(&user.id)
            .bind(&user.domain_id)
            .bind(&user.name)
            .bind(&user.password_hash)
            .bind(&user.email)
            .bind(if user.enabled { 1 } else { 0 })
            .bind(&user.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_user_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneUserRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneUserRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            password_hash: r.get("password_hash"),
            email: r.get("email"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_roles (id, name, description, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description")
            .bind(&role.id)
            .bind(&role.name)
            .bind(&role.description)
            .bind(&role.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_role_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneRoleRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneRoleRecord {
            id: r.get("id"),
            name: r.get("name"),
            description: r.get("description"),
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_role_assignments (id, user_id, project_id, role_id, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(user_id, project_id, role_id) DO NOTHING")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .bind(&assignment.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_user_role_names(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query("SELECT r.name FROM keystone_roles r JOIN keystone_role_assignments ra ON r.id = ra.role_id WHERE ra.user_id = ? AND ra.project_id = ?")
            .bind(user_id)
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows.into_iter().map(|r| r.get("name")).collect())
    }

    pub async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_services (id, name, type, description, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, type=excluded.type, enabled=excluded.enabled")
            .bind(&service.id)
            .bind(&service.name)
            .bind(&service.r#type)
            .bind(&service.description)
            .bind(if service.enabled { 1 } else { 0 })
            .bind(&service.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, type, description, enabled, created_at FROM keystone_services WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneServiceRecord {
                id: r.get("id"),
                name: r.get("name"),
                r#type: r.get("type"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_endpoints (id, service_id, interface, url, region, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET url=excluded.url, enabled=excluded.enabled")
            .bind(&endpoint.id)
            .bind(&endpoint.service_id)
            .bind(&endpoint.interface)
            .bind(&endpoint.url)
            .bind(&endpoint.region)
            .bind(if endpoint.enabled { 1 } else { 0 })
            .bind(&endpoint.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, service_id, interface, url, region, enabled, created_at FROM keystone_endpoints WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneEndpointRecord {
                id: r.get("id"),
                service_id: r.get("service_id"),
                interface: r.get("interface"),
                url: r.get("url"),
                region: r.get("region"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, description, parent_region_id, enabled, created_at FROM keystone_regions WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneRegionRecord {
                id: r.get("id"),
                description: r.get("description"),
                parent_region_id: r.get("parent_region_id"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_regions (id, description, parent_region_id, enabled, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET description=excluded.description, parent_region_id=excluded.parent_region_id, enabled=excluded.enabled")
            .bind(&region.id)
            .bind(&region.description)
            .bind(&region.parent_region_id)
            .bind(if region.enabled { 1 } else { 0 })
            .bind(&region.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, description, enabled, created_at FROM keystone_domains WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneDomainRecord {
                id: r.get("id"),
                name: r.get("name"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneProjectRecord {
                id: r.get("id"),
                domain_id: r.get("domain_id"),
                name: r.get("name"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneUserRecord {
                id: r.get("id"),
                domain_id: r.get("domain_id"),
                name: r.get("name"),
                password_hash: r.get("password_hash"),
                email: r.get("email"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, description, created_at FROM keystone_roles")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneRoleRecord {
                id: r.get("id"),
                name: r.get("name"),
                description: r.get("description"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneRoleAssignmentRecord {
                id: r.get("id"),
                user_id: r.get("user_id"),
                project_id: r.get("project_id"),
                role_id: r.get("role_id"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn get_keystone_user_by_id(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneUserRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(row.map(|r| KeystoneUserRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            password_hash: r.get("password_hash"),
            email: r.get("email"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn get_keystone_project_by_id(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(row.map(|r| KeystoneProjectRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        let (key_type, fingerprint, canonical) = validate_public_key(&keypair.public_key)?;
        if keypair.key_type != key_type
            || keypair.fingerprint != fingerprint
            || keypair.public_key != canonical
        {
            return Err(StoreError::InvalidKeypair(
                "keypair record is not canonical".to_owned(),
            ));
        }
        let result = sqlx::query("INSERT INTO keypairs (id, user_id, project_id, name, key_type, public_key, fingerprint, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(keypair.id.to_string()).bind(&keypair.user_id).bind(&keypair.project_id)
            .bind(&keypair.name).bind(&keypair.key_type).bind(&keypair.public_key)
            .bind(&keypair.fingerprint).bind(&keypair.created_at).execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::KeypairAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }
}

impl SqliteStore {
    pub async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, user_id, project_id, name, key_type, public_key, fingerprint, created_at FROM keypairs WHERE user_id = ? AND project_id = ? ORDER BY name")
            .bind(user_id).bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(keypair_from_row).collect()
    }

    pub async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        let row = sqlx::query("SELECT id, user_id, project_id, name, key_type, public_key, fingerprint, created_at FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?")
            .bind(user_id).bind(project_id).bind(name).fetch_optional(&self.pool).await.map_err(StoreError::Database)?
            .ok_or(StoreError::KeypairNotFound)?;
        keypair_from_row(&row)
    }

    pub async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let attached: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM server_keypairs WHERE keypair_id = (SELECT id FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?)")
            .bind(user_id).bind(project_id).bind(name).fetch_one(&mut *transaction).await.map_err(StoreError::Database)?;
        if attached > 0 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairInUse);
        }
        let pending_reference: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resources WHERE project_id = ? AND kind = 'compute_instance' AND observed_state != 'DELETED' AND EXISTS (SELECT 1 FROM operations WHERE operations.resource_id = resources.id AND operations.kind = 'create' AND operations.state IN ('pending', 'running', 'unknown_outcome')) AND (json_extract(desired_state, '$.keypair_id') = (SELECT id FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?) OR (json_extract(desired_state, '$.keypair_id') IS NULL AND json_extract(desired_state, '$.key_name') = ?))",
        )
        .bind(project_id)
        .bind(user_id)
        .bind(project_id)
        .bind(name)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::Database)?;
        if pending_reference > 0 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairInUse);
        }
        let result =
            sqlx::query("DELETE FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?")
                .bind(user_id)
                .bind(project_id)
                .bind(name)
                .execute(&mut *transaction)
                .await
                .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            Err(StoreError::KeypairNotFound)
        } else {
            Ok(())
        }
    }

    pub async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let owned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resources JOIN keypairs ON keypairs.project_id = resources.project_id WHERE resources.id = ? AND resources.kind = 'compute_instance' AND keypairs.id = ?")
            .bind(server_id.to_string()).bind(keypair_id.to_string()).fetch_one(&mut *transaction).await.map_err(StoreError::Database)?;
        if owned != 1 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairOwnershipConflict);
        }
        sqlx::query("INSERT INTO server_keypairs (server_id, keypair_id) VALUES (?, ?)")
            .bind(server_id.to_string())
            .bind(keypair_id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)
    }

    pub async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM server_keypairs WHERE server_id = ?")
            .bind(server_id.to_string())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(StoreError::Database)
    }

    pub async fn get_server_keypair_name(
        &self,
        server_id: Uuid,
    ) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT keypairs.name FROM server_keypairs JOIN keypairs ON keypairs.id = server_keypairs.keypair_id WHERE server_keypairs.server_id = ?")
            .bind(server_id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)
    }
}

#[async_trait]
impl IdentityRepository for SqliteStore {
    async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_domain(domain).await
    }

    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        self.list_keystone_domains().await
    }

    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_project(project).await
    }

    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        self.list_keystone_projects().await
    }

    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        self.insert_keystone_user(user).await
    }

    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        self.list_keystone_users().await
    }

    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        self.insert_keystone_role(role).await
    }

    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        self.list_keystone_roles().await
    }

    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_role_assignment(assignment).await
    }

    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        self.list_keystone_role_assignments().await
    }

    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_service(service).await
    }

    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        self.list_keystone_services().await
    }

    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_endpoint(endpoint).await
    }

    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        self.list_keystone_endpoints().await
    }

    async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_region(region).await
    }

    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        self.list_keystone_regions().await
    }
}

#[async_trait]
impl KeypairRepository for SqliteStore {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        self.insert_keypair(keypair).await
    }

    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        self.list_keypairs(user_id, project_id).await
    }

    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        self.get_keypair(user_id, project_id, name).await
    }

    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        self.delete_keypair(user_id, project_id, name).await
    }

    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        self.attach_server_keypair(server_id, keypair_id).await
    }

    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        self.detach_server_keypair(server_id).await
    }

    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError> {
        self.get_server_keypair_name(server_id).await
    }
}
