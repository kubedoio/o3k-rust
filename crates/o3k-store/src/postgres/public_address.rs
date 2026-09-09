use super::PostgresStore;
use crate::{PublicAddressBindingRecord, PublicAddressRepository, StoreError};
use async_trait::async_trait;
use sqlx::Row;
use std::net::Ipv4Addr;
use uuid::Uuid;

fn parse(row: &sqlx::postgres::PgRow) -> Result<PublicAddressBindingRecord, StoreError> {
    Ok(PublicAddressBindingRecord {
        allocation_id: row
            .get::<String, _>("allocation_id")
            .parse()
            .map_err(StoreError::InvalidUuid)?,
        operation_id: row.get("operation_id"),
        project_id: row.get("project_id"),
        public_address: row
            .get::<String, _>("public_address")
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid public address".into()))?,
        endpoint_id: row
            .get::<Option<String>, _>("endpoint_id")
            .map(|v| v.parse())
            .transpose()
            .map_err(StoreError::InvalidUuid)?,
        generation: u64::try_from(row.get::<i64, _>("generation"))
            .map_err(|_| StoreError::Corrupt("invalid generation".into()))?,
    })
}
const COLS: &str = "allocation_id,operation_id,project_id,public_address,endpoint_id,generation";
#[async_trait]
impl PublicAddressRepository for PostgresStore {
    async fn allocate_public_address(
        &self,
        p: &str,
        o: &str,
        id: Uuid,
        first: Ipv4Addr,
        last: Ipv4Addr,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        if p.is_empty() || o.is_empty() || u32::from(first) > u32::from(last) {
            return Err(StoreError::Corrupt(
                "invalid public allocation request".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        if let Some(r) = sqlx::query(&format!(
            "SELECT {COLS} FROM public_address_bindings WHERE operation_id=$1"
        ))
        .bind(o)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        {
            let x = parse(&r)?;
            if x.project_id != p {
                return Err(StoreError::OwnershipConflict);
            }
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(x);
        }
        let used=sqlx::query("SELECT public_address FROM public_address_bindings WHERE public_address >= $1::inet AND public_address <= $2::inet").bind(first.to_string()).bind(last.to_string()).fetch_all(&mut *tx).await.map_err(StoreError::Database)?;
        let set: std::collections::HashSet<Ipv4Addr> = used
            .into_iter()
            .filter_map(|r| r.get::<String, _>("public_address").parse().ok())
            .collect();
        let a = (u32::from(first)..=u32::from(last))
            .map(Ipv4Addr::from)
            .find(|x| !set.contains(x))
            .ok_or(StoreError::NetworkAddressExhausted)?;
        sqlx::query("INSERT INTO public_address_bindings (allocation_id,operation_id,project_id,public_address,generation) VALUES ($1,$2,$3,$4::inet,1)").bind(id.to_string()).bind(o).bind(p).bind(a.to_string()).execute(&mut *tx).await.map_err(StoreError::Database)?;
        let r = sqlx::query(&format!(
            "SELECT {COLS} FROM public_address_bindings WHERE allocation_id=$1"
        ))
        .bind(id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        let x = parse(&r)?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(x)
    }
    async fn associate_public_address(
        &self,
        p: &str,
        id: Uuid,
        e: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        self.update(p, id, Some(e)).await
    }
    async fn disassociate_public_address(
        &self,
        p: &str,
        id: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        self.update(p, id, None).await
    }
    async fn release_public_address(&self, p: &str, id: Uuid) -> Result<(), StoreError> {
        let existing = sqlx::query("SELECT endpoint_id FROM public_address_bindings WHERE allocation_id=$1 AND project_id=$2")
            .bind(id.to_string()).bind(p).fetch_optional(&self.pool)
            .await.map_err(StoreError::Database)?;
        let Some(existing) = existing else {
            return Err(StoreError::NetworkNotFound);
        };
        if existing.get::<Option<String>, _>("endpoint_id").is_some() {
            return Err(StoreError::NetworkInUse);
        }
        let r=sqlx::query("DELETE FROM public_address_bindings WHERE allocation_id=$1 AND project_id=$2 AND endpoint_id IS NULL").bind(id.to_string()).bind(p).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        Ok(())
    }
    async fn get_public_address(
        &self,
        p: &str,
        id: Uuid,
    ) -> Result<Option<PublicAddressBindingRecord>, StoreError> {
        let r = sqlx::query(&format!(
            "SELECT {COLS} FROM public_address_bindings WHERE allocation_id=$1 AND project_id=$2"
        ))
        .bind(id.to_string())
        .bind(p)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        r.as_ref().map(parse).transpose()
    }
    async fn list_public_addresses(
        &self,
        p: &str,
        l: usize,
    ) -> Result<Vec<PublicAddressBindingRecord>, StoreError> {
        let rs=sqlx::query(&format!("SELECT {COLS} FROM public_address_bindings WHERE project_id=$1 ORDER BY allocation_id LIMIT $2")).bind(p).bind(i64::try_from(l.min(10000)).unwrap_or(10000)).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rs.iter().map(parse).collect()
    }
}
impl PostgresStore {
    async fn update(
        &self,
        p: &str,
        id: Uuid,
        e: Option<Uuid>,
    ) -> Result<PublicAddressBindingRecord, StoreError> {
        let r = if let Some(endpoint_id) = e {
            sqlx::query("UPDATE public_address_bindings SET endpoint_id=$1,generation=generation+1 WHERE allocation_id=$2 AND project_id=$3 AND (endpoint_id IS NULL OR endpoint_id=$1)")
                .bind(endpoint_id.to_string()).bind(id.to_string()).bind(p).execute(&self.pool).await
        } else {
            sqlx::query("UPDATE public_address_bindings SET endpoint_id=NULL,generation=generation+1 WHERE allocation_id=$1 AND project_id=$2 AND endpoint_id IS NOT NULL")
                .bind(id.to_string()).bind(p).execute(&self.pool).await
        }.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        self.get_public_address(p, id)
            .await?
            .ok_or(StoreError::NetworkNotFound)
    }
}
