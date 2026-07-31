//! Small Placement-compatible inventory and allocation ledger.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
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
}

#[derive(Clone)]
pub struct PlacementLedger {
    root: PathBuf,
    state: Arc<Mutex<BTreeMap<String, ResourceProvider>>>,
}

impl PlacementLedger {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PlacementError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(PlacementError::Storage)?;
        let path = root.join("placement.json");
        let providers = if path.exists() {
            serde_json::from_slice(&fs::read(path).map_err(PlacementError::Storage)?)
                .map_err(PlacementError::Corrupt)?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            root,
            state: Arc::new(Mutex::new(providers)),
        })
    }

    pub fn register_provider(
        &self,
        node_id: &str,
        inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        if node_id.trim().is_empty() {
            return Err(PlacementError::InvalidAllocation);
        }
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let provider = state
            .entry(node_id.to_owned())
            .or_insert_with(|| ResourceProvider {
                id: node_id.to_owned(),
                node_id: node_id.to_owned(),
                state: ProviderState::Enabled,
                generation: 0,
                inventories: BTreeMap::new(),
                allocations: BTreeMap::new(),
            });
        provider.inventories = inventories;
        provider.generation = provider.generation.saturating_add(1);
        let result = provider.clone();
        persist(&self.root, &state)?;
        Ok(result)
    }

    pub fn refresh_inventory(
        &self,
        provider_id: &str,
        generation: u64,
        inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        if generation != provider.generation {
            return Err(PlacementError::StaleGeneration);
        }
        provider.inventories = inventories;
        provider.generation = provider.generation.saturating_add(1);
        let result = provider.clone();
        persist(&self.root, &state)?;
        Ok(result)
    }

    pub fn allocate(
        &self,
        provider_id: &str,
        allocation_id: &str,
        consumer_id: &str,
        resources: BTreeMap<String, u64>,
        generation: u64,
    ) -> Result<Allocation, PlacementError> {
        if allocation_id.is_empty()
            || consumer_id.is_empty()
            || resources.is_empty()
            || resources.values().any(|value| *value == 0)
        {
            return Err(PlacementError::InvalidAllocation);
        }
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        if let Some(existing) = provider.allocations.get(allocation_id) {
            if existing.consumer_id == consumer_id && existing.resources == resources {
                return Ok(existing.clone());
            }
            return Err(PlacementError::InvalidAllocation);
        }
        if generation != provider.generation {
            return Err(PlacementError::StaleGeneration);
        }
        if !matches!(provider.state, ProviderState::Enabled) {
            return Err(PlacementError::NotSchedulable);
        }
        if resources.iter().any(|(class, amount)| {
            provider
                .inventories
                .get(class)
                .is_none_or(|inventory| inventory.available() < *amount)
        }) {
            return Err(PlacementError::OverCapacity);
        }
        for (class, amount) in &resources {
            provider
                .inventories
                .get_mut(class)
                .ok_or(PlacementError::OverCapacity)?
                .used += amount;
        }
        let allocation = Allocation {
            provider_id: provider_id.to_owned(),
            consumer_id: consumer_id.to_owned(),
            resources,
        };
        provider
            .allocations
            .insert(allocation_id.to_owned(), allocation.clone());
        provider.generation += 1;
        persist(&self.root, &state)?;
        Ok(allocation)
    }

    pub fn release(&self, provider_id: &str, allocation_id: &str) -> Result<(), PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        if let Some(allocation) = provider.allocations.remove(allocation_id) {
            for (class, amount) in allocation.resources {
                if let Some(inventory) = provider.inventories.get_mut(&class) {
                    inventory.used = inventory.used.saturating_sub(amount);
                }
            }
            provider.generation += 1;
            persist(&self.root, &state)?;
        }
        Ok(())
    }

    pub fn set_state(
        &self,
        provider_id: &str,
        state_value: ProviderState,
    ) -> Result<(), PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        provider.state = state_value;
        provider.generation += 1;
        persist(&self.root, &state)
    }
    pub fn provider(&self, provider_id: &str) -> Result<ResourceProvider, PlacementError> {
        self.state
            .lock()
            .map_err(|_| PlacementError::Lock)?
            .get(provider_id)
            .cloned()
            .ok_or(PlacementError::NotFound)
    }
}

fn persist(root: &Path, state: &BTreeMap<String, ResourceProvider>) -> Result<(), PlacementError> {
    let temporary = root.join(format!("placement.json.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(state).map_err(PlacementError::Corrupt)?,
    )
    .map_err(PlacementError::Storage)?;
    fs::rename(&temporary, root.join("placement.json")).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        PlacementError::Storage(error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inventory() -> BTreeMap<String, Inventory> {
        BTreeMap::from([
            (
                VCPU.to_owned(),
                Inventory {
                    total: 4,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
            (
                MEMORY_MB.to_owned(),
                Inventory {
                    total: 4096,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
        ])
    }
    #[test]
    fn allocation_is_atomic_idempotent_and_restartable() -> Result<(), PlacementError> {
        let root = std::env::temp_dir().join(format!("o3k-placement-{}", std::process::id()));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.register_provider("node-1", inventory())?;
        let allocation = BTreeMap::from([(VCPU.to_owned(), 2)]);
        let first = ledger.allocate(
            "node-1",
            "alloc-1",
            "server-1",
            allocation.clone(),
            provider.generation,
        )?;
        assert_eq!(
            ledger.allocate(
                "node-1",
                "alloc-1",
                "server-1",
                allocation.clone(),
                provider.generation
            )?,
            first
        );
        assert!(matches!(
            ledger.allocate(
                "node-1",
                "alloc-2",
                "server-2",
                BTreeMap::from([(VCPU.to_owned(), 3)]),
                ledger.provider("node-1")?.generation
            ),
            Err(PlacementError::OverCapacity)
        ));
        let reopened = PlacementLedger::open(&root)?;
        assert_eq!(reopened.provider("node-1")?.allocations.len(), 1);
        reopened.release("node-1", "alloc-1")?;
        reopened.release("node-1", "alloc-1")?;
        fs::remove_dir_all(root).map_err(PlacementError::Storage)?;
        Ok(())
    }
    #[test]
    fn stale_and_unschedulable_updates_are_rejected() -> Result<(), PlacementError> {
        let root = std::env::temp_dir().join(format!("o3k-placement-state-{}", std::process::id()));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.register_provider("node-1", inventory())?;
        assert!(matches!(
            ledger.refresh_inventory("node-1", provider.generation - 1, inventory()),
            Err(PlacementError::StaleGeneration)
        ));
        ledger.set_state("node-1", ProviderState::Unavailable)?;
        let current = ledger.provider("node-1")?;
        assert!(matches!(
            ledger.allocate(
                "node-1",
                "a",
                "s",
                BTreeMap::from([(VCPU.to_owned(), 1)]),
                current.generation
            ),
            Err(PlacementError::NotSchedulable)
        ));
        fs::remove_dir_all(root).map_err(PlacementError::Storage)?;
        Ok(())
    }
}
