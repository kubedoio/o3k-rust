//! SQLite durable topology store (`TopologyStore`).
//!
//! Location/failure-domain validation, nesting ranks, and generation semantics
//! are owned by `o3k_kernel::location`; this module only persists topology and
//! enforces the invariants kernel validation cannot make atomic: uniqueness,
//! referential integrity (FK), mutation serialization (`BEGIN IMMEDIATE` plus
//! the pool `busy_timeout`), and optimistic-concurrency generations (CAS
//! updates/deletes).
//!
//! Class and target-kind values are stored as their stable kebab-case serde
//! wire representation (see `FailureDomainClass::as_str` /
//! `BindingTargetKind::as_str`); failure-domain metadata is stored as JSON.

use async_trait::async_trait;
use o3k_kernel::{
    AuditEvent, AvailabilityDomain, BindingTargetKind, FailureDomain, FailureDomainClass,
    KernelError, RegionDeclaration, TopologyBinding, TopologySnapshot, TopologyStore,
};
use sqlx::Row;

use super::SqliteStore;

/// Maps any store/driver failure to a secret-free topology-unavailable error.
fn store_err(error: impl std::fmt::Display) -> KernelError {
    KernelError::TopologyUnavailable(error.to_string())
}

/// Builds the conflict error used for every durable-topology invariant
/// violation (FK protection, duplicate identity, stale CAS generation).
fn conflict(reason: impl Into<String>) -> KernelError {
    KernelError::TopologyCorrupt(reason.into())
}

/// SQLite extended codes for a unique constraint (2067) or primary key
/// (1555) violation.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            matches!(database.code().as_deref(), Some("1555") | Some("2067"))
        }
        _ => false,
    }
}

