//! Small Placement-compatible inventory and allocation ledger.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;
use uuid::Uuid;

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
        mut inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        if node_id.trim().is_empty() {
            return Err(PlacementError::InvalidAllocation);
        }
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let previous = state.clone();
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
        reconcile_inventory_usage(&mut inventories, &provider.allocations);
        provider.inventories = inventories;
        provider.generation = provider.generation.saturating_add(1);
        let result = provider.clone();
        persist_or_restore(&self.root, &mut state, previous)?;
        Ok(result)
    }

    pub fn refresh_inventory(
        &self,
        provider_id: &str,
        generation: u64,
        mut inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let previous = state.clone();
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        if generation != provider.generation {
            return Err(PlacementError::StaleGeneration);
        }
        reconcile_inventory_usage(&mut inventories, &provider.allocations);
        provider.inventories = inventories;
        provider.generation = provider.generation.saturating_add(1);
        let result = provider.clone();
        persist_or_restore(&self.root, &mut state, previous)?;
        Ok(result)
    }

    /// Reconciles a provider snapshot while retaining allocations owned by
    /// this ledger. Reported `used` values are not trusted: usage is derived
    /// from the durable allocation map so a capability refresh cannot erase
    /// reservations or make them available twice.
    pub fn sync_provider(
        &self,
        node_id: &str,
        mut inventories: BTreeMap<String, Inventory>,
        state_value: ProviderState,
    ) -> Result<ResourceProvider, PlacementError> {
        if node_id.trim().is_empty() {
            return Err(PlacementError::InvalidAllocation);
        }
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let previous = state.clone();
        let provider = state
            .entry(node_id.to_owned())
            .or_insert_with(|| ResourceProvider {
                id: node_id.to_owned(),
                node_id: node_id.to_owned(),
                state: state_value,
                generation: 0,
                inventories: BTreeMap::new(),
                allocations: BTreeMap::new(),
            });
        reconcile_inventory_usage(&mut inventories, &provider.allocations);
        provider.inventories = inventories;
        provider.state = state_value;
        provider.generation = provider.generation.saturating_add(1);
        let result = provider.clone();
        persist_or_restore(&self.root, &mut state, previous)?;
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
        let previous = state.clone();
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
        persist_or_restore(&self.root, &mut state, previous)?;
        Ok(allocation)
    }

    pub fn release(&self, provider_id: &str, allocation_id: &str) -> Result<(), PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let previous = state.clone();
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        if let Some(allocation) = provider.allocations.remove(allocation_id) {
            for (class, amount) in allocation.resources {
                if let Some(inventory) = provider.inventories.get_mut(&class) {
                    inventory.used = inventory.used.saturating_sub(amount);
                }
            }
            provider.generation += 1;
            persist_or_restore(&self.root, &mut state, previous)?;
        }
        Ok(())
    }

    pub fn set_state(
        &self,
        provider_id: &str,
        state_value: ProviderState,
    ) -> Result<(), PlacementError> {
        let mut state = self.state.lock().map_err(|_| PlacementError::Lock)?;
        let previous = state.clone();
        let provider = state.get_mut(provider_id).ok_or(PlacementError::NotFound)?;
        provider.state = state_value;
        provider.generation += 1;
        persist_or_restore(&self.root, &mut state, previous)
    }
    pub fn provider(&self, provider_id: &str) -> Result<ResourceProvider, PlacementError> {
        self.state
            .lock()
            .map_err(|_| PlacementError::Lock)?
            .get(provider_id)
            .cloned()
            .ok_or(PlacementError::NotFound)
    }
    pub fn providers(&self) -> Result<Vec<ResourceProvider>, PlacementError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| PlacementError::Lock)?
            .values()
            .cloned()
            .collect())
    }
}

