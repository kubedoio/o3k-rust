use super::*;

#[async_trait]
impl IdentityRepository for PostgresStore {
    async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_domains (id, name, description, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (name) DO UPDATE SET enabled = EXCLUDED.enabled, description = EXCLUDED.description",
        )
        .bind(&domain.id)
        .bind(&domain.name)
        .bind(&domain.description)
        .bind(if domain.enabled { 1i32 } else { 0i32 })
        .bind(&domain.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, description, enabled, created_at FROM keystone_domains ORDER BY name ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneDomainRecord {
                    id: r.get("id"),
                    name: r.get("name"),
                    description: r.get("description"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }

    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_projects (id, domain_id, name, description, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (domain_id, name) DO UPDATE SET enabled = EXCLUDED.enabled, description = EXCLUDED.description",
        )
        .bind(&project.id)
        .bind(&project.domain_id)
        .bind(&project.name)
        .bind(&project.description)
        .bind(if project.enabled { 1i32 } else { 0i32 })
        .bind(&project.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects ORDER BY name ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneProjectRecord {
                    id: r.get("id"),
                    domain_id: r.get("domain_id"),
                    name: r.get("name"),
                    description: r.get("description"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }

    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_users (id, domain_id, name, password_hash, email, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (name) DO UPDATE
             SET password_hash = EXCLUDED.password_hash, email = EXCLUDED.email, enabled = EXCLUDED.enabled",
        )
        .bind(&user.id)
        .bind(&user.domain_id)
        .bind(&user.name)
        .bind(&user.password_hash)
        .bind(&user.email)
        .bind(if user.enabled { 1i32 } else { 0i32 })
        .bind(&user.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users ORDER BY name ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneUserRecord {
                    id: r.get("id"),
                    domain_id: r.get("domain_id"),
                    name: r.get("name"),
                    password_hash: r.get("password_hash"),
                    email: r.get("email"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }

    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_roles (id, name, description, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (name) DO UPDATE SET description = EXCLUDED.description",
        )
        .bind(&role.id)
        .bind(&role.name)
        .bind(&role.description)
        .bind(&role.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles ORDER BY name ASC",
        )
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

    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_role_assignments (id, user_id, project_id, role_id, created_at)
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
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_role_assignments(
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

    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_services (id, name, type, description, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE
             SET name = EXCLUDED.name, type = EXCLUDED.type, description = EXCLUDED.description, enabled = EXCLUDED.enabled",
        )
        .bind(&service.id)
        .bind(&service.name)
        .bind(&service.r#type)
        .bind(&service.description)
        .bind(if service.enabled { 1i32 } else { 0i32 })
        .bind(&service.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, type, description, enabled, created_at FROM keystone_services ORDER BY type ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneServiceRecord {
                    id: r.get("id"),
                    name: r.get("name"),
                    r#type: r.get("type"),
                    description: r.get("description"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }

    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_endpoints (id, service_id, interface, url, region, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (id) DO UPDATE
             SET service_id = EXCLUDED.service_id, interface = EXCLUDED.interface, url = EXCLUDED.url, region = EXCLUDED.region, enabled = EXCLUDED.enabled",
        )
        .bind(&endpoint.id)
        .bind(&endpoint.service_id)
        .bind(&endpoint.interface)
        .bind(&endpoint.url)
        .bind(&endpoint.region)
        .bind(if endpoint.enabled { 1i32 } else { 0i32 })
        .bind(&endpoint.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, service_id, interface, url, region, enabled, created_at FROM keystone_endpoints ORDER BY url ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneEndpointRecord {
                    id: r.get("id"),
                    service_id: r.get("service_id"),
                    interface: r.get("interface"),
                    url: r.get("url"),
                    region: r.get("region"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }

    async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO keystone_regions (id, description, parent_region_id, enabled, created_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE
             SET description = EXCLUDED.description, parent_region_id = EXCLUDED.parent_region_id, enabled = EXCLUDED.enabled",
        )
        .bind(&region.id)
        .bind(&region.description)
        .bind(&region.parent_region_id)
        .bind(if region.enabled { 1i32 } else { 0i32 })
        .bind(&region.created_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, description, parent_region_id, enabled, created_at FROM keystone_regions ORDER BY id ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let enabled_val: i32 = r.get("enabled");
                KeystoneRegionRecord {
                    id: r.get("id"),
                    description: r.get("description"),
                    parent_region_id: r.get("parent_region_id"),
                    enabled: enabled_val != 0,
                    created_at: r.get("created_at"),
                }
            })
            .collect())
    }
}

#[async_trait]
impl KeypairRepository for PostgresStore {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        let (key_type, fingerprint, canonical) = crate::validate_public_key(&keypair.public_key)?;
        if keypair.key_type != key_type
            || keypair.fingerprint != fingerprint
            || keypair.public_key != canonical
        {
            return Err(StoreError::InvalidKeypair(
                "keypair record is not canonical".to_owned(),
            ));
        }
        let id_str = keypair.id.to_string();
        let res = sqlx::query(
            "INSERT INTO keypairs (id, user_id, project_id, name, key_type, public_key, fingerprint, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&id_str)
        .bind(&keypair.user_id)
        .bind(&keypair.project_id)
        .bind(&keypair.name)
        .bind(&keypair.key_type)
        .bind(&keypair.public_key)
        .bind(&keypair.fingerprint)
        .bind(&keypair.created_at)
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(err)) if err.code().as_deref() == Some("23505") => {
                Err(StoreError::KeypairAlreadyExists)
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM keypairs WHERE user_id = $1 AND project_id = $2 ORDER BY created_at ASC",
        )
        .bind(user_id)
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.into_iter()
            .map(|r| {
                let id_str: String = r.get("id");
                Ok(KeypairRecord {
                    id: parse_uuid(&id_str)?,
                    user_id: r.get("user_id"),
                    project_id: r.get("project_id"),
                    name: r.get("name"),
                    key_type: r.get("key_type"),
                    public_key: r.get("public_key"),
                    fingerprint: r.get("fingerprint"),
                    created_at: r.get("created_at"),
                })
            })
            .collect()
    }

    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM keypairs WHERE user_id = $1 AND project_id = $2 AND name = $3",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        match row {
            Some(r) => {
                let id_str: String = r.get("id");
                Ok(KeypairRecord {
                    id: parse_uuid(&id_str)?,
                    user_id: r.get("user_id"),
                    project_id: r.get("project_id"),
                    name: r.get("name"),
                    key_type: r.get("key_type"),
                    public_key: r.get("public_key"),
                    fingerprint: r.get("fingerprint"),
                    created_at: r.get("created_at"),
                })
            }
            None => Err(StoreError::KeypairNotFound),
        }
    }

    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let res = sqlx::query(
            "DELETE FROM keypairs WHERE user_id = $1 AND project_id = $2 AND name = $3",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::KeypairNotFound);
        }
        Ok(())
    }

    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        let srv_id_str = server_id.to_string();
        let key_id_str = keypair_id.to_string();
        sqlx::query(
            "INSERT INTO server_keypairs (server_id, keypair_id, created_at)
             VALUES ($1, $2, CURRENT_TIMESTAMP)
             ON CONFLICT (server_id) DO UPDATE SET keypair_id = EXCLUDED.keypair_id",
        )
        .bind(&srv_id_str)
        .bind(&key_id_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        let srv_id_str = server_id.to_string();
        sqlx::query("DELETE FROM server_keypairs WHERE server_id = $1")
            .bind(&srv_id_str)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError> {
        let srv_id_str = server_id.to_string();
        let row = sqlx::query(
            "SELECT k.name FROM server_keypairs sk
             JOIN keypairs k ON sk.keypair_id = k.id
             WHERE sk.server_id = $1",
        )
        .bind(&srv_id_str)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(row.map(|r| r.get("name")))
    }
}
