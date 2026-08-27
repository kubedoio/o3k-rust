use std::{net::Ipv4Addr, str::FromStr, time::Duration};

use async_trait::async_trait;
use o3k_kernel::{
    LimitKey, LimitValue, OwnershipScope, Reservation, ReservationId, ReservationState,
    ResourceAmount, ScopeId, ScopeKind, Usage,
};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions, PgRow},
};
use uuid::Uuid;

use crate::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferState,
    ArtifactTransferUpdate, CanonicalAddressPoolRecord, CanonicalAddressRealmRecord,
    CanonicalEndpointRecord, CanonicalL3GatewayAttachmentRecord, CanonicalL3GatewayRecord,
    CanonicalNetworkPolicyRecord, CanonicalNetworkRecord, CanonicalOperationRecord,
    CanonicalRealmBindingRecord, ComputeRepository, DatabaseHealth, DurableStore,
    IdempotencyReservation, IdempotencyReservationRequest, IdentityRepository, ImageMetadataRecord,
    ImageOverlayIdentity, ImageOverlayOwnershipRecord, ImageOverlayState, ImageOverlayUpdate,
    ImageRepository, KeypairRecord, KeypairRepository, KeystoneDomainRecord,
    KeystoneEndpointRecord, KeystoneProjectRecord, KeystoneRegionRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord, KeystoneUserRecord,
    NetworkAddressAllocationRecord, NetworkIntentRecord, NetworkRecord, NetworkRepository,
    ObservationUpdate, OperationRecord, OperationState, PlacementAllocationRecord,
    PlacementIntentRecord, PlacementInventoryRecord, PlacementProviderRecord,
    PlacementReconcileRecord, PlacementRepository, PlacementResourceRecord, PortRecord,
    ProviderReference, ResourceRecord, ResourceRelationshipRecord, SecurityGroupBindingRecord,
    SecurityGroupRecord, SecurityGroupRuleRecord, StoreError, SubnetRecord, VolumeAttachmentRecord,
    VolumeAttachmentRepository, quota::QuotaRepository,
    validate_canonical_idempotent_operation_identity,
};

#[derive(Clone, Debug)]
pub struct PostgresStore {
    pub(crate) pool: PgPool,
}

