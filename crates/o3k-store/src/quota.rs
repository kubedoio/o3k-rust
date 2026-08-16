use async_trait::async_trait;
use o3k_kernel::{
    LimitKey, LimitValue, OwnershipScope, Reservation, ReservationId, ReservationState,
    ResourceAmount, ScopeId, ScopeKind, Usage,
};
use sqlx::{Row, SqliteConnection};

use crate::{SqliteStore, StoreError};

/// Narrow repository port for durable resource governance, limits, usage, and reservations.
#[async_trait]
pub trait QuotaRepository: Send + Sync {
    /// Look up the configured limit for a given scope and key (defaults to `Unlimited`).
    async fn get_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<LimitValue, StoreError>;

    /// Set a configured limit ceiling for a given scope and key.
    async fn set_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
    ) -> Result<(), StoreError>;

    /// Observe durable usage (committed in-use + active pending reservations) for a scope and key.
    async fn get_usage(&self, scope: &OwnershipScope, key: &LimitKey) -> Result<Usage, StoreError>;

    /// Atomically evaluate limit ceilings and create a pending reservation correlated with `operation_id`.
    ///
    /// - If a pending reservation already exists for the same `(scope, operation_id)`:
    ///   - If requested amounts match exactly: returns `Ok(existing)` (idempotent retry).
    ///   - If requested amounts differ: returns `Err(StoreError::ReservationConflict)`.
    /// - If requested amounts exceed configured limit: returns `Err(StoreError::QuotaExceeded)`.
    async fn reserve_quota(
        &self,
        scope: &OwnershipScope,
        operation_id: &str,
        amounts: &[ResourceAmount],
    ) -> Result<Reservation, StoreError>;

    /// Commit a pending reservation into committed state once the operation succeeds.
    async fn commit_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError>;

    /// Release a reservation once the operation terminally fails or is compensated.
    async fn release_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError>;

    /// Release a reservation correlated by operation_id.
    async fn release_reservation_for_operation(&self, operation_id: &str)
    -> Result<(), StoreError>;

    /// Look up an existing reservation by operation_id.
    async fn get_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<Reservation>, StoreError>;
}