fn persist(root: &Path, state: &BTreeMap<String, ResourceProvider>) -> Result<(), PlacementError> {
    let temporary = root.join(format!("placement.json.tmp-{}", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(state).map_err(PlacementError::Corrupt)?;
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(PlacementError::Storage(error));
    }
    if let Err(error) = fs::rename(&temporary, root.join("placement.json")) {
        let _ = fs::remove_file(&temporary);
        return Err(PlacementError::Storage(error));
    }
    Ok(())
}

fn persist_or_restore(
    root: &Path,
    state: &mut BTreeMap<String, ResourceProvider>,
    previous: BTreeMap<String, ResourceProvider>,
) -> Result<(), PlacementError> {
    match persist(root, state) {
        Ok(()) => Ok(()),
        Err(error) => {
            *state = previous;
            Err(error)
        }
    }
}

fn reconcile_inventory_usage(
    inventories: &mut BTreeMap<String, Inventory>,
    allocations: &BTreeMap<String, Allocation>,
) {
    for inventory in inventories.values_mut() {
        inventory.used = 0;
    }
    for allocation in allocations.values() {
        for (class, amount) in &allocation.resources {
            if let Some(inventory) = inventories.get_mut(class) {
                inventory.used = inventory.used.saturating_add(*amount);
            }
        }
    }
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
    fn failed_allocation_publication_rolls_back_memory_state() -> Result<(), PlacementError> {
        let root = std::env::temp_dir().join(format!(
            "o3k-placement-publication-rollback-{}",
            Uuid::now_v7()
        ));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.register_provider("node-1", inventory())?;

        // A directory at the final publication path makes the atomic rename
        // fail after the temporary state has been written.
        fs::remove_file(root.join("placement.json")).map_err(PlacementError::Storage)?;
        fs::create_dir(root.join("placement.json")).map_err(PlacementError::Storage)?;

        assert!(matches!(
            ledger.allocate(
                "node-1",
                "allocation-1",
                "server-1",
                BTreeMap::from([(VCPU.to_owned(), 1)]),
                provider.generation,
            ),
            Err(PlacementError::Storage(_))
        ));

        let current = ledger.provider("node-1")?;
        assert!(current.allocations.is_empty());
        assert_eq!(current.inventories[VCPU].used, 0);
        assert_eq!(current.generation, provider.generation);

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

    #[test]
    fn refresh_inventory_preserves_durable_allocation_usage() -> Result<(), PlacementError> {
        let root = std::env::temp_dir().join(format!("o3k-placement-refresh-{}", Uuid::now_v7()));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.register_provider("node-1", inventory())?;
        ledger.allocate(
            "node-1",
            "allocation-1",
            "server-1",
            BTreeMap::from([(VCPU.to_owned(), 2)]),
            provider.generation,
        )?;
        let current = ledger.provider("node-1")?;
        let refreshed = ledger.refresh_inventory("node-1", current.generation, inventory())?;
        assert_eq!(refreshed.inventories[VCPU].used, 2);
        assert!(matches!(
            ledger.allocate(
                "node-1",
                "allocation-2",
                "server-2",
                BTreeMap::from([(VCPU.to_owned(), 3)]),
                refreshed.generation
            ),
            Err(PlacementError::OverCapacity)
        ));
        let reopened = PlacementLedger::open(&root)?;
        assert_eq!(reopened.provider("node-1")?.inventories[VCPU].used, 2);
        fs::remove_dir_all(root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[test]
    fn reregister_reconciles_usage_and_preserves_capacity_after_reopen()
    -> Result<(), PlacementError> {
        let root =
            std::env::temp_dir().join(format!("o3k-placement-reregister-{}", Uuid::now_v7()));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.register_provider("node-1", inventory())?;
        ledger.allocate(
            "node-1",
            "allocation-1",
            "server-1",
            BTreeMap::from([(VCPU.to_owned(), 2), (MEMORY_MB.to_owned(), 1024)]),
            provider.generation,
        )?;

        let mut reported = inventory();
        reported
            .get_mut(VCPU)
            .ok_or(PlacementError::InvalidAllocation)?
            .used = 999;
        reported
            .get_mut(MEMORY_MB)
            .ok_or(PlacementError::InvalidAllocation)?
            .used = 999;
        reported.insert(
            "CUSTOM_RESOURCE".to_owned(),
            Inventory {
                total: 8,
                reserved: 0,
                allocation_ratio: 1.0,
                used: 999,
            },
        );

        let reregistered = ledger.register_provider("node-1", reported)?;
        assert_eq!(reregistered.inventories[VCPU].used, 2);
        assert_eq!(reregistered.inventories[MEMORY_MB].used, 1024);
        assert_eq!(reregistered.inventories["CUSTOM_RESOURCE"].used, 0);
        assert!(matches!(
            ledger.allocate(
                "node-1",
                "allocation-2",
                "server-2",
                BTreeMap::from([(VCPU.to_owned(), 3)]),
                reregistered.generation
            ),
            Err(PlacementError::OverCapacity)
        ));

        let reopened = PlacementLedger::open(&root)?;
        let persisted = reopened.provider("node-1")?;
        assert_eq!(persisted.inventories[VCPU].used, 2);
        assert_eq!(persisted.inventories[MEMORY_MB].used, 1024);
        assert_eq!(persisted.allocations.len(), 1);
        fs::remove_dir_all(root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[test]
    fn sync_preserves_allocations_across_capacity_refresh_and_unavailability()
    -> Result<(), PlacementError> {
        let root = std::env::temp_dir().join(format!("o3k-placement-sync-{}", std::process::id()));
        let ledger = PlacementLedger::open(&root)?;
        let provider = ledger.sync_provider("node-1", inventory(), ProviderState::Enabled)?;
        ledger.allocate(
            "node-1",
            "allocation-1",
            "server-1",
            BTreeMap::from([(VCPU.to_owned(), 2)]),
            provider.generation,
        )?;
        let refreshed = ledger.sync_provider(
            "node-1",
            BTreeMap::from([(
                VCPU.to_owned(),
                Inventory {
                    total: 2,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 999,
                },
            )]),
            ProviderState::Unavailable,
        )?;
        assert_eq!(refreshed.inventories[VCPU].used, 2);
        assert_eq!(refreshed.allocations.len(), 1);
        assert_eq!(refreshed.state, ProviderState::Unavailable);
        let reopened = PlacementLedger::open(&root)?;
        assert_eq!(reopened.provider("node-1")?.allocations.len(), 1);
        fs::remove_dir_all(root).map_err(PlacementError::Storage)?;
        Ok(())
    }
}