impl PostgresStore {
    pub async fn insert_canonical_l3_gateway(
        &self,
        g: &CanonicalL3GatewayRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&g.state)?;
        crate::checked_generation(g.generation)?;
        sqlx::query("INSERT INTO canonical_l3_gateways (id,project_id,name,external_realm_id,enable_snat,generation,state) VALUES ($1,$2,$3,$4,$5,$6,$7)").bind(g.id).bind(&g.project_id).bind(&g.name).bind(g.external_realm_id).bind(g.enable_snat).bind(g.generation as i64).bind(&g.state).execute(&self.pool).await.map_err(crate::map_canonical_insert_error).map(|_|())
    }
    pub async fn get_canonical_l3_gateway(
        &self,
        p: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayRecord>, StoreError> {
        let r=sqlx::query("SELECT id,project_id,name,external_realm_id,enable_snat,generation,state FROM canonical_l3_gateways WHERE id=$1 AND project_id=$2").bind(id).bind(p).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        Ok(r.map(|x| {
            Ok(CanonicalL3GatewayRecord {
                id: x.try_get("id").map_err(StoreError::Database)?,
                project_id: x.try_get("project_id").map_err(StoreError::Database)?,
                name: x.try_get("name").map_err(StoreError::Database)?,
                external_realm_id: x
                    .try_get("external_realm_id")
                    .map_err(StoreError::Database)?,
                enable_snat: x.try_get("enable_snat").map_err(StoreError::Database)?,
                generation: crate::checked_generation(
                    x.try_get::<i64, _>("generation")
                        .map_err(StoreError::Database)? as u64,
                )? as u64,
                state: x.try_get("state").map_err(StoreError::Database)?,
            })
        })
        .transpose()?)
    }
    pub async fn list_canonical_l3_gateways(
        &self,
        p: &str,
    ) -> Result<Vec<CanonicalL3GatewayRecord>, StoreError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM canonical_l3_gateways WHERE project_id=$1 ORDER BY id",
        )
        .bind(p)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        let mut out = Vec::new();
        for id in ids {
            out.push(
                self.get_canonical_l3_gateway(p, &id)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("gateway disappeared".into()))?,
            )
        }
        Ok(out)
    }
    pub async fn update_canonical_l3_gateway(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
        n: &str,
        x: Option<Uuid>,
        s: bool,
    ) -> Result<CanonicalL3GatewayRecord, StoreError> {
        let r=sqlx::query("UPDATE canonical_l3_gateways SET name=$1,external_realm_id=$2,enable_snat=$3,generation=generation+1 WHERE id=$4 AND project_id=$5 AND generation=$6 AND state='active'").bind(n).bind(x).bind(s).bind(id).bind(p).bind(e as i64).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_canonical_l3_gateway(p, id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("gateway disappeared".into()))
    }
    pub async fn begin_canonical_l3_gateway_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<CanonicalL3GatewayRecord, StoreError> {
        let r=sqlx::query("UPDATE canonical_l3_gateways SET state='deleting',generation=generation+1 WHERE id=$1 AND project_id=$2 AND generation=$3 AND state='active' AND NOT EXISTS (SELECT 1 FROM canonical_l3_gateway_attachments WHERE gateway_id=$1 AND state IN ('active','deleting'))").bind(id).bind(p).bind(e as i64).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::OwnershipConflict);
        }
        self.get_canonical_l3_gateway(p, id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("gateway disappeared".into()))
    }
    pub async fn finalize_canonical_l3_gateway_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<(), StoreError> {
        let r=sqlx::query("DELETE FROM canonical_l3_gateways WHERE id=$1 AND project_id=$2 AND generation=$3 AND state='deleting' AND NOT EXISTS (SELECT 1 FROM canonical_l3_gateway_attachments WHERE gateway_id=$1 AND state IN ('active','deleting'))").bind(id).bind(p).bind(e as i64).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            Err(StoreError::OwnershipConflict)
        } else {
            Ok(())
        }
    }
    pub async fn insert_canonical_l3_gateway_attachment(
        &self,
        a: &CanonicalL3GatewayAttachmentRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&a.state)?;
        crate::checked_generation(a.generation)?;
        sqlx::query("INSERT INTO canonical_l3_gateway_attachments (id,gateway_id,realm_id,project_id,generation,state) VALUES ($1,$2,$3,$4,$5,$6)").bind(a.id).bind(a.gateway_id).bind(a.realm_id).bind(&a.project_id).bind(a.generation as i64).bind(&a.state).execute(&self.pool).await.map_err(crate::map_canonical_insert_error).map(|_|())
    }
    pub async fn get_canonical_l3_gateway_attachment(
        &self,
        p: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        let r=sqlx::query("SELECT id,gateway_id,realm_id,project_id,generation,state FROM canonical_l3_gateway_attachments WHERE id=$1 AND project_id=$2").bind(id).bind(p).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        Ok(r.map(|x| {
            Ok(CanonicalL3GatewayAttachmentRecord {
                id: x.try_get("id").map_err(StoreError::Database)?,
                gateway_id: x.try_get("gateway_id").map_err(StoreError::Database)?,
                realm_id: x.try_get("realm_id").map_err(StoreError::Database)?,
                project_id: x.try_get("project_id").map_err(StoreError::Database)?,
                generation: crate::checked_generation(
                    x.try_get::<i64, _>("generation")
                        .map_err(StoreError::Database)? as u64,
                )? as u64,
                state: x.try_get("state").map_err(StoreError::Database)?,
            })
        })
        .transpose()?)
    }
    pub async fn list_canonical_l3_gateway_attachments(
        &self,
        p: &str,
        g: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM canonical_l3_gateway_attachments WHERE project_id=$1 AND gateway_id=$2 ORDER BY id").bind(p).bind(g).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        let mut out = Vec::new();
        for id in ids {
            out.push(
                self.get_canonical_l3_gateway_attachment(p, &id)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("attachment disappeared".into()))?,
            )
        }
        Ok(out)
    }
    pub async fn list_canonical_realm_l3_gateway_attachments(
        &self,
        p: &str,
        r: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM canonical_l3_gateway_attachments WHERE project_id=$1 AND realm_id=$2 ORDER BY id").bind(p).bind(r).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        let mut out = Vec::new();
        for id in ids {
            out.push(
                self.get_canonical_l3_gateway_attachment(p, &id)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("attachment disappeared".into()))?,
            )
        }
        Ok(out)
    }
    pub async fn begin_canonical_l3_gateway_attachment_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<CanonicalL3GatewayAttachmentRecord, StoreError> {
        let r=sqlx::query("UPDATE canonical_l3_gateway_attachments SET state='deleting',generation=generation+1 WHERE id=$1 AND project_id=$2 AND generation=$3 AND state='active'").bind(id).bind(p).bind(e as i64).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_canonical_l3_gateway_attachment(p, id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("attachment disappeared".into()))
    }
    pub async fn finalize_canonical_l3_gateway_attachment_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<(), StoreError> {
        let r=sqlx::query("DELETE FROM canonical_l3_gateway_attachments WHERE id=$1 AND project_id=$2 AND generation=$3 AND state='deleting'").bind(id).bind(p).bind(e as i64).execute(&self.pool).await.map_err(StoreError::Database)?;
        if r.rows_affected() == 0 {
            Err(StoreError::StaleGeneration)
        } else {
            Ok(())
        }
    }
    pub async fn insert_canonical_network(
        &self,
        network: &CanonicalNetworkRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&network.state)?;
        sqlx::query(
            "INSERT INTO canonical_networks (id, project_id, name, admin_state_up, generation, state) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(network.id.to_string())
        .bind(&network.project_id)
        .bind(&network.name)
        .bind(network.admin_state_up)
        .bind(crate::checked_generation(network.generation)?)
        .bind(&network.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn get_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalNetworkRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, project_id, name, admin_state_up, generation, state FROM canonical_networks WHERE id = $1 AND project_id = $2",
        )
        .bind(id.to_string())
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.as_ref().map(canonical_network_from_pg_row).transpose()
    }

    pub async fn list_canonical_networks(
        &self,
        project_id: &str,
    ) -> Result<Vec<CanonicalNetworkRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, project_id, name, admin_state_up, generation, state FROM canonical_networks WHERE project_id = $1 ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(canonical_network_from_pg_row).collect()
    }

    pub async fn update_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        name: &str,
        admin_state_up: bool,
    ) -> Result<CanonicalNetworkRecord, StoreError> {
        if name.trim().is_empty() {
            return Err(StoreError::ResourceNotFound);
        }
        if sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM canonical_networks WHERE project_id = $1 AND name = $2 AND id <> $3",
        )
        .bind(project_id)
        .bind(name)
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?
            != 0
        {
            return Err(StoreError::ResourceAlreadyExists);
        }
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let result = sqlx::query(
            "UPDATE canonical_networks SET name = $1, admin_state_up = $2, generation = generation + 1 WHERE id = $3 AND project_id = $4 AND generation = $5 AND state = 'active'",
        )
        .bind(name)
        .bind(admin_state_up)
        .bind(id.to_string())
        .bind(project_id)
        .bind(crate::checked_generation(expected_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return match self.get_canonical_network(project_id, id).await? {
                Some(_) => Err(StoreError::StaleGeneration),
                None => Err(StoreError::ResourceNotFound),
            };
        }
        sqlx::query("UPDATE network_networks SET name = $1 WHERE id = $2 AND project_id = $3")
            .bind(name)
            .bind(id.to_string())
            .bind(project_id)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)?;
        self.get_canonical_network(project_id, id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    pub async fn delete_canonical_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM canonical_networks WHERE id = $1 AND project_id = $2 AND NOT EXISTS (SELECT 1 FROM canonical_address_realms WHERE network_id = canonical_networks.id)",
        )
        .bind(network_id.to_string())
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_canonical_network(project_id, network_id).await? {
                Some(_) => Err(StoreError::NetworkInUse),
                None => Err(StoreError::NetworkNotFound),
            };
        }
        Ok(())
    }

    pub async fn insert_canonical_realm(
        &self,
        realm: &CanonicalAddressRealmRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&realm.state)?;
        let network = sqlx::query("SELECT project_id FROM canonical_networks WHERE id = $1")
            .bind(realm.network_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::NetworkNotFound)?;
        if network.get::<String, _>("project_id") != realm.project_id {
            return Err(StoreError::OwnershipConflict);
        }
        sqlx::query(
            "INSERT INTO canonical_address_realms (id, network_id, project_id, prefix, overlapping_prefixes, generation, state) VALUES ($1, $2, $3, $4::cidr, $5, $6, $7)",
        )
        .bind(realm.id.to_string())
        .bind(realm.network_id.to_string())
        .bind(&realm.project_id)
        .bind(&realm.prefix)
        .bind(realm.overlapping_prefixes)
        .bind(crate::checked_generation(realm.generation)?)
        .bind(&realm.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn list_canonical_realms(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressRealmRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, network_id, project_id, prefix::text AS prefix, overlapping_prefixes, generation, state FROM canonical_address_realms WHERE project_id = $1 AND network_id = $2 ORDER BY id",
        )
        .bind(project_id)
        .bind(network_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(canonical_realm_from_pg_row).collect()
    }

    pub async fn get_canonical_realm(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalAddressRealmRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, network_id, project_id, prefix::text AS prefix, overlapping_prefixes, generation, state FROM canonical_address_realms WHERE project_id = $1 AND id = $2",
        )
        .bind(project_id)
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.as_ref().map(canonical_realm_from_pg_row).transpose()
    }

    pub async fn delete_canonical_realm(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM canonical_address_realms WHERE id = $1 AND project_id = $2 AND NOT EXISTS (SELECT 1 FROM canonical_address_pools WHERE realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_endpoints WHERE realm_id = canonical_address_realms.id)",
        )
        .bind(realm_id.to_string())
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_canonical_realm(project_id, realm_id).await? {
                Some(_) => Err(StoreError::NetworkInUse),
                None => Err(StoreError::ResourceNotFound),
            };
        }
        Ok(())
    }

    pub async fn insert_canonical_pool(
        &self,
        pool: &CanonicalAddressPoolRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&pool.state)?;
        let realm = sqlx::query("SELECT project_id FROM canonical_address_realms WHERE id = $1")
            .bind(pool.realm_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        if realm.get::<String, _>("project_id") != pool.project_id {
            return Err(StoreError::OwnershipConflict);
        }
        sqlx::query(
            "INSERT INTO canonical_address_pools (id, realm_id, project_id, prefix, gateway, first_usable, last_usable, generation, state) VALUES ($1, $2, $3, $4::cidr, $5::inet, $6::inet, $7::inet, $8, $9)",
        )
        .bind(pool.id.to_string())
        .bind(pool.realm_id.to_string())
        .bind(&pool.project_id)
        .bind(&pool.prefix)
        .bind(pool.gateway.map(|value| value.to_string()))
        .bind(pool.first_usable.to_string())
        .bind(pool.last_usable.to_string())
        .bind(crate::checked_generation(pool.generation)?)
        .bind(&pool.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn insert_subnet_bundle(
        &self,
        realm: &CanonicalAddressRealmRecord,
        pool: &CanonicalAddressPoolRecord,
        subnet: &SubnetRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&realm.state)?;
        crate::validate_canonical_state(&pool.state)?;
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let owner: Option<String> = sqlx::query_scalar("SELECT project_id FROM canonical_networks WHERE id = $1 AND state = 'active' FOR UPDATE")
            .bind(realm.network_id.to_string()).fetch_optional(&mut *tx).await.map_err(StoreError::Database)?;
        if owner.as_deref() != Some(realm.project_id.as_str())
            || realm.project_id != pool.project_id
            || realm.project_id != subnet.project_id
            || realm.id != subnet.id
        {
            return Err(StoreError::OwnershipConflict);
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM canonical_address_realms WHERE network_id = $1 AND project_id = $2 AND state = 'active'")
            .bind(realm.network_id.to_string()).bind(&realm.project_id).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
        if count != 0 {
            return Err(StoreError::NetworkInUse);
        }
        sqlx::query("INSERT INTO canonical_address_realms (id, network_id, project_id, prefix, overlapping_prefixes, generation, state) VALUES ($1, $2, $3, $4::cidr, $5, $6, $7)")
            .bind(realm.id.to_string()).bind(realm.network_id.to_string()).bind(&realm.project_id).bind(&realm.prefix).bind(realm.overlapping_prefixes).bind(crate::checked_generation(realm.generation)?).bind(&realm.state).execute(&mut *tx).await.map_err(crate::map_canonical_insert_error)?;
        sqlx::query("INSERT INTO canonical_address_pools (id, realm_id, project_id, prefix, gateway, first_usable, last_usable, generation, state) VALUES ($1, $2, $3, $4::cidr, $5::inet, $6::inet, $7::inet, $8, $9)")
            .bind(pool.id.to_string()).bind(pool.realm_id.to_string()).bind(&pool.project_id).bind(&pool.prefix).bind(pool.gateway.map(|v| v.to_string())).bind(pool.first_usable.to_string()).bind(pool.last_usable.to_string()).bind(crate::checked_generation(pool.generation)?).bind(&pool.state).execute(&mut *tx).await.map_err(crate::map_canonical_insert_error)?;
        sqlx::query("INSERT INTO network_subnets (id, network_id, name, project_id, cidr, gateway_ip, allocation_start, allocation_end, ip_version, enable_dhcp) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(subnet.id.to_string()).bind(subnet.network_id.to_string()).bind(&subnet.name).bind(&subnet.project_id).bind(&subnet.cidr).bind(subnet.gateway_ip.to_string()).bind(subnet.allocation_start.to_string()).bind(subnet.allocation_end.to_string()).bind(i16::from(subnet.ip_version)).bind(subnet.enable_dhcp).execute(&mut *tx).await.map_err(crate::map_canonical_insert_error)?;
        tx.commit().await.map_err(StoreError::Database)
    }

    pub async fn list_canonical_pools(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressPoolRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, realm_id, project_id, prefix::text AS prefix, gateway::text AS gateway, first_usable::text AS first_usable, last_usable::text AS last_usable, generation, state FROM canonical_address_pools WHERE project_id = $1 AND realm_id = $2 ORDER BY id",
        )
        .bind(project_id)
        .bind(realm_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(canonical_pool_from_pg_row).collect()
    }

    pub async fn delete_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("DELETE FROM canonical_address_pools WHERE id = $1 AND project_id = $2")
                .bind(pool_id.to_string())
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    pub async fn update_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
        expected_generation: u64,
        gateway: Option<Ipv4Addr>,
    ) -> Result<CanonicalAddressPoolRecord, StoreError> {
        let result = sqlx::query(
            "UPDATE canonical_address_pools SET gateway = $1::inet, generation = generation + 1 WHERE id = $2 AND project_id = $3 AND generation = $4 AND state = 'active'",
        )
        .bind(gateway.map(|value| value.to_string()))
        .bind(pool_id.to_string())
        .bind(project_id)
        .bind(crate::checked_generation(expected_generation)?)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        let row = sqlx::query("SELECT id, realm_id, project_id, prefix::text AS prefix, gateway::text AS gateway, first_usable::text AS first_usable, last_usable::text AS last_usable, generation, state FROM canonical_address_pools WHERE id = $1 AND project_id = $2")
            .bind(pool_id.to_string())
            .bind(project_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        canonical_pool_from_pg_row(&row)
    }

    pub async fn insert_canonical_endpoint(
        &self,
        endpoint: &CanonicalEndpointRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&endpoint.state)?;
        let realm = sqlx::query("SELECT project_id FROM canonical_address_realms WHERE id = $1")
            .bind(endpoint.realm_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        if realm.get::<String, _>("project_id") != endpoint.project_id {
            return Err(StoreError::OwnershipConflict);
        }
        sqlx::query(
            "INSERT INTO canonical_endpoints (id, realm_id, project_id, fixed_ip, mac, generation, state) VALUES ($1, $2, $3, $4::inet, $5, $6, $7)",
        )
        .bind(endpoint.id.to_string())
        .bind(endpoint.realm_id.to_string())
        .bind(&endpoint.project_id)
        .bind(endpoint.fixed_ip.to_string())
        .bind(&endpoint.mac)
        .bind(crate::checked_generation(endpoint.generation)?)
        .bind(&endpoint.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn insert_canonical_endpoint_and_port(
        &self,
        endpoint: &CanonicalEndpointRecord,
        port: &PortRecord,
    ) -> Result<(), StoreError> {
        crate::validate_canonical_state(&endpoint.state)?;
        let subnet_id = port.subnet_id.ok_or(StoreError::ResourceNotFound)?;
        if endpoint.id != port.id
            || endpoint.realm_id != subnet_id
            || endpoint.project_id != port.project_id
        {
            return Err(StoreError::OwnershipConflict);
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let realm = sqlx::query(
            "SELECT network_id, project_id FROM canonical_address_realms WHERE id = $1 AND state = 'active'",
        )
        .bind(endpoint.realm_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        .ok_or(StoreError::ResourceNotFound)?;
        if realm.get::<String, _>("project_id") != endpoint.project_id
            || realm.get::<String, _>("network_id") != port.network_id.to_string()
        {
            return Err(StoreError::OwnershipConflict);
        }
        sqlx::query(
            "INSERT INTO canonical_endpoints (id, realm_id, project_id, fixed_ip, mac, generation, state) VALUES ($1, $2, $3, $4::inet, $5, $6, $7)",
        )
        .bind(endpoint.id.to_string())
        .bind(endpoint.realm_id.to_string())
        .bind(&endpoint.project_id)
        .bind(endpoint.fixed_ip.to_string())
        .bind(&endpoint.mac)
        .bind(crate::checked_generation(endpoint.generation)?)
        .bind(&endpoint.state)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_canonical_insert_error)?;
        sqlx::query(
            "INSERT INTO network_ports (id, network_id, subnet_id, project_id, name, mac_address, fixed_ip, status, binding_host, binding_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(port.id.to_string())
        .bind(port.network_id.to_string())
        .bind(subnet_id.to_string())
        .bind(&port.project_id)
        .bind(&port.name)
        .bind(&port.mac_address)
        .bind(port.fixed_ip.to_string())
        .bind(&port.status)
        .bind(&port.binding_host)
        .bind(&port.binding_state)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_canonical_insert_error)?;
        tx.commit().await.map_err(StoreError::Database)
    }

    pub async fn list_canonical_endpoints(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalEndpointRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, realm_id, project_id, fixed_ip::text AS fixed_ip, mac, generation, state FROM canonical_endpoints WHERE project_id = $1 AND realm_id = $2 ORDER BY id",
        )
        .bind(project_id)
        .bind(realm_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(canonical_endpoint_from_pg_row).collect()
    }

    pub async fn get_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<Option<CanonicalEndpointRecord>, StoreError> {
        let row = sqlx::query("SELECT id, realm_id, project_id, fixed_ip::text AS fixed_ip, mac, generation, state FROM canonical_endpoints WHERE id = $1 AND project_id = $2")
            .bind(endpoint_id.to_string()).bind(project_id).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.as_ref().map(canonical_endpoint_from_pg_row).transpose()
    }

    pub async fn delete_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM canonical_network_policies WHERE endpoint_id = $1 AND project_id = $2",
        )
        .bind(endpoint_id.to_string())
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        let result =
            sqlx::query("DELETE FROM canonical_endpoints WHERE id = $1 AND project_id = $2")
                .bind(endpoint_id.to_string())
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    pub async fn delete_canonical_endpoint_and_port(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query(
            "DELETE FROM canonical_network_policies WHERE endpoint_id = $1 AND project_id = $2",
        )
        .bind(endpoint_id.to_string())
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        let result =
            sqlx::query("DELETE FROM canonical_endpoints WHERE id = $1 AND project_id = $2")
                .bind(endpoint_id.to_string())
                .bind(project_id)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        sqlx::query("DELETE FROM network_ports WHERE id = $1 AND project_id = $2")
            .bind(endpoint_id.to_string())
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)
    }

    pub async fn upsert_canonical_policy(
        &self,
        policy: &CanonicalNetworkPolicyRecord,
    ) -> Result<(), StoreError> {
        let endpoint_project: String =
            sqlx::query_scalar("SELECT project_id FROM canonical_endpoints WHERE id = $1")
                .bind(policy.endpoint_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?
                .ok_or(StoreError::ResourceNotFound)?;
        if endpoint_project != policy.project_id {
            return Err(StoreError::OwnershipConflict);
        }
        crate::checked_generation(policy.generation)?;
        sqlx::query(
            "INSERT INTO canonical_network_policies (id, project_id, endpoint_id, direction, protocol, port_min, port_max, source, destination, action, generation, state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::cidr, $9::cidr, $10, $11, $12) ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, endpoint_id=excluded.endpoint_id, direction=excluded.direction, protocol=excluded.protocol, port_min=excluded.port_min, port_max=excluded.port_max, source=excluded.source, destination=excluded.destination, action=excluded.action, generation=excluded.generation, state=excluded.state",
        )
        .bind(policy.id.to_string())
        .bind(&policy.project_id)
        .bind(policy.endpoint_id.to_string())
        .bind(&policy.direction)
        .bind(&policy.protocol)
        .bind(policy.port_min.map(i32::from))
        .bind(policy.port_max.map(i32::from))
        .bind(&policy.source)
        .bind(&policy.destination)
        .bind(&policy.action)
        .bind(crate::checked_generation(policy.generation)?)
        .bind(&policy.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn list_canonical_policies(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalNetworkPolicyRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT p.id, p.project_id, p.endpoint_id, p.direction, p.protocol, p.port_min, p.port_max, p.source::text AS source, p.destination::text AS destination, p.action, p.generation, p.state FROM canonical_network_policies p JOIN canonical_endpoints e ON e.id = p.endpoint_id JOIN canonical_address_realms r ON r.id = e.realm_id WHERE p.project_id = $1 AND r.project_id = $1 AND r.network_id = $2 ORDER BY p.id",
        )
        .bind(project_id)
        .bind(network_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(canonical_policy_from_pg_row).collect()
    }

    pub async fn delete_canonical_policy(
        &self,
        project_id: &str,
        policy_id: &Uuid,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("DELETE FROM canonical_network_policies WHERE id = $1 AND project_id = $2")
                .bind(policy_id.to_string())
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    pub async fn begin_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<CanonicalAddressRealmRecord, StoreError> {
        let result = sqlx::query(
            "UPDATE canonical_address_realms SET state = 'deleting', generation = generation + 1 WHERE id = $1 AND project_id = $2 AND generation = $3 AND state = 'active' AND NOT EXISTS (SELECT 1 FROM canonical_address_pools WHERE realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_endpoints WHERE realm_id = canonical_address_realms.id)",
        )
        .bind(realm_id.to_string())
        .bind(project_id)
        .bind(crate::checked_generation(expected_generation)?)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_canonical_realm(project_id, realm_id).await? {
                Some(realm) if realm.state == "deleting" => Ok(realm),
                Some(_) => Err(StoreError::NetworkInUse),
                None => Err(StoreError::ResourceNotFound),
            };
        }
        self.get_canonical_realm(project_id, realm_id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    pub async fn finalize_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM canonical_address_realms WHERE id = $1 AND project_id = $2 AND generation = $3 AND state = 'deleting' AND NOT EXISTS (SELECT 1 FROM canonical_address_pools WHERE realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_endpoints WHERE realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_network_policies p JOIN canonical_endpoints e ON e.id = p.endpoint_id WHERE e.realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_realm_encapsulation_bindings WHERE realm_id = canonical_address_realms.id)",
        )
        .bind(realm_id.to_string())
        .bind(project_id)
        .bind(crate::checked_generation(expected_generation)?)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_canonical_realm(project_id, realm_id).await? {
                Some(_) => Err(StoreError::NetworkInUse),
                None => Err(StoreError::ResourceNotFound),
            };
        }
        Ok(())
    }

    pub async fn insert_canonical_realm_binding(
        &self,
        binding: &CanonicalRealmBindingRecord,
    ) -> Result<(), StoreError> {
        let realm = sqlx::query("SELECT 1 FROM canonical_address_realms WHERE id = $1")
            .bind(binding.realm_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if realm.is_none() {
            return Err(StoreError::ResourceNotFound);
        }
        crate::validate_canonical_state(&binding.state)?;
        sqlx::query(
            "INSERT INTO canonical_realm_encapsulation_bindings (fabric_domain_id, realm_id, provider_kind, provider_segment_id, binding_generation, state) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&binding.fabric_domain_id)
        .bind(binding.realm_id.to_string())
        .bind(&binding.provider_kind)
        .bind(crate::checked_generation(binding.provider_segment_id)?)
        .bind(crate::checked_generation(binding.binding_generation)?)
        .bind(&binding.state)
        .execute(&self.pool)
        .await
        .map_err(crate::map_canonical_insert_error)
        .map(|_| ())
    }

    pub async fn get_canonical_realm_binding(
        &self,
        fabric_domain_id: &str,
        realm_id: &Uuid,
    ) -> Result<Option<CanonicalRealmBindingRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT fabric_domain_id, realm_id, provider_kind, provider_segment_id, binding_generation, state FROM canonical_realm_encapsulation_bindings WHERE fabric_domain_id = $1 AND realm_id = $2",
        )
        .bind(fabric_domain_id)
        .bind(realm_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.map(|row| {
            Ok(CanonicalRealmBindingRecord {
                fabric_domain_id: row.get("fabric_domain_id"),
                realm_id: Uuid::parse_str(row.get::<&str, _>("realm_id"))
                    .map_err(StoreError::InvalidUuid)?,
                provider_kind: row.get("provider_kind"),
                provider_segment_id: u64::try_from(row.get::<i64, _>("provider_segment_id"))
                    .map_err(|_| StoreError::Corrupt("negative provider segment".into()))?,
                binding_generation: u64::try_from(row.get::<i64, _>("binding_generation"))
                    .map_err(|_| StoreError::Corrupt("negative binding generation".into()))?,
                state: row.get("state"),
            })
        })
        .transpose()
    }

    pub async fn list_canonical_realm_bindings(
        &self,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalRealmBindingRecord>, StoreError> {
        let rows = sqlx::query("SELECT fabric_domain_id, realm_id, provider_kind, provider_segment_id, binding_generation, state FROM canonical_realm_encapsulation_bindings WHERE realm_id = $1 ORDER BY fabric_domain_id")
            .bind(realm_id.to_string()).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.into_iter()
            .map(|row| {
                Ok(CanonicalRealmBindingRecord {
                    fabric_domain_id: row.get("fabric_domain_id"),
                    realm_id: Uuid::parse_str(row.get::<&str, _>("realm_id"))
                        .map_err(StoreError::InvalidUuid)?,
                    provider_kind: row.get("provider_kind"),
                    provider_segment_id: u64::try_from(row.get::<i64, _>("provider_segment_id"))
                        .map_err(|_| StoreError::Corrupt("negative provider segment".into()))?,
                    binding_generation: u64::try_from(row.get::<i64, _>("binding_generation"))
                        .map_err(|_| StoreError::Corrupt("negative binding generation".into()))?,
                    state: row.get("state"),
                })
            })
            .collect()
    }

    pub async fn delete_canonical_realm_binding(
        &self,
        binding: &CanonicalRealmBindingRecord,
        expected_realm_generation: u64,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM canonical_realm_encapsulation_bindings WHERE fabric_domain_id = $1 AND realm_id = $2 AND provider_kind = $3 AND provider_segment_id = $4 AND binding_generation = $5 AND binding_generation < $6")
            .bind(&binding.fabric_domain_id).bind(binding.realm_id.to_string()).bind(&binding.provider_kind)
            .bind(crate::checked_generation(binding.provider_segment_id)?).bind(crate::checked_generation(binding.binding_generation)?)
            .bind(crate::checked_generation(expected_realm_generation)?).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        Ok(())
    }

    async fn backfill_canonical_network_state(&self) -> Result<(), StoreError> {
        // PostgreSQL uses the same legacy-source audit and identity-preserving
        // materialization as SQLite. The transaction makes retry after a
        // failed validation safe and prevents partially inserted relations.
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let intents = sqlx::query(
            "SELECT id, project_id, generation, payload, status FROM network_intents ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        for intent in &intents {
            let id_text: String = intent.get("id");
            if Uuid::parse_str(&id_text).is_err() {
                continue;
            }
            let generation: i64 = intent.get("generation");
            let generation = u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("negative network intent generation".into()))?;
            let state: String = intent.get("status");
            crate::validate_canonical_state(&state)?;
            sqlx::query(
                "INSERT INTO canonical_networks (id, project_id, name, generation, state) VALUES ($1, $2, '', $3, $4) ON CONFLICT(id) DO NOTHING",
            )
            .bind(&id_text)
            .bind(intent.get::<String, _>("project_id"))
            .bind(crate::checked_generation(generation)?)
            .bind(&state)
            .execute(&mut *tx)
            .await
            .map_err(crate::map_canonical_insert_error)?;
            let payload: serde_json::Value =
                serde_json::from_str(intent.get("payload")).map_err(|error| {
                    StoreError::Corrupt(format!("invalid network intent JSON: {error}"))
                })?;
            if let Some(payload_id) = payload.get("id").and_then(serde_json::Value::as_str)
                && payload_id != id_text
            {
                return Err(StoreError::Corrupt(
                    "network intent payload identity contradicts its row".into(),
                ));
            }
        }
        let networks = sqlx::query("SELECT id, name, project_id FROM network_networks ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        for network in &networks {
            sqlx::query(
                "INSERT INTO canonical_networks (id, project_id, name, generation, state) VALUES ($1, $2, $3, 1, 'active') ON CONFLICT(id) DO NOTHING",
            )
            .bind(network.get::<String, _>("id"))
            .bind(network.get::<String, _>("project_id"))
            .bind(network.get::<String, _>("name"))
            .execute(&mut *tx)
            .await
            .map_err(crate::map_canonical_insert_error)?;
            let canonical: (String, String) =
                sqlx::query_as("SELECT project_id, name FROM canonical_networks WHERE id = $1")
                    .bind(network.get::<String, _>("id"))
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(StoreError::Database)?;
            if canonical.0 != network.get::<String, _>("project_id")
                || (canonical.1 != network.get::<String, _>("name") && !canonical.1.is_empty())
            {
                return Err(StoreError::OwnershipConflict);
            }
            if canonical.1.is_empty() {
                sqlx::query("UPDATE canonical_networks SET name = $1 WHERE id = $2")
                    .bind(network.get::<String, _>("name"))
                    .bind(network.get::<String, _>("id"))
                    .execute(&mut *tx)
                    .await
                    .map_err(StoreError::Database)?;
            }
        }
        let subnets = sqlx::query(
            "SELECT id, network_id, project_id, cidr::text AS cidr, gateway_ip::text AS gateway_ip, allocation_start::text AS allocation_start, allocation_end::text AS allocation_end FROM network_subnets ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        for subnet in &subnets {
            let network_id: String = subnet.get("network_id");
            let project_id: String = subnet.get("project_id");
            let network_project: String =
                sqlx::query_scalar("SELECT project_id FROM canonical_networks WHERE id = $1")
                    .bind(&network_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(StoreError::Database)?
                    .ok_or(StoreError::NetworkNotFound)?;
            if network_project != project_id {
                return Err(StoreError::OwnershipConflict);
            }
            let id: String = subnet.get("id");
            let prefix: String = subnet.get("cidr");
            crate::validate_ipv4_cidr(&prefix)?;
            sqlx::query(
                "INSERT INTO canonical_address_realms (id, network_id, project_id, prefix, overlapping_prefixes, generation, state) VALUES ($1, $2, $3, $4::cidr, false, 1, 'active') ON CONFLICT(id) DO NOTHING",
            )
            .bind(&id)
            .bind(&network_id)
            .bind(&project_id)
            .bind(&prefix)
            .execute(&mut *tx)
            .await
            .map_err(crate::map_canonical_insert_error)?;
            sqlx::query(
                "INSERT INTO canonical_address_pools (id, realm_id, project_id, prefix, gateway, first_usable, last_usable, generation, state) VALUES ($1, $2, $3, $4::cidr, $5::inet, $6::inet, $7::inet, 1, 'active') ON CONFLICT(id) DO NOTHING",
            )
            .bind(&id)
            .bind(&id)
            .bind(&project_id)
            .bind(&prefix)
            .bind(subnet.get::<String, _>("gateway_ip"))
            .bind(subnet.get::<String, _>("allocation_start"))
            .bind(subnet.get::<String, _>("allocation_end"))
            .execute(&mut *tx)
            .await
            .map_err(crate::map_canonical_insert_error)?;
        }
        let ports = sqlx::query(
            "SELECT id, subnet_id, project_id, fixed_ip::text AS fixed_ip, mac_address FROM network_ports ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        for port in &ports {
            let realm_id: String = port.get::<Option<String>, _>("subnet_id").ok_or_else(|| {
                StoreError::Corrupt("legacy endpoint has no explicit subnet owner".into())
            })?;
            let project_id: String = port.get("project_id");
            let realm_project: String =
                sqlx::query_scalar("SELECT project_id FROM canonical_address_realms WHERE id = $1")
                    .bind(&realm_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(StoreError::Database)?
                    .ok_or(StoreError::ResourceNotFound)?;
            if realm_project != project_id {
                return Err(StoreError::OwnershipConflict);
            }
            sqlx::query(
                "INSERT INTO canonical_endpoints (id, realm_id, project_id, fixed_ip, mac, generation, state) VALUES ($1, $2, $3, $4::inet, $5, 1, 'active') ON CONFLICT(id) DO NOTHING",
            )
            .bind(port.get::<String, _>("id"))
            .bind(&realm_id)
            .bind(&project_id)
            .bind(port.get::<String, _>("fixed_ip"))
            .bind(port.get::<String, _>("mac_address"))
            .execute(&mut *tx)
            .await
            .map_err(crate::map_canonical_insert_error)?;
        }
        for intent in &intents {
            let project_id: String = intent.get("project_id");
            for policy in crate::legacy_policy_records(intent.get("payload"), &project_id)? {
                let endpoint_project: Option<String> =
                    sqlx::query_scalar("SELECT project_id FROM canonical_endpoints WHERE id = $1")
                        .bind(policy.endpoint_id.to_string())
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(StoreError::Database)?;
                if endpoint_project.as_deref() != Some(project_id.as_str()) {
                    return Err(StoreError::OwnershipConflict);
                }
                sqlx::query(
                    "INSERT INTO canonical_network_policies (id, project_id, endpoint_id, direction, protocol, port_min, port_max, source, destination, action, generation, state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::cidr, $9::cidr, $10, $11, $12) ON CONFLICT(id) DO NOTHING",
                )
                .bind(policy.id.to_string())
                .bind(&policy.project_id)
                .bind(policy.endpoint_id.to_string())
                .bind(&policy.direction)
                .bind(&policy.protocol)
                .bind(policy.port_min.map(i32::from))
                .bind(policy.port_max.map(i32::from))
                .bind(&policy.source)
                .bind(&policy.destination)
                .bind(&policy.action)
                .bind(crate::checked_generation(policy.generation)?)
                .bind(&policy.state)
                .execute(&mut *tx)
                .await
                .map_err(crate::map_canonical_insert_error)?;
            }
        }
        tx.commit().await.map_err(StoreError::Database)
    }
    pub async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        let result = sqlx::query("INSERT INTO resource_relationships (parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(record.parent_resource_id.to_string()).bind(&record.parent_resource_type).bind(&record.slot).bind(&record.expected_child_resource_type)
            .bind(record.child_resource_id.map(|id| id.to_string())).bind(&record.ownership).bind(record.parent_operation_id.to_string()).bind(record.child_operation_id.map(|id| id.to_string()))
            .bind(&record.owner_scope).bind("reserved").bind(&record.fingerprint).execute(&self.pool).await;
        if let Err(error) = result {
            let conflict = matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation());
            if !conflict {
                return Err(StoreError::Database(error));
            }
            let existing = self
                .get_relationship(record.parent_resource_id, &record.slot)
                .await?;
            if existing.fingerprint == record.fingerprint
                && existing.expected_child_resource_type == record.expected_child_resource_type
                && existing.ownership == record.ownership
                && existing.owner_scope == record.owner_scope
            {
                return Ok(existing);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        self.get_relationship(record.parent_resource_id, &record.slot)
            .await
    }

    pub async fn get_relationship(
        &self,
        parent: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        let row = sqlx::query("SELECT parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint FROM resource_relationships WHERE parent_resource_id=$1 AND slot=$2")
            .bind(parent.to_string()).bind(slot).fetch_optional(&self.pool).await.map_err(StoreError::Database)?.ok_or(StoreError::ResourceNotFound)?;
        relationship_from_pg_row(&row)
    }

    pub async fn list_relationships(
        &self,
        parent: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        let rows = sqlx::query("SELECT parent_resource_id,parent_resource_type,slot,expected_child_resource_type,child_resource_id,ownership,parent_operation_id,child_operation_id,owner_scope,state,fingerprint FROM resource_relationships WHERE parent_resource_id=$1 ORDER BY slot")
            .bind(parent.to_string()).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(relationship_from_pg_row).collect()
    }

    pub async fn bind_relationship(
        &self,
        parent: Uuid,
        slot: &str,
        child: Uuid,
        child_operation: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        sqlx::query("UPDATE resource_relationships SET child_resource_id=$1,child_operation_id=$2,state='bound' WHERE parent_resource_id=$3 AND slot=$4 AND state IN ('reserved','unknown')")
            .bind(child.to_string()).bind(child_operation.to_string()).bind(parent.to_string()).bind(slot).execute(&self.pool).await.map_err(StoreError::Database)?;
        self.get_relationship(parent, slot).await
    }

    pub async fn set_relationship_state(
        &self,
        parent: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        if !matches!(
            state,
            "reserved" | "bound" | "deleting" | "deleted" | "unknown"
        ) {
            return Err(StoreError::Corrupt("invalid relationship state".into()));
        }
        sqlx::query(
            "UPDATE resource_relationships SET state=$1 WHERE parent_resource_id=$2 AND slot=$3",
        )
        .bind(state)
        .bind(parent.to_string())
        .bind(slot)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        self.get_relationship(parent, slot).await
    }

    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(database_url).map_err(StoreError::Database)?;
        let pool = PgPoolOptions::new()
            .max_connections(50)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(StoreError::Database)?;

        let store = Self { pool };
        store.migrate().await?;
        store.backfill_canonical_network_state().await?;
        Ok(store)
    }

    pub async fn connect_pool(pool: PgPool) -> Result<Self, StoreError> {
        let store = Self { pool };
        store.migrate().await?;
        store.backfill_canonical_network_state().await?;
        Ok(store)
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations_postgres")
            .run(&self.pool)
            .await
            .map_err(StoreError::Migration)
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn database_health(&self) -> Result<DatabaseHealth, StoreError> {
        let row = sqlx::query("SELECT version()")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let ver: String = row.get(0);
        Ok(DatabaseHealth {
            status: "ok".to_owned(),
            journal_mode: "postgres".to_owned(),
            foreign_keys: true,
            integrity_check: ver,
            page_count: 0,
            page_size: 0,
            wal_checkpoint_status: None,
        })
    }

    pub async fn readiness_check(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn clean_tables_for_testing(&self) -> Result<(), StoreError> {
        sqlx::query(
            "TRUNCATE TABLE
                resources, operations, canonical_operation_metadata, idempotency_reservations,
                provider_refs, observation_watermarks,
                keypairs, server_keypairs, agent_commands, artifact_transfers,
                image_overlay_ownership, volume_attachments,
                keystone_domains, keystone_projects, keystone_users, keystone_roles,
                keystone_role_assignments, keystone_services, keystone_endpoints, keystone_regions,
                image_metadata, network_intents, network_networks, network_subnets, network_ports,
                canonical_realm_encapsulation_bindings, canonical_endpoints, canonical_address_pools, canonical_address_realms, canonical_networks,
                network_security_group_bindings, network_security_group_rules, network_security_groups,
                placement_providers, placement_inventories, placement_allocations,
                placement_allocation_resources, placement_allocation_intents, placement_allocation_intent_resources,
                quota_limits, quota_reservations, quota_reservation_amounts,
                controller_sessions, work_leases
            CASCADE",
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }
}

#[async_trait]
impl crate::RelationshipRepository for PostgresStore {
    async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::reserve_relationship(self, record).await
    }

    async fn get_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::get_relationship(self, parent_resource_id, slot).await
    }

    async fn list_relationships(
        &self,
        parent_resource_id: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        Self::list_relationships(self, parent_resource_id).await
    }

    async fn bind_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        child_resource_id: Uuid,
        child_operation_id: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::bind_relationship(
            self,
            parent_resource_id,
            slot,
            child_resource_id,
            child_operation_id,
        )
        .await
    }

    async fn set_relationship_state(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        Self::set_relationship_state(self, parent_resource_id, slot, state).await
    }
}

fn relationship_from_pg_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ResourceRelationshipRecord, StoreError> {
    Ok(ResourceRelationshipRecord {
        parent_resource_id: Uuid::parse_str(
            &row.try_get::<String, _>("parent_resource_id")
                .map_err(StoreError::Database)?,
        )
        .map_err(StoreError::InvalidUuid)?,
        parent_resource_type: row
            .try_get("parent_resource_type")
            .map_err(StoreError::Database)?,
        slot: row.try_get("slot").map_err(StoreError::Database)?,
        expected_child_resource_type: row
            .try_get("expected_child_resource_type")
            .map_err(StoreError::Database)?,
        child_resource_id: row
            .try_get::<Option<String>, _>("child_resource_id")
            .map_err(StoreError::Database)?
            .map(|id| Uuid::parse_str(&id).map_err(StoreError::InvalidUuid))
            .transpose()?,
        ownership: row.try_get("ownership").map_err(StoreError::Database)?,
        parent_operation_id: Uuid::parse_str(
            &row.try_get::<String, _>("parent_operation_id")
                .map_err(StoreError::Database)?,
        )
        .map_err(StoreError::InvalidUuid)?,
        child_operation_id: row
            .try_get::<Option<String>, _>("child_operation_id")
            .map_err(StoreError::Database)?
            .map(|id| Uuid::parse_str(&id).map_err(StoreError::InvalidUuid))
            .transpose()?,
        owner_scope: row.try_get("owner_scope").map_err(StoreError::Database)?,
        state: row.try_get("state").map_err(StoreError::Database)?,
        fingerprint: row.try_get("fingerprint").map_err(StoreError::Database)?,
    })
}

fn map_pg_error(error: sqlx::Error) -> StoreError {
    match &error {
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505") => {
            StoreError::ResourceAlreadyExists
        }
        _ => StoreError::Database(error),
    }
}

fn parse_uuid(s: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(s).map_err(StoreError::InvalidUuid)
}

fn row_to_resource(row: &PgRow) -> Result<ResourceRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    let generation: i64 = row.get("generation");
    let observed_generation: i64 = row.get("observed_generation");
    Ok(ResourceRecord {
        id,
        kind: row.get("kind"),
        project_id: row.get("project_id"),
        generation,
        observed_generation,
        desired_state: row.get("desired_state"),
        observed_state: row.get("observed_state"),
        provider_id: row.get("provider_id"),
    })
}

fn row_to_operation(row: &PgRow) -> Result<OperationRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    let resource_id_str: String = row.get("resource_id");
    let resource_id = parse_uuid(&resource_id_str)?;
    let state_str: String = row.get("state");
    let state = OperationState::parse(&state_str)?;

    Ok(OperationRecord {
        id,
        resource_id,
        kind: row.get("kind"),
        state,
        provider_operation_id: row.get("provider_operation_id"),
        error_category: row.get("error_category"),
        error_message: row.get("error_message"),
    })
}

fn validate_image_overlay_transition(
    current: ImageOverlayState,
    next: ImageOverlayState,
) -> Result<(), StoreError> {
    if current == next {
        return Ok(());
    }
    match (current, next) {
        (ImageOverlayState::Pending, ImageOverlayState::Materializing) => Ok(()),
        (ImageOverlayState::Materializing, ImageOverlayState::Ready) => Ok(()),
        (ImageOverlayState::Ready, ImageOverlayState::Deleting) => Ok(()),
        (ImageOverlayState::Deleting, ImageOverlayState::Deleted) => Ok(()),
        (ImageOverlayState::Pending, ImageOverlayState::Failed) => Ok(()),
        (ImageOverlayState::Materializing, ImageOverlayState::Failed) => Ok(()),
        (ImageOverlayState::Ready, ImageOverlayState::Failed) => Ok(()),
        (ImageOverlayState::Deleting, ImageOverlayState::Failed) => Ok(()),
        (ImageOverlayState::Failed, ImageOverlayState::Deleting) => Ok(()),
        (ImageOverlayState::Failed, ImageOverlayState::Deleted) => Ok(()),
        _ => Err(StoreError::ImageOverlayConflict(format!(
            "invalid image overlay state transition from {current:?} to {next:?}"
        ))),
    }
}

async fn validate_existing_canonical_reservation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: Uuid,
    request: &IdempotencyReservationRequest,
) -> Result<(), StoreError> {
    let durable_row = sqlx::query("SELECT * FROM operations WHERE id=$1")
        .bind(operation_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(StoreError::Database)?
        .ok_or_else(|| {
            StoreError::Corrupt("idempotency reservation references missing operation".into())
        })?;
    let durable = row_to_operation(&durable_row)?;
    let metadata = sqlx::query("SELECT * FROM canonical_operation_metadata WHERE operation_id=$1")
        .bind(operation_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(StoreError::Database)?
        .ok_or_else(|| {
            StoreError::Corrupt(
                "idempotency reservation references operation without canonical metadata".into(),
            )
        })?;
    let canonical = CanonicalOperationRecord {
        id: operation_id,
        service: metadata.try_get("service").map_err(StoreError::Database)?,
        action: metadata.try_get("action").map_err(StoreError::Database)?,
        actor: metadata.try_get("actor").map_err(StoreError::Database)?,
        owner_scope: metadata
            .try_get("owner_scope")
            .map_err(StoreError::Database)?,
        resource_type: metadata
            .try_get("resource_type")
            .map_err(StoreError::Database)?,
        resource_id: metadata
            .try_get("resource_id")
            .map_err(StoreError::Database)?,
        state: durable.state,
        attempt: u32::try_from(
            metadata
                .try_get::<i32, _>("attempt")
                .map_err(StoreError::Database)?,
        )
        .map_err(|_| StoreError::Corrupt("invalid operation attempt".into()))?,
        created_at: metadata
            .try_get("created_at")
            .map_err(StoreError::Database)?,
        started_at: metadata
            .try_get("started_at")
            .map_err(StoreError::Database)?,
        finished_at: metadata
            .try_get("finished_at")
            .map_err(StoreError::Database)?,
        error: metadata.try_get("error").map_err(StoreError::Database)?,
        request_id: metadata
            .try_get("request_id")
            .map_err(StoreError::Database)?,
    };
    let mut winning_request = request.clone();
    winning_request.operation_id = operation_id;
    validate_canonical_idempotent_operation_identity(&durable, &canonical, &winning_request)?;

    let resource_owner: String = sqlx::query("SELECT project_id FROM resources WHERE id=$1")
        .bind(durable.resource_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(StoreError::Database)?
        .ok_or_else(|| {
            StoreError::Corrupt("canonical operation references missing resource".into())
        })?
        .try_get("project_id")
        .map_err(StoreError::Database)?;
    if resource_owner != canonical.owner_scope {
        return Err(StoreError::Corrupt(
            "canonical operation resource and owner scopes differ".into(),
        ));
    }
    Ok(())
}

async fn postgres_existing_acceptance(
    pool: &sqlx::PgPool,
    request: &IdempotencyReservationRequest,
) -> Result<Option<crate::CanonicalAcceptanceOutcome>, StoreError> {
    let Some(row) = sqlx::query("SELECT fingerprint,operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3")
        .bind(&request.owner_scope).bind(&request.action).bind(&request.key)
        .fetch_optional(pool).await.map_err(StoreError::Database)? else { return Ok(None); };
    if row
        .try_get::<String, _>("fingerprint")
        .map_err(StoreError::Database)?
        != request.fingerprint
    {
        return Ok(Some(crate::CanonicalAcceptanceOutcome::Conflict));
    }
    let operation_id = Uuid::parse_str(
        &row.try_get::<String, _>("operation_id")
            .map_err(StoreError::Database)?,
    )
    .map_err(StoreError::InvalidUuid)?;
    let durable = row_to_operation(
        &sqlx::query("SELECT * FROM operations WHERE id=$1")
            .bind(operation_id.to_string())
            .fetch_one(pool)
            .await
            .map_err(StoreError::Database)?,
    )?;
    let metadata = sqlx::query("SELECT * FROM canonical_operation_metadata WHERE operation_id=$1")
        .bind(operation_id.to_string())
        .fetch_one(pool)
        .await
        .map_err(StoreError::Database)?;
    let canonical = CanonicalOperationRecord {
        id: operation_id,
        service: metadata.try_get("service").map_err(StoreError::Database)?,
        action: metadata.try_get("action").map_err(StoreError::Database)?,
        actor: metadata.try_get("actor").map_err(StoreError::Database)?,
        owner_scope: metadata
            .try_get("owner_scope")
            .map_err(StoreError::Database)?,
        resource_type: metadata
            .try_get("resource_type")
            .map_err(StoreError::Database)?,
        resource_id: metadata
            .try_get("resource_id")
            .map_err(StoreError::Database)?,
        state: durable.state,
        attempt: u32::try_from(
            metadata
                .try_get::<i32, _>("attempt")
                .map_err(StoreError::Database)?,
        )
        .map_err(|_| StoreError::Corrupt("invalid operation attempt".into()))?,
        created_at: metadata
            .try_get("created_at")
            .map_err(StoreError::Database)?,
        started_at: metadata
            .try_get("started_at")
            .map_err(StoreError::Database)?,
        finished_at: metadata
            .try_get("finished_at")
            .map_err(StoreError::Database)?,
        error: metadata.try_get("error").map_err(StoreError::Database)?,
        request_id: metadata
            .try_get("request_id")
            .map_err(StoreError::Database)?,
    };
    let resource = row_to_resource(
        &sqlx::query("SELECT * FROM resources WHERE id=$1")
            .bind(durable.resource_id.to_string())
            .fetch_one(pool)
            .await
            .map_err(StoreError::Database)?,
    )?;
    let mut replay = request.clone();
    replay.operation_id = operation_id;
    crate::validate_canonical_resource_acceptance(&resource, &durable, &canonical, &replay)?;
    Ok(Some(
        crate::CanonicalAcceptanceOutcome::ExistingEquivalent {
            operation_id,
            resource_id: resource.id,
        },
    ))
}

async fn postgres_existing_acceptance_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &IdempotencyReservationRequest,
) -> Result<Option<crate::CanonicalAcceptanceOutcome>, StoreError> {
    let Some(row) = sqlx::query("SELECT fingerprint,operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3")
        .bind(&request.owner_scope).bind(&request.action).bind(&request.key)
        .fetch_optional(&mut **tx).await.map_err(StoreError::Database)? else { return Ok(None); };
    if row
        .try_get::<String, _>("fingerprint")
        .map_err(StoreError::Database)?
        != request.fingerprint
    {
        return Ok(Some(crate::CanonicalAcceptanceOutcome::Conflict));
    }
    let operation_id = Uuid::parse_str(
        &row.try_get::<String, _>("operation_id")
            .map_err(StoreError::Database)?,
    )
    .map_err(StoreError::InvalidUuid)?;
    validate_existing_canonical_reservation(tx, operation_id, request).await?;
    let resource_id = Uuid::parse_str(
        &sqlx::query("SELECT resource_id FROM operations WHERE id=$1")
            .bind(operation_id.to_string())
            .fetch_one(&mut **tx)
            .await
            .map_err(StoreError::Database)?
            .try_get::<String, _>("resource_id")
            .map_err(StoreError::Database)?,
    )
    .map_err(StoreError::InvalidUuid)?;
    Ok(Some(
        crate::CanonicalAcceptanceOutcome::ExistingEquivalent {
            operation_id,
            resource_id,
        },
    ))
}

async fn insert_postgres_canonical_acceptance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation: &OperationRecord,
    canonical: &CanonicalOperationRecord,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO operations (id,resource_id,kind,state,provider_operation_id,error_category,error_message) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind).bind(operation.state.as_str())
        .bind(&operation.provider_operation_id).bind(&operation.error_category).bind(&operation.error_message)
        .execute(&mut **tx).await.map_err(map_pg_error)?;
    sqlx::query("INSERT INTO canonical_operation_metadata (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,created_at,started_at,finished_at,error,request_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
        .bind(canonical.id.to_string()).bind(&canonical.service).bind(&canonical.action).bind(&canonical.actor).bind(&canonical.owner_scope)
        .bind(&canonical.resource_type).bind(&canonical.resource_id).bind(i32::try_from(canonical.attempt).map_err(|_| StoreError::Corrupt("operation attempt exceeds storage range".into()))?)
        .bind(&canonical.created_at).bind(&canonical.started_at).bind(&canonical.finished_at).bind(&canonical.error).bind(&canonical.request_id)
        .execute(&mut **tx).await.map_err(map_pg_error)?;
    Ok(())
}

#[async_trait]
impl DurableStore for PostgresStore {
    async fn insert_resource(&self, resource: &ResourceRecord) -> Result<(), StoreError> {
        let id_str = resource.id.to_string();
        sqlx::query(
            "INSERT INTO resources (id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&id_str)
        .bind(&resource.kind)
        .bind(&resource.project_id)
        .bind(resource.generation)
        .bind(resource.observed_generation)
        .bind(&resource.desired_state)
        .bind(&resource.observed_state)
        .bind(&resource.provider_id)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn get_resource(&self, id: Uuid) -> Result<ResourceRecord, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM resources WHERE id = $1")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        match row {
            Some(row) => row_to_resource(&row),
            None => Err(StoreError::ResourceNotFound),
        }
    }

    async fn list_resources(
        &self,
        project_id: &str,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM resources WHERE project_id = $1 AND kind = $2 ORDER BY created_at ASC",
        )
        .bind(project_id)
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(row_to_resource).collect()
    }

    async fn update_resource(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE resources
             SET desired_state = $1, observed_state = $2, observed_generation = $3, provider_id = $4, generation = generation + 1
             WHERE id = $5 AND generation = $6",
        )
        .bind(desired_state)
        .bind(observed_state)
        .bind(observed_generation)
        .bind(provider_id)
        .bind(&id_str)
        .bind(expected_generation)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            let exists = sqlx::query("SELECT 1 FROM resources WHERE id = $1")
                .bind(&id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;
            if exists.is_none() {
                return Err(StoreError::ResourceNotFound);
            }
            return Err(StoreError::StaleGeneration);
        }

        self.get_resource(id).await
    }

    async fn update_resource_from_observation(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let id_str = id.to_string();

        let row = sqlx::query("SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE id = $1 FOR UPDATE")
            .bind(&id_str)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;

        let current = row_to_resource(&row)?;
        if current.generation != update.expected_generation {
            return Err(StoreError::StaleGeneration);
        }

        let watermark = sqlx::query(
            "SELECT agent_epoch, observation_sequence FROM observation_watermarks WHERE resource_id = $1",
        )
        .bind(&id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if let Some(watermark) = watermark {
            let previous_epoch: String = watermark.get("agent_epoch");
            let previous_sequence: i64 = watermark.get("observation_sequence");
            if previous_epoch == update.agent_epoch
                && update.observation_sequence
                    <= u64::try_from(previous_sequence).unwrap_or(u64::MAX)
            {
                return Ok(current);
            }
        }

        sqlx::query("UPDATE resources SET generation = generation + 1, desired_state = $1, observed_state = $2, observed_generation = $3, provider_id = $4 WHERE id = $5 AND generation = $6")
            .bind(update.desired_state)
            .bind(update.observed_state)
            .bind(update.observed_generation)
            .bind(update.provider_id)
            .bind(&id_str)
            .bind(update.expected_generation)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        sqlx::query("INSERT INTO observation_watermarks (resource_id, agent_epoch, observation_sequence) VALUES ($1, $2, $3) ON CONFLICT(resource_id) DO UPDATE SET agent_epoch = EXCLUDED.agent_epoch, observation_sequence = EXCLUDED.observation_sequence")
            .bind(&id_str)
            .bind(update.agent_epoch)
            .bind(update.observation_sequence as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_resource(id).await
    }

    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        let id_str = operation.id.to_string();
        let resource_id_str = operation.resource_id.to_string();
        sqlx::query(
            "INSERT INTO operations (id, resource_id, kind, state, provider_operation_id, error_category, error_message)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id_str)
        .bind(&resource_id_str)
        .bind(&operation.kind)
        .bind(operation.state.as_str())
        .bind(&operation.provider_operation_id)
        .bind(&operation.error_category)
        .bind(&operation.error_message)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn reserve_idempotent_operation(
        &self,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        let result = sqlx::query("INSERT INTO idempotency_reservations (owner_scope, action, idempotency_key, fingerprint, operation_id) VALUES ($1, $2, $3, $4, $5)").bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint).bind(request.operation_id.to_string()).execute(&self.pool).await;
        match result {
            Ok(_) => Ok(IdempotencyReservation::Created(request.operation_id)),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                let row = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope = $1 AND action = $2 AND idempotency_key = $3").bind(&request.owner_scope).bind(&request.action).bind(&request.key).fetch_one(&self.pool).await.map_err(StoreError::Database)?;
                let fingerprint: String =
                    row.try_get("fingerprint").map_err(StoreError::Database)?;
                let id: String = row.try_get("operation_id").map_err(StoreError::Database)?;
                if fingerprint != request.fingerprint {
                    return Ok(IdempotencyReservation::Conflict);
                }
                Ok(IdempotencyReservation::ExistingEquivalent(
                    Uuid::parse_str(&id).map_err(StoreError::InvalidUuid)?,
                ))
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn create_or_replay_idempotent_operation(
        &self,
        operation: &OperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        if operation.id != request.operation_id {
            return Err(StoreError::Corrupt(
                "operation and idempotency identities differ".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let inserted = sqlx::query("INSERT INTO operations (id, resource_id, kind, state, provider_operation_id, error_category, error_message) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(operation.id.to_string()).bind(operation.resource_id.to_string()).bind(&operation.kind)
            .bind(operation.state.as_str()).bind(&operation.provider_operation_id)
            .bind(&operation.error_category).bind(&operation.error_message).execute(&mut *tx).await;
        let operation_inserted = match inserted {
            Ok(_) => true,
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => false,
            Err(error) => return Err(StoreError::Database(error)),
        };
        let result = sqlx::query("INSERT INTO idempotency_reservations (owner_scope, action, idempotency_key, fingerprint, operation_id) VALUES ($1,$2,$3,$4,$5)")
            .bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint)
            .bind(request.operation_id.to_string()).execute(&mut *tx).await;
        let outcome = match result {
            Ok(_) => IdempotencyReservation::Created(request.operation_id),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                let row = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3")
                    .bind(&request.owner_scope).bind(&request.action).bind(&request.key).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
                let fingerprint: String =
                    row.try_get("fingerprint").map_err(StoreError::Database)?;
                let id = Uuid::parse_str(
                    &row.try_get::<String, _>("operation_id")
                        .map_err(StoreError::Database)?,
                )
                .map_err(StoreError::InvalidUuid)?;
                if fingerprint == request.fingerprint {
                    IdempotencyReservation::ExistingEquivalent(id)
                } else {
                    IdempotencyReservation::Conflict
                }
            }
            Err(error) => return Err(StoreError::Database(error)),
        };
        if operation_inserted
            && (matches!(outcome, IdempotencyReservation::Conflict)
                || matches!(outcome, IdempotencyReservation::ExistingEquivalent(id) if id != operation.id))
        {
            sqlx::query("DELETE FROM operations WHERE id=$1")
                .bind(operation.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
        }
        if let IdempotencyReservation::ExistingEquivalent(id) = outcome {
            let exists = sqlx::query("SELECT 1 FROM operations WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?
                .is_some();
            if !exists {
                return Err(StoreError::Corrupt(
                    "idempotency reservation references missing operation".into(),
                ));
            }
        }
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(outcome)
    }

    async fn create_or_replay_canonical_idempotent_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        if request.key.is_empty()
            || request.key.len() > IdempotencyReservationRequest::MAX_KEY_LENGTH
        {
            return Err(StoreError::Corrupt(
                "invalid idempotency key byte length".into(),
            ));
        }
        validate_canonical_idempotent_operation_identity(operation, canonical, request)?;
        let attempt = i32::try_from(canonical.attempt)
            .map_err(|_| StoreError::Corrupt("operation attempt exceeds storage range".into()))?;
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        // Resolve an accepted request before inspecting the caller's new
        // proposal. A retry is identified by its scoped idempotency identity;
        // it must not depend on a newly proposed resource still existing.
        if let Some(reservation) = sqlx::query(
            "SELECT fingerprint, operation_id FROM idempotency_reservations
             WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3",
        )
        .bind(&request.owner_scope)
        .bind(&request.action)
        .bind(&request.key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        {
            let fingerprint: String = reservation
                .try_get("fingerprint")
                .map_err(StoreError::Database)?;
            if fingerprint != request.fingerprint {
                tx.commit().await.map_err(StoreError::Database)?;
                return Ok(IdempotencyReservation::Conflict);
            }
            let winning_id = Uuid::parse_str(
                &reservation
                    .try_get::<String, _>("operation_id")
                    .map_err(StoreError::Database)?,
            )
            .map_err(StoreError::InvalidUuid)?;
            validate_existing_canonical_reservation(&mut tx, winning_id, request).await?;
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(IdempotencyReservation::ExistingEquivalent(winning_id));
        }

        let resource_owner = sqlx::query("SELECT project_id FROM resources WHERE id=$1")
            .bind(operation.resource_id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?
            .try_get::<String, _>("project_id")
            .map_err(StoreError::Database)?;
        if resource_owner != request.owner_scope {
            return Err(StoreError::Corrupt(
                "operation resource and canonical owner scopes differ".into(),
            ));
        }

        let operation_inserted = sqlx::query(
            "INSERT INTO operations
             (id, resource_id, kind, state, provider_operation_id, error_category, error_message)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (id) DO NOTHING
             RETURNING id",
        )
        .bind(operation.id.to_string())
        .bind(operation.resource_id.to_string())
        .bind(&operation.kind)
        .bind(operation.state.as_str())
        .bind(&operation.provider_operation_id)
        .bind(&operation.error_category)
        .bind(&operation.error_message)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        .is_some();

        if operation_inserted {
            sqlx::query(
                "INSERT INTO canonical_operation_metadata
                 (operation_id,service,action,actor,owner_scope,resource_type,resource_id,attempt,
                  created_at,started_at,finished_at,error,request_id)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
            )
            .bind(canonical.id.to_string())
            .bind(&canonical.service)
            .bind(&canonical.action)
            .bind(&canonical.actor)
            .bind(&canonical.owner_scope)
            .bind(&canonical.resource_type)
            .bind(&canonical.resource_id)
            .bind(attempt)
            .bind(&canonical.created_at)
            .bind(&canonical.started_at)
            .bind(&canonical.finished_at)
            .bind(&canonical.error)
            .bind(&canonical.request_id)
            .execute(&mut *tx)
            .await
            .map_err(map_pg_error)?;
        }

        let reservation_inserted = sqlx::query(
            "INSERT INTO idempotency_reservations
             (owner_scope, action, idempotency_key, fingerprint, operation_id)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (owner_scope, action, idempotency_key) DO NOTHING
             RETURNING operation_id",
        )
        .bind(&request.owner_scope)
        .bind(&request.action)
        .bind(&request.key)
        .bind(&request.fingerprint)
        .bind(request.operation_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        .is_some();

        if reservation_inserted {
            if !operation_inserted {
                return Err(StoreError::Corrupt(
                    "new idempotency reservation references a pre-existing operation".into(),
                ));
            }
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(IdempotencyReservation::Created(operation.id));
        }

        if operation_inserted {
            // The competing reservation won. Removing the losing operation
            // cascades to its canonical metadata inside this transaction.
            sqlx::query("DELETE FROM operations WHERE id=$1")
                .bind(operation.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
        }

        let reservation = sqlx::query(
            "SELECT fingerprint, operation_id FROM idempotency_reservations
             WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3",
        )
        .bind(&request.owner_scope)
        .bind(&request.action)
        .bind(&request.key)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        let fingerprint: String = reservation
            .try_get("fingerprint")
            .map_err(StoreError::Database)?;
        if fingerprint != request.fingerprint {
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(IdempotencyReservation::Conflict);
        }
        let winning_id = Uuid::parse_str(
            &reservation
                .try_get::<String, _>("operation_id")
                .map_err(StoreError::Database)?,
        )
        .map_err(StoreError::InvalidUuid)?;

        validate_existing_canonical_reservation(&mut tx, winning_id, request).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(IdempotencyReservation::ExistingEquivalent(winning_id))
    }

    async fn create_or_replay_canonical_scoped_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        if operation.id != request.operation_id {
            return Err(StoreError::Corrupt(
                "operation and idempotency identities differ".into(),
            ));
        }
        crate::validate_canonical_idempotent_operation_identity(operation, canonical, request)?;
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "{}\n{}\n{}",
                request.owner_scope, request.action, request.key
            ))
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        let resource = sqlx::query("SELECT kind, project_id FROM resources WHERE id=$1")
            .bind(operation.resource_id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        if resource.get::<String, _>("kind") != canonical.resource_type
            || resource.get::<String, _>("project_id") != canonical.owner_scope
        {
            return Err(StoreError::Corrupt(
                "canonical scoped operation resource index differs".into(),
            ));
        }
        if let Some(row) = sqlx::query("SELECT fingerprint, operation_id FROM idempotency_reservations WHERE owner_scope=$1 AND action=$2 AND idempotency_key=$3")
            .bind(&request.owner_scope).bind(&request.action).bind(&request.key)
            .fetch_optional(&mut *tx).await.map_err(StoreError::Database)?
        {
            let fingerprint: String = row.try_get("fingerprint").map_err(StoreError::Database)?;
            let existing = Uuid::parse_str(&row.try_get::<String, _>("operation_id").map_err(StoreError::Database)?)
                .map_err(StoreError::InvalidUuid)?;
            if fingerprint != request.fingerprint {
                tx.commit().await.map_err(StoreError::Database)?;
                return Ok(IdempotencyReservation::Conflict);
            }
            let op = sqlx::query("SELECT resource_id FROM operations WHERE id=$1")
                .bind(existing.to_string()).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
            let metadata = sqlx::query("SELECT owner_scope, action, resource_id FROM canonical_operation_metadata WHERE operation_id=$1")
                .bind(existing.to_string()).fetch_one(&mut *tx).await.map_err(StoreError::Database)?;
            if op.get::<String, _>("resource_id") != operation.resource_id.to_string()
                || metadata.get::<String, _>("owner_scope") != request.owner_scope
                || metadata.get::<String, _>("action") != request.action
                || metadata.get::<Option<String>, _>("resource_id") != Some(operation.resource_id.to_string())
            {
                return Err(StoreError::Corrupt("canonical scoped operation replay identity differs".into()));
            }
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(IdempotencyReservation::ExistingEquivalent(existing));
        }
        insert_postgres_canonical_acceptance(&mut tx, operation, canonical).await?;
        sqlx::query("INSERT INTO idempotency_reservations (owner_scope, action, idempotency_key, fingerprint, operation_id) VALUES ($1,$2,$3,$4,$5)")
            .bind(&request.owner_scope).bind(&request.action).bind(&request.key)
            .bind(&request.fingerprint).bind(request.operation_id.to_string())
            .execute(&mut *tx).await.map_err(map_pg_error)?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(IdempotencyReservation::Created(operation.id))
    }

    async fn create_or_replay_canonical_resource_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<crate::CanonicalAcceptanceOutcome, StoreError> {
        crate::validate_canonical_resource_acceptance(resource, operation, canonical, request)?;
        if let Some(outcome) = postgres_existing_acceptance(&self.pool, request).await? {
            return Ok(outcome);
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let lock_identity = format!(
            "{}\n{}\n{}",
            request.owner_scope, request.action, request.key
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(lock_identity)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if let Some(outcome) = postgres_existing_acceptance_tx(&mut tx, request).await? {
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(outcome);
        }
        if let Some(allocation_id) = expected_placement_allocation_id
            && sqlx::query("SELECT 1 FROM placement_allocations WHERE id=$1 FOR SHARE")
                .bind(allocation_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?
                .is_none()
        {
            return Err(StoreError::PlacementAllocationNotFound);
        }
        sqlx::query("INSERT INTO resources (id,kind,project_id,generation,observed_generation,desired_state,observed_state,provider_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(resource.id.to_string()).bind(&resource.kind).bind(&resource.project_id).bind(resource.generation)
            .bind(resource.observed_generation).bind(&resource.desired_state).bind(&resource.observed_state).bind(&resource.provider_id)
            .execute(&mut *tx).await.map_err(map_pg_error)?;
        insert_postgres_canonical_acceptance(&mut tx, operation, canonical).await?;
        let inserted = sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (owner_scope,action,idempotency_key) DO NOTHING RETURNING operation_id")
            .bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint)
            .bind(request.operation_id.to_string()).fetch_optional(&mut *tx).await.map_err(StoreError::Database)?.is_some();
        if !inserted {
            tx.rollback().await.map_err(StoreError::Database)?;
            return postgres_existing_acceptance(&self.pool, request)
                .await?
                .ok_or(StoreError::IdempotencyConflict);
        }
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(crate::CanonicalAcceptanceOutcome::Created {
            operation_id: operation.id,
            resource_id: resource.id,
        })
    }

    async fn create_or_replay_canonical_lifecycle_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<crate::CanonicalAcceptanceOutcome, StoreError> {
        if let Some(outcome) = postgres_existing_acceptance(&self.pool, request).await? {
            return Ok(outcome);
        }
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let lock_identity = format!(
            "{}\n{}\n{}",
            request.owner_scope, request.action, request.key
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(lock_identity)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        if let Some(outcome) = postgres_existing_acceptance_tx(&mut tx, request).await? {
            tx.commit().await.map_err(StoreError::Database)?;
            return Ok(outcome);
        }
        let row = sqlx::query("SELECT * FROM resources WHERE id=$1 FOR SHARE")
            .bind(operation.resource_id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        let resource = row_to_resource(&row)?;
        crate::validate_canonical_resource_acceptance(&resource, operation, canonical, request)?;
        insert_postgres_canonical_acceptance(&mut tx, operation, canonical).await?;
        let inserted = sqlx::query("INSERT INTO idempotency_reservations (owner_scope,action,idempotency_key,fingerprint,operation_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (owner_scope,action,idempotency_key) DO NOTHING RETURNING operation_id")
            .bind(&request.owner_scope).bind(&request.action).bind(&request.key).bind(&request.fingerprint)
            .bind(request.operation_id.to_string()).fetch_optional(&mut *tx).await.map_err(StoreError::Database)?.is_some();
        if !inserted {
            tx.rollback().await.map_err(StoreError::Database)?;
            return postgres_existing_acceptance(&self.pool, request)
                .await?
                .ok_or(StoreError::IdempotencyConflict);
        }
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(crate::CanonicalAcceptanceOutcome::Created {
            operation_id: operation.id,
            resource_id: operation.resource_id,
        })
    }

    async fn get_operation(&self, id: Uuid) -> Result<OperationRecord, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM operations WHERE id = $1")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        match row {
            Some(row) => row_to_operation(&row),
            None => Err(StoreError::OperationNotFound),
        }
    }

    async fn get_canonical_operation(
        &self,
        id: Uuid,
    ) -> Result<CanonicalOperationRecord, StoreError> {
        let row = sqlx::query("SELECT * FROM canonical_operation_metadata WHERE operation_id=$1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        let operation = self.get_operation(id).await?;
        let resource = self.get_resource(operation.resource_id).await?;
        let canonical = CanonicalOperationRecord {
            id,
            service: row.try_get("service").map_err(StoreError::Database)?,
            action: row.try_get("action").map_err(StoreError::Database)?,
            actor: row.try_get("actor").map_err(StoreError::Database)?,
            owner_scope: row.try_get("owner_scope").map_err(StoreError::Database)?,
            resource_type: row.try_get("resource_type").map_err(StoreError::Database)?,
            resource_id: row.try_get("resource_id").map_err(StoreError::Database)?,
            state: operation.state,
            attempt: u32::try_from(
                row.try_get::<i32, _>("attempt")
                    .map_err(StoreError::Database)?,
            )
            .map_err(|_| StoreError::Corrupt("invalid operation attempt".into()))?,
            created_at: row.try_get("created_at").map_err(StoreError::Database)?,
            started_at: row.try_get("started_at").map_err(StoreError::Database)?,
            finished_at: row.try_get("finished_at").map_err(StoreError::Database)?,
            error: row.try_get("error").map_err(StoreError::Database)?,
            request_id: row.try_get("request_id").map_err(StoreError::Database)?,
        };
        crate::validate_canonical_operation_read(&operation, &canonical, &resource)?;
        Ok(canonical)
    }

    async fn update_operation(
        &self,
        id: Uuid,
        state: OperationState,
        provider_operation_id: Option<&str>,
        error_category: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationRecord, StoreError> {
        let id_str = id.to_string();
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let row = sqlx::query(
            "SELECT id, resource_id, kind, state, provider_operation_id, error_category, error_message FROM operations WHERE id = $1 FOR UPDATE",
        )
        .bind(&id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?
        .ok_or(StoreError::OperationNotFound)?;

        let current = row_to_operation(&row)?;
        if matches!(
            current.state,
            OperationState::Succeeded | OperationState::Failed
        ) {
            if current
                .provider_operation_id
                .as_deref()
                .is_some_and(|existing| {
                    provider_operation_id.is_some_and(|incoming| incoming != existing)
                })
            {
                return Err(StoreError::Corrupt(
                    "terminal operation provider identity conflicts with durable state".to_owned(),
                ));
            }
            if current.state != state {
                if matches!(state, OperationState::Succeeded | OperationState::Failed) {
                    return Err(StoreError::Corrupt(
                        "terminal operation state cannot conflict with durable state".to_owned(),
                    ));
                }
                return Ok(current);
            }

            sqlx::query("UPDATE operations SET provider_operation_id = COALESCE($1, provider_operation_id), error_category = COALESCE($2, error_category), error_message = COALESCE($3, error_message) WHERE id = $4")
                .bind(provider_operation_id)
                .bind(error_category)
                .bind(error_message)
                .bind(&id_str)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;

            tx.commit().await.map_err(StoreError::Database)?;
            return self.get_operation(id).await;
        }

        sqlx::query("UPDATE operations SET state = $1, provider_operation_id = $2, error_category = $3, error_message = $4 WHERE id = $5")
            .bind(state.as_str())
            .bind(provider_operation_id)
            .bind(error_category)
            .bind(error_message)
            .bind(&id_str)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        let now = chrono::Utc::now().to_rfc3339();
        let started_at = (!matches!(state, OperationState::Pending)).then_some(now.clone());
        let finished_at =
            matches!(state, OperationState::Succeeded | OperationState::Failed).then_some(now);
        sqlx::query("UPDATE canonical_operation_metadata SET started_at=COALESCE(started_at,$1), finished_at=$2, error=$3 WHERE operation_id=$4")
            .bind(started_at).bind(finished_at).bind(error_category).bind(&id_str)
            .execute(&mut *tx).await.map_err(StoreError::Database)?;

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_operation(id).await
    }

    async fn update_canonical_operation_lifecycle(
        &self,
        id: Uuid,
        update: &crate::CanonicalOperationLifecycleUpdate,
    ) -> Result<CanonicalOperationRecord, StoreError> {
        crate::validate_canonical_lifecycle_update(update)?;
        let attempt = i32::try_from(update.attempt).map_err(|_| {
            StoreError::Corrupt("canonical operation attempt exceeds PostgreSQL INT4".into())
        })?;
        let id_str = id.to_string();
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let exists = sqlx::query("SELECT state FROM operations WHERE id = $1 FOR UPDATE")
            .bind(&id_str)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        let current: String = exists.try_get("state").map_err(StoreError::Database)?;
        if (current == OperationState::Succeeded.as_str()
            && update.state != OperationState::Succeeded)
            || (current == OperationState::Failed.as_str()
                && update.state != OperationState::Failed)
        {
            return Err(StoreError::Corrupt(
                "terminal operation state cannot regress".into(),
            ));
        }
        let metadata = sqlx::query("SELECT operation_id FROM canonical_operation_metadata WHERE operation_id = $1 FOR UPDATE")
            .bind(&id_str).fetch_optional(&mut *tx).await.map_err(StoreError::Database)?
            .ok_or_else(|| StoreError::Corrupt("canonical lifecycle metadata is missing".into()))?;
        let _ = metadata;
        sqlx::query("UPDATE operations SET state = $1 WHERE id = $2")
            .bind(update.state.as_str())
            .bind(&id_str)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        sqlx::query("UPDATE canonical_operation_metadata SET attempt = $1, started_at = $2, finished_at = $3, error = $4 WHERE operation_id = $5")
            .bind(attempt).bind(&update.started_at).bind(&update.finished_at).bind(&update.public_error).bind(&id_str)
            .execute(&mut *tx).await.map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)?;
        self.get_canonical_operation(id).await
    }

    async fn list_non_terminal_lifecycle_operations(
        &self,
    ) -> Result<Vec<OperationRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM operations
             WHERE kind LIKE 'lifecycle:%' AND state NOT IN ('succeeded', 'failed')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(row_to_operation).collect()
    }

    async fn attach_provider_reference(
        &self,
        reference: &ProviderReference,
    ) -> Result<(), StoreError> {
        let resource_id_str = reference.resource_id.to_string();
        let res = sqlx::query(
            "INSERT INTO provider_refs (resource_id, provider_name, provider_resource_id)
             VALUES ($1, $2, $3)",
        )
        .bind(&resource_id_str)
        .bind(&reference.provider_name)
        .bind(&reference.provider_resource_id)
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(err)) if err.code().as_deref() == Some("23505") => {
                Err(StoreError::ProviderReferenceAlreadyExists)
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn get_provider_reference(
        &self,
        resource_id: Uuid,
        provider_name: &str,
    ) -> Result<ProviderReference, StoreError> {
        let resource_id_str = resource_id.to_string();
        let row = sqlx::query(
            "SELECT * FROM provider_refs WHERE resource_id = $1 AND provider_name = $2",
        )
        .bind(&resource_id_str)
        .bind(provider_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        match row {
            Some(row) => Ok(ProviderReference {
                resource_id,
                provider_name: row.get("provider_name"),
                provider_resource_id: row.get("provider_resource_id"),
            }),
            None => Err(StoreError::ProviderReferenceNotFound),
        }
    }

    async fn insert_agent_command(
        &self,
        command: &AgentCommandRecord,
    ) -> Result<AgentCommandRecord, StoreError> {
        let operation_id_str = command.operation_id.to_string();
        let resource_id_str = command.resource_id.to_string();
        sqlx::query(
            "INSERT INTO agent_commands (command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&command.command_id)
        .bind(&command.idempotency_key)
        .bind(&operation_id_str)
        .bind(&resource_id_str)
        .bind(&command.agent_id)
        .bind(&command.agent_epoch)
        .bind(&command.payload_fingerprint_sha256)
        .bind(&command.payload)
        .bind(command.state.as_str())
        .bind(command.accepted_sequence as i64)
        .bind(command.last_sequence as i64)
        .bind(&command.provider_operation_id)
        .bind(&command.provider_resource_id)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;

        self.get_agent_command(&command.command_id).await
    }

    async fn get_agent_command(&self, command_id: &str) -> Result<AgentCommandRecord, StoreError> {
        let row = sqlx::query("SELECT * FROM agent_commands WHERE command_id = $1")
            .bind(command_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        match row {
            Some(row) => {
                let op_id_str: String = row.get("operation_id");
                let op_id = parse_uuid(&op_id_str)?;
                let res_id_str: String = row.get("resource_id");
                let res_id = parse_uuid(&res_id_str)?;
                let state_str: String = row.get("state");
                let state = AgentCommandState::parse(&state_str)?;
                let acc_seq: i64 = row.get("accepted_sequence");
                let last_seq: i64 = row.get("last_sequence");

                Ok(AgentCommandRecord {
                    command_id: row.get("command_id"),
                    idempotency_key: row.get("idempotency_key"),
                    operation_id: op_id,
                    resource_id: res_id,
                    agent_id: row.get("agent_id"),
                    agent_epoch: row.get("agent_epoch"),
                    payload_fingerprint_sha256: row.get("payload_fingerprint_sha256"),
                    payload: row.get("payload"),
                    state,
                    accepted_sequence: acc_seq as u64,
                    last_sequence: last_seq as u64,
                    provider_operation_id: row.get("provider_operation_id"),
                    provider_resource_id: row.get("provider_resource_id"),
                })
            }
            None => Err(StoreError::Corrupt(format!(
                "agent command `{command_id}` not found"
            ))),
        }
    }

    async fn get_agent_command_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AgentCommandRecord, StoreError> {
        let row = sqlx::query("SELECT command_id FROM agent_commands WHERE idempotency_key = $1")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        match row {
            Some(row) => {
                let id: String = row.get("command_id");
                self.get_agent_command(&id).await
            }
            None => Err(StoreError::Corrupt(format!(
                "agent command with idempotency key `{idempotency_key}` not found"
            ))),
        }
    }

    async fn get_agent_command_by_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<AgentCommandRecord, StoreError> {
        let op_str = operation_id.to_string();
        let row = sqlx::query("SELECT command_id FROM agent_commands WHERE operation_id = $1")
            .bind(&op_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        match row {
            Some(row) => {
                let id: String = row.get("command_id");
                self.get_agent_command(&id).await
            }
            None => Err(StoreError::Corrupt(format!(
                "agent command for operation `{operation_id}` not found"
            ))),
        }
    }

    async fn update_agent_command(
        &self,
        command_id: &str,
        state: AgentCommandState,
        accepted_sequence: u64,
        last_sequence: u64,
        provider_operation_id: Option<&str>,
        provider_resource_id: Option<&str>,
    ) -> Result<AgentCommandRecord, StoreError> {
        let res = sqlx::query(
            "UPDATE agent_commands
             SET state = $1, accepted_sequence = $2, last_sequence = $3,
                 provider_operation_id = COALESCE($4, provider_operation_id),
                 provider_resource_id = COALESCE($5, provider_resource_id),
                 updated_at = CURRENT_TIMESTAMP
             WHERE command_id = $6",
        )
        .bind(state.as_str())
        .bind(accepted_sequence as i64)
        .bind(last_sequence as i64)
        .bind(provider_operation_id)
        .bind(provider_resource_id)
        .bind(command_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "agent command `{command_id}` not found"
            )));
        }

        self.get_agent_command(command_id).await
    }

    async fn list_recoverable_agent_commands(&self) -> Result<Vec<AgentCommandRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT command_id FROM agent_commands
             WHERE state IN ('pending', 'accepted', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut commands = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("command_id");
            commands.push(self.get_agent_command(&id).await?);
        }
        Ok(commands)
    }

    async fn insert_artifact_transfer(
        &self,
        transfer: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        transfer.validate()?;
        let res_id_str = transfer.resource_id.to_string();
        let op_id_str = transfer.operation_id.to_string();

        let result = sqlx::query(
            "INSERT INTO artifact_transfers (transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&transfer.transfer_id)
        .bind(&transfer.command_id)
        .bind(&op_id_str)
        .bind(&res_id_str)
        .bind(&transfer.agent_id)
        .bind(&transfer.agent_epoch)
        .bind(&transfer.artifact_id)
        .bind(&transfer.artifact_kind)
        .bind(&transfer.sha256)
        .bind(transfer.size_bytes as i64)
        .bind(transfer.expires_at_unix_ms)
        .bind(&transfer.format)
        .bind(transfer.chunk_size_bytes as i64)
        .bind(transfer.chunk_count as i64)
        .bind(transfer.state.as_str())
        .bind(transfer.contiguous_bytes as i64)
        .bind(transfer.next_chunk_index as i64)
        .bind(transfer.retry_count as i32)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => self.get_artifact_transfer(&transfer.transfer_id).await,
            Err(sqlx::Error::Database(err)) if err.code().as_deref() == Some("23505") => {
                let existing = self.get_artifact_transfer(&transfer.transfer_id).await?;
                if existing.transfer_id == transfer.transfer_id
                    && existing.command_id == transfer.command_id
                    && existing.operation_id == transfer.operation_id
                    && existing.resource_id == transfer.resource_id
                    && existing.agent_id == transfer.agent_id
                    && existing.agent_epoch == transfer.agent_epoch
                    && existing.artifact_id == transfer.artifact_id
                    && existing.artifact_kind == transfer.artifact_kind
                    && existing.sha256 == transfer.sha256
                    && existing.size_bytes == transfer.size_bytes
                    && existing.expires_at_unix_ms == transfer.expires_at_unix_ms
                    && existing.format == transfer.format
                    && existing.chunk_size_bytes == transfer.chunk_size_bytes
                    && existing.chunk_count == transfer.chunk_count
                {
                    Ok(existing)
                } else {
                    Err(StoreError::ArtifactTransferConflict(
                        "transfer identity conflicts with durable state".to_owned(),
                    ))
                }
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn get_artifact_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        let row = sqlx::query("SELECT transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count, created_at, updated_at FROM artifact_transfers WHERE transfer_id = $1")
            .bind(transfer_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ArtifactTransferNotFound)?;

        let op_id_str: String = row.get("operation_id");
        let res_id_str: String = row.get("resource_id");
        let state_str: String = row.get("state");
        let retry_count: i32 = row.get("retry_count");

        let rec = ArtifactTransferRecord {
            transfer_id: row.get("transfer_id"),
            command_id: row.get("command_id"),
            operation_id: parse_uuid(&op_id_str)?,
            resource_id: parse_uuid(&res_id_str)?,
            agent_id: row.get("agent_id"),
            agent_epoch: row.get("agent_epoch"),
            artifact_id: row.get("artifact_id"),
            artifact_kind: row.get("artifact_kind"),
            sha256: row.get("sha256"),
            size_bytes: row.get::<i64, _>("size_bytes") as u64,
            expires_at_unix_ms: row.get::<Option<i64>, _>("expires_at_unix_ms").ok_or_else(
                || StoreError::Corrupt("artifact transfer expiry is missing".to_owned()),
            )?,
            format: row.get("format"),
            chunk_size_bytes: row.get::<i64, _>("chunk_size_bytes") as u64,
            chunk_count: row.get::<i64, _>("chunk_count") as u64,
            state: ArtifactTransferState::parse(&state_str)?,
            contiguous_bytes: row.get::<i64, _>("contiguous_bytes") as u64,
            next_chunk_index: row.get::<i64, _>("next_chunk_index") as u64,
            retry_count: retry_count as u8,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        };
        rec.validate()?;
        Ok(rec)
    }

    async fn rebind_artifact_transfer_epoch(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        new_agent_epoch: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        if expected_agent_epoch == new_agent_epoch {
            return self.get_artifact_transfer(transfer_id).await;
        }

        let res = sqlx::query(
            "UPDATE artifact_transfers
             SET agent_epoch = $1, updated_at = CURRENT_TIMESTAMP
             WHERE transfer_id = $2 AND agent_epoch = $3 AND state IN ('offered', 'receiving')",
        )
        .bind(new_agent_epoch)
        .bind(transfer_id)
        .bind(expected_agent_epoch)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() != 1 {
            let current = self.get_artifact_transfer(transfer_id).await?;
            if current.agent_epoch != expected_agent_epoch {
                return Err(StoreError::ArtifactTransferEpochConflict);
            }
            return Err(StoreError::ArtifactTransferConflict(
                "terminal artifact transfer cannot be rebound".to_owned(),
            ));
        }

        self.get_artifact_transfer(transfer_id).await
    }

    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let current = self.get_artifact_transfer(transfer_id).await?;
        if current.agent_epoch != expected_agent_epoch {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }
        update.validate_against(&current)?;

        if update
            == (ArtifactTransferUpdate {
                state: current.state,
                contiguous_bytes: current.contiguous_bytes,
                next_chunk_index: current.next_chunk_index,
                retry_count: current.retry_count,
            })
        {
            return Ok(current);
        }

        let res = sqlx::query(
            "UPDATE artifact_transfers
             SET state = $1, contiguous_bytes = $2, next_chunk_index = $3, retry_count = $4, updated_at = CURRENT_TIMESTAMP
             WHERE transfer_id = $5 AND agent_epoch = $6",
        )
        .bind(update.state.as_str())
        .bind(update.contiguous_bytes as i64)
        .bind(update.next_chunk_index as i64)
        .bind(update.retry_count as i32)
        .bind(transfer_id)
        .bind(expected_agent_epoch)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() != 1 {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_artifact_transfer(transfer_id).await
    }

    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT transfer_id FROM artifact_transfers
             WHERE state IN ('offered', 'receiving')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut transfers = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("transfer_id");
            transfers.push(self.get_artifact_transfer(&id).await?);
        }
        Ok(transfers)
    }

    async fn expire_transfers_of_terminal_operations(&self) -> Result<u64, StoreError> {
        let res = sqlx::query(
            "UPDATE artifact_transfers
             SET state = 'expired', updated_at = CURRENT_TIMESTAMP
             WHERE state NOT IN ('committed', 'rejected', 'expired')
               AND operation_id IN (SELECT id FROM operations WHERE state IN ('succeeded', 'failed'))",
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(res.rows_affected())
    }

    async fn insert_image_overlay(
        &self,
        overlay: &ImageOverlayOwnershipRecord,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let res_id_str = overlay.identity.resource_id.to_string();
        let op_id_str = overlay.identity.operation_id.to_string();

        let res = sqlx::query(
            "INSERT INTO image_overlay_ownership (overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&overlay.overlay_id)
        .bind(&res_id_str)
        .bind(&op_id_str)
        .bind(&overlay.identity.command_id)
        .bind(&overlay.identity.agent_id)
        .bind(&overlay.identity.agent_epoch)
        .bind(&overlay.identity.base_sha256)
        .bind(&overlay.identity.base_format)
        .bind(&overlay.identity.overlay_format)
        .bind(overlay.state.as_str())
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => self.get_image_overlay(&overlay.overlay_id).await,
            Err(sqlx::Error::Database(err)) if err.code().as_deref() == Some("23505") => {
                let existing = self.get_image_overlay(&overlay.overlay_id).await;
                match existing {
                    Ok(existing)
                        if existing.overlay_id == overlay.overlay_id
                            && existing.identity == overlay.identity =>
                    {
                        Ok(existing)
                    }
                    Ok(_) => Err(StoreError::ImageOverlayConflict(
                        "overlay identity conflicts with durable state".to_owned(),
                    )),
                    Err(StoreError::ImageOverlayNotFound) => {
                        let count: i64 = sqlx::query_scalar(
                            "SELECT COUNT(*) FROM image_overlay_ownership WHERE resource_id = $1 AND operation_id = $2 AND command_id = $3",
                        )
                        .bind(&res_id_str)
                        .bind(&op_id_str)
                        .bind(&overlay.identity.command_id)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(StoreError::Database)?;
                        if count != 0 {
                            Err(StoreError::ImageOverlayConflict(
                                "resource operation already owns an overlay".to_owned(),
                            ))
                        } else {
                            Err(StoreError::ImageOverlayNotFound)
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn get_image_overlay(
        &self,
        overlay_id: &str,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let row = sqlx::query("SELECT overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at FROM image_overlay_ownership WHERE overlay_id = $1")
            .bind(overlay_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ImageOverlayNotFound)?;

        let res_id_str: String = row.get("resource_id");
        let op_id_str: String = row.get("operation_id");
        let state_str: String = row.get("state");

        Ok(ImageOverlayOwnershipRecord {
            overlay_id: row.get("overlay_id"),
            identity: ImageOverlayIdentity {
                resource_id: parse_uuid(&res_id_str)?,
                operation_id: parse_uuid(&op_id_str)?,
                command_id: row.get("command_id"),
                agent_id: row.get("agent_id"),
                agent_epoch: row.get("agent_epoch"),
                base_sha256: row.get("base_sha256"),
                base_format: row.get("base_format"),
                overlay_format: row.get("overlay_format"),
            },
            state: ImageOverlayState::parse(&state_str)?,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    async fn update_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
        update: ImageOverlayUpdate,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let current = self.get_image_overlay(overlay_id).await?;
        if current.identity.agent_epoch != expected_identity.agent_epoch {
            return Err(StoreError::ImageOverlayEpochConflict);
        }
        if current.identity != *expected_identity {
            return Err(StoreError::ImageOverlayConflict(
                "overlay identity conflict".to_owned(),
            ));
        }
        validate_image_overlay_transition(current.state, update.state)?;

        if current.state == update.state {
            return Ok(current);
        }

        let res = sqlx::query(
            "UPDATE image_overlay_ownership
             SET state = $1, updated_at = CURRENT_TIMESTAMP
             WHERE overlay_id = $2 AND agent_epoch = $3 AND state = $4",
        )
        .bind(update.state.as_str())
        .bind(overlay_id)
        .bind(&expected_identity.agent_epoch)
        .bind(current.state.as_str())
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() != 1 {
            return Err(StoreError::ImageOverlayConflict(
                "overlay update failed".to_owned(),
            ));
        }

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_image_overlay(overlay_id).await
    }

    async fn list_image_overlays(
        &self,
        resource_id: Uuid,
    ) -> Result<Vec<ImageOverlayOwnershipRecord>, StoreError> {
        let res_id_str = resource_id.to_string();
        let rows = sqlx::query("SELECT overlay_id FROM image_overlay_ownership WHERE resource_id = $1 ORDER BY created_at ASC")
            .bind(&res_id_str)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        let mut overlays = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("overlay_id");
            overlays.push(self.get_image_overlay(&id).await?);
        }
        Ok(overlays)
    }

    async fn count_image_overlay_references(
        &self,
        base_sha256: &str,
        base_format: &str,
    ) -> Result<u64, StoreError> {
        let row = sqlx::query("SELECT COUNT(*) FROM image_overlay_ownership WHERE base_sha256 = $1 AND base_format = $2 AND state != 'deleted'")
            .bind(base_sha256)
            .bind(base_format)
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        let count: i64 = row.get(0);
        Ok(count.max(0) as u64)
    }

    async fn delete_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let current = self.get_image_overlay(overlay_id).await?;
        if current.state == ImageOverlayState::Deleted {
            if current.identity.agent_epoch != expected_identity.agent_epoch {
                return Err(StoreError::ImageOverlayEpochConflict);
            }
            if current.identity != *expected_identity {
                return Err(StoreError::ImageOverlayConflict(
                    "overlay identity conflicts with durable state".to_owned(),
                ));
            }
            return Ok(current);
        }
        let deleting = self
            .update_image_overlay(
                overlay_id,
                expected_identity,
                ImageOverlayUpdate {
                    state: ImageOverlayState::Deleting,
                },
            )
            .await?;
        if deleting.state == ImageOverlayState::Deleted {
            return Ok(deleting);
        }
        self.update_image_overlay(
            overlay_id,
            expected_identity,
            ImageOverlayUpdate {
                state: ImageOverlayState::Deleted,
            },
        )
        .await
    }

    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let op_id_str = operation_id.to_string();
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT attempts FROM operation_retry_state WHERE operation_id = $1",
        )
        .bind(&op_id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        let attempts = current.unwrap_or(0).saturating_add(1);
        sqlx::query(
            "INSERT INTO operation_retry_state (operation_id, attempts, updated_at)
             VALUES ($1, $2, CURRENT_TIMESTAMP)
             ON CONFLICT (operation_id) DO UPDATE
             SET attempts = $2, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&op_id_str)
        .bind(attempts)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        // Synchronise canonical operation attempt when canonical metadata
        // exists; a no-op for legacy operations without metadata.
        sqlx::query("UPDATE canonical_operation_metadata SET attempt = $1 WHERE operation_id = $2")
            .bind(attempts)
            .bind(&op_id_str)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        tx.commit().await.map_err(StoreError::Database)?;
        u8::try_from(attempts)
            .map_err(|_| StoreError::Corrupt("operation retry count exceeds limit".to_owned()))
    }

    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        if let Some(allocation_id) = expected_placement_allocation_id {
            let alloc_row = sqlx::query("SELECT 1 FROM placement_allocations WHERE id = $1")
                .bind(allocation_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
            if alloc_row.is_none() {
                return Err(StoreError::PlacementAllocationNotFound);
            }
        }

        let res_id_str = resource.id.to_string();
        sqlx::query(
            "INSERT INTO resources (id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&res_id_str)
        .bind(&resource.kind)
        .bind(&resource.project_id)
        .bind(resource.generation)
        .bind(resource.observed_generation)
        .bind(&resource.desired_state)
        .bind(&resource.observed_state)
        .bind(&resource.provider_id)
        .execute(&mut *tx)
        .await
        .map_err(map_pg_error)?;

        let op_id_str = operation.id.to_string();
        sqlx::query(
            "INSERT INTO operations (id, resource_id, kind, state, provider_operation_id, error_category, error_message)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&op_id_str)
        .bind(&res_id_str)
        .bind(&operation.kind)
        .bind(operation.state.as_str())
        .bind(&operation.provider_operation_id)
        .bind(&operation.error_category)
        .bind(&operation.error_message)
        .execute(&mut *tx)
        .await
        .map_err(map_pg_error)?;

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }

    async fn revive_resource_and_operation(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
        operation: &OperationRecord,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        if let Some(allocation_id) = expected_placement_allocation_id {
            let alloc_row = sqlx::query("SELECT 1 FROM placement_allocations WHERE id = $1")
                .bind(allocation_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
            if alloc_row.is_none() {
                return Err(StoreError::PlacementAllocationNotFound);
            }
        }

        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE resources
             SET desired_state = $1, observed_state = $2, observed_generation = $3, provider_id = $4, generation = generation + 1
             WHERE id = $5 AND generation = $6",
        )
        .bind(desired_state)
        .bind(observed_state)
        .bind(observed_generation)
        .bind(provider_id)
        .bind(&id_str)
        .bind(expected_generation)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }

        let op_id_str = operation.id.to_string();
        sqlx::query(
            "INSERT INTO operations (id, resource_id, kind, state, provider_operation_id, error_category, error_message)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&op_id_str)
        .bind(&id_str)
        .bind(&operation.kind)
        .bind(operation.state.as_str())
        .bind(&operation.provider_operation_id)
        .bind(&operation.error_category)
        .bind(&operation.error_message)
        .execute(&mut *tx)
        .await
        .map_err(map_pg_error)?;

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_resource(id).await
    }

    async fn readiness_check(&self) -> Result<(), StoreError> {
        self.readiness_check().await
    }
}

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

#[async_trait]
impl VolumeAttachmentRepository for PostgresStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        let id_str = record.id.to_string();
        let srv_id_str = record.server_id.to_string();
        let vol_id_str = record.volume_id.to_string();
        let op_id_str = record.operation_id.map(|id| id.to_string());

        sqlx::query(
            "INSERT INTO volume_attachments (id, server_id, volume_id, device, tag, delete_on_termination, created_at, status, operation_id, idempotency_key, cinder_attachment_id, connector_host, connector_ip, connector_initiator, driver_volume_type, target_iqn, target_portal, target_lun, connection_info_digest, error)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
        )
        .bind(&id_str)
        .bind(&srv_id_str)
        .bind(&vol_id_str)
        .bind(&record.device)
        .bind(&record.tag)
        .bind(if record.delete_on_termination { 1i32 } else { 0i32 })
        .bind(&record.created_at)
        .bind(&record.status)
        .bind(op_id_str.as_deref())
        .bind(&record.idempotency_key)
        .bind(&record.cinder_attachment_id)
        .bind(&record.connector_host)
        .bind(&record.connector_ip)
        .bind(&record.connector_initiator)
        .bind(&record.driver_volume_type)
        .bind(&record.target_iqn)
        .bind(&record.target_portal)
        .bind(record.target_lun.map(|l| l as i32))
        .bind(&record.connection_info_digest)
        .bind(&record.error)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE volume_attachments
             SET status = $1, error = $2
             WHERE id = $3",
        )
        .bind(status)
        .bind(error)
        .bind(&id_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )));
        }

        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )))
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE volume_attachments
             SET status = $1, cinder_attachment_id = COALESCE($2, cinder_attachment_id),
                 connector_host = COALESCE($3, connector_host), connector_ip = COALESCE($4, connector_ip),
                 connector_initiator = COALESCE($5, connector_initiator), driver_volume_type = COALESCE($6, driver_volume_type),
                 target_iqn = COALESCE($7, target_iqn), target_portal = COALESCE($8, target_portal),
                 target_lun = COALESCE($9, target_lun), connection_info_digest = COALESCE($10, connection_info_digest),
                 device = COALESCE($11, device)
             WHERE id = $12",
        )
        .bind(status)
        .bind(cinder_attachment_id)
        .bind(connector_host)
        .bind(connector_ip)
        .bind(connector_initiator)
        .bind(driver_volume_type)
        .bind(target_iqn)
        .bind(target_portal)
        .bind(target_lun.map(|l| l as i32))
        .bind(connection_info_digest)
        .bind(device)
        .bind(&id_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )));
        }

        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::Corrupt(format!(
                "volume attachment `{id}` not found"
            )))
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE id = $1")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let vol_id_str = volume_id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = $1")
            .bind(&vol_id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let vol_id_str = volume_id.to_string();
        let srv_id_str = server_id.to_string();
        let row =
            sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = $1 AND server_id = $2")
                .bind(&vol_id_str)
                .bind(&srv_id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE idempotency_key = $1")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM volume_attachments WHERE status = ANY($1) ORDER BY created_at ASC",
        )
        .bind(terminal)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_volume_attachment).collect()
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let srv_id_str = server_id.to_string();
        let rows = sqlx::query(
            "SELECT * FROM volume_attachments WHERE server_id = $1 ORDER BY created_at ASC",
        )
        .bind(&srv_id_str)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_volume_attachment).collect()
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let srv_id_str = server_id.to_string();
        let att_id_str = attachment_id.to_string();
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE server_id = $1 AND id = $2")
            .bind(&srv_id_str)
            .bind(&att_id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_volume_attachment(&r)).transpose()
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        let srv_id_str = server_id.to_string();
        let att_id_str = attachment_id.to_string();
        let res = sqlx::query("DELETE FROM volume_attachments WHERE server_id = $1 AND id = $2")
            .bind(&srv_id_str)
            .bind(&att_id_str)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "volume attachment `{attachment_id}` not found"
            )));
        }
        Ok(())
    }
}

fn parse_pg_volume_attachment(row: &PgRow) -> Result<VolumeAttachmentRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    let srv_id_str: String = row.get("server_id");
    let server_id = parse_uuid(&srv_id_str)?;
    let vol_id_str: String = row.get("volume_id");
    let volume_id = parse_uuid(&vol_id_str)?;
    let op_id = row
        .get::<Option<String>, _>("operation_id")
        .as_deref()
        .map(parse_uuid)
        .transpose()?;
    let target_lun = row.get::<Option<i32>, _>("target_lun").map(|l| l as u32);
    let del_term: i32 = row.get("delete_on_termination");

    Ok(VolumeAttachmentRecord {
        id,
        server_id,
        volume_id,
        device: row.get("device"),
        tag: row.get("tag"),
        delete_on_termination: del_term != 0,
        created_at: row.get("created_at"),
        status: row.get("status"),
        operation_id: op_id,
        idempotency_key: row.get("idempotency_key"),
        cinder_attachment_id: row.get("cinder_attachment_id"),
        connector_host: row.get("connector_host"),
        connector_ip: row.get("connector_ip"),
        connector_initiator: row.get("connector_initiator"),
        driver_volume_type: row.get("driver_volume_type"),
        target_iqn: row.get("target_iqn"),
        target_portal: row.get("target_portal"),
        target_lun,
        connection_info_digest: row.get("connection_info_digest"),
        error: row.get("error"),
    })
}

#[async_trait]
impl ImageRepository for PostgresStore {
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        let id_str = image.id.to_string();
        sqlx::query(
            "INSERT INTO image_metadata (id, name, project_id, status, visibility, container_format, disk_format, size, checksum)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&id_str)
        .bind(&image.name)
        .bind(&image.project_id)
        .bind(&image.status)
        .bind(&image.visibility)
        .bind(&image.container_format)
        .bind(&image.disk_format)
        .bind(image.size)
        .bind(&image.checksum)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM image_metadata
             WHERE (project_id = $1 OR visibility = 'public') AND status != 'deleted'
             ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_image).collect()
    }

    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query(
            "SELECT * FROM image_metadata
             WHERE id = $1 AND (project_id = $2 OR visibility = 'public') AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_image(&r)).transpose()
    }

    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE image_metadata
             SET status = 'active', size = $1, checksum = $2
             WHERE id = $3 AND project_id = $4 AND status != 'deleted'",
        )
        .bind(size as i64)
        .bind(checksum)
        .bind(&id_str)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }

        self.get_image(project_id, id)
            .await?
            .ok_or(StoreError::ImageNotFound)
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE image_metadata
             SET status = 'deleted'
             WHERE id = $1 AND project_id = $2 AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::ImageNotFound);
        }
        Ok(())
    }
}