#[async_trait]
impl QuotaRepository for SqliteStore {
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
            "SELECT limit_value FROM quota_limits WHERE scope_id = ? AND scope_kind = ? AND namespace = ? AND resource = ?",
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
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(scope_id, scope_kind, namespace, resource) DO UPDATE SET limit_value = excluded.limit_value",
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
        let mut conn = self.pool.acquire().await.map_err(StoreError::Database)?;
        let in_use = query_in_use_usage(&mut conn, scope, key).await?;
        let reserved = query_reserved_usage(&mut conn, scope, key, None).await?;
        Ok(Usage::new(scope.clone(), key.clone(), in_use, reserved))
    }

    async fn reserve_quota(
        &self,
        scope: &OwnershipScope,
        operation_id: &str,
        amounts: &[ResourceAmount],
    ) -> Result<Reservation, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::Database)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *conn)
            .await
            .map_err(StoreError::Database)?;

        let result = reserve_quota_inner(&mut conn, scope, operation_id, amounts).await;

        SqliteStore::commit_or_rollback(&mut conn, result).await
    }

    async fn commit_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        let res = sqlx::query(
            "UPDATE quota_reservations SET state = 'committed' WHERE id = ? AND state = 'pending'",
        )
        .bind(reservation_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            // Check if already committed (idempotent)
            let exists = sqlx::query("SELECT state FROM quota_reservations WHERE id = ?")
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
            "UPDATE quota_reservations SET state = 'released' WHERE id = ? AND state != 'released'",
        )
        .bind(reservation_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        if res.rows_affected() == 0 {
            let exists = sqlx::query("SELECT state FROM quota_reservations WHERE id = ?")
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
            "UPDATE quota_reservations SET state = 'released' WHERE operation_id = ? AND state != 'released'",
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
        let mut conn = self.pool.acquire().await.map_err(StoreError::Database)?;
        query_reservation_by_op(&mut conn, operation_id).await
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

async fn reserve_quota_inner(
    conn: &mut SqliteConnection,
    scope: &OwnershipScope,
    operation_id: &str,
    amounts: &[ResourceAmount],
) -> Result<Reservation, StoreError> {
    // 0. Validate dimensions and safe bounds; reject duplicate limit keys
    let mut seen_keys = std::collections::BTreeSet::new();
    for req in amounts {
        if !req.key.is_known() {
            return Err(StoreError::Corrupt(format!(
                "unknown or unregistered limit key '{}' in reservation request",
                req.key
            )));
        }
        i64::try_from(req.amount).map_err(|_| {
            StoreError::Corrupt(format!(
                "reservation amount {} for '{}' exceeds maximum supported signed 64-bit integer",
                req.amount, req.key
            ))
        })?;
        if !seen_keys.insert(&req.key) {
            return Err(StoreError::Corrupt(format!(
                "duplicate limit key '{}' in reservation request",
                req.key
            )));
        }
    }

    // 1. Check for existing reservation by operation_id (idempotency check)
    let existing = query_reservation_by_op(&mut *conn, operation_id).await?;
    if let Some(res) = existing {
        if res.state == ReservationState::Released {
            return Err(StoreError::ReservationConflict(format!(
                "operation '{operation_id}' has already been released and cannot be re-reserved"
            )));
        } else if res.scope.id() == scope.id()
            && res.scope.kind() == scope.kind()
            && amounts_match(&res.amounts, amounts)
        {
            return Ok(res);
        } else {
            return Err(StoreError::ReservationConflict(operation_id.to_owned()));
        }
    }

    // 2. For each requested dimension, check limit against (in_use + reserved + requested)
    for req in amounts {
        let limit = query_limit_in_tx(&mut *conn, scope, &req.key).await?;
        if let LimitValue::Maximum(max) = limit {
            let in_use = query_in_use_usage(&mut *conn, scope, &req.key).await?;
            let reserved = query_reserved_usage(&mut *conn, scope, &req.key, None).await?;
            let total = in_use.saturating_add(reserved).saturating_add(req.amount);
            if total > max {
                return Err(StoreError::QuotaExceeded {
                    key: req.key.clone(),
                    limit,
                    used: in_use.saturating_add(reserved),
                    requested: req.amount,
                });
            }
        }
    }

    // 3. Create the new pending reservation
    let res = Reservation::new(scope.clone(), operation_id.to_owned(), amounts.to_vec());

    sqlx::query(
        "INSERT INTO quota_reservations (id, scope_id, scope_kind, operation_id, state, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(res.id.as_str())
    .bind(scope.id().as_str())
    .bind(scope.kind().as_str())
    .bind(operation_id)
    .bind(res.state.to_string())
    .bind(&res.created_at)
    .execute(&mut *conn)
    .await
    .map_err(StoreError::Database)?;

    for amt in amounts {
        let amount_i64 = i64::try_from(amt.amount).map_err(|_| {
            StoreError::Corrupt(format!(
                "reservation amount {} for '{}' exceeds maximum supported signed 64-bit integer",
                amt.amount, amt.key
            ))
        })?;
        sqlx::query(
            "INSERT INTO quota_reservation_amounts (reservation_id, namespace, resource, amount)
             VALUES (?, ?, ?, ?)",
        )
        .bind(res.id.as_str())
        .bind(amt.key.namespace().as_str())
        .bind(amt.key.resource())
        .bind(amount_i64)
        .execute(&mut *conn)
        .await
        .map_err(StoreError::Database)?;
    }

    Ok(res)
}

async fn query_limit_in_tx(
    tx: &mut SqliteConnection,
    scope: &OwnershipScope,
    key: &LimitKey,
) -> Result<LimitValue, StoreError> {
    if !key.is_known() {
        return Err(StoreError::Corrupt(format!(
            "unknown or unregistered limit key '{key}'"
        )));
    }
    let row = sqlx::query(
        "SELECT limit_value FROM quota_limits WHERE scope_id = ? AND scope_kind = ? AND namespace = ? AND resource = ?",
    )
    .bind(scope.id().as_str())
    .bind(scope.kind().as_str())
    .bind(key.namespace().as_str())
    .bind(key.resource())
    .fetch_optional(tx)
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

fn parse_non_negative_u64(val: i64, context: &str) -> Result<u64, StoreError> {
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

async fn query_in_use_usage(
    conn: &mut SqliteConnection,
    scope: &OwnershipScope,
    key: &LimitKey,
) -> Result<u64, StoreError> {
    let ns = key.namespace().as_str();
    let res = key.resource();
    let scope_id = scope.id().as_str();

    match (ns, res) {
        ("compute", "servers") => {
            let row = sqlx::query(
                "SELECT COUNT(*) FROM resources WHERE project_id = ? AND kind = 'compute_instance' AND UPPER(observed_state) != 'DELETED'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let count: i64 = row.get(0);
            parse_non_negative_u64(count, "compute:servers count")
        }
        ("compute", "vcpus") => {
            // Look up from placement allocation resources for server instances
            let row = sqlx::query(
                "SELECT COALESCE(SUM(r.amount), 0) FROM placement_allocations a
                 JOIN placement_allocation_resources r ON a.id = r.allocation_id
                 JOIN resources res ON a.consumer_id = res.id
                 WHERE res.project_id = ? AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'VCPU'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let sum: i64 = row.get(0);
            parse_non_negative_u64(sum, "compute:vcpus sum")
        }
        ("compute", "memory_mb") => {
            let row = sqlx::query(
                "SELECT COALESCE(SUM(r.amount), 0) FROM placement_allocations a
                 JOIN placement_allocation_resources r ON a.id = r.allocation_id
                 JOIN resources res ON a.consumer_id = res.id
                 WHERE res.project_id = ? AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'MEMORY_MB'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let sum: i64 = row.get(0);
            parse_non_negative_u64(sum, "compute:memory_mb sum")
        }
        ("compute", "disk_gb") => {
            let row = sqlx::query(
                "SELECT COALESCE(SUM(r.amount), 0) FROM placement_allocations a
                 JOIN placement_allocation_resources r ON a.id = r.allocation_id
                 JOIN resources res ON a.consumer_id = res.id
                 WHERE res.project_id = ? AND res.kind = 'compute_instance' AND UPPER(res.observed_state) != 'DELETED' AND r.resource_class = 'DISK_GB'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let sum: i64 = row.get(0);
            parse_non_negative_u64(sum, "compute:disk_gb sum")
        }
        ("image", "images") => {
            let row = sqlx::query(
                "SELECT COUNT(*) FROM image_metadata WHERE project_id = ? AND status != 'deleted'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let count: i64 = row.get(0);
            parse_non_negative_u64(count, "image:images count")
        }
        ("image", "bytes") => {
            let row = sqlx::query(
                "SELECT COALESCE(SUM(size), 0) FROM image_metadata WHERE project_id = ? AND status = 'active'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let sum: i64 = row.get(0);
            parse_non_negative_u64(sum, "image:bytes sum")
        }
        ("network", "networks") => {
            let row = sqlx::query(
                "SELECT COUNT(*) FROM network_networks WHERE project_id = ? AND status != 'deleted'",
            )
            .bind(scope_id)
            .fetch_one(conn)
            .await
            .map_err(StoreError::Database)?;
            let count: i64 = row.get(0);
            parse_non_negative_u64(count, "network:networks count")
        }
        ("network", "subnets") => {
            let row = sqlx::query("SELECT COUNT(*) FROM network_subnets WHERE project_id = ?")
                .bind(scope_id)
                .fetch_one(conn)
                .await
                .map_err(StoreError::Database)?;
            let count: i64 = row.get(0);
            parse_non_negative_u64(count, "network:subnets count")
        }
        ("network", "ports") => {
            let row = sqlx::query("SELECT COUNT(*) FROM network_ports WHERE project_id = ?")
                .bind(scope_id)
                .fetch_one(conn)
                .await
                .map_err(StoreError::Database)?;
            let count: i64 = row.get(0);
            parse_non_negative_u64(count, "network:ports count")
        }
        _ => Err(StoreError::Corrupt(format!(
            "unknown or unregistered limit key '{key}'"
        ))),
    }
}

async fn query_reserved_usage(
    conn: &mut SqliteConnection,
    scope: &OwnershipScope,
    key: &LimitKey,
    exclude_op: Option<&str>,
) -> Result<u64, StoreError> {
    let query_str = if let Some(op) = exclude_op {
        sqlx::query(
            "SELECT COALESCE(SUM(a.amount), 0) FROM quota_reservations r
             JOIN quota_reservation_amounts a ON r.id = a.reservation_id
             WHERE r.scope_id = ? AND r.scope_kind = ? AND r.state = 'pending' AND a.namespace = ? AND a.resource = ? AND r.operation_id != ?",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .bind(op)
    } else {
        sqlx::query(
            "SELECT COALESCE(SUM(a.amount), 0) FROM quota_reservations r
             JOIN quota_reservation_amounts a ON r.id = a.reservation_id
             WHERE r.scope_id = ? AND r.scope_kind = ? AND r.state = 'pending' AND a.namespace = ? AND a.resource = ?",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
    };

    let row = query_str
        .fetch_one(conn)
        .await
        .map_err(StoreError::Database)?;
    let sum: i64 = row.get(0);
    parse_non_negative_u64(sum, "reserved amounts sum")
}

async fn query_reservation_by_op(
    conn: &mut SqliteConnection,
    operation_id: &str,
) -> Result<Option<Reservation>, StoreError> {
    let res_row = sqlx::query(
        "SELECT id, scope_id, scope_kind, operation_id, state, created_at FROM quota_reservations WHERE operation_id = ?",
    )
    .bind(operation_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(StoreError::Database)?;

    let row = match res_row {
        Some(r) => r,
        None => return Ok(None),
    };

    let res_id_str: String = row.get(0);
    let scope_id: String = row.get(1);
    let scope_kind_str: String = row.get(2);
    let op_id: String = row.get(3);
    let state_str: String = row.get(4);
    let created_at: String = row.get(5);

    let scope_kind = match scope_kind_str.as_str() {
        "domain" => ScopeKind::Domain,
        "system" => ScopeKind::System,
        _ => ScopeKind::Project,
    };
    let scope = OwnershipScope::new(ScopeId::new_unchecked(scope_id), scope_kind, None, None);

    let state = match state_str.as_str() {
        "committed" => ReservationState::Committed,
        "released" => ReservationState::Released,
        _ => ReservationState::Pending,
    };

    let amounts_rows = sqlx::query(
        "SELECT namespace, resource, amount FROM quota_reservation_amounts WHERE reservation_id = ?",
    )
    .bind(&res_id_str)
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::Database)?;

    let mut amounts = Vec::with_capacity(amounts_rows.len());
    for a in amounts_rows {
        let ns: String = a.get(0);
        let res: String = a.get(1);
        let amt: i64 = a.get(2);
        if amt < 0 {
            return Err(StoreError::Corrupt(format!(
                "malformed negative reservation amount {amt} for '{ns}:{res}' in durable storage"
            )));
        }
        let key = LimitKey::new(&ns, &res).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        amounts.push(ResourceAmount::new_unchecked(key, amt as u64));
    }

    let id = ReservationId::parse(res_id_str).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(Some(Reservation {
        id,
        scope,
        operation_id: op_id,
        amounts,
        state,
        created_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_kernel::ScopeId;

    #[tokio::test]
    async fn quota_repository_lifecycle_and_enforcement() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("tenant-a"), None, None);
        let scope_b = OwnershipScope::project(ScopeId::new_unchecked("tenant-b"), None, None);
        let key = LimitKey::compute_servers();

        // 1. Defaults to Unlimited
        assert_eq!(
            store.get_limit(&scope_a, &key).await?,
            LimitValue::Unlimited
        );

        // 2. Set limit for Tenant A to 1 server
        store
            .set_limit(&scope_a, &key, LimitValue::Maximum(1))
            .await?;
        assert_eq!(
            store.get_limit(&scope_a, &key).await?,
            LimitValue::Maximum(1)
        );

        // Tenant B remains Unlimited
        assert_eq!(
            store.get_limit(&scope_b, &key).await?,
            LimitValue::Unlimited
        );

        // 3. First reservation for Tenant A succeeds
        let amounts = vec![ResourceAmount::new(key.clone(), 1)];
        let res1 = store.reserve_quota(&scope_a, "op-1", &amounts).await?;
        assert_eq!(res1.state, ReservationState::Pending);

        // 4. Idempotent retry with same operation_id and amounts returns same reservation
        let retry = store.reserve_quota(&scope_a, "op-1", &amounts).await?;
        assert_eq!(retry.id, res1.id);

        // 5. Conflict if amounts differ on same operation_id
        let diff_amounts = vec![ResourceAmount::new(key.clone(), 2)];
        assert!(matches!(
            store.reserve_quota(&scope_a, "op-1", &diff_amounts).await,
            Err(StoreError::ReservationConflict(_))
        ));

        // 6. Second reservation for Tenant A with new op-2 fails with QuotaExceeded
        let err = store.reserve_quota(&scope_a, "op-2", &amounts).await;
        assert!(matches!(err, Err(StoreError::QuotaExceeded { .. })));

        // 7. Tenant B reservation succeeds (isolation)
        let res_b = store.reserve_quota(&scope_b, "op-b1", &amounts).await?;
        assert_eq!(res_b.state, ReservationState::Pending);

        // 8. Releasing Tenant A's op-1 allows op-2 to proceed
        store.release_reservation(&res1.id).await?;
        let res2 = store.reserve_quota(&scope_a, "op-2", &amounts).await?;
        assert_eq!(res2.state, ReservationState::Pending);

        // 9. Committing res2 makes it committed
        store.commit_reservation(&res2.id).await?;
        let stored = store
            .get_reservation_for_operation("op-2")
            .await?
            .ok_or_else(|| StoreError::ReservationNotFound)?;
        assert_eq!(stored.state, ReservationState::Committed);

        Ok(())
    }

    #[tokio::test]
    async fn signed_sqlite_numeric_bounds_fail_closed() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        let scope = OwnershipScope::project(ScopeId::new_unchecked("tenant-bounds"), None, None);
        let key = LimitKey::compute_servers();

        // 1. Valid i64::MAX is accepted
        store
            .set_limit(&scope, &key, LimitValue::Maximum(i64::MAX as u64))
            .await?;
        assert_eq!(
            store.get_limit(&scope, &key).await?,
            LimitValue::Maximum(i64::MAX as u64)
        );

        // 2. Setting limit exceeding i64::MAX fails closed
        assert!(matches!(
            store
                .set_limit(&scope, &key, LimitValue::Maximum((i64::MAX as u64) + 1))
                .await,
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store
                .set_limit(&scope, &key, LimitValue::Maximum(u64::MAX))
                .await,
            Err(StoreError::Corrupt(_))
        ));

        // 3. Reserving amount exceeding i64::MAX fails closed
        let overflow_amounts = vec![ResourceAmount::new_unchecked(
            key.clone(),
            (i64::MAX as u64) + 1,
        )];
        assert!(matches!(
            store
                .reserve_quota(&scope, "op-overflow", &overflow_amounts)
                .await,
            Err(StoreError::Corrupt(_))
        ));

        // 4. Corrupt negative persisted limit fails closed with StoreError::Corrupt
        sqlx::query(
            "INSERT INTO quota_limits (scope_id, scope_kind, namespace, resource, limit_value)
             VALUES (?, ?, ?, ?, -42)
             ON CONFLICT(scope_id, scope_kind, namespace, resource) DO UPDATE SET limit_value = -42",
        )
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .execute(&store.pool)
        .await
        .map_err(StoreError::Database)?;

        assert!(matches!(
            store.get_limit(&scope, &key).await,
            Err(StoreError::Corrupt(_))
        ));

        // 5. Corrupt negative persisted reservation amount fails closed
        let res_id = ReservationId::new();
        sqlx::query(
            "INSERT INTO quota_reservations (id, scope_id, scope_kind, operation_id, state, created_at)
             VALUES (?, ?, ?, 'op-corrupt-amt', 'pending', '2026-08-17T00:00:00Z')",
        )
        .bind(res_id.as_str())
        .bind(scope.id().as_str())
        .bind(scope.kind().as_str())
        .execute(&store.pool)
        .await
        .map_err(StoreError::Database)?;

        sqlx::query(
            "INSERT INTO quota_reservation_amounts (reservation_id, namespace, resource, amount)
             VALUES (?, ?, ?, -100)",
        )
        .bind(res_id.as_str())
        .bind(key.namespace().as_str())
        .bind(key.resource())
        .execute(&store.pool)
        .await
        .map_err(StoreError::Database)?;

        assert!(matches!(
            store.get_reservation_for_operation("op-corrupt-amt").await,
            Err(StoreError::Corrupt(_))
        ));

        Ok(())
    }

    #[tokio::test]
    async fn unknown_keys_and_duplicate_amounts_fail_closed() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        let scope = OwnershipScope::project(ScopeId::new_unchecked("tenant-bad"), None, None);
        let valid_key = LimitKey::compute_servers();
        let unknown_key = LimitKey::new_unchecked(
            o3k_kernel::ServiceNamespace::new_unchecked("compute".to_owned()),
            "servres".to_owned(),
        );

        // 1. Unknown key fails closed on set_limit, get_limit, get_usage
        assert!(matches!(
            store
                .set_limit(&scope, &unknown_key, LimitValue::Maximum(5))
                .await,
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store.get_limit(&scope, &unknown_key).await,
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store.get_usage(&scope, &unknown_key).await,
            Err(StoreError::Corrupt(_))
        ));

        // 2. Duplicate keys in reservation request fail closed
        let dup_amounts = vec![
            ResourceAmount::new_unchecked(valid_key.clone(), 1),
            ResourceAmount::new_unchecked(valid_key.clone(), 2),
        ];
        assert!(matches!(
            store.reserve_quota(&scope, "op-dup", &dup_amounts).await,
            Err(StoreError::Corrupt(_))
        ));

        Ok(())
    }

    #[tokio::test]
    async fn released_reservation_cannot_be_reused_for_same_op() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        let scope = OwnershipScope::project(ScopeId::new_unchecked("tenant-rel"), None, None);
        let key = LimitKey::compute_servers();
        let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 1)];

        // Reserve and release
        let res = store.reserve_quota(&scope, "op-terminal", &amounts).await?;
        store.release_reservation(&res.id).await?;

        // Re-reserving the same operation ID fails closed with conflict
        assert!(matches!(
            store.reserve_quota(&scope, "op-terminal", &amounts).await,
            Err(StoreError::ReservationConflict(_))
        ));

        Ok(())
    }

    #[tokio::test]
    async fn two_tenant_finite_quota_isolation() -> Result<(), StoreError> {
        let store = crate::testkit::open_memory().await?;
        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-a"), None, None);
        let scope_b = OwnershipScope::project(ScopeId::new_unchecked("proj-b"), None, None);
        let key = LimitKey::compute_servers();
        let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 1)];

        // Tenant A: limit = 1, Tenant B: limit = 2
        store
            .set_limit(&scope_a, &key, LimitValue::Maximum(1))
            .await?;
        store
            .set_limit(&scope_b, &key, LimitValue::Maximum(2))
            .await?;

        // Tenant A: 1st succeeds, 2nd fails
        let res_a1 = store.reserve_quota(&scope_a, "op-a1", &amounts).await?;
        assert_eq!(res_a1.state, ReservationState::Pending);
        assert!(matches!(
            store.reserve_quota(&scope_a, "op-a2", &amounts).await,
            Err(StoreError::QuotaExceeded { .. })
        ));

        // Tenant B: 1st and 2nd succeed, 3rd fails
        let res_b1 = store.reserve_quota(&scope_b, "op-b1", &amounts).await?;
        let res_b2 = store.reserve_quota(&scope_b, "op-b2", &amounts).await?;
        assert_eq!(res_b1.state, ReservationState::Pending);
        assert_eq!(res_b2.state, ReservationState::Pending);
        assert!(matches!(
            store.reserve_quota(&scope_b, "op-b3", &amounts).await,
            Err(StoreError::QuotaExceeded { .. })
        ));

        // Tenant A usage is 1, Tenant B usage is 2
        let usage_a = store.get_usage(&scope_a, &key).await?;
        let usage_b = store.get_usage(&scope_b, &key).await?;
        assert_eq!(usage_a.total_consumed(), 1);
        assert_eq!(usage_b.total_consumed(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn concurrent_reservations_cannot_over_allocate() -> Result<(), Box<dyn std::error::Error>>
    {
        for iteration in 0..25 {
            let path = std::path::PathBuf::from(format!(
                "/tmp/o3k-quota-race-{}-{}.sqlite",
                std::process::id(),
                iteration
            ));
            let _ = std::fs::remove_file(&path);
            let store = crate::testkit::open_file(&path).await?;
            let scope = OwnershipScope::project(
                ScopeId::new_unchecked(format!("proj-race-{iteration}")),
                None,
                None,
            );
            let key = LimitKey::compute_servers();
            store
                .set_limit(&scope, &key, LimitValue::Maximum(1))
                .await?;

            let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 1)];

            let store1 = store.clone();
            let store2 = store.clone();
            let scope1 = scope.clone();
            let scope2 = scope.clone();
            let amounts1 = amounts.clone();
            let amounts2 = amounts.clone();

            let task1 =
                tokio::spawn(
                    async move { store1.reserve_quota(&scope1, "op-race-1", &amounts1).await },
                );
            let task2 =
                tokio::spawn(
                    async move { store2.reserve_quota(&scope2, "op-race-2", &amounts2).await },
                );

            let (res1, res2) = tokio::join!(task1, task2);
            let r1 = res1?;
            let r2 = res2?;

            let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&ok| ok).count();
            assert_eq!(
                successes, 1,
                "iteration {iteration}: exactly one concurrent reservation must succeed for limit=1"
            );

            let usage = store.get_usage(&scope, &key).await?;
            assert_eq!(usage.total_consumed(), 1);

            let _ = std::fs::remove_file(&path);
        }

        Ok(())
    }

    #[tokio::test]
    async fn concurrent_port_reservations_cannot_over_allocate()
    -> Result<(), Box<dyn std::error::Error>> {
        for iteration in 0..25 {
            let path = std::path::PathBuf::from(format!(
                "/tmp/o3k-port-quota-race-{}-{}.sqlite",
                std::process::id(),
                iteration
            ));
            let _ = std::fs::remove_file(&path);
            let store = crate::testkit::open_file(&path).await?;
            let scope = OwnershipScope::project(
                ScopeId::new_unchecked(format!("proj-port-race-{iteration}")),
                None,
                None,
            );
            let key = LimitKey::network_ports();
            store
                .set_limit(&scope, &key, LimitValue::Maximum(1))
                .await?;

            let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 1)];

            let store1 = store.clone();
            let store2 = store.clone();
            let scope1 = scope.clone();
            let scope2 = scope.clone();
            let amounts1 = amounts.clone();
            let amounts2 = amounts.clone();

            let task1 =
                tokio::spawn(
                    async move { store1.reserve_quota(&scope1, "op-port-1", &amounts1).await },
                );
            let task2 =
                tokio::spawn(
                    async move { store2.reserve_quota(&scope2, "op-port-2", &amounts2).await },
                );

            let (res1, res2) = tokio::join!(task1, task2);
            let r1 = res1?;
            let r2 = res2?;

            let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&ok| ok).count();
            assert_eq!(
                successes, 1,
                "iteration {iteration}: exactly one concurrent port reservation must succeed for limit=1"
            );

            let usage = store.get_usage(&scope, &key).await?;
            assert_eq!(usage.total_consumed(), 1);

            let _ = std::fs::remove_file(&path);
        }

        Ok(())
    }

    #[tokio::test]
    async fn concurrent_image_bytes_reservations_cannot_over_allocate()
    -> Result<(), Box<dyn std::error::Error>> {
        for iteration in 0..25 {
            let path = std::path::PathBuf::from(format!(
                "/tmp/o3k-image-bytes-race-{}-{}.sqlite",
                std::process::id(),
                iteration
            ));
            let _ = std::fs::remove_file(&path);
            let store = crate::testkit::open_file(&path).await?;
            let scope = OwnershipScope::project(
                ScopeId::new_unchecked(format!("proj-bytes-race-{iteration}")),
                None,
                None,
            );
            let key = LimitKey::image_bytes();
            // 100 MB limit
            store
                .set_limit(&scope, &key, LimitValue::Maximum(100 * 1024 * 1024))
                .await?;

            // Each request is 60 MB (so 2 concurrent requests sum to 120 MB > 100 MB limit)
            let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 60 * 1024 * 1024)];

            let store1 = store.clone();
            let store2 = store.clone();
            let scope1 = scope.clone();
            let scope2 = scope.clone();
            let amounts1 = amounts.clone();
            let amounts2 = amounts.clone();

            let task1 = tokio::spawn(async move {
                store1.reserve_quota(&scope1, "op-bytes-1", &amounts1).await
            });
            let task2 = tokio::spawn(async move {
                store2.reserve_quota(&scope2, "op-bytes-2", &amounts2).await
            });

            let (res1, res2) = tokio::join!(task1, task2);
            let r1 = res1?;
            let r2 = res2?;

            let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&ok| ok).count();
            assert_eq!(
                successes, 1,
                "iteration {iteration}: exactly one overlapping image-byte reservation must succeed"
            );

            let usage = store.get_usage(&scope, &key).await?;
            assert_eq!(usage.total_consumed(), 60 * 1024 * 1024);

            let _ = std::fs::remove_file(&path);
        }

        Ok(())
    }

    #[tokio::test]
    async fn existing_db_migration_defaults_to_unlimited() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = std::path::PathBuf::from(format!(
            "/tmp/o3k-db-migration-test-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        // Open store (which runs all migrations including 0018)
        let store = crate::testkit::open_file(&path).await?;
        let scope = OwnershipScope::project(ScopeId::new_unchecked("legacy-tenant"), None, None);
        let key = LimitKey::compute_servers();

        // Defaults to Unlimited without manual configuration
        assert_eq!(store.get_limit(&scope, &key).await?, LimitValue::Unlimited);

        // Usage and reservations work out-of-the-box
        let amounts = vec![ResourceAmount::new_unchecked(key.clone(), 1)];
        let res = store.reserve_quota(&scope, "op-migrated", &amounts).await?;
        assert_eq!(res.state, ReservationState::Pending);

        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}
