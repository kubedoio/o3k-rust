use async_trait::async_trait;
use o3k_kernel::{
    LimitKey, LimitValue, OwnershipScope, Reservation, ReservationId, ReservationState,
    ResourceAmount, ScopeId, ScopeKind, Usage,
};
use sqlx::Row;

use crate::{StoreError, quota::QuotaRepository};

use super::{
    PostgresStore,
    helpers::{amounts_match, parse_pg_non_negative_u64},
};

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