fn parse_pg_image(row: &PgRow) -> Result<ImageMetadataRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;

    Ok(ImageMetadataRecord {
        id,
        name: row.get("name"),
        project_id: row.get("project_id"),
        status: row.get("status"),
        visibility: row.get("visibility"),
        container_format: row.get("container_format"),
        disk_format: row.get("disk_format"),
        size: row.get("size"),
        checksum: row.get("checksum"),
    })
}

#[async_trait]
impl NetworkRepository for PostgresStore {
    async fn get_canonical_owner(
        &self,
        resource_name: &str,
        id: &Uuid,
    ) -> Result<Option<String>, StoreError> {
        let table = match resource_name {
            "network" => "canonical_networks",
            "address_realm" => "canonical_address_realms",
            "address_pool" => "canonical_address_pools",
            "endpoint" => "canonical_endpoints",
            "l3_gateway" => "canonical_l3_gateways",
            "l3_gateway_attachment" => "canonical_l3_gateway_attachments",
            _ => {
                return Err(StoreError::Corrupt(
                    "unknown canonical resource type".into(),
                ));
            }
        };
        let query = format!("SELECT project_id FROM {table} WHERE id = $1");
        sqlx::query_scalar(&query)
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)
    }
    async fn insert_canonical_network(
        &self,
        network: &CanonicalNetworkRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_network(network).await
    }
    async fn get_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalNetworkRecord>, StoreError> {
        self.get_canonical_network(project_id, id).await
    }
    async fn list_canonical_networks(
        &self,
        project_id: &str,
    ) -> Result<Vec<CanonicalNetworkRecord>, StoreError> {
        self.list_canonical_networks(project_id).await
    }
    async fn update_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        name: &str,
        admin_state_up: bool,
    ) -> Result<CanonicalNetworkRecord, StoreError> {
        self.update_canonical_network(project_id, id, expected_generation, name, admin_state_up)
            .await
    }
    async fn insert_canonical_l3_gateway(
        &self,
        g: &CanonicalL3GatewayRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_l3_gateway(g).await
    }
    async fn get_canonical_l3_gateway(
        &self,
        p: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayRecord>, StoreError> {
        self.get_canonical_l3_gateway(p, id).await
    }
    async fn list_canonical_l3_gateways(
        &self,
        p: &str,
    ) -> Result<Vec<CanonicalL3GatewayRecord>, StoreError> {
        self.list_canonical_l3_gateways(p).await
    }
    async fn update_canonical_l3_gateway(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
        n: &str,
        x: Option<Uuid>,
        s: bool,
    ) -> Result<CanonicalL3GatewayRecord, StoreError> {
        self.update_canonical_l3_gateway(p, id, e, n, x, s).await
    }
    async fn begin_canonical_l3_gateway_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<CanonicalL3GatewayRecord, StoreError> {
        self.begin_canonical_l3_gateway_deletion(p, id, e).await
    }
    async fn finalize_canonical_l3_gateway_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<(), StoreError> {
        self.finalize_canonical_l3_gateway_deletion(p, id, e).await
    }
    async fn insert_canonical_l3_gateway_attachment(
        &self,
        a: &CanonicalL3GatewayAttachmentRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_l3_gateway_attachment(a).await
    }
    async fn get_canonical_l3_gateway_attachment(
        &self,
        p: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        self.get_canonical_l3_gateway_attachment(p, id).await
    }
    async fn list_canonical_l3_gateway_attachments(
        &self,
        p: &str,
        g: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        self.list_canonical_l3_gateway_attachments(p, g).await
    }
    async fn list_canonical_realm_l3_gateway_attachments(
        &self,
        p: &str,
        r: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError> {
        self.list_canonical_realm_l3_gateway_attachments(p, r).await
    }
    async fn begin_canonical_l3_gateway_attachment_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<CanonicalL3GatewayAttachmentRecord, StoreError> {
        self.begin_canonical_l3_gateway_attachment_deletion(p, id, e)
            .await
    }
    async fn finalize_canonical_l3_gateway_attachment_deletion(
        &self,
        p: &str,
        id: &Uuid,
        e: u64,
    ) -> Result<(), StoreError> {
        self.finalize_canonical_l3_gateway_attachment_deletion(p, id, e)
            .await
    }
    async fn insert_canonical_realm(
        &self,
        realm: &CanonicalAddressRealmRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_realm(realm).await
    }
    async fn get_canonical_realm(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalAddressRealmRecord>, StoreError> {
        self.get_canonical_realm(project_id, id).await
    }
    async fn list_canonical_realms(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressRealmRecord>, StoreError> {
        self.list_canonical_realms(project_id, network_id).await
    }
    async fn insert_canonical_pool(
        &self,
        pool: &CanonicalAddressPoolRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_pool(pool).await
    }
    async fn insert_subnet_bundle(
        &self,
        realm: &CanonicalAddressRealmRecord,
        pool: &CanonicalAddressPoolRecord,
        subnet: &SubnetRecord,
    ) -> Result<(), StoreError> {
        self.insert_subnet_bundle(realm, pool, subnet).await
    }
    async fn list_canonical_pools(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressPoolRecord>, StoreError> {
        self.list_canonical_pools(project_id, realm_id).await
    }
    async fn delete_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_pool(project_id, pool_id).await
    }
    async fn update_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
        expected_generation: u64,
        gateway: Option<Ipv4Addr>,
    ) -> Result<CanonicalAddressPoolRecord, StoreError> {
        self.update_canonical_pool(project_id, pool_id, expected_generation, gateway)
            .await
    }
    async fn insert_canonical_endpoint(
        &self,
        endpoint: &CanonicalEndpointRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_endpoint(endpoint).await
    }
    async fn insert_canonical_endpoint_and_port(
        &self,
        endpoint: &CanonicalEndpointRecord,
        port: &PortRecord,
    ) -> Result<(), StoreError> {
        self.insert_canonical_endpoint_and_port(endpoint, port)
            .await
    }
    async fn list_canonical_endpoints(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalEndpointRecord>, StoreError> {
        self.list_canonical_endpoints(project_id, realm_id).await
    }
    async fn get_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<Option<CanonicalEndpointRecord>, StoreError> {
        self.get_canonical_endpoint(project_id, endpoint_id).await
    }
    async fn delete_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_endpoint(project_id, endpoint_id)
            .await
    }
    async fn delete_canonical_endpoint_and_port(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_endpoint_and_port(project_id, endpoint_id)
            .await
    }
    async fn upsert_canonical_policy(
        &self,
        policy: &CanonicalNetworkPolicyRecord,
    ) -> Result<(), StoreError> {
        self.upsert_canonical_policy(policy).await
    }
    async fn list_canonical_policies(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalNetworkPolicyRecord>, StoreError> {
        self.list_canonical_policies(project_id, network_id).await
    }
    async fn delete_canonical_policy(
        &self,
        project_id: &str,
        policy_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_policy(project_id, policy_id).await
    }
    async fn begin_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<CanonicalAddressRealmRecord, StoreError> {
        self.begin_canonical_realm_deletion(project_id, realm_id, expected_generation)
            .await
    }
    async fn finalize_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), StoreError> {
        self.finalize_canonical_realm_deletion(project_id, realm_id, expected_generation)
            .await
    }
    async fn list_canonical_realm_bindings(
        &self,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalRealmBindingRecord>, StoreError> {
        self.list_canonical_realm_bindings(realm_id).await
    }
    async fn delete_canonical_realm_binding(
        &self,
        binding: &CanonicalRealmBindingRecord,
        expected_realm_generation: u64,
    ) -> Result<(), StoreError> {
        self.delete_canonical_realm_binding(binding, expected_realm_generation)
            .await
    }
    async fn delete_canonical_realm(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_realm(project_id, realm_id).await
    }
    async fn delete_canonical_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_canonical_network(project_id, network_id).await
    }
    async fn backfill_canonical_network_state(&self) -> Result<(), StoreError> {
        self.backfill_canonical_network_state().await
    }
    async fn allocate_network_address(
        &self,
        realm_id: &Uuid,
        project_id: &str,
        endpoint_id: &Uuid,
        operation_id: &str,
        prefix: &str,
    ) -> Result<NetworkAddressAllocationRecord, StoreError> {
        if project_id.trim().is_empty() || operation_id.trim().is_empty() {
            return Err(StoreError::Corrupt(
                "network address allocation has empty identity".to_owned(),
            ));
        }
        let (network, prefix_len) = parse_pg_ipv4_prefix(prefix)?;
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(realm_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        let existing = sqlx::query(
            "SELECT realm_id, project_id, endpoint_id, operation_id, address::text AS address
             FROM network_address_allocations WHERE endpoint_id = $1 OR operation_id = $2
             FOR UPDATE",
        )
        .bind(endpoint_id.to_string())
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        if let Some(row) = existing {
            let allocation = parse_pg_network_allocation(&row)?;
            if allocation.realm_id == *realm_id
                && allocation.project_id == project_id
                && allocation.endpoint_id == *endpoint_id
                && allocation.operation_id == operation_id
            {
                tx.commit().await.map_err(StoreError::Database)?;
                return Ok(allocation);
            }
            return Err(StoreError::NetworkAddressConflict);
        }
        let occupied = sqlx::query(
            "SELECT address::text AS address FROM network_address_allocations WHERE realm_id = $1",
        )
        .bind(realm_id.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        let occupied: std::collections::HashSet<std::net::Ipv4Addr> = occupied
            .iter()
            .map(|row| {
                row.get::<String, _>("address")
                    .split('/')
                    .next()
                    .ok_or_else(|| {
                        StoreError::Corrupt("invalid allocated network address".to_owned())
                    })?
                    .parse()
                    .map_err(|_| {
                        StoreError::Corrupt("invalid allocated network address".to_owned())
                    })
            })
            .collect::<Result<_, StoreError>>()?;
        let (first, last) = allocation_bounds(network, prefix_len);
        let address = (first..=last)
            .map(std::net::Ipv4Addr::from)
            .find(|candidate| !occupied.contains(candidate))
            .ok_or(StoreError::NetworkAddressExhausted)?;
        sqlx::query(
            "INSERT INTO network_address_allocations (realm_id, project_id, endpoint_id, operation_id, address)
             VALUES ($1, $2, $3, $4, $5::inet)",
        )
        .bind(realm_id.to_string())
        .bind(project_id)
        .bind(endpoint_id.to_string())
        .bind(operation_id)
        .bind(address.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(database) if database.is_unique_violation() => {
                StoreError::NetworkAddressConflict
            }
            other => StoreError::Database(other),
        })?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(NetworkAddressAllocationRecord {
            realm_id: *realm_id,
            project_id: project_id.to_owned(),
            endpoint_id: *endpoint_id,
            operation_id: operation_id.to_owned(),
            address,
        })
    }

    async fn release_network_address(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM network_address_allocations WHERE project_id = $1 AND endpoint_id = $2",
        )
        .bind(project_id)
        .bind(endpoint_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn insert_network_intent(&self, intent: &NetworkIntentRecord) -> Result<(), StoreError> {
        validate_network_intent(intent)?;
        let generation = i64::try_from(intent.generation).map_err(|_| {
            StoreError::Corrupt("network intent generation exceeds PostgreSQL range".to_owned())
        })?;
        let result = sqlx::query(
            "INSERT INTO network_intents (id, project_id, generation, payload, plan_fingerprint_sha256, status)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(intent.id.to_string())
        .bind(&intent.project_id)
        .bind(generation)
        .bind(&intent.payload)
        .bind(&intent.plan_fingerprint_sha256)
        .bind(&intent.status)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                match self
                    .get_network_intent(&intent.project_id, &intent.id)
                    .await?
                {
                    Some(existing) if existing == *intent => Ok(()),
                    _ => Err(StoreError::ResourceAlreadyExists),
                }
            }
            Err(error) => Err(map_pg_error(error)),
        }
    }

    async fn list_network_intents(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkIntentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, project_id, generation, payload, plan_fingerprint_sha256, status
             FROM network_intents WHERE project_id = $1 ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(parse_pg_network_intent).collect()
    }

    async fn get_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkIntentRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, project_id, generation, payload, plan_fingerprint_sha256, status
             FROM network_intents WHERE id = $1 AND project_id = $2",
        )
        .bind(id.to_string())
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        row.as_ref().map(parse_pg_network_intent).transpose()
    }

    async fn update_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        payload: &str,
        plan_fingerprint_sha256: Option<&str>,
        status: &str,
    ) -> Result<NetworkIntentRecord, StoreError> {
        crate::validate_network_intent_update(project_id, payload, status)?;
        let existing = self
            .get_network_intent(project_id, id)
            .await?
            .ok_or(StoreError::NetworkIntentNotFound)?;
        crate::validate_network_intent_transition(&existing.status, status)?;
        let expected = i64::try_from(expected_generation).map_err(|_| {
            StoreError::Corrupt("network intent generation exceeds PostgreSQL range".to_owned())
        })?;
        let result = sqlx::query(
            "UPDATE network_intents
             SET generation = generation + 1, payload = $1, plan_fingerprint_sha256 = $2, status = $3, updated_at = NOW()
             WHERE id = $4 AND project_id = $5 AND generation = $6",
        )
        .bind(payload)
        .bind(plan_fingerprint_sha256)
        .bind(status)
        .bind(id.to_string())
        .bind(project_id)
        .bind(expected)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_network_intent(project_id, id).await? {
                Some(_) => Err(StoreError::StaleGeneration),
                None => Err(StoreError::NetworkIntentNotFound),
            };
        }
        self.get_network_intent(project_id, id)
            .await?
            .ok_or(StoreError::Corrupt(
                "updated network intent disappeared".to_owned(),
            ))
    }

    async fn insert_network(&self, network: &NetworkRecord) -> Result<(), StoreError> {
        let id_str = network.id.to_string();
        sqlx::query(
            "INSERT INTO network_networks (id, name, project_id, status)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&id_str)
        .bind(&network.name)
        .bind(&network.project_id)
        .bind(&network.status)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn list_networks(&self, project_id: &str) -> Result<Vec<NetworkRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM network_networks
             WHERE project_id = $1 AND status != 'deleted'
             ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_network).collect()
    }

    async fn get_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query(
            "SELECT * FROM network_networks
             WHERE id = $1 AND project_id = $2 AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_network(&r)).transpose()
    }

    async fn delete_network(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let id_str = id.to_string();
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let subnet_count: i64 =
            sqlx::query("SELECT COUNT(*) FROM network_subnets WHERE network_id = $1")
                .bind(&id_str)
                .fetch_one(&mut *tx)
                .await
                .map_err(StoreError::Database)?
                .get(0);

        if subnet_count > 0 {
            return Err(StoreError::NetworkInUse);
        }

        let res = sqlx::query(
            "UPDATE network_networks
             SET status = 'deleted'
             WHERE id = $1 AND project_id = $2 AND status != 'deleted'",
        )
        .bind(&id_str)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }

    async fn insert_subnet(&self, subnet: &SubnetRecord) -> Result<(), StoreError> {
        let id_str = subnet.id.to_string();
        let net_id_str = subnet.network_id.to_string();

        sqlx::query(
            "INSERT INTO network_subnets (id, network_id, name, project_id, cidr, gateway_ip, allocation_start, allocation_end, ip_version, enable_dhcp)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id_str)
        .bind(&net_id_str)
        .bind(&subnet.name)
        .bind(&subnet.project_id)
        .bind(&subnet.cidr)
        .bind(subnet.gateway_ip.to_string())
        .bind(subnet.allocation_start.to_string())
        .bind(subnet.allocation_end.to_string())
        .bind(i16::from(subnet.ip_version))
        .bind(subnet.enable_dhcp)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn list_subnets(&self, project_id: &str) -> Result<Vec<SubnetRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM network_subnets WHERE project_id = $1 ORDER BY id")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_subnet).collect()
    }

    async fn list_subnets_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<SubnetRecord>, StoreError> {
        let net_id_str = network_id.to_string();
        let rows = sqlx::query(
            "SELECT * FROM network_subnets WHERE project_id = $1 AND network_id = $2 ORDER BY id",
        )
        .bind(project_id)
        .bind(&net_id_str)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_subnet).collect()
    }

    async fn get_subnet(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SubnetRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM network_subnets WHERE id = $1 AND project_id = $2")
            .bind(&id_str)
            .bind(project_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_subnet(&r)).transpose()
    }

    async fn delete_subnet(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let id_str = id.to_string();
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let port_count: i64 =
            sqlx::query("SELECT COUNT(*) FROM network_ports WHERE subnet_id = $1")
                .bind(&id_str)
                .fetch_one(&mut *tx)
                .await
                .map_err(StoreError::Database)?
                .get(0);

        if port_count > 0 {
            return Err(StoreError::NetworkInUse);
        }

        let res = sqlx::query("DELETE FROM network_subnets WHERE id = $1 AND project_id = $2")
            .bind(&id_str)
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }

    async fn update_subnet(&self, subnet: &SubnetRecord) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE network_subnets SET name = $1, gateway_ip = $2, allocation_start = $3, allocation_end = $4, ip_version = $5, enable_dhcp = $6 WHERE id = $7 AND project_id = $8",
        )
        .bind(&subnet.name)
        .bind(subnet.gateway_ip.to_string())
        .bind(subnet.allocation_start.to_string())
        .bind(subnet.allocation_end.to_string())
        .bind(i16::from(subnet.ip_version))
        .bind(subnet.enable_dhcp)
        .bind(subnet.id.to_string())
        .bind(&subnet.project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        Ok(())
    }

    async fn delete_subnet_bundle(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let endpoints: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM canonical_endpoints WHERE realm_id = $1 AND project_id = $2",
        )
        .bind(id.to_string())
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        if endpoints != 0 {
            return Err(StoreError::NetworkInUse);
        }
        let realm: Option<String> = sqlx::query_scalar(
            "SELECT id FROM canonical_address_realms WHERE id = $1 AND project_id = $2 FOR UPDATE",
        )
        .bind(id.to_string())
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?;
        if realm.is_some() {
            sqlx::query(
                "DELETE FROM canonical_address_pools WHERE realm_id = $1 AND project_id = $2",
            )
            .bind(id.to_string())
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
            let result = sqlx::query("DELETE FROM canonical_address_realms WHERE id = $1 AND project_id = $2 AND NOT EXISTS (SELECT 1 FROM canonical_address_pools WHERE realm_id = canonical_address_realms.id) AND NOT EXISTS (SELECT 1 FROM canonical_endpoints WHERE realm_id = canonical_address_realms.id)")
                .bind(id.to_string()).bind(project_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
            if result.rows_affected() == 0 {
                return Err(StoreError::NetworkInUse);
            }
        }
        sqlx::query("DELETE FROM network_subnets WHERE id = $1 AND project_id = $2")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        tx.commit().await.map_err(StoreError::Database)
    }

    async fn update_subnet_bundle(
        &self,
        subnet: &SubnetRecord,
        pool_id: &Uuid,
        expected_pool_generation: u64,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let pool = sqlx::query("UPDATE canonical_address_pools SET gateway = $1::inet, generation = generation + 1 WHERE id = $2 AND project_id = $3 AND generation = $4 AND state = 'active'")
            .bind(subnet.gateway_ip.to_string()).bind(pool_id.to_string()).bind(&subnet.project_id).bind(crate::checked_generation(expected_pool_generation)?).execute(&mut *tx).await.map_err(StoreError::Database)?;
        if pool.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        let metadata = sqlx::query("UPDATE network_subnets SET name = $1, gateway_ip = $2, allocation_start = $3, allocation_end = $4, ip_version = $5, enable_dhcp = $6 WHERE id = $7 AND project_id = $8")
            .bind(&subnet.name).bind(subnet.gateway_ip.to_string()).bind(subnet.allocation_start.to_string()).bind(subnet.allocation_end.to_string()).bind(i16::from(subnet.ip_version)).bind(subnet.enable_dhcp).bind(subnet.id.to_string()).bind(&subnet.project_id).execute(&mut *tx).await.map_err(StoreError::Database)?;
        if metadata.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        tx.commit().await.map_err(StoreError::Database)
    }

    async fn insert_port(&self, port: &PortRecord) -> Result<(), StoreError> {
        let id_str = port.id.to_string();
        let net_id_str = port.network_id.to_string();
        let sub_id_str = port.subnet_id.map(|id| id.to_string());

        sqlx::query(
            "INSERT INTO network_ports (id, network_id, subnet_id, project_id, name, mac_address, fixed_ip, status, binding_host, binding_state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id_str)
        .bind(&net_id_str)
        .bind(sub_id_str.as_deref())
        .bind(&port.project_id)
        .bind(&port.name)
        .bind(&port.mac_address)
        .bind(port.fixed_ip.to_string())
        .bind(&port.status)
        .bind(&port.binding_host)
        .bind(&port.binding_state)
        .execute(&self.pool)
        .await
        .map_err(map_pg_error)?;
        Ok(())
    }

    async fn list_ports(&self, project_id: &str) -> Result<Vec<PortRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM network_ports WHERE project_id = $1 ORDER BY id")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_port).collect()
    }

    async fn list_ports_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<PortRecord>, StoreError> {
        let net_id_str = network_id.to_string();
        let rows = sqlx::query(
            "SELECT * FROM network_ports WHERE project_id = $1 AND network_id = $2 ORDER BY id",
        )
        .bind(project_id)
        .bind(&net_id_str)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(parse_pg_port).collect()
    }

    async fn get_port(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<PortRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM network_ports WHERE id = $1 AND project_id = $2")
            .bind(&id_str)
            .bind(project_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| parse_pg_port(&r)).transpose()
    }

    async fn get_port_by_id(&self, id: &Uuid) -> Result<Option<PortRecord>, StoreError> {
        let id_str = id.to_string();
        let row = sqlx::query("SELECT * FROM network_ports WHERE id = $1")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| parse_pg_port(&r)).transpose()
    }

    async fn delete_port(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        let id_str = id.to_string();
        sqlx::query("DELETE FROM network_security_group_bindings WHERE project_id = $1 AND endpoint_id = $2")
            .bind(project_id)
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let res = sqlx::query("DELETE FROM network_ports WHERE id = $1 AND project_id = $2")
            .bind(&id_str)
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        Ok(())
    }

    async fn update_port_binding(
        &self,
        project_id: &str,
        id: &Uuid,
        binding_host: Option<&str>,
        binding_state: Option<&str>,
    ) -> Result<PortRecord, StoreError> {
        let id_str = id.to_string();
        let res = sqlx::query(
            "UPDATE network_ports
             SET binding_host = COALESCE($1, binding_host),
                 binding_state = COALESCE($2, binding_state)
             WHERE id = $3 AND project_id = $4",
        )
        .bind(binding_host)
        .bind(binding_state)
        .bind(&id_str)
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }

        self.get_port(project_id, id)
            .await?
            .ok_or(StoreError::NetworkNotFound)
    }

    async fn update_port_name(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
    ) -> Result<PortRecord, StoreError> {
        let id_str = id.to_string();
        let res =
            sqlx::query("UPDATE network_ports SET name = $1 WHERE id = $2 AND project_id = $3")
                .bind(name)
                .bind(&id_str)
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        self.get_port(project_id, id)
            .await?
            .ok_or(StoreError::NetworkNotFound)
    }

    async fn insert_security_group(&self, group: &SecurityGroupRecord) -> Result<(), StoreError> {
        self.insert_security_group(group).await
    }
    async fn list_security_groups(
        &self,
        project_id: &str,
    ) -> Result<Vec<SecurityGroupRecord>, StoreError> {
        self.list_security_groups(project_id).await
    }
    async fn get_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRecord>, StoreError> {
        self.get_security_group(project_id, id).await
    }
    async fn update_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
        description: &str,
    ) -> Result<SecurityGroupRecord, StoreError> {
        self.update_security_group(project_id, id, name, description)
            .await
    }
    async fn delete_security_group(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        self.delete_security_group(project_id, id).await
    }
    async fn insert_security_group_rule(
        &self,
        rule: &SecurityGroupRuleRecord,
    ) -> Result<(), StoreError> {
        self.insert_security_group_rule(rule).await
    }
    async fn list_security_group_rules(
        &self,
        project_id: &str,
        group_id: &Uuid,
    ) -> Result<Vec<SecurityGroupRuleRecord>, StoreError> {
        self.list_security_group_rules(project_id, group_id).await
    }
    async fn get_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRuleRecord>, StoreError> {
        self.get_security_group_rule(project_id, id).await
    }
    async fn delete_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<(), StoreError> {
        self.delete_security_group_rule(project_id, id).await
    }
    async fn list_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: Option<&Uuid>,
    ) -> Result<Vec<SecurityGroupBindingRecord>, StoreError> {
        self.list_security_group_bindings(project_id, endpoint_id)
            .await
    }
    async fn replace_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
        group_ids: &[Uuid],
    ) -> Result<(), StoreError> {
        self.replace_security_group_bindings(project_id, endpoint_id, group_ids)
            .await
    }
}

