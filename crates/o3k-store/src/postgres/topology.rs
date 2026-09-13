//! PostgreSQL durable topology store (`TopologyStore`).
//!
//! Location/failure-domain validation, nesting ranks, and generation semantics
//! are owned by `o3k_kernel::location`; this module only persists topology and
//! enforces the invariants kernel validation cannot make atomic: uniqueness,
//! referential integrity (FK), mutation serialization (a transaction-level
//! advisory lock), and optimistic-concurrency generations (CAS
//! updates/deletes).
//!
//! Class and target-kind values are stored as their stable kebab-case serde
//! wire representation (see `FailureDomainClass::as_str` /
//! `BindingTargetKind::as_str`); failure-domain metadata is stored as JSON.

use async_trait::async_trait;
use o3k_kernel::{
    AuditEvent, AvailabilityDomain, BindingTarget, BindingTargetKind, FailureDomain,
    FailureDomainClass, KernelError, RegionDeclaration, TopologyBinding, TopologySnapshot,
    TopologyStore,
};
use sqlx::Row;

use super::PostgresStore;

/// Fixed advisory-lock key: serializes topology hierarchy mutations across
/// connections so concurrent writers observe one consistent durable sequence
/// (the kernel port requires store-level mutation serialization).
const ADVISORY_LOCK_KEY: i64 = 72_340_123;

/// Maps any store/driver failure to a secret-free topology-unavailable error.
fn store_err(error: impl std::fmt::Display) -> KernelError {
    KernelError::TopologyUnavailable(error.to_string())
}

/// Builds the conflict error used for every durable-topology invariant
/// violation (FK protection, duplicate identity, stale CAS generation).
fn conflict(reason: impl Into<String>) -> KernelError {
    KernelError::TopologyCorrupt(reason.into())
}

/// SQLSTATE 23505: unique/primary-key violation.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            database.code().as_deref() == Some("23505") || database.is_unique_violation()
        }
        _ => false,
    }
}

/// SQLSTATE 23503: foreign-key violation.
fn is_fk_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => database.code().as_deref() == Some("23503"),
        _ => false,
    }
}

/// Stable kebab-case class name; identical to the serde wire representation.
fn class_wire(class: FailureDomainClass) -> &'static str {
    class.as_str()
}

/// Stable kebab-case target-kind name; identical to the serde wire
/// representation.
fn kind_wire(kind: BindingTargetKind) -> &'static str {
    kind.as_str()
}

/// Parses a stored class value back through serde so only the kebab-case wire
/// vocabulary is accepted; anything else fails closed as corruption.
fn parse_class(raw: &str) -> Result<FailureDomainClass, KernelError> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .map_err(|_| conflict(format!("unknown failure-domain class '{raw}'")))
}

/// Parses a stored target-kind value back through serde (see `parse_class`).
fn parse_kind(raw: &str) -> Result<BindingTargetKind, KernelError> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .map_err(|_| conflict(format!("unknown binding target kind '{raw}'")))
}

/// Maps one stored binding row to its canonical representation.
fn parse_binding_row(row: &sqlx::postgres::PgRow) -> Result<TopologyBinding, KernelError> {
    let raw_kind: String = row.try_get("target_kind").map_err(store_err)?;
    Ok(TopologyBinding {
        failure_domain: row.try_get("failure_domain_id").map_err(store_err)?,
        target: BindingTarget {
            kind: parse_kind(&raw_kind)?,
            id: row.try_get("target_id").map_err(store_err)?,
        },
    })
}

/// Maps a *required* audit write failure to a topology-fatal kernel error so
/// the enclosing mutation transaction aborts and nothing (mutation or audit) is
/// committed.
fn audit_error(error: crate::StoreError) -> KernelError {
    match error {
        crate::StoreError::AuditEventConflict => KernelError::AuditConflict,
        crate::StoreError::Database(error) => KernelError::TopologyUnavailable(error.to_string()),
        other => KernelError::TopologyCorrupt(other.to_string()),
    }
}