/// SQLite extended code for a foreign-key violation (787).
fn is_fk_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => database.code().as_deref() == Some("787"),
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
fn parse_binding_row(row: &sqlx::sqlite::SqliteRow) -> Result<TopologyBinding, KernelError> {
    let raw_kind: String = row.try_get("target_kind").map_err(store_err)?;
    Ok(TopologyBinding {
        failure_domain: row.try_get("failure_domain_id").map_err(store_err)?,
        target: o3k_kernel::BindingTarget {
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
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    audit: &AuditEvent,
) -> Result<(), KernelError> {
    let record = crate::AuditEventRecord::from_kernel_event(audit);
    crate::sqlite::audit_store::insert_audit_event_tx(tx, &record)
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

impl SqliteStore {
    async fn load_snapshot_tx(
        connection: &mut sqlx::sqlite::SqliteConnection,
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
                target: o3k_kernel::BindingTarget {
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
        connection: &mut sqlx::sqlite::SqliteConnection,
        domain: &FailureDomain,
    ) -> Result<(), KernelError> {
        let metadata = metadata_json(&domain.metadata)?;
        let generation = generation_i64(domain.generation)?;
        let result = sqlx::query(
            "INSERT INTO failure_domains \
             (id, class, name, availability_domain_id, parent_id, generation, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
        connection: &mut sqlx::sqlite::SqliteConnection,
        domain_id: &str,
        expected_generation: u64,
    ) -> KernelError {
        let stored: Result<Option<i64>, sqlx::Error> =
            sqlx::query_scalar("SELECT generation FROM failure_domains WHERE id = ?1")
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
impl TopologyStore for SqliteStore {
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
        // `BEGIN IMMEDIATE` takes the write lock up front so the configured
        // `busy_timeout` applies, matching the metering write path.
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        sqlx::query(
            "INSERT INTO topology_regions (id, declared_at_ms) VALUES (?1, ?2) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(region_id)
        .bind(now_ms())
        .execute(&mut *transaction)
        .await
        .map_err(store_err)?;
        // The mandatory audit is folded into this SAME transaction: if either
        // the mutation or the audit write fails, neither is visible.
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        let result = sqlx::query("DELETE FROM topology_regions WHERE id = ?1")
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        let result = sqlx::query(
            "INSERT INTO topology_availability_domains (id, region_id, declared_at_ms) \
             VALUES (?1, ?2, ?3) ON CONFLICT (id) DO NOTHING",
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        let result = sqlx::query("DELETE FROM topology_availability_domains WHERE id = ?1")
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        // CAS: only `class`/`availability_domain`/`parent`-preserving rows
        // (name, metadata, generation are mutable) whose stored generation
        // still equals `expected_generation` are updated.
        let updated = sqlx::query(
            "UPDATE failure_domains SET name = ?2, generation = ?3, metadata = ?4 \
             WHERE id = ?1 AND generation = ?5",
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        let result = sqlx::query("DELETE FROM failure_domains WHERE id = ?1 AND generation = ?2")
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        let result = sqlx::query(
            "INSERT INTO topology_bindings (failure_domain_id, target_kind, target_id, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (failure_domain_id, target_kind, target_id) DO NOTHING",
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
        // Removing an absent binding is an idempotent no-op (replay
        // convergence), so `rows_affected == 0` is success here.
        sqlx::query(
            "DELETE FROM topology_bindings \
             WHERE failure_domain_id = ?1 AND target_kind = ?2 AND target_id = ?3",
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(store_err)?;
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
        // A deferred read transaction gives a consistent WAL snapshot across
        // the page; unlike PostgreSQL, SQLite needs no advisory lock because
        // writers serialize on `BEGIN IMMEDIATE` and readers never observe a
        // partially-committed mutation.
        let mut transaction = self.pool.begin().await.map_err(store_err)?;
        // Keyset page: `id > after` keeps the scan bounded and the ordering
        // stable; `limit + 1` rows let the caller detect a further page.
        let domain_rows = if let Some(after) = after_id {
            sqlx::query(
                "SELECT id, class, name, availability_domain_id, parent_id, generation, metadata \
                 FROM failure_domains WHERE id > ?1 ORDER BY id LIMIT ?2",
            )
            .bind(after)
            .bind(take)
            .fetch_all(&mut *transaction)
            .await
        } else {
            sqlx::query(
                "SELECT id, class, name, availability_domain_id, parent_id, generation, metadata \
                 FROM failure_domains ORDER BY id LIMIT ?1",
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
        let binding_rows = if let Some(cursor) = after {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 WHERE (failure_domain_id > ?1) \
                    OR (failure_domain_id = ?1 AND (target_kind, target_id) > (?2, ?3)) \
                 ORDER BY failure_domain_id, target_kind, target_id LIMIT ?4",
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
                 ORDER BY failure_domain_id, target_kind, target_id LIMIT ?1",
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
        let binding_rows = if let Some(cursor) = after {
            sqlx::query(
                "SELECT failure_domain_id, target_kind, target_id FROM topology_bindings \
                 WHERE failure_domain_id = ?1 AND (target_kind, target_id) > (?2, ?3) \
                 ORDER BY target_kind, target_id LIMIT ?4",
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
                 WHERE failure_domain_id = ?1 ORDER BY target_kind, target_id LIMIT ?2",
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::AuditRepository;
    use o3k_kernel::BindingTarget;
    use std::collections::BTreeMap;

    fn domain(
        id: &str,
        class: FailureDomainClass,
        az: &str,
        parent: Option<&str>,
    ) -> FailureDomain {
        FailureDomain {
            id: id.to_owned(),
            class,
            name: id.to_owned(),
            availability_domain: az.to_owned(),
            parent: parent.map(str::to_owned),
            generation: 1,
            metadata: BTreeMap::new(),
        }
    }

    fn binding(failure_domain: &str, kind: BindingTargetKind, id: &str) -> TopologyBinding {
        TopologyBinding {
            failure_domain: failure_domain.to_owned(),
            target: BindingTarget {
                kind,
                id: id.to_owned(),
            },
        }
    }

    async fn seeded_store() -> SqliteStore {
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        store.insert_region("region-a", None).await.unwrap();
        store.insert_region("region-b", None).await.unwrap();
        store
            .insert_availability_domain("region-a", "az-2", None)
            .await
            .unwrap();
        store
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();
        store
    }

    #[tokio::test]
    async fn crud_round_trip_and_snapshot_assembly() {
        let store = seeded_store().await;
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let mut with_metadata = domain("rack-2", FailureDomainClass::Rack, "az-1", None);
        with_metadata.metadata.insert("aisle".into(), "7".into());
        store
            .insert_failure_domain(&with_metadata, None)
            .await
            .unwrap();
        store
            .insert_failure_domain(
                &domain(
                    "chassis-1",
                    FailureDomainClass::Chassis,
                    "az-2",
                    Some("rack-2"),
                ),
                None,
            )
            .await
            .unwrap();
        store
            .insert_binding(&binding("rack-1", BindingTargetKind::Host, "host-9"), None)
            .await
            .unwrap();
        store
            .insert_binding(&binding("rack-1", BindingTargetKind::Host, "host-1"), None)
            .await
            .unwrap();

        let snapshot = store.load_snapshot().await.unwrap();
        // Regions sorted by id, availability domains sorted within their
        // region, failure domains and bindings sorted by identity.
        let region_ids: Vec<&str> = snapshot.regions.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(region_ids, vec!["region-a", "region-b"]);
        let az_ids: Vec<&str> = snapshot.regions[0]
            .availability_domains
            .iter()
            .map(|az| az.id.as_str())
            .collect();
        assert_eq!(az_ids, vec!["az-1", "az-2"]);
        assert!(snapshot.regions[1].availability_domains.is_empty());

        let domain_ids: Vec<&str> = snapshot
            .failure_domains
            .iter()
            .map(|domain| domain.id.as_str())
            .collect();
        assert_eq!(domain_ids, vec!["chassis-1", "rack-1", "rack-2"]);
        let restored = snapshot
            .failure_domains
            .iter()
            .find(|domain| domain.id == "rack-2")
            .unwrap();
        assert_eq!(
            restored.metadata.get("aisle").map(String::as_str),
            Some("7")
        );
        assert_eq!(restored.class, FailureDomainClass::Rack);
        let chassis = snapshot
            .failure_domains
            .iter()
            .find(|domain| domain.id == "chassis-1")
            .unwrap();
        assert_eq!(chassis.parent.as_deref(), Some("rack-2"));
        assert_eq!(chassis.availability_domain, "az-2");

        let binding_keys: Vec<(&str, &str, &str)> = snapshot
            .bindings
            .iter()
            .map(|binding| {
                (
                    binding.failure_domain.as_str(),
                    binding.target.kind.as_str(),
                    binding.target.id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            binding_keys,
            vec![("rack-1", "host", "host-1"), ("rack-1", "host", "host-9")]
        );
    }

    #[tokio::test]
    async fn replay_inserts_are_idempotent_noops() {
        let store = seeded_store().await;
        store.insert_region("region-a", None).await.unwrap();
        store
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        store
            .insert_binding(&binding("rack-1", BindingTargetKind::Host, "host-1"), None)
            .await
            .unwrap();

        // Re-inserting the same identities converges instead of conflicting.
        store.insert_region("region-a", None).await.unwrap();
        store
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();
        store
            .insert_binding(&binding("rack-1", BindingTargetKind::Host, "host-1"), None)
            .await
            .unwrap();
        // Removing an absent binding is a no-op too.
        store
            .delete_binding(&binding("rack-1", BindingTargetKind::Host, "host-9"), None)
            .await
            .unwrap();

        let snapshot = store.load_snapshot().await.unwrap();
        assert_eq!(snapshot.regions.len(), 2);
        assert_eq!(snapshot.regions[0].availability_domains.len(), 2);
        assert_eq!(snapshot.failure_domains.len(), 1);
        assert_eq!(snapshot.bindings.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_failure_domain_insert_conflicts() {
        let store = seeded_store().await;
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let error = store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("duplicate")),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn unknown_availability_domain_or_parent_conflicts() {
        let store = seeded_store().await;
        let error = store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-missing", None),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, KernelError::TopologyCorrupt(_)));

        let error = store
            .insert_failure_domain(
                &domain(
                    "rack-1",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("ghost-parent"),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, KernelError::TopologyCorrupt(_)));
    }

    #[tokio::test]
    async fn cas_update_bumps_generation_and_stale_generation_conflicts() {
        let store = seeded_store().await;
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let mut updated = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        updated.name = "Rack One".into();
        updated.generation = 2;
        store
            .update_failure_domain(&updated, 1, None)
            .await
            .unwrap();

        // A stale expected generation must fail and change nothing.
        let mut stale = updated.clone();
        stale.name = "Renamed Behind Your Back".into();
        stale.generation = 3;
        let error = store
            .update_failure_domain(&stale, 1, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("stale generation")),
            "unexpected error: {error:?}"
        );

        // CAS against a missing row reports the unknown domain.
        let error = store
            .update_failure_domain(
                &domain("ghost", FailureDomainClass::Rack, "az-1", None),
                1,
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("unknown failure domain")),
            "unexpected error: {error:?}"
        );

        let snapshot = store.load_snapshot().await.unwrap();
        let restored = &snapshot.failure_domains[0];
        assert_eq!(restored.name, "Rack One");
        assert_eq!(restored.generation, 2);
    }

    #[tokio::test]
    async fn delete_protections_surface_foreign_key_conflicts() {
        let store = seeded_store().await;
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        store
            .insert_failure_domain(
                &domain(
                    "chassis-1",
                    FailureDomainClass::Chassis,
                    "az-1",
                    Some("rack-1"),
                ),
                None,
            )
            .await
            .unwrap();
        store
            .insert_binding(
                &binding("chassis-1", BindingTargetKind::Host, "host-1"),
                None,
            )
            .await
            .unwrap();

        // Region still has AZs.
        let error = store.delete_region("region-a", None).await.unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("availability domains")),
            "unexpected error: {error:?}"
        );
        // AZ still has failure domains.
        let error = store
            .delete_availability_domain("az-1", None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("failure domains")),
            "unexpected error: {error:?}"
        );
        // Parent still has a child.
        let error = store
            .delete_failure_domain("rack-1", 1, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("child failure domains or bindings")),
            "unexpected error: {error:?}"
        );
        // Child still has a binding.
        let error = store
            .delete_failure_domain("chassis-1", 1, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("child failure domains or bindings")),
            "unexpected error: {error:?}"
        );

        // Tear down in dependency order: every delete succeeds.
        store
            .delete_binding(
                &binding("chassis-1", BindingTargetKind::Host, "host-1"),
                None,
            )
            .await
            .unwrap();
        store
            .delete_failure_domain("chassis-1", 1, None)
            .await
            .unwrap();
        // Bump rack-1 to generation 2 so the stale-generation delete below has
        // a moved generation to lose against.
        let mut renamed = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        renamed.name = "Rack One".into();
        renamed.generation = 2;
        store
            .update_failure_domain(&renamed, 1, None)
            .await
            .unwrap();
        // Deleting with a stale generation conflicts even with no children.
        let error = store
            .delete_failure_domain("rack-1", 1, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(ref reason) if reason.contains("stale generation")),
            "unexpected error: {error:?}"
        );
        store
            .delete_failure_domain("rack-1", 2, None)
            .await
            .unwrap();
        store
            .delete_availability_domain("az-1", None)
            .await
            .unwrap();
        store
            .delete_availability_domain("az-2", None)
            .await
            .unwrap();
        store.delete_region("region-a", None).await.unwrap();

        let snapshot = store.load_snapshot().await.unwrap();
        let region_ids: Vec<&str> = snapshot.regions.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(region_ids, vec!["region-b"]);
        assert!(snapshot.failure_domains.is_empty());
        assert!(snapshot.bindings.is_empty());
    }

    #[tokio::test]
    async fn two_file_backed_writers_race_with_exactly_one_winner() {
        let path = std::env::temp_dir().join(format!(
            "o3k-topology-race-{}.db",
            uuid::Uuid::now_v7().simple()
        ));
        let store_a = SqliteStore::connect_file(&path).await.unwrap();
        let store_b = SqliteStore::connect_file(&path).await.unwrap();
        store_a.insert_region("region-a", None).await.unwrap();
        store_a
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();

        // Both stores race to create the same failure domain id: `BEGIN
        // IMMEDIATE` plus the busy timeout serializes them, and the PK makes
        // exactly one insert win.
        let duplicate = domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        let (a, b) = tokio::join!(
            store_a.insert_failure_domain(&duplicate, None),
            store_b.insert_failure_domain(&duplicate, None),
        );
        assert_eq!(
            usize::from(a.is_ok()) + usize::from(b.is_ok()),
            1,
            "exactly one racing create may win: {a:?} {b:?}"
        );
        let loser = if a.is_ok() { b } else { a };
        assert!(matches!(loser, Err(KernelError::TopologyCorrupt(_))));

        // Each writer still persists its own unique id.
        let unique_a = domain("rack-a-only", FailureDomainClass::Rack, "az-1", None);
        let unique_b = domain("rack-b-only", FailureDomainClass::Rack, "az-1", None);
        let (first, second) = tokio::join!(
            store_a.insert_failure_domain(&unique_a, None),
            store_b.insert_failure_domain(&unique_b, None),
        );
        first.unwrap();
        second.unwrap();

        let snapshot = store_a.load_snapshot().await.unwrap();
        let domain_ids: Vec<&str> = snapshot
            .failure_domains
            .iter()
            .map(|domain| domain.id.as_str())
            .collect();
        assert_eq!(domain_ids, vec!["rack-1", "rack-a-only", "rack-b-only"]);

        drop(store_a);
        drop(store_b);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[tokio::test]
    async fn dropped_store_reconstructs_the_same_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "o3k-topology-restart-{}.db",
            uuid::Uuid::now_v7().simple()
        ));
        let store = SqliteStore::connect_file(&path).await.unwrap();
        store.insert_region("region-a", None).await.unwrap();
        store
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        store
            .insert_binding(
                &binding("rack-1", BindingTargetKind::ResourceProvider, "provider-1"),
                None,
            )
            .await
            .unwrap();
        let expected = store.load_snapshot().await.unwrap();
        drop(store);

        let reopened = SqliteStore::connect_file(&path).await.unwrap();
        let reconstructed = reopened.load_snapshot().await.unwrap();
        assert_eq!(reconstructed, expected);

        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    /// The shared conformance suite runs against the SQLite adapter so both
    /// backends prove parity through identical assertions.
    #[tokio::test]
    async fn shared_conformance_suite_passes_on_sqlite() {
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        crate::run_topology_store_conformance(&store).await.unwrap();
    }

    /// Deterministic upgrade check: a database migrated only up to 0045 (i.e.
    /// created before 0046_topology.sql) gains usable topology tables through
    /// the normal connect path.
    #[tokio::test]
    async fn pre_topology_schema_upgrades_with_usable_topology_tables() {
        let path = std::env::temp_dir().join(format!(
            "o3k-topology-migration-{}.db",
            uuid::Uuid::now_v7().simple()
        ));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(&url)
            .await
            .unwrap();
        let all = sqlx::migrate!("./migrations");
        let legacy = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                all.migrations
                    .iter()
                    .filter(|migration| migration.version < 46)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        legacy.run(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'topology_regions'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "topology_regions must not exist before 0046");
        pool.close().await;

        let store = SqliteStore::connect_file(&path).await.unwrap();
        store.insert_region("region-a", None).await.unwrap();
        store
            .insert_availability_domain("region-a", "az-1", None)
            .await
            .unwrap();
        store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            store.load_snapshot().await.unwrap().failure_domains.len(),
            1
        );
        drop(store);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn audit_event(event_id: &str, principal: &str) -> AuditEvent {
        use o3k_kernel::{
            ActionId, AuditOutcome, EventId, OwnershipScope, PrincipalId, PrincipalKind,
            ResourceId, ResourceType, ScopeId, ScopeKind, ServiceNamespace,
        };
        AuditEvent {
            event_id: EventId::from_string(event_id.to_owned()),
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            request_id: "req-1".to_owned(),
            audit_id: "trace-1".to_owned(),
            principal_id: PrincipalId::new_unchecked(principal),
            principal_kind: PrincipalKind::User,
            effective_scope: OwnershipScope::new(
                ScopeId::new_unchecked("system"),
                ScopeKind::System,
                None,
                None,
            ),
            service_namespace: ServiceNamespace::new_unchecked("topology".to_owned()),
            action: ActionId::new_unchecked("topology", "ManageTopology"),
            resource_type: Some(ResourceType::new_unchecked("topology", "failure_domain")),
            resource_id: Some(ResourceId::new_unchecked("rack-1")),
            owner_scope: None,
            authorization_decision: None,
            operation_id: None,
            outcome: AuditOutcome::Succeeded,
            reason_category: None,
            service_principal: None,
        }
    }

    #[tokio::test]
    async fn successful_mutation_writes_both_the_mutation_and_its_audit_tag() {
        let store = seeded_store().await;
        let audit = audit_event("audit-ok-1", "operator-1");
        store
            .insert_region("audit-region", Some(&audit))
            .await
            .unwrap();

        // Both landed: the region exists and exactly one equivalent audit row.
        let snapshot = store.load_snapshot().await.unwrap();
        assert!(
            snapshot.regions.iter().any(|r| r.id == "audit-region"),
            "mutation persisted"
        );
        let stored = store
            .get_audit_event("system", audit.event_id.as_str())
            .await
            .unwrap();
        assert_eq!(
            stored,
            crate::AuditEventRecord::from_kernel_event(&audit),
            "exactly one durable audit row matching the caller's event"
        );
    }

    #[tokio::test]
    async fn audit_conflict_aborts_the_whole_mutation_atomically() {
        let store = seeded_store().await;
        // Pre-write an audit row with this event_id but DIFFERENT content.
        let prior = audit_event("collide-1", "someone-else");
        store.record_audit(&prior).await.unwrap();
        // The mutation wants to fold in an audit with the SAME event_id but
        // different content -> the equivalence check fails inside the mutation
        // transaction, which must roll back the mutation too.
        let conflicting = audit_event("collide-1", "operator-1");
        let error = store
            .insert_failure_domain(
                &domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                Some(&conflicting),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::AuditConflict),
            "unexpected: {error:?}"
        );

        // Neither the mutation nor any second audit row persisted.
        let snapshot = store.load_snapshot().await.unwrap();
        assert!(
            snapshot.failure_domains.iter().all(|d| d.id != "rack-1"),
            "failed mutation must not persist: {snapshot:?}"
        );
        // Only the pre-existing audit row survives the rollback.
        let surviving = store.get_audit_event("system", "collide-1").await.unwrap();
        assert_eq!(
            surviving,
            crate::AuditEventRecord::from_kernel_event(&prior),
            "the failed write must not have replaced the pre-existing audit row"
        );
    }

    #[tokio::test]
    async fn store_failure_leaves_neither_the_mutation_nor_its_audit() {
        let store = seeded_store().await;
        // An FK-violating insert with an audit must fail without persisting
        // either the mutation or the audit (the mutation never succeeded).
        let audit = audit_event("fk-fail-1", "operator-1");
        let error = store
            .insert_failure_domain(
                &domain("bad-az", FailureDomainClass::Rack, "ghost-az", None),
                Some(&audit),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, KernelError::TopologyCorrupt(_)),
            "unexpected: {error:?}"
        );
        let snapshot = store.load_snapshot().await.unwrap();
        assert!(snapshot.failure_domains.iter().all(|d| d.id != "bad-az"));
        let rows = store.get_audit_event("system", "fk-fail-1").await;
        assert!(
            matches!(rows, Err(crate::StoreError::ResourceNotFound)),
            "failed mutation leaves no audit row: {rows:?}"
        );
    }
}