fn validate_network_intent(intent: &NetworkIntentRecord) -> Result<(), StoreError> {
    if intent.project_id.is_empty() || intent.payload.is_empty() || intent.status.is_empty() {
        return Err(StoreError::Corrupt(
            "network intent has empty required field".to_owned(),
        ));
    }
    if intent.generation == 0 {
        return Err(StoreError::Corrupt(
            "network intent generation must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn parse_pg_network_intent(row: &PgRow) -> Result<NetworkIntentRecord, StoreError> {
    let generation: i64 = row.get("generation");
    Ok(NetworkIntentRecord {
        id: parse_uuid(&row.get::<String, _>("id"))?,
        project_id: row.get("project_id"),
        generation: u64::try_from(generation)
            .map_err(|_| StoreError::Corrupt("negative network intent generation".to_owned()))?,
        payload: row.get("payload"),
        plan_fingerprint_sha256: row.get("plan_fingerprint_sha256"),
        status: row.get("status"),
    })
}

fn parse_pg_network(row: &PgRow) -> Result<NetworkRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    Ok(NetworkRecord {
        id,
        name: row.get("name"),
        project_id: row.get("project_id"),
        status: row.get("status"),
    })
}

fn canonical_network_from_pg_row(row: &PgRow) -> Result<CanonicalNetworkRecord, StoreError> {
    let generation: i64 = row.get("generation");
    Ok(CanonicalNetworkRecord {
        id: Uuid::parse_str(row.get::<&str, _>("id")).map_err(StoreError::InvalidUuid)?,
        project_id: row.get("project_id"),
        name: row.get("name"),
        admin_state_up: row.get("admin_state_up"),
        generation: u64::try_from(generation)
            .map_err(|_| StoreError::Corrupt("negative canonical generation".into()))?,
        state: row.get("state"),
    })
}

fn canonical_realm_from_pg_row(row: &PgRow) -> Result<CanonicalAddressRealmRecord, StoreError> {
    let generation: i64 = row.get("generation");
    Ok(CanonicalAddressRealmRecord {
        id: Uuid::parse_str(row.get::<&str, _>("id")).map_err(StoreError::InvalidUuid)?,
        network_id: Uuid::parse_str(row.get::<&str, _>("network_id"))
            .map_err(StoreError::InvalidUuid)?,
        project_id: row.get("project_id"),
        prefix: row.get("prefix"),
        overlapping_prefixes: row.get("overlapping_prefixes"),
        generation: u64::try_from(generation)
            .map_err(|_| StoreError::Corrupt("negative canonical generation".into()))?,
        state: row.get("state"),
    })
}

fn canonical_pool_from_pg_row(row: &PgRow) -> Result<CanonicalAddressPoolRecord, StoreError> {
    let generation: i64 = row.get("generation");
    Ok(CanonicalAddressPoolRecord {
        id: Uuid::parse_str(row.get::<&str, _>("id")).map_err(StoreError::InvalidUuid)?,
        realm_id: Uuid::parse_str(row.get::<&str, _>("realm_id"))
            .map_err(StoreError::InvalidUuid)?,
        project_id: row.get("project_id"),
        prefix: row.get("prefix"),
        gateway: row
            .get::<Option<String>, _>("gateway")
            .map(|value| parse_pg_ipv4(&value))
            .transpose()
            .map_err(|_| StoreError::Corrupt("invalid canonical gateway".into()))?,
        first_usable: parse_pg_ipv4(&row.get::<String, _>("first_usable"))
            .map_err(|_| StoreError::Corrupt("invalid canonical pool start".into()))?,
        last_usable: parse_pg_ipv4(&row.get::<String, _>("last_usable"))
            .map_err(|_| StoreError::Corrupt("invalid canonical pool end".into()))?,
        generation: u64::try_from(generation)
            .map_err(|_| StoreError::Corrupt("negative canonical generation".into()))?,
        state: row.get("state"),
    })
}

fn canonical_endpoint_from_pg_row(row: &PgRow) -> Result<CanonicalEndpointRecord, StoreError> {
    let generation: i64 = row.get("generation");
    Ok(CanonicalEndpointRecord {
        id: Uuid::parse_str(row.get::<&str, _>("id")).map_err(StoreError::InvalidUuid)?,
        realm_id: Uuid::parse_str(row.get::<&str, _>("realm_id"))
            .map_err(StoreError::InvalidUuid)?,
        project_id: row.get("project_id"),
        fixed_ip: parse_pg_ipv4(&row.get::<String, _>("fixed_ip"))
            .map_err(|_| StoreError::Corrupt("invalid canonical endpoint IP".into()))?,
        mac: row.get("mac"),
        generation: u64::try_from(generation)
            .map_err(|_| StoreError::Corrupt("negative canonical generation".into()))?,
        state: row.get("state"),
    })
}

fn canonical_policy_from_pg_row(row: &PgRow) -> Result<CanonicalNetworkPolicyRecord, StoreError> {
    Ok(CanonicalNetworkPolicyRecord {
        id: parse_uuid(row.get("id"))?,
        project_id: row.get("project_id"),
        endpoint_id: parse_uuid(row.get("endpoint_id"))?,
        direction: row.get("direction"),
        protocol: row.get("protocol"),
        port_min: row
            .get::<Option<i32>, _>("port_min")
            .map(parse_port)
            .transpose()?,
        port_max: row
            .get::<Option<i32>, _>("port_max")
            .map(parse_port)
            .transpose()?,
        source: row.get("source"),
        destination: row.get("destination"),
        action: row.get("action"),
        generation: u64::try_from(row.get::<i64, _>("generation"))
            .map_err(|_| StoreError::Corrupt("invalid policy generation".into()))?,
        state: row.get("state"),
    })
}

fn parse_port(value: i32) -> Result<u16, StoreError> {
    u16::try_from(value).map_err(|_| StoreError::Corrupt("invalid policy port".into()))
}

fn parse_pg_ipv4(value: &str) -> Result<Ipv4Addr, std::net::AddrParseError> {
    value.split('/').next().unwrap_or(value).parse()
}

fn parse_pg_ipv4_prefix(value: &str) -> Result<(u32, u8), StoreError> {
    let (address, length) = value
        .split_once('/')
        .ok_or_else(|| StoreError::Corrupt("network prefix is missing length".to_owned()))?;
    let address = address
        .parse::<std::net::Ipv4Addr>()
        .map_err(|_| StoreError::Corrupt("network prefix has invalid address".to_owned()))?;
    let prefix_len = length
        .parse::<u8>()
        .map_err(|_| StoreError::Corrupt("network prefix has invalid length".to_owned()))?;
    if !(1..=30).contains(&prefix_len) {
        return Err(StoreError::Corrupt(
            "network allocation prefix must leave usable addresses".to_owned(),
        ));
    }
    let mask = u32::MAX << (32 - prefix_len);
    let network = u32::from(address) & mask;
    if network != u32::from(address) {
        return Err(StoreError::Corrupt(
            "network prefix is not canonical".to_owned(),
        ));
    }
    Ok((network, prefix_len))
}

fn allocation_bounds(network: u32, prefix_len: u8) -> (u32, u32) {
    let size = 1u32 << (32 - prefix_len);
    (network + 1, network + size - 2)
}

fn parse_pg_network_allocation(row: &PgRow) -> Result<NetworkAddressAllocationRecord, StoreError> {
    Ok(NetworkAddressAllocationRecord {
        realm_id: parse_uuid(&row.get::<String, _>("realm_id"))?,
        project_id: row.get("project_id"),
        endpoint_id: parse_uuid(&row.get::<String, _>("endpoint_id"))?,
        operation_id: row.get("operation_id"),
        address: row
            .get::<String, _>("address")
            .split('/')
            .next()
            .ok_or_else(|| StoreError::Corrupt("invalid allocated network address".to_owned()))?
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid allocated network address".to_owned()))?,
    })
}

fn parse_pg_subnet(row: &PgRow) -> Result<SubnetRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    let net_id_str: String = row.get("network_id");
    let network_id = parse_uuid(&net_id_str)?;
    let gateway_ip: String = row.get("gateway_ip");
    let alloc_start: String = row.get("allocation_start");
    let alloc_end: String = row.get("allocation_end");

    Ok(SubnetRecord {
        id,
        network_id,
        name: row.get("name"),
        project_id: row.get("project_id"),
        cidr: row.get("cidr"),
        gateway_ip: gateway_ip
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid IPv4 address in durable state".to_owned()))?,
        allocation_start: alloc_start
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid IPv4 address in durable state".to_owned()))?,
        allocation_end: alloc_end
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid IPv4 address in durable state".to_owned()))?,
        ip_version: row.get::<i16, _>("ip_version") as u8,
        enable_dhcp: row.get("enable_dhcp"),
    })
}

fn pg_security_group_from_row(row: &PgRow) -> Result<SecurityGroupRecord, StoreError> {
    Ok(SecurityGroupRecord {
        id: parse_uuid(row.get("id"))?,
        project_id: row.get("project_id"),
        name: row.get("name"),
        description: row.get("description"),
    })
}

fn pg_security_group_rule_from_row(row: &PgRow) -> Result<SecurityGroupRuleRecord, StoreError> {
    let port_min = row
        .get::<Option<i32>, _>("port_min")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| StoreError::Corrupt("security-group port is out of range".to_owned()))?;
    let port_max = row
        .get::<Option<i32>, _>("port_max")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| StoreError::Corrupt("security-group port is out of range".to_owned()))?;
    Ok(SecurityGroupRuleRecord {
        id: parse_uuid(row.get("id"))?,
        security_group_id: parse_uuid(row.get("security_group_id"))?,
        project_id: row.get("project_id"),
        direction: row.get("direction"),
        protocol: row.get("protocol"),
        port_min,
        port_max,
        remote_ip_prefix: row.get("remote_ip_prefix"),
    })
}

