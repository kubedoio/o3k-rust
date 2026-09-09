use super::SqliteStore;
use crate::{PublicAddressBindingRecord, PublicAddressRepository, StoreError};
use async_trait::async_trait;
use sqlx::Row;
use std::net::Ipv4Addr;
use uuid::Uuid;

fn row_binding(row: &sqlx::sqlite::SqliteRow) -> Result<PublicAddressBindingRecord, StoreError> {
    let allocation_id = row
        .get::<String, _>("allocation_id")
        .parse()
        .map_err(StoreError::InvalidUuid)?;
    let endpoint_id = row
        .get::<Option<String>, _>("endpoint_id")
        .map(|v| v.parse())
        .transpose()
        .map_err(StoreError::InvalidUuid)?;
    let public_address = row
        .get::<String, _>("public_address")
        .parse()
        .map_err(|_| StoreError::Corrupt("invalid public address".into()))?;
    let generation = u64::try_from(row.get::<i64, _>("generation"))
        .map_err(|_| StoreError::Corrupt("invalid public address generation".into()))?;
    Ok(PublicAddressBindingRecord {
        allocation_id,
        operation_id: row.get("operation_id"),
        project_id: row.get("project_id"),
        public_address,
        endpoint_id,
        generation,
    })
}

#[async_trait]
impl PublicAddressRepository for SqliteStore {
    async fn allocate_public_address(
        &self,
        project_id: &str,
        operation_id: &str,
        allocation_id: Uuid,
        first: Ipv4Addr,
        last: Ipv4Addr,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        if project_id.is_empty() || operation_id.is_empty() || u32::from(first) > u32::from(last) {
            return Err(StoreError::Corrupt(
                "invalid public allocation request".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        if let Some(row) = sqlx::query("SELECT allocation_id,operation_id,project_id,public_address,endpoint_id,generation FROM public_address_bindings WHERE operation_id=?").bind(operation_id).fetch_optional(&mut *tx).await.map_err(StoreError::Database)? {
            let existing = row_binding(&row)?;
            if existing.project_id != project_id { return Err(StoreError::OwnershipConflict); }
            tx.commit().await.map_err(StoreError::Database)?; return Ok(existing);
        }
        let rows = sqlx::query("SELECT public_address FROM public_address_bindings WHERE public_address >= ? AND public_address <= ?").bind(first.to_string()).bind(last.to_string()).fetch_all(&mut *tx).await.map_err(StoreError::Database)?;
        let used: std::collections::HashSet<Ipv4Addr> = rows
            .into_iter()
            .filter_map(|r| r.get::<String, _>("public_address").parse().ok())
            .collect();
        let address = (u32::from(first)..=u32::from(last))
            .map(Ipv4Addr::from)
            .find(|a| !used.contains(a))
            .ok_or(StoreError::NetworkAddressExhausted)?;
        sqlx::query("INSERT INTO public_address_bindings (allocation_id,operation_id,project_id,public_address,endpoint_id,generation) VALUES (?,?,?,?,NULL,1)").bind(allocation_id.to_string()).bind(operation_id).bind(project_id).bind(address.to_string()).execute(&mut *tx).await.map_err(|e| if matches!(&e, sqlx::Error::Database(d) if d.constraint().is_some()) { StoreError::ResourceAlreadyExists } else { StoreError::Database(e) })?;
        let row = sqlx::query("SELECT allocation_id,operation_id,project_id,public_address,endpoint_id,generation FROM public_address_bindings WHERE allocation_id=?").bind(allocation_id.to_string()).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
        let result = row_binding(&row)?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(result)
    }
    async fn associate_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
        endpoint_id: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        self.update_binding(project_id, allocation_id, Some(endpoint_id))
            .await
    }
    async fn disassociate_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        self.update_binding(project_id, allocation_id, None).await
    }
    async fn release_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<(), StoreError> {
        let existing = sqlx::query("SELECT endpoint_id FROM public_address_bindings WHERE allocation_id=? AND project_id=?")
            .bind(allocation_id.to_string()).bind(project_id).fetch_optional(&self.pool)
            .await.map_err(StoreError::Database)?;
        let Some(existing) = existing else {
            return Err(StoreError::NetworkNotFound);
        };
        if existing.get::<Option<String>, _>("endpoint_id").is_some() {
            return Err(StoreError::NetworkInUse);
        }
        let result=sqlx::query("DELETE FROM public_address_bindings WHERE allocation_id=? AND project_id=? AND endpoint_id IS NULL").bind(allocation_id.to_string()).bind(project_id).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        Ok(())
    }
    async fn get_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<Option<PublicAddressBindingRecord>, StoreError> {
        let row=sqlx::query("SELECT allocation_id,operation_id,project_id,public_address,endpoint_id,generation FROM public_address_bindings WHERE allocation_id=? AND project_id=?").bind(allocation_id.to_string()).bind(project_id).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.as_ref().map(row_binding).transpose()
    }
    async fn list_public_addresses(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<PublicAddressBindingRecord>, StoreError> {
        let rows=sqlx::query("SELECT allocation_id,operation_id,project_id,public_address,endpoint_id,generation FROM public_address_bindings WHERE project_id=? ORDER BY allocation_id LIMIT ?").bind(project_id).bind(i64::try_from(limit.min(10_000)).unwrap_or(10_000)).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(row_binding).collect()
    }
}

impl SqliteStore {
    async fn update_binding(
        &self,
        project_id: &str,
        allocation_id: Uuid,
        endpoint_id: Option<Uuid>,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        let result = if let Some(endpoint_id) = endpoint_id {
            sqlx::query("UPDATE public_address_bindings SET endpoint_id=?, generation=generation+1 WHERE allocation_id=? AND project_id=? AND (endpoint_id IS NULL OR endpoint_id=?)")
                .bind(endpoint_id.to_string()).bind(allocation_id.to_string()).bind(project_id).bind(endpoint_id.to_string()).execute(&self.pool).await
        } else {
            sqlx::query("UPDATE public_address_bindings SET endpoint_id=NULL, generation=generation+1 WHERE allocation_id=? AND project_id=? AND endpoint_id IS NOT NULL")
                .bind(allocation_id.to_string()).bind(project_id).execute(&self.pool).await
        }.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        self.get_public_address(project_id, allocation_id)
            .await?
            .ok_or(StoreError::NetworkNotFound)
    }
}