/// Writes the mutation's mandatory audit event inside the SAME transaction as
/// the topology mutation. Reuses the canonical audit_store insert + column
/// mapping (`insert_audit_event_tx`) so the two paths cannot drift. A failure
/// aborts the whole mutation.
async fn insert_topology_audit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    audit: &AuditEvent,
) -> Result<(), KernelError> {
    let record = crate::AuditEventRecord::from_kernel_event(audit);
    crate::postgres::audit_store::insert_audit_event_tx(tx, &record)
        .await
        .map_err(audit_error)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn generation_i64(generation: u64) -> Result<i64, KernelError> {
    i64::try_from(generation)
        .map_err(|_| conflict("failure domain generation exceeds the storable range"))
}

fn metadata_json(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Result<String, KernelError> {
    serde_json::to_string(metadata).map_err(|error| {
        conflict(format!(
            "failure domain metadata is not serializable: {error}"
        ))
    })
}

impl PostgresStore {
    /// Takes the topology mutation serialization lock for the current
    /// transaction. Must be the first statement of every mutation
    /// transaction.
    async fn lock_topology_tx(connection: &mut sqlx::PgConnection) -> Result<(), KernelError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ADVISORY_LOCK_KEY)
            .execute(connection)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn load_snapshot_tx(
        connection: &mut sqlx::PgConnection,
    ) -> Result<TopologySnapshot, KernelError> {
        // A read transaction keeps the four queries on one stable snapshot so
        // a concurrent writer can never make a region, domain, or binding
        // vanish between reads (rollback-on-drop keeps cancelled futures safe).
        let region_rows = sqlx::query("SELECT id FROM topology_regions ORDER BY id")
            .fetch_all(&mut *connection)
            .await
            .map_err(store_err)?;
        let mut regions: Vec<RegionDeclaration> = Vec::with_capacity(region_rows.len());
        for row in &region_rows {
            regions.push(RegionDeclaration {
                id: row.try_get("id").map_err(store_err)?,
                availability_domains: Vec::new(),
            });
        }
        let az_rows = sqlx::query(
            "SELECT id, region_id FROM topology_availability_domains ORDER BY region_id, id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(store_err)?;
        for row in &az_rows {
            let az_id: String = row.try_get("id").map_err(store_err)?;
            let region_id: String = row.try_get("region_id").map_err(store_err)?;
            let Some(region) = regions.iter_mut().find(|region| region.id == region_id) else {
                return Err(conflict(format!(
                    "availability domain '{az_id}' references missing region '{region_id}'"
                )));
            };
            region
                .availability_domains
                .push(AvailabilityDomain { id: az_id });
        }

        let domain_rows = sqlx::query(
            "SELECT id, class, name, availability_domain_id, parent_id, generation, metadata \
             FROM failure_domains ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(store_err)?;
        let mut failure_domains = Vec::with_capacity(domain_rows.len());
        for row in &domain_rows {
            let raw_class: String = row.try_get("class").map_err(store_err)?;
            let metadata: String = row.try_get("metadata").map_err(store_err)?;
            let generation: i64 = row.try_get("generation").map_err(store_err)?;
            failure_domains.push(FailureDomain {
                id: row.try_get("id").map_err(store_err)?,
                class: parse_class(&raw_class)?,
                name: row.try_get("name").map_err(store_err)?,
                availability_domain: row.try_get("availability_domain_id").map_err(store_err)?,
                parent: row.try_get("parent_id").map_err(store_err)?,
                generation: u64::try_from(generation)
                    .map_err(|_| conflict("stored failure domain generation is negative"))?,
                metadata: serde_json::from_str(&metadata)
                    .map_err(|_| conflict("stored failure domain metadata is not valid JSON"))?,
            });
        }

        let binding_rows = sqlx::query(
            "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
             ORDER BY failure_domain_id, target_kind, target_id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(store_err)?;
        let mut bindings = Vec::with_capacity(binding_rows.len());
        for row in &binding_rows {
            let raw_kind: String = row.try_get("target_kind").map_err(store_err)?;
            bindings.push(TopologyBinding {
                failure_domain: row.try_get("failure_domain_id").map_err(store_err)?,
                target: BindingTarget {
                    kind: parse_kind(&raw_kind)?,
                    id: row.try_get("target_id").map_err(store_err)?,
                },
            });
        }

        Ok(TopologySnapshot {
            regions,
            failure_domains,
            bindings,
        })
    }

    async fn insert_failure_domain_tx(
        connection: &mut sqlx::PgConnection,
        domain: &FailureDomain,
    ) -> Result<(), KernelError> {
        let metadata = metadata_json(&domain.metadata)?;
        let generation = generation_i64(domain.generation)?;
        let result = sqlx::query(
            "INSERT INTO failure_domains \
             (id, class, name, availability_domain_id, parent_id, generation, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&domain.id)
        .bind(class_wire(domain.class))
        .bind(&domain.name)
        .bind(&domain.availability_domain)
        .bind(&domain.parent)
        .bind(generation)
        .bind(metadata)
        .execute(&mut *connection)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_unique_violation(&error) => Err(conflict(format!(
                "duplicate failure domain '{}'",
                domain.id
            ))),
            Err(error) if is_fk_violation(&error) => Err(conflict(format!(
                "failure domain '{}' references an unknown availability domain or parent",
                domain.id
            ))),
            Err(error) => Err(store_err(error)),
        }
    }

    /// Distinguishes "stored row vanished or generation moved" after a CAS
    /// miss so the conflict reason is truthful, not just generic.
    async fn cas_miss_reason_tx(
        connection: &mut sqlx::PgConnection,
        domain_id: &str,
        expected_generation: u64,
    ) -> KernelError {
        let stored: Result<Option<i64>, sqlx::Error> =
            sqlx::query_scalar("SELECT generation FROM failure_domains WHERE id = $1")
                .bind(domain_id)
                .fetch_optional(&mut *connection)
                .await;
        match stored {
            Err(error) => store_err(error),
            Ok(None) => conflict(format!("unknown failure domain '{domain_id}'")),
            Ok(Some(_)) => conflict(format!(
                "stale generation for failure domain '{domain_id}' (expected {expected_generation})"
            )),
        }
    }
}

#[async_trait]
impl TopologyStore for PostgresStore {
    async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        let snapshot = Self::load_snapshot_tx(&mut transaction).await?;
        transaction.commit().await.map_err(store_err)?;
        Ok(snapshot)
    }

    async fn insert_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        sqlx::query(
            "INSERT INTO topology_regions (id, declared_at_ms) VALUES ($1, $2) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(region_id)
        .bind(now_ms())
        .execute(&mut *transaction)
        .await
        .map_err(store_err)?;
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn delete_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let result = sqlx::query("DELETE FROM topology_regions WHERE id = $1")
            .bind(region_id)
            .execute(&mut *transaction)
            .await;
        match result {
            // Deleting an absent region is an idempotent no-op (replay
            // convergence); a region that still has availability domains is
            // rejected by the FK and surfaces as a conflict.
            Ok(_) => {}
            Err(error) if is_fk_violation(&error) => {
                return Err(conflict(format!(
                    "region '{region_id}' still has availability domains"
                )));
            }
            Err(error) => return Err(store_err(error)),
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn insert_availability_domain(
        &self,
        region_id: &str,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let result = sqlx::query(
            "INSERT INTO topology_availability_domains (id, region_id, declared_at_ms) \
             VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(az_id)
        .bind(region_id)
        .bind(now_ms())
        .execute(&mut *transaction)
        .await;
        match result {
            Ok(_) => {}
            Err(error) if is_fk_violation(&error) => {
                return Err(conflict(format!(
                    "availability domain '{az_id}' references unknown region '{region_id}'"
                )));
            }
            Err(error) => return Err(store_err(error)),
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn delete_availability_domain(
        &self,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let result = sqlx::query("DELETE FROM topology_availability_domains WHERE id = $1")
            .bind(az_id)
            .execute(&mut *transaction)
            .await;
        match result {
            Ok(_) => {}
            Err(error) if is_fk_violation(&error) => {
                return Err(conflict(format!(
                    "availability domain '{az_id}' still has failure domains"
                )));
            }
            Err(error) => return Err(store_err(error)),
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn insert_failure_domain(
        &self,
        domain: &FailureDomain,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        Self::insert_failure_domain_tx(&mut transaction, domain).await?;
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn update_failure_domain(
        &self,
        domain: &FailureDomain,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let metadata = metadata_json(&domain.metadata)?;
        let generation = generation_i64(domain.generation)?;
        let expected = generation_i64(expected_generation)?;
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        // CAS: only rows whose stored generation still equals
        // `expected_generation` are updated; name, metadata, and generation
        // are the mutable columns.
        let updated = sqlx::query(
            "UPDATE failure_domains SET name = $2, generation = $3, metadata = $4 \
             WHERE id = $1 AND generation = $5",
        )
        .bind(&domain.id)
        .bind(&domain.name)
        .bind(generation)
        .bind(metadata)
        .bind(expected)
        .execute(&mut *transaction)
        .await
        .map_err(store_err)?;
        if updated.rows_affected() == 0 {
            return Err(Self::cas_miss_reason_tx(
                &mut transaction,
                &domain.id,
                expected_generation,
            )
            .await);
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn delete_failure_domain(
        &self,
        domain_id: &str,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let expected = generation_i64(expected_generation)?;
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let result = sqlx::query("DELETE FROM failure_domains WHERE id = $1 AND generation = $2")
            .bind(domain_id)
            .bind(expected)
            .execute(&mut *transaction)
            .await;
        match result {
            Ok(deleted) if deleted.rows_affected() == 1 => {}
            Ok(_) => {
                return Err(Self::cas_miss_reason_tx(
                    &mut transaction,
                    domain_id,
                    expected_generation,
                )
                .await);
            }
            Err(error) if is_fk_violation(&error) => {
                return Err(conflict(format!(
                    "failure domain '{domain_id}' still has child failure domains or bindings"
                )));
            }
            Err(error) => return Err(store_err(error)),
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn insert_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let result = sqlx::query(
            "INSERT INTO topology_bindings (failure_domain_id, target_kind, target_id, created_at_ms) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (failure_domain_id, target_kind, target_id) DO NOTHING",
        )
        .bind(&binding.failure_domain)
        .bind(kind_wire(binding.target.kind))
        .bind(&binding.target.id)
        .bind(now_ms())
        .execute(&mut *transaction)
        .await;
        match result {
            Ok(_) => {}
            Err(error) if is_fk_violation(&error) => {
                return Err(conflict(format!(
                    "binding references unknown failure domain '{}'",
                    binding.failure_domain
                )));
            }
            Err(error) => return Err(store_err(error)),
        }
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn delete_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError> {
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        // Removing an absent binding is an idempotent no-op (replay
        // convergence), so `rows_affected == 0` is success here.
        sqlx::query(
            "DELETE FROM topology_bindings \
             WHERE failure_domain_id = $1 AND target_kind = $2 AND target_id = $3",
        )
        .bind(&binding.failure_domain)
        .bind(kind_wire(binding.target.kind))
        .bind(&binding.target.id)
        .execute(&mut *transaction)
        .await
        .map_err(store_err)?;
        if let Some(audit) = audit {
            insert_topology_audit_tx(&mut transaction, audit).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn record_audit(&self, audit: &AuditEvent) -> Result<(), KernelError> {
        // Standalone audit write for no-op replay requests: there is no mutation
        // transaction to fold into, so the audit row is committed on its own.
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        insert_topology_audit_tx(&mut transaction, audit).await?;
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn list_failure_domains(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FailureDomain>, KernelError> {
        let take = i64::try_from(limit.saturating_add(1))
            .map_err(|_| conflict("failure domain page bound exceeds the storable range"))?;
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        // The advisory lock keeps paged reads consistent with the serialized
        // mutation sequence: a page can never interleave another writer's
        // durable state.
        Self::lock_topology_tx(&mut transaction).await?;
        // Keyset page: `id > after` keeps the scan bounded and the ordering
        // stable; `limit + 1` rows let the caller detect a further page.
        let domain_rows = if let Some(after) = after_id {
            sqlx::query(
                "SELECT id, class, name, availability_domain_id, parent_id, generation, metadata \
                 FROM failure_domains WHERE id > $1 ORDER BY id LIMIT $2",
            )
            .bind(after)
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        } else {
            sqlx::query(
                "SELECT id, class, name, availability_domain_id, parent_id, generation, metadata \
                 FROM failure_domains ORDER BY id LIMIT $1",
            )
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        }
        .map_err(store_err)?;
        let mut failure_domains = Vec::with_capacity(domain_rows.len());
        for row in &domain_rows {
            let raw_class: String = row.try_get("class").map_err(store_err)?;
            let metadata: String = row.try_get("metadata").map_err(store_err)?;
            let generation: i64 = row.try_get("generation").map_err(store_err)?;
            failure_domains.push(FailureDomain {
                id: row.try_get("id").map_err(store_err)?,
                class: parse_class(&raw_class)?,
                name: row.try_get("name").map_err(store_err)?,
                availability_domain: row.try_get("availability_domain_id").map_err(store_err)?,
                parent: row.try_get("parent_id").map_err(store_err)?,
                generation: u64::try_from(generation)
                    .map_err(|_| conflict("stored failure domain generation is negative"))?,
                metadata: serde_json::from_str(&metadata)
                    .map_err(|_| conflict("stored failure domain metadata is not valid JSON"))?,
            });
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(failure_domains)
    }

    async fn list_bindings(
        &self,
        after: Option<&o3k_kernel::BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError> {
        let take = i64::try_from(limit.saturating_add(1))
            .map_err(|_| conflict("binding page bound exceeds the storable range"))?;
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let binding_rows = if let Some(cursor) = after {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 WHERE (failure_domain_id > $1) \
                    OR (failure_domain_id = $1 AND (target_kind, target_id) > ($2, $3)) \
                 ORDER BY failure_domain_id, target_kind, target_id LIMIT $4",
            )
            .bind(&cursor.failure_domain)
            .bind(&cursor.target_kind)
            .bind(&cursor.target_id)
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        } else {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 ORDER BY failure_domain_id, target_kind, target_id LIMIT $1",
            )
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        }
        .map_err(store_err)?;
        let mut bindings = Vec::with_capacity(binding_rows.len());
        for row in &binding_rows {
            bindings.push(parse_binding_row(row)?);
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(bindings)
    }

    async fn list_bindings_of(
        &self,
        failure_domain: &str,
        after: Option<&o3k_kernel::BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError> {
        let take = i64::try_from(limit.saturating_add(1))
            .map_err(|_| conflict("binding page bound exceeds the storable range"))?;
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        Self::lock_topology_tx(&mut transaction).await?;
        let binding_rows = if let Some(cursor) = after {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 WHERE failure_domain_id = $1 AND (target_kind, target_id) > ($2, $3) \
                 ORDER BY target_kind, target_id LIMIT $4",
            )
            .bind(failure_domain)
            .bind(&cursor.target_kind)
            .bind(&cursor.target_id)
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        } else {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 WHERE failure_domain_id = $1 ORDER BY target_kind, target_id LIMIT $2",
            )
            .bind(failure_domain)
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        }
        .map_err(store_err)?;
        let mut bindings = Vec::with_capacity(binding_rows.len());
        for row in &binding_rows {
            bindings.push(parse_binding_row(row)?);
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(bindings)
    }
}