fn pg_security_group_binding_from_row(
    row: &PgRow,
) -> Result<SecurityGroupBindingRecord, StoreError> {
    Ok(SecurityGroupBindingRecord {
        project_id: row.get("project_id"),
        endpoint_id: parse_uuid(row.get("endpoint_id"))?,
        security_group_id: parse_uuid(row.get("security_group_id"))?,
    })
}

fn parse_pg_port(row: &PgRow) -> Result<PortRecord, StoreError> {
    let id_str: String = row.get("id");
    let id = parse_uuid(&id_str)?;
    let net_id_str: String = row.get("network_id");
    let network_id = parse_uuid(&net_id_str)?;
    let sub_id = row
        .get::<Option<String>, _>("subnet_id")
        .as_deref()
        .map(parse_uuid)
        .transpose()?;
    let fixed_ip: String = row.get("fixed_ip");

    Ok(PortRecord {
        id,
        network_id,
        subnet_id: sub_id,
        project_id: row.get("project_id"),
        name: row.get("name"),
        mac_address: row.get("mac_address"),
        fixed_ip: fixed_ip
            .parse()
            .map_err(|_| StoreError::Corrupt("invalid IPv4 address in durable state".to_owned()))?,
        status: row.get("status"),
        binding_host: row.get("binding_host"),
        binding_state: row.get("binding_state"),
    })
}

