//! Domain types for placement: providers, inventories, allocations.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io};
use thiserror::Error;

pub const VCPU: &str = "VCPU";
pub const MEMORY_MB: &str = "MEMORY_MB";
pub const DISK_GB: &str = "DISK_GB";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderState {
    Enabled,
    Draining,
    Unavailable,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Inventory {
    pub total: u64,
    pub reserved: u64,
    pub allocation_ratio: f64,
    pub used: u64,
}

impl Inventory {
    pub fn available(&self) -> u64 {
        ((self.total as f64 * self.allocation_ratio).floor() as u64)
            .saturating_sub(self.reserved)
            .saturating_sub(self.used)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Allocation {
    pub provider_id: String,
    pub consumer_id: String,
    pub resources: BTreeMap<String, u64>,
}

/// Durable control-plane intent recorded before a caller reserves capacity.
/// The intent is deliberately independent from provider execution: a restart
/// can either finish the idempotent allocation or abandon it during
/// reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AllocationIntent {
    pub provider_id: String,
    pub allocation_id: String,
    pub consumer_id: String,
    pub resources: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedAllocation {
    pub provider_id: String,
    pub allocation_id: String,
    pub consumer_id: String,
    pub resources: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub orphaned_allocations: Vec<OrphanedAllocation>,
    pub abandoned_intents: Vec<AllocationIntent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceProvider {
    pub id: String,
    pub node_id: String,
    pub state: ProviderState,
    pub generation: u64,
    pub inventories: BTreeMap<String, Inventory>,
    pub allocations: BTreeMap<String, Allocation>,
}

#[derive(Debug, Error)]
pub enum PlacementError {
    #[error("provider not found")]
    NotFound,
    #[error("provider generation is stale")]
    StaleGeneration,
    #[error("provider is not schedulable")]
    NotSchedulable,
    #[error("allocation exceeds available capacity")]
    OverCapacity,
    #[error("allocation is invalid")]
    InvalidAllocation,
    #[error("placement storage failed")]
    Storage(#[source] io::Error),
    #[error("placement state is corrupt")]
    Corrupt(#[source] serde_json::Error),
    #[error("placement lock is unavailable")]
    Lock,
    #[error("placement store failed")]
    Store(#[source] o3k_store::StoreError),
}
