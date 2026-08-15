//! Row types and the read-only SQLite implementation of the database seam.
//!
//! Table and column names mirror `crates/o3k-store/migrations/*.sql` exactly;
//! doctor never opens the database in a mode that could write to it.

use crate::context::{DoctorDb, sanitize_error};
use async_trait::async_trait;
use sqlx::Row;
use std::path::Path;
use std::time::Duration;

/// One `placement_providers` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    pub id: String,
    pub node_id: String,
    pub state: String,
    pub generation: i64,
}

/// One `placement_inventories` row.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryRow {
    pub provider_id: String,
    pub resource_class: String,
    pub total: i64,
    pub reserved: i64,
    pub allocation_ratio: f64,
    pub used: i64,
}

/// Summed allocation amount for one (provider, resource class) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationRow {
    pub provider_id: String,
    pub resource_class: String,
    pub amount: i64,
}

/// Latest persisted agent epoch from one durable source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochRow {
    pub source: String,
    pub agent_id: String,
    pub agent_epoch: String,
}

/// One compute instance (`resources` row) with its display name extracted
/// from the durable desired-state JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRow {
    pub id: String,
    pub name: String,
    pub observed_state: String,
}

/// One `network_ports` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub id: String,
    pub binding_host: Option<String>,
    pub binding_state: Option<String>,
    pub status: String,
}

/// Real [`DoctorDb`] implementation: a single-connection read-only SQLite
/// pool opened per query. Any SQL or connection failure becomes a short
/// sanitized error string; SQL parameters are never included in messages.
#[derive(Debug, Clone)]
pub struct SqlxDoctorDb;

impl SqlxDoctorDb {
    /// Opens a read-only connection for one logical query.
    async fn read_only_connection(
        path: &Path,
    ) -> Result<sqlx::pool::PoolConnection<sqlx::Sqlite>, String> {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(|error| sanitize_error(&format!("database is not readable: {error}")))?;
        pool.acquire()
            .await
            .map_err(|error| sanitize_error(&format!("database is not readable: {error}")))
    }
}

#[async_trait]
impl DoctorDb for SqlxDoctorDb {
    async fn pragma_journal_mode(&self, path: &Path) -> Result<String, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let row = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| sanitize_error(&format!("journal mode query failed: {error}")))?;
        Ok(row)
    }

    async fn pragma_quick_check(&self, path: &Path) -> Result<String, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let row = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| sanitize_error(&format!("integrity check failed: {error}")))?;
        Ok(row)
    }

    async fn placement_providers(&self, path: &Path) -> Result<Vec<ProviderRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let rows = sqlx::query(
            "SELECT id, node_id, state, generation FROM placement_providers ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("placement provider query failed: {error}")))?;
        Ok(rows
            .into_iter()
            .map(|row| ProviderRow {
                id: row.get("id"),
                node_id: row.get("node_id"),
                state: row.get("state"),
                generation: row.get("generation"),
            })
            .collect())
    }

    async fn placement_inventories(&self, path: &Path) -> Result<Vec<InventoryRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let rows = sqlx::query(
            "SELECT provider_id, resource_class, total, reserved, allocation_ratio, used \
             FROM placement_inventories ORDER BY provider_id, resource_class",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("placement inventory query failed: {error}")))?;
        Ok(rows
            .into_iter()
            .map(|row| InventoryRow {
                provider_id: row.get("provider_id"),
                resource_class: row.get("resource_class"),
                total: row.get("total"),
                reserved: row.get("reserved"),
                allocation_ratio: row.get("allocation_ratio"),
                used: row.get("used"),
            })
            .collect())
    }

    async fn live_allocation_resources(&self, path: &Path) -> Result<Vec<AllocationRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        // Mirrors `o3k-store::reconcile_consumers`: the live consumer set is
        // every `compute_instance` resource that is not DELETED.
        let rows = sqlx::query(
            "SELECT a.provider_id, r.resource_class, SUM(r.amount) AS amount \
             FROM placement_allocations a \
             JOIN placement_allocation_resources r ON r.allocation_id = a.id \
             WHERE a.consumer_id IN (\
                 SELECT id FROM resources \
                 WHERE kind = 'compute_instance' AND observed_state != 'DELETED') \
             GROUP BY a.provider_id, r.resource_class \
             ORDER BY a.provider_id, r.resource_class",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("allocation query failed: {error}")))?;
        Ok(rows
            .into_iter()
            .map(|row| AllocationRow {
                provider_id: row.get("provider_id"),
                resource_class: row.get("resource_class"),
                amount: row.get("amount"),
            })
            .collect())
    }

    async fn latest_epochs(&self, path: &Path) -> Result<Vec<EpochRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        // MAX() on TEXT is a time-order maximum only because agent epochs
        // are UUIDv7 strings (timestamp-prefixed, lexicographically
        // time-ordered). If the epoch format ever changes to v4 or a
        // counter, this comparison must change with it (see
        // compute.agent_epoch, which relies on the same property).
        let watermark_rows =
            sqlx::query("SELECT MAX(agent_epoch) AS agent_epoch FROM observation_watermarks")
                .fetch_all(&mut *connection)
                .await
                .map_err(|error| {
                    sanitize_error(&format!("watermark epoch query failed: {error}"))
                })?;
        let mut epochs = Vec::new();
        for row in watermark_rows {
            let epoch: Option<String> = row.get("agent_epoch");
            if let Some(agent_epoch) = epoch {
                epochs.push(EpochRow {
                    source: "observation_watermarks".to_owned(),
                    agent_id: String::new(),
                    agent_epoch,
                });
            }
        }
        let command_rows = sqlx::query(
            "SELECT agent_id, MAX(agent_epoch) AS agent_epoch FROM agent_commands GROUP BY agent_id ORDER BY agent_id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("command epoch query failed: {error}")))?;
        for row in command_rows {
            let epoch: Option<String> = row.get("agent_epoch");
            if let Some(agent_epoch) = epoch {
                epochs.push(EpochRow {
                    source: "agent_commands".to_owned(),
                    agent_id: row.get("agent_id"),
                    agent_epoch,
                });
            }
        }
        Ok(epochs)
    }

    async fn compute_instances(&self, path: &Path) -> Result<Vec<InstanceRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let rows = sqlx::query(
            "SELECT id, \
                    COALESCE(json_extract(desired_state, '$.name'), '') AS name, \
                    observed_state \
             FROM resources WHERE kind = 'compute_instance' ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("compute instance query failed: {error}")))?;
        Ok(rows
            .into_iter()
            .map(|row| InstanceRow {
                id: row.get("id"),
                name: row.get("name"),
                observed_state: row.get("observed_state"),
            })
            .collect())
    }

    async fn network_ports(&self, path: &Path) -> Result<Vec<PortRow>, String> {
        let mut connection = Self::read_only_connection(path).await?;
        let rows = sqlx::query(
            "SELECT id, binding_host, binding_state, status FROM network_ports ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| sanitize_error(&format!("network port query failed: {error}")))?;
        Ok(rows
            .into_iter()
            .map(|row| PortRow {
                id: row.get("id"),
                binding_host: row.get("binding_host"),
                binding_state: row.get("binding_state"),
                status: row.get("status"),
            })
            .collect())
    }
}