#[async_trait]
impl PlacementRepository for PostgresStore {
    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<PlacementProviderRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, node_id, state, generation FROM placement_providers WHERE id = $1",
        )
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let Some(row) = row else {
            return Ok(None);
        };
        let provider = PlacementProviderRecord {
            id: row.get("id"),
            node_id: row.get("node_id"),
            state: row.get("state"),
            generation: row.get::<i64, _>("generation") as u64,
            inventories: self.load_placement_inventories(provider_id).await?,
            allocations: self.load_placement_allocations(provider_id).await?,
        };
        Ok(Some(provider))
    }

    async fn list_providers(&self) -> Result<Vec<PlacementProviderRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, node_id, state, generation FROM placement_providers ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut providers = Vec::new();
        for row in rows {
            let provider_id: String = row.get("id");
            let provider = PlacementProviderRecord {
                id: row.get("id"),
                node_id: row.get("node_id"),
                state: row.get("state"),
                generation: row.get::<i64, _>("generation") as u64,
                inventories: self.load_placement_inventories(&provider_id).await?,
                allocations: self.load_placement_allocations(&provider_id).await?,
            };
            providers.push(provider);
        }
        Ok(providers)
    }

    async fn register_provider(
        &self,
        node_id: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let row = sqlx::query(
            "INSERT INTO placement_providers (id, node_id, state, generation)
             VALUES ($1, $1, 'Enabled', 1)
             ON CONFLICT (node_id) DO UPDATE SET state = 'Enabled'
             RETURNING id, node_id, state, generation",
        )
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        let id: String = row.get("id");
        for inv in inventories {
            sqlx::query(
                "INSERT INTO placement_inventories (provider_id, resource_class, total, reserved, allocation_ratio, used)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (provider_id, resource_class) DO UPDATE
                 SET total = EXCLUDED.total, reserved = EXCLUDED.reserved, allocation_ratio = EXCLUDED.allocation_ratio",
            )
            .bind(&id)
            .bind(&inv.resource_class)
            .bind(inv.total as i64)
            .bind(inv.reserved as i64)
            .bind(inv.allocation_ratio)
            .bind(inv.used as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_provider(&id)
            .await?
            .ok_or(StoreError::PlacementProviderNotFound)
    }

    async fn sync_provider(
        &self,
        node_id: &str,
        state: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let row = sqlx::query(
            "INSERT INTO placement_providers (id, node_id, state, generation)
             VALUES ($1, $1, $2, 1)
             ON CONFLICT (node_id) DO UPDATE SET state = EXCLUDED.state
             RETURNING id, node_id, state, generation",
        )
        .bind(node_id)
        .bind(state)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        let id: String = row.get("id");
        for inv in inventories {
            sqlx::query(
                "INSERT INTO placement_inventories (provider_id, resource_class, total, reserved, allocation_ratio, used)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (provider_id, resource_class) DO UPDATE
                 SET total = EXCLUDED.total, reserved = EXCLUDED.reserved, allocation_ratio = EXCLUDED.allocation_ratio",
            )
            .bind(&id)
            .bind(&inv.resource_class)
            .bind(inv.total as i64)
            .bind(inv.reserved as i64)
            .bind(inv.allocation_ratio)
            .bind(inv.used as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_provider(&id)
            .await?
            .ok_or(StoreError::PlacementProviderNotFound)
    }

    async fn refresh_inventories(
        &self,
        provider_id: &str,
        expected_generation: u64,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let res = sqlx::query(
            "UPDATE placement_providers
             SET generation = generation + 1
             WHERE id = $1 AND generation = $2",
        )
        .bind(provider_id)
        .bind(expected_generation as i64)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            let exists = sqlx::query("SELECT 1 FROM placement_providers WHERE id = $1")
                .bind(provider_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
            if exists.is_none() {
                return Err(StoreError::PlacementProviderNotFound);
            }
            return Err(StoreError::PlacementStaleGeneration);
        }

        for inv in inventories {
            sqlx::query(
                "INSERT INTO placement_inventories (provider_id, resource_class, total, reserved, allocation_ratio, used)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (provider_id, resource_class) DO UPDATE
                 SET total = EXCLUDED.total, reserved = EXCLUDED.reserved, allocation_ratio = EXCLUDED.allocation_ratio",
            )
            .bind(provider_id)
            .bind(&inv.resource_class)
            .bind(inv.total as i64)
            .bind(inv.reserved as i64)
            .bind(inv.allocation_ratio)
            .bind(inv.used as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        self.get_provider(provider_id)
            .await?
            .ok_or(StoreError::PlacementProviderNotFound)
    }

    async fn set_provider_state(&self, provider_id: &str, state: &str) -> Result<(), StoreError> {
        let res = sqlx::query("UPDATE placement_providers SET state = $1 WHERE id = $2")
            .bind(state)
            .bind(provider_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            return Err(StoreError::PlacementProviderNotFound);
        }
        Ok(())
    }

    async fn commit_allocation(
        &self,
        provider_id: &str,
        expected_generation: u64,
        allocation: &PlacementAllocationRecord,
    ) -> Result<PlacementAllocationRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let existing_alloc_row =
            sqlx::query("SELECT provider_id, consumer_id FROM placement_allocations WHERE id = $1")
                .bind(&allocation.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
        if let Some(row) = existing_alloc_row {
            let pid: String = row.get("provider_id");
            let cid: String = row.get("consumer_id");
            let res_rows = sqlx::query("SELECT resource_class, amount FROM placement_allocation_resources WHERE allocation_id = $1 ORDER BY resource_class")
                .bind(&allocation.id)
                .fetch_all(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
            let mut resources = Vec::new();
            for r in res_rows {
                resources.push(PlacementResourceRecord {
                    resource_class: r.get("resource_class"),
                    amount: r.get::<i64, _>("amount") as u64,
                });
            }
            let mut expected_resources = allocation.resources.clone();
            expected_resources.sort_by(|a, b| a.resource_class.cmp(&b.resource_class));
            if pid == provider_id
                && cid == allocation.consumer_id
                && resources == expected_resources
            {
                return Ok(allocation.clone());
            }
            return Err(StoreError::PlacementAllocationConflict);
        }

        let prov_row =
            sqlx::query("SELECT generation FROM placement_providers WHERE id = $1 FOR UPDATE")
                .bind(provider_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StoreError::Database)?;

        let Some(prov_row) = prov_row else {
            return Err(StoreError::PlacementProviderNotFound);
        };
        let current_gen: i64 = prov_row.get("generation");
        if current_gen as u64 != expected_generation {
            return Err(StoreError::PlacementStaleGeneration);
        }

        sqlx::query(
            "INSERT INTO placement_allocations (id, provider_id, consumer_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&allocation.id)
        .bind(provider_id)
        .bind(&allocation.consumer_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        for res in &allocation.resources {
            sqlx::query(
                "INSERT INTO placement_allocation_resources (allocation_id, resource_class, amount)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (allocation_id, resource_class) DO UPDATE SET amount = EXCLUDED.amount",
            )
            .bind(&allocation.id)
            .bind(&res.resource_class)
            .bind(res.amount as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

            sqlx::query(
                "UPDATE placement_inventories SET used = used + $1 WHERE provider_id = $2 AND resource_class = $3",
            )
            .bind(res.amount as i64)
            .bind(provider_id)
            .bind(&res.resource_class)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        sqlx::query("UPDATE placement_providers SET generation = generation + 1 WHERE id = $1")
            .bind(provider_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(allocation.clone())
    }

    async fn release_allocation(
        &self,
        provider_id: &str,
        allocation_id: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let res_rows = sqlx::query(
            "SELECT resource_class, amount FROM placement_allocation_resources WHERE allocation_id = $1",
        )
        .bind(allocation_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        for row in res_rows {
            let rc: String = row.get("resource_class");
            let amt: i64 = row.get("amount");
            sqlx::query(
                "UPDATE placement_inventories SET used = used - $1 WHERE provider_id = $2 AND resource_class = $3",
            )
            .bind(amt)
            .bind(provider_id)
            .bind(&rc)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        sqlx::query("DELETE FROM placement_allocations WHERE id = $1")
            .bind(allocation_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        sqlx::query("UPDATE placement_providers SET generation = generation + 1 WHERE id = $1")
            .bind(provider_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }

    async fn upsert_intent(
        &self,
        intent: &PlacementIntentRecord,
    ) -> Result<PlacementIntentRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        sqlx::query(
            "INSERT INTO placement_allocation_intents (id, provider_id, consumer_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET provider_id = EXCLUDED.provider_id, consumer_id = EXCLUDED.consumer_id",
        )
        .bind(&intent.id)
        .bind(&intent.provider_id)
        .bind(&intent.consumer_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        sqlx::query("DELETE FROM placement_allocation_intent_resources WHERE intent_id = $1")
            .bind(&intent.id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        for res in &intent.resources {
            sqlx::query(
                "INSERT INTO placement_allocation_intent_resources (intent_id, resource_class, amount)
                 VALUES ($1, $2, $3)",
            )
            .bind(&intent.id)
            .bind(&res.resource_class)
            .bind(res.amount as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(intent.clone())
    }

    async fn get_intent(
        &self,
        allocation_id: &str,
    ) -> Result<Option<PlacementIntentRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, provider_id, consumer_id FROM placement_allocation_intents WHERE id = $1",
        )
        .bind(allocation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let Some(row) = row else {
            return Ok(None);
        };
        let resources = self.load_intent_resources(allocation_id).await?;
        Ok(Some(PlacementIntentRecord {
            id: row.get("id"),
            provider_id: row.get("provider_id"),
            consumer_id: row.get("consumer_id"),
            resources,
        }))
    }

    async fn list_intents(&self) -> Result<Vec<PlacementIntentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, provider_id, consumer_id FROM placement_allocation_intents ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut intents = Vec::new();
        for row in rows {
            let intent_id: String = row.get("id");
            let resources = self.load_intent_resources(&intent_id).await?;
            intents.push(PlacementIntentRecord {
                id: row.get("id"),
                provider_id: row.get("provider_id"),
                consumer_id: row.get("consumer_id"),
                resources,
            });
        }
        Ok(intents)
    }

    async fn delete_intent(&self, allocation_id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM placement_allocation_intents WHERE id = $1")
            .bind(allocation_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn reconcile_consumers(
        &self,
        durable_consumer_ids: &[String],
    ) -> Result<PlacementReconcileRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        let alloc_rows = if durable_consumer_ids.is_empty() {
            sqlx::query("SELECT id, provider_id, consumer_id FROM placement_allocations")
                .fetch_all(&mut *tx)
                .await
                .map_err(StoreError::Database)?
        } else {
            sqlx::query(
                "SELECT id, provider_id, consumer_id FROM placement_allocations WHERE NOT (consumer_id = ANY($1))",
            )
            .bind(durable_consumer_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?
        };

        let mut orphaned_allocations = Vec::new();
        for row in alloc_rows {
            let aid: String = row.get("id");
            let pid: String = row.get("provider_id");
            let cid: String = row.get("consumer_id");

            let res_rows = sqlx::query(
                "SELECT resource_class, amount FROM placement_allocation_resources WHERE allocation_id = $1",
            )
            .bind(&aid)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

            let mut resources = Vec::new();
            for rrow in res_rows {
                let rc: String = rrow.get("resource_class");
                let amt: i64 = rrow.get("amount");
                resources.push(PlacementResourceRecord {
                    resource_class: rc.clone(),
                    amount: amt as u64,
                });
                sqlx::query(
                    "UPDATE placement_inventories SET used = used - $1 WHERE provider_id = $2 AND resource_class = $3",
                )
                .bind(amt)
                .bind(&pid)
                .bind(&rc)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;
            }

            sqlx::query("DELETE FROM placement_allocations WHERE id = $1")
                .bind(&aid)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;

            orphaned_allocations.push(PlacementAllocationRecord {
                id: aid,
                provider_id: pid,
                consumer_id: cid,
                resources,
            });
        }

        let intent_rows = if durable_consumer_ids.is_empty() {
            sqlx::query("SELECT id, provider_id, consumer_id FROM placement_allocation_intents")
                .fetch_all(&mut *tx)
                .await
                .map_err(StoreError::Database)?
        } else {
            sqlx::query(
                "SELECT id, provider_id, consumer_id FROM placement_allocation_intents WHERE NOT (consumer_id = ANY($1))",
            )
            .bind(durable_consumer_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?
        };

        let mut abandoned_intents = Vec::new();
        for row in intent_rows {
            let iid: String = row.get("id");
            let pid: String = row.get("provider_id");
            let cid: String = row.get("consumer_id");

            let res_rows = sqlx::query(
                "SELECT resource_class, amount FROM placement_allocation_intent_resources WHERE intent_id = $1",
            )
            .bind(&iid)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

            let mut resources = Vec::new();
            for rrow in res_rows {
                resources.push(PlacementResourceRecord {
                    resource_class: rrow.get("resource_class"),
                    amount: rrow.get::<i64, _>("amount") as u64,
                });
            }

            sqlx::query("DELETE FROM placement_allocation_intents WHERE id = $1")
                .bind(&iid)
                .execute(&mut *tx)
                .await
                .map_err(StoreError::Database)?;

            abandoned_intents.push(PlacementIntentRecord {
                id: iid,
                provider_id: pid,
                consumer_id: cid,
                resources,
            });
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(PlacementReconcileRecord {
            orphaned_allocations,
            abandoned_intents,
        })
    }

    async fn import_provider(&self, provider: &PlacementProviderRecord) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        sqlx::query(
            "INSERT INTO placement_providers (id, node_id, state, generation)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE
             SET node_id = EXCLUDED.node_id, state = EXCLUDED.state, generation = EXCLUDED.generation",
        )
        .bind(&provider.id)
        .bind(&provider.node_id)
        .bind(&provider.state)
        .bind(provider.generation as i64)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        for inv in &provider.inventories {
            sqlx::query(
                "INSERT INTO placement_inventories (provider_id, resource_class, total, reserved, allocation_ratio, used)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (provider_id, resource_class) DO UPDATE
                 SET total = EXCLUDED.total, reserved = EXCLUDED.reserved, allocation_ratio = EXCLUDED.allocation_ratio, used = EXCLUDED.used",
            )
            .bind(&provider.id)
            .bind(&inv.resource_class)
            .bind(inv.total as i64)
            .bind(inv.reserved as i64)
            .bind(inv.allocation_ratio)
            .bind(inv.used as i64)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(())
    }
}

impl PostgresStore {
    pub async fn insert_security_group(
        &self,
        group: &SecurityGroupRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO network_security_groups (id, project_id, name, description) VALUES ($1, $2, $3, $4)").bind(group.id.to_string()).bind(&group.project_id).bind(&group.name).bind(&group.description).execute(&self.pool).await.map(|_| ()).map_err(map_pg_error)
    }
    pub async fn list_security_groups(
        &self,
        project_id: &str,
    ) -> Result<Vec<SecurityGroupRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, project_id, name, description FROM network_security_groups WHERE project_id = $1 ORDER BY id").bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(pg_security_group_from_row).collect()
    }
    pub async fn get_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRecord>, StoreError> {
        let row = sqlx::query("SELECT id, project_id, name, description FROM network_security_groups WHERE project_id = $1 AND id = $2").bind(project_id).bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.as_ref().map(pg_security_group_from_row).transpose()
    }
    pub async fn update_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
        description: &str,
    ) -> Result<SecurityGroupRecord, StoreError> {
        let result = sqlx::query("UPDATE network_security_groups SET name = $1, description = $2 WHERE project_id = $3 AND id = $4").bind(name).bind(description).bind(project_id).bind(id.to_string()).execute(&self.pool).await.map_err(map_pg_error)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NetworkNotFound);
        }
        self.get_security_group(project_id, id)
            .await?
            .ok_or(StoreError::NetworkNotFound)
    }
    pub async fn delete_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM network_security_groups WHERE project_id = $1 AND id = $2 AND NOT EXISTS (SELECT 1 FROM network_security_group_rules WHERE security_group_id = $2) AND NOT EXISTS (SELECT 1 FROM network_security_group_bindings WHERE security_group_id = $2)").bind(project_id).bind(id.to_string()).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() != 0 {
            Ok(())
        } else {
            match self.get_security_group(project_id, id).await? {
                Some(_) => Err(StoreError::NetworkInUse),
                None => Err(StoreError::NetworkNotFound),
            }
        }
    }
    pub async fn insert_security_group_rule(
        &self,
        rule: &SecurityGroupRuleRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO network_security_group_rules (id, security_group_id, project_id, direction, protocol, port_min, port_max, remote_ip_prefix) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::inet)").bind(rule.id.to_string()).bind(rule.security_group_id.to_string()).bind(&rule.project_id).bind(&rule.direction).bind(&rule.protocol).bind(rule.port_min.map(i32::from)).bind(rule.port_max.map(i32::from)).bind(&rule.remote_ip_prefix).execute(&self.pool).await.map(|_| ()).map_err(map_pg_error)
    }
    pub async fn list_security_group_rules(
        &self,
        project_id: &str,
        group_id: &Uuid,
    ) -> Result<Vec<SecurityGroupRuleRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, security_group_id, project_id, direction, protocol, port_min, port_max, remote_ip_prefix::text AS remote_ip_prefix FROM network_security_group_rules WHERE project_id = $1 AND security_group_id = $2 ORDER BY id").bind(project_id).bind(group_id.to_string()).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(pg_security_group_rule_from_row).collect()
    }
    pub async fn get_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRuleRecord>, StoreError> {
        let row = sqlx::query("SELECT id, security_group_id, project_id, direction, protocol, port_min, port_max, remote_ip_prefix::text AS remote_ip_prefix FROM network_security_group_rules WHERE project_id = $1 AND id = $2").bind(project_id).bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.as_ref()
            .map(pg_security_group_rule_from_row)
            .transpose()
    }
    pub async fn delete_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM network_security_group_rules WHERE project_id = $1 AND id = $2",
        )
        .bind(project_id)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            Err(StoreError::NetworkNotFound)
        } else {
            Ok(())
        }
    }
    pub async fn list_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: Option<&Uuid>,
    ) -> Result<Vec<SecurityGroupBindingRecord>, StoreError> {
        let (sql, endpoint) = match endpoint_id {
            Some(id) => (
                "SELECT project_id, endpoint_id, security_group_id FROM network_security_group_bindings WHERE project_id = $1 AND endpoint_id = $2 ORDER BY security_group_id",
                Some(id.to_string()),
            ),
            None => (
                "SELECT project_id, endpoint_id, security_group_id FROM network_security_group_bindings WHERE project_id = $1 ORDER BY endpoint_id, security_group_id",
                None,
            ),
        };
        let mut query = sqlx::query(sql).bind(project_id);
        if let Some(endpoint) = endpoint {
            query = query.bind(endpoint);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter()
            .map(pg_security_group_binding_from_row)
            .collect()
    }
    pub async fn replace_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
        group_ids: &[Uuid],
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("DELETE FROM network_security_group_bindings WHERE project_id = $1 AND endpoint_id = $2").bind(project_id).bind(endpoint_id.to_string()).execute(&mut *tx).await.map_err(StoreError::Database)?;
        for group_id in group_ids {
            sqlx::query("INSERT INTO network_security_group_bindings (project_id, endpoint_id, security_group_id) VALUES ($1, $2, $3)").bind(project_id).bind(endpoint_id.to_string()).bind(group_id.to_string()).execute(&mut *tx).await.map_err(map_pg_error)?;
        }
        tx.commit().await.map_err(StoreError::Database)
    }

    async fn load_placement_inventories(
        &self,
        provider_id: &str,
    ) -> Result<Vec<PlacementInventoryRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT resource_class, total, reserved, allocation_ratio, used FROM placement_inventories WHERE provider_id = $1 ORDER BY resource_class",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| PlacementInventoryRecord {
                resource_class: r.get("resource_class"),
                total: r.get::<i64, _>("total") as u64,
                reserved: r.get::<i64, _>("reserved") as u64,
                allocation_ratio: r.get::<f64, _>("allocation_ratio"),
                used: r.get::<i64, _>("used") as u64,
            })
            .collect())
    }

    async fn load_placement_allocations(
        &self,
        provider_id: &str,
    ) -> Result<Vec<PlacementAllocationRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, provider_id, consumer_id FROM placement_allocations WHERE provider_id = $1 ORDER BY id",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut allocations = Vec::new();
        for row in rows {
            let aid: String = row.get("id");
            let resources = self.load_allocation_resources(&aid).await?;
            allocations.push(PlacementAllocationRecord {
                id: aid,
                provider_id: row.get("provider_id"),
                consumer_id: row.get("consumer_id"),
                resources,
            });
        }
        Ok(allocations)
    }

    async fn load_allocation_resources(
        &self,
        allocation_id: &str,
    ) -> Result<Vec<PlacementResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT resource_class, amount FROM placement_allocation_resources WHERE allocation_id = $1 ORDER BY resource_class",
        )
        .bind(allocation_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| PlacementResourceRecord {
                resource_class: r.get("resource_class"),
                amount: r.get::<i64, _>("amount") as u64,
            })
            .collect())
    }

    async fn load_intent_resources(
        &self,
        intent_id: &str,
    ) -> Result<Vec<PlacementResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT resource_class, amount FROM placement_allocation_intent_resources WHERE intent_id = $1 ORDER BY resource_class",
        )
        .bind(intent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| PlacementResourceRecord {
                resource_class: r.get("resource_class"),
                amount: r.get::<i64, _>("amount") as u64,
            })
            .collect())
    }
}

fn amounts_match(a: &[ResourceAmount], b: &[ResourceAmount]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_sorted: Vec<(&LimitKey, u64)> =
        a.iter().map(|item| (&item.key, item.amount)).collect();
    let mut b_sorted: Vec<(&LimitKey, u64)> =
        b.iter().map(|item| (&item.key, item.amount)).collect();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

#[async_trait]
impl QuotaRepository for PostgresStore {
    async fn get_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<LimitValue, StoreError> {
        if !key.is_known() {
            return Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{key}'"
            )));
        }

        let row = sqlx::query(
            "SELECT limit_value FROM quota_limits WHERE scope_id = $1 AND scope_kind = $2 AND namespace = $3 AND resource = $4",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        match row {
            Some(row) => {
                let val: Option<i64> = row.get(0);
                match val {
                    None => Ok(LimitValue::Unlimited),
                    Some(max) if max >= 0 => Ok(LimitValue::Maximum(max as u64)),
                    Some(neg) => Err(StoreError::Corrupt(format!(
                        "malformed negative limit value {neg} for '{key}' in durable storage"
                    ))),
                }
            }
            None => Ok(LimitValue::Unlimited),
        }
    }

    async fn set_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
    ) -> Result<(), StoreError> {
        if !key.is_known() {
            return Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{key}'"
            )));
        }

        let limit_val: Option<i64> = match limit {
            LimitValue::Unlimited => None,
            LimitValue::Maximum(max) => {
                let val = i64::try_from(max).map_err(|_| {
                    StoreError::Corrupt(format!(
                        "limit maximum {max} for '{key}' exceeds maximum supported signed 64-bit integer"
                    ))
                })?;
                Some(val)
            }
        };

        sqlx::query(
            "INSERT INTO quota_limits (scope_id, scope_kind, namespace, resource, limit_value)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (scope_id, scope_kind, namespace, resource) DO UPDATE SET limit_value = EXCLUDED.limit_value",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .bind(limit_val)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(())
    }

    async fn get_usage(&self, scope: &OwnershipScope, key: &LimitKey) -> Result<Usage, StoreError> {
        if !key.is_known() {
            return Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{key}'"
            )));
        }

        let in_use = self.query_pg_in_use_usage(scope, key).await?;
        let reserved = self.query_pg_reserved_usage(scope, key, None).await?;
        Ok(Usage::new(scope.clone(), key.clone(), in_use, reserved))
    }

    async fn reserve_quota(
        &self,
        scope: &OwnershipScope,
        operation_id: &str,
        amounts: &[ResourceAmount],
    ) -> Result<Reservation, StoreError> {
        for amount in amounts {
            if !amount.key.is_known() {
                return Err(StoreError::Corrupt(format!(
                    "unknown or unregistered limit key '{}'",
                    amount.key
                )));
            }
        }

        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;

        // Acquire advisory transaction lock for this tenant scope to serialize concurrent quota reservations
        let lock_key = format!("quota:{}:{}", scope.kind().as_str(), scope.id().as_str());
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&lock_key)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

        // Check if a reservation already exists for this operation_id
        let existing = sqlx::query(
            "SELECT id, scope_id, scope_kind, state, created_at FROM quota_reservations WHERE operation_id = $1 FOR UPDATE",
        )
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        if let Some(row) = existing {
            let res_id_str: String = row.get("id");
            let state_str: String = row.get("state");
            let created_at: String = row.get("created_at");
            let res_id = ReservationId::parse(res_id_str.clone())
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;

            let amt_rows = sqlx::query(
                "SELECT namespace, resource, amount FROM quota_reservation_amounts WHERE reservation_id = $1",
            )
            .bind(&res_id_str)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::Database)?;

            let mut existing_amounts = Vec::new();
            for r in amt_rows {
                let ns: String = r.get("namespace");
                let res: String = r.get("resource");
                let amt: i64 = r.get("amount");
                let k = LimitKey::new(&ns, &res).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                existing_amounts.push(ResourceAmount::new_unchecked(k, amt as u64));
            }

            if state_str == "released" {
                return Err(StoreError::ReservationConflict(format!(
                    "operation '{operation_id}' has already been released and cannot be re-reserved"
                )));
            }

            if amounts_match(&existing_amounts, amounts) {
                tx.commit().await.map_err(StoreError::Database)?;
                let st = match state_str.as_str() {
                    "committed" => ReservationState::Committed,
                    "released" => ReservationState::Released,
                    _ => ReservationState::Pending,
                };
                return Ok(Reservation {
                    id: res_id,
                    scope: scope.clone(),
                    operation_id: operation_id.to_owned(),
                    amounts: existing_amounts,
                    state: st,
                    created_at,
                });
            }
            return Err(StoreError::ReservationConflict(operation_id.to_owned()));
        }

        // Evaluate limit headroom for each requested amount inside transaction
        for amount in amounts {
            let limit = Self::get_limit_tx(&mut tx, scope, &amount.key).await?;
            if let LimitValue::Maximum(max) = limit {
                let in_use = Self::query_pg_in_use_usage_tx(&mut tx, scope, &amount.key).await?;
                let reserved = Self::query_pg_reserved_usage_tx(
                    &mut tx,
                    scope,
                    &amount.key,
                    Some(operation_id),
                )
                .await?;
                let current_consumed = in_use + reserved;
                if current_consumed + amount.amount > max {
                    return Err(StoreError::QuotaExceeded {
                        key: amount.key.clone(),
                        limit,
                        used: current_consumed,
                        requested: amount.amount,
                    });
                }
            }
        }

        let res = Reservation::new(scope.clone(), operation_id.to_owned(), amounts.to_vec());

        sqlx::query(
            "INSERT INTO quota_reservations (id, scope_id, scope_kind, operation_id, state, created_at)
             VALUES ($1, $2, $3, $4, 'pending', $5)",
        )
        .bind(res.id.as_str())
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(operation_id)
        .bind(&res.created_at)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Database)?;

        for amount in amounts {
            let amt = i64::try_from(amount.amount).map_err(|_| {
                StoreError::Corrupt(format!(
                    "reservation amount {} for '{}' exceeds maximum supported signed 64-bit integer",
                    amount.amount, amount.key
                ))
            })?;

            sqlx::query(
                "INSERT INTO quota_reservation_amounts (reservation_id, namespace, resource, amount)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(res.id.as_str())
            .bind(amount.key.namespace().as_str())
            .bind(amount.key.resource())
            .bind(amt)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Database)?;
        }

        tx.commit().await.map_err(StoreError::Database)?;
        Ok(res)
    }

    async fn commit_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        let res = sqlx::query(
            "UPDATE quota_reservations SET state = 'committed' WHERE id = $1 AND state = 'pending'",
        )
        .bind(reservation_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            let exists = sqlx::query("SELECT state FROM quota_reservations WHERE id = $1")
                .bind(reservation_id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;
            if exists.is_none() {
                return Err(StoreError::ReservationNotFound);
            }
        }
        Ok(())
    }

    async fn release_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        let res = sqlx::query(
            "UPDATE quota_reservations SET state = 'released' WHERE id = $1 AND state != 'released'",
        )
        .bind(reservation_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            let exists = sqlx::query("SELECT state FROM quota_reservations WHERE id = $1")
                .bind(reservation_id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;
            if exists.is_none() {
                return Err(StoreError::ReservationNotFound);
            }
        }
        Ok(())
    }

    async fn release_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE quota_reservations SET state = 'released' WHERE operation_id = $1 AND state != 'released'",
        )
        .bind(operation_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(())
    }

    async fn get_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<Reservation>, StoreError> {
        let row = sqlx::query(
            "SELECT id, scope_id, scope_kind, state, created_at FROM quota_reservations WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let Some(row) = row else {
            return Ok(None);
        };

        let res_id_str: String = row.get("id");
        let scope_id: String = row.get("scope_id");
        let scope_kind: String = row.get("scope_kind");
        let state_str: String = row.get("state");
        let created_at: String = row.get("created_at");

        let sk = match scope_kind.as_str() {
            "domain" => ScopeKind::Domain,
            "system" => ScopeKind::System,
            _ => ScopeKind::Project,
        };
        let scope = OwnershipScope::new(ScopeId::new_unchecked(scope_id), sk, None, None);
        let reservation_id = ReservationId::parse(res_id_str.clone())
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;

        let amt_rows = sqlx::query(
            "SELECT namespace, resource, amount FROM quota_reservation_amounts WHERE reservation_id = $1",
        )
        .bind(&res_id_str)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        let mut amounts = Vec::new();
        for r in amt_rows {
            let ns: String = r.get("namespace");
            let res: String = r.get("resource");
            let amt: i64 = r.get("amount");
            let key = LimitKey::new(&ns, &res).map_err(|e| StoreError::Corrupt(e.to_string()))?;
            amounts.push(ResourceAmount::new_unchecked(key, amt as u64));
        }

        let st = match state_str.as_str() {
            "committed" => ReservationState::Committed,
            "released" => ReservationState::Released,
            _ => ReservationState::Pending,
        };

        Ok(Some(Reservation {
            id: reservation_id,
            scope,
            operation_id: operation_id.to_owned(),
            amounts,
            state: st,
            created_at,
        }))
    }
}

impl PostgresStore {
    async fn get_limit_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<LimitValue, StoreError> {
        if !key.is_known() {
            return Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{key}'"
            )));
        }

        let row = sqlx::query(
            "SELECT limit_value FROM quota_limits WHERE scope_id = $1 AND scope_kind = $2 AND namespace = $3 AND resource = $4",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .fetch_optional(&mut **tx)
        .await
        .map_err(StoreError::Database)?;

        match row {
            Some(row) => {
                let val: Option<i64> = row.get(0);
                match val {
                    None => Ok(LimitValue::Unlimited),
                    Some(max) if max >= 0 => Ok(LimitValue::Maximum(max as u64)),
                    Some(neg) => Err(StoreError::Corrupt(format!(
                        "malformed negative limit value {neg} for '{key}' in durable storage"
                    ))),
                }
            }
            None => Ok(LimitValue::Unlimited),
        }
    }

    async fn query_pg_in_use_usage_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<u64, StoreError> {
        let ns = key.namespace().as_str();
        let res = key.resource();
        let scope_id = scope.id().as_str();

        match (ns, res) {
            ("compute", "servers") => {
                let row = sqlx::query(
                    "SELECT COUNT(*)::BIGINT FROM resources WHERE project_id = $1 AND kind = 'compute_instance' AND UPPER(observed_state) != 'DELETED'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let count: i64 = row.get(0);
                parse_pg_non_negative_u64(count, "compute:servers count")
            }
            ("compute", "vcpus") => {
                let row = sqlx::query(
                    "SELECT COALESCE(SUM(r.amount), 0)::BIGINT FROM placement_allocations a
                     JOIN placement_allocation_resources r ON a.id = r.allocation_id
                     JOIN resources res ON a.consumer_id = res.id
                     WHERE res.project_id = $1 AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'VCPU'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let sum: i64 = row.get(0);
                parse_pg_non_negative_u64(sum, "compute:vcpus sum")
            }
            ("compute", "memory_mb") => {
                let row = sqlx::query(
                    "SELECT COALESCE(SUM(r.amount), 0)::BIGINT FROM placement_allocations a
                     JOIN placement_allocation_resources r ON a.id = r.allocation_id
                     JOIN resources res ON a.consumer_id = res.id
                     WHERE res.project_id = $1 AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'MEMORY_MB'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let sum: i64 = row.get(0);
                parse_pg_non_negative_u64(sum, "compute:memory_mb sum")
            }
            ("compute", "disk_gb") => {
                let row = sqlx::query(
                    "SELECT COALESCE(SUM(r.amount), 0)::BIGINT FROM placement_allocations a
                     JOIN placement_allocation_resources r ON a.id = r.allocation_id
                     JOIN resources res ON a.consumer_id = res.id
                     WHERE res.project_id = $1 AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'DISK_GB'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let sum: i64 = row.get(0);
                parse_pg_non_negative_u64(sum, "compute:disk_gb sum")
            }
            ("image", "images") => {
                let row = sqlx::query(
                    "SELECT COUNT(*)::BIGINT FROM image_metadata WHERE project_id = $1 AND status != 'deleted'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let count: i64 = row.get(0);
                parse_pg_non_negative_u64(count, "image:images count")
            }
            ("image", "bytes") => {
                let row = sqlx::query(
                    "SELECT COALESCE(SUM(size), 0)::BIGINT FROM image_metadata WHERE project_id = $1 AND status = 'active'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let sum: i64 = row.get(0);
                parse_pg_non_negative_u64(sum, "image:bytes sum")
            }
            ("network", "networks") => {
                let row = sqlx::query(
                    "SELECT COUNT(*)::BIGINT FROM network_networks WHERE project_id = $1 AND status != 'deleted'",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let count: i64 = row.get(0);
                parse_pg_non_negative_u64(count, "network:networks count")
            }
            ("network", "subnets") => {
                let row = sqlx::query(
                    "SELECT COUNT(*)::BIGINT FROM network_subnets WHERE project_id = $1",
                )
                .bind(scope_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(StoreError::Database)?;
                let count: i64 = row.get(0);
                parse_pg_non_negative_u64(count, "network:subnets count")
            }
            ("network", "ports") => {
                let row =
                    sqlx::query("SELECT COUNT(*)::BIGINT FROM network_ports WHERE project_id = $1")
                        .bind(scope_id)
                        .fetch_one(&mut **tx)
                        .await
                        .map_err(StoreError::Database)?;
                let count: i64 = row.get(0);
                parse_pg_non_negative_u64(count, "network:ports count")
            }
            _ => Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{key}'"
            ))),
        }
    }

    async fn query_pg_reserved_usage_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &OwnershipScope,
        key: &LimitKey,
        exclude_op: Option<&str>,
    ) -> Result<u64, StoreError> {
        let row = if let Some(op) = exclude_op {
            sqlx::query(
                "SELECT COALESCE(SUM(a.amount), 0)::BIGINT FROM quota_reservations r
                 JOIN quota_reservation_amounts a ON r.id = a.reservation_id
                 WHERE r.scope_id = $1 AND r.scope_kind = $2 AND r.state = 'pending' AND a.namespace = $3 AND a.resource = $4 AND r.operation_id != $5",
            )
            .bind(scope.id().as_str())
            .bind(scope.kind().as_str())
            .bind(key.namespace().as_str())
            .bind(key.resource())
            .bind(op)
            .fetch_one(&mut **tx)
            .await
            .map_err(StoreError::Database)?
        } else {
            sqlx::query(
                "SELECT COALESCE(SUM(a.amount), 0)::BIGINT FROM quota_reservations r
                 JOIN quota_reservation_amounts a ON r.id = a.reservation_id
                 WHERE r.scope_id = $1 AND r.scope_kind = $2 AND r.state = 'pending' AND a.namespace = $3 AND a.resource = $4",
            )
            .bind(scope.id().as_str())
            .bind(scope.kind().as_str())
            .bind(key.namespace().as_str())
            .bind(key.resource())
            .fetch_one(&mut **tx)
            .await
            .map_err(StoreError::Database)?
        };

        let sum: i64 = row.get(0);
        parse_pg_non_negative_u64(sum, "reserved amounts sum")
    }

    async fn query_pg_in_use_usage(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let res = Self::query_pg_in_use_usage_tx(&mut tx, scope, key).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(res)
    }

    async fn query_pg_reserved_usage(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
        exclude_op: Option<&str>,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::Database)?;
        let res = Self::query_pg_reserved_usage_tx(&mut tx, scope, key, exclude_op).await?;
        tx.commit().await.map_err(StoreError::Database)?;
        Ok(res)
    }
}

fn parse_pg_non_negative_u64(val: i64, context: &str) -> Result<u64, StoreError> {
    if val < 0 {
        return Err(StoreError::Corrupt(format!(
            "malformed negative count/amount {val} for {context} in durable storage"
        )));
    }
    u64::try_from(val).map_err(|_| {
        StoreError::Corrupt(format!(
            "count/amount {val} for {context} exceeds maximum supported 64-bit integer"
        ))
    })
}

#[async_trait]
impl ComputeRepository for PostgresStore {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM resources WHERE kind = $1 AND UPPER(observed_state) != 'DELETED' ORDER BY created_at ASC",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        rows.iter().map(row_to_resource).collect()
    }
}
