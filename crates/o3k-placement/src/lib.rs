//! Small Placement-compatible inventory and allocation ledger.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
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

fn map_store_error(error: o3k_store::StoreError) -> PlacementError {
    match error {
        o3k_store::StoreError::PlacementProviderNotFound => PlacementError::NotFound,
        o3k_store::StoreError::PlacementStaleGeneration => PlacementError::StaleGeneration,
        o3k_store::StoreError::PlacementAllocationConflict => PlacementError::InvalidAllocation,
        o3k_store::StoreError::PlacementIntentConflict => PlacementError::InvalidAllocation,
        other => PlacementError::Store(other),
    }
}

#[derive(Clone)]
pub struct PlacementLedger {
    repository: Arc<dyn o3k_store::PlacementRepository>,
}

impl PlacementLedger {
    pub async fn open(
        root: impl Into<PathBuf>,
        repository: Arc<dyn o3k_store::PlacementRepository>,
    ) -> Result<Self, PlacementError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(PlacementError::Storage)?;
        // Either legacy journal may still exist: the two files are renamed
        // independently after a successful import, so a crash between the
        // renames must still import and rename the remaining file.
        if root.join("placement.json").exists() || root.join("allocation-intents.json").exists() {
            import_legacy_files(&root, repository.as_ref()).await?;
        }
        Ok(Self { repository })
    }

    pub async fn begin_allocation_intent(
        &self,
        provider_id: &str,
        allocation_id: &str,
        consumer_id: &str,
        resources: BTreeMap<String, u64>,
    ) -> Result<AllocationIntent, PlacementError> {
        if provider_id.is_empty()
            || allocation_id.is_empty()
            || consumer_id.is_empty()
            || resources.is_empty()
            || resources.values().any(|value| *value == 0)
        {
            return Err(PlacementError::InvalidAllocation);
        }
        let intent = AllocationIntent {
            provider_id: provider_id.to_owned(),
            allocation_id: allocation_id.to_owned(),
            consumer_id: consumer_id.to_owned(),
            resources,
        };
        self.repository
            .upsert_intent(&intent_to_record(&intent))
            .await
            .map_err(map_store_error)?;
        Ok(intent)
    }

    pub async fn allocation_intent(
        &self,
        allocation_id: &str,
    ) -> Result<Option<AllocationIntent>, PlacementError> {
        let stored = self
            .repository
            .get_intent(allocation_id)
            .await
            .map_err(map_store_error)?;
        Ok(stored.as_ref().map(intent_from_record))
    }

    pub async fn commit_allocation_intent(
        &self,
        intent: &AllocationIntent,
        generation: u64,
    ) -> Result<Allocation, PlacementError> {
        let stored = match self.allocation_intent(&intent.allocation_id).await? {
            Some(stored) => stored,
            None => match self.provider(&intent.provider_id).await {
                Ok(provider) => provider
                    .allocations
                    .get(&intent.allocation_id)
                    .filter(|allocation| {
                        allocation.consumer_id == intent.consumer_id
                            && allocation.resources == intent.resources
                    })
                    .map(|_| intent.clone()),
                Err(_) => None,
            }
            .ok_or(PlacementError::InvalidAllocation)?,
        };
        if stored != *intent {
            return Err(PlacementError::InvalidAllocation);
        }
        let allocation = self
            .allocate(
                &intent.provider_id,
                &intent.allocation_id,
                &intent.consumer_id,
                intent.resources.clone(),
                generation,
            )
            .await?;
        self.repository
            .delete_intent(&intent.allocation_id)
            .await
            .map_err(map_store_error)?;
        Ok(allocation)
    }

    /// Abandons one exact pending intent after a candidate reservation could
    /// not be committed. The identity check prevents a retry from deleting a
    /// newer request that reused the same allocation key.
    pub async fn abandon_allocation_intent(
        &self,
        intent: &AllocationIntent,
    ) -> Result<(), PlacementError> {
        if let Some(stored) = self.allocation_intent(&intent.allocation_id).await? {
            if stored != *intent {
                return Err(PlacementError::InvalidAllocation);
            }
            self.repository
                .delete_intent(&intent.allocation_id)
                .await
                .map_err(map_store_error)?;
        }
        Ok(())
    }

    /// Release allocations and pending intents whose consumers are absent
    /// from the durable control-plane resource set supplied by the caller.
    /// The result is deterministic and the changes are persisted before the
    /// method returns, so reopening the ledger cannot resurrect them.
    pub async fn reconcile_consumers<I, S>(
        &self,
        durable_consumer_ids: I,
    ) -> Result<ReconciliationReport, PlacementError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let consumers: Vec<String> = durable_consumer_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
        let record = self
            .repository
            .reconcile_consumers(&consumers)
            .await
            .map_err(map_store_error)?;
        let mut orphaned_allocations: Vec<OrphanedAllocation> = record
            .orphaned_allocations
            .iter()
            .map(|allocation| OrphanedAllocation {
                provider_id: allocation.provider_id.clone(),
                allocation_id: allocation.id.clone(),
                consumer_id: allocation.consumer_id.clone(),
                resources: resource_map(&allocation.resources),
            })
            .collect();
        orphaned_allocations.sort_by(|left, right| {
            (&left.provider_id, &left.allocation_id)
                .cmp(&(&right.provider_id, &right.allocation_id))
        });
        let abandoned_intents: Vec<AllocationIntent> = record
            .abandoned_intents
            .iter()
            .map(intent_from_record)
            .collect();
        Ok(ReconciliationReport {
            orphaned_allocations,
            abandoned_intents,
        })
    }

    pub async fn register_provider(
        &self,
        node_id: &str,
        inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        if node_id.trim().is_empty() {
            return Err(PlacementError::InvalidAllocation);
        }
        let record = self
            .repository
            .register_provider(node_id, &inventory_records(&inventories))
            .await
            .map_err(map_store_error)?;
        provider_from_record(&record)
    }

    pub async fn refresh_inventory(
        &self,
        provider_id: &str,
        generation: u64,
        inventories: BTreeMap<String, Inventory>,
    ) -> Result<ResourceProvider, PlacementError> {
        let record = self
            .repository
            .refresh_inventories(provider_id, generation, &inventory_records(&inventories))
            .await
            .map_err(map_store_error)?;
        provider_from_record(&record)
    }

    /// Reconciles a provider snapshot while retaining allocations owned by
    /// this ledger. Reported `used` values are not trusted: usage is derived
    /// from the durable allocation map so a capability refresh cannot erase
    /// reservations or make them available twice.
    pub async fn sync_provider(
        &self,
        node_id: &str,
        inventories: BTreeMap<String, Inventory>,
        state_value: ProviderState,
    ) -> Result<ResourceProvider, PlacementError> {
        if node_id.trim().is_empty() {
            return Err(PlacementError::InvalidAllocation);
        }
        let record = self
            .repository
            .sync_provider(
                node_id,
                provider_state_as_str(state_value),
                &inventory_records(&inventories),
            )
            .await
            .map_err(map_store_error)?;
        provider_from_record(&record)
    }

    pub async fn allocate(
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
        let record = self
            .repository
            .get_provider(provider_id)
            .await
            .map_err(map_store_error)?
            .ok_or(PlacementError::NotFound)?;
        if let Some(existing) = record
            .allocations
            .iter()
            .find(|allocation| allocation.id == allocation_id)
        {
            let allocation = allocation_from_record(existing);
            if allocation.consumer_id == consumer_id && allocation.resources == resources {
                return Ok(allocation);
            }
            return Err(PlacementError::InvalidAllocation);
        }
        if record.generation != generation {
            return Err(PlacementError::StaleGeneration);
        }
        if provider_state_from_str(&record.state) != Some(ProviderState::Enabled) {
            return Err(PlacementError::NotSchedulable);
        }
        let inventories = inventory_map(&record.inventories);
        if resources.iter().any(|(class, amount)| {
            inventories
                .get(class)
                .is_none_or(|inventory| inventory.available() < *amount)
        }) {
            return Err(PlacementError::OverCapacity);
        }
        let allocation_record = o3k_store::PlacementAllocationRecord {
            id: allocation_id.to_owned(),
            provider_id: provider_id.to_owned(),
            consumer_id: consumer_id.to_owned(),
            resources: resource_records(&resources),
        };
        let committed = self
            .repository
            .commit_allocation(provider_id, generation, &allocation_record)
            .await
            .map_err(map_store_error)?;
        Ok(allocation_from_record(&committed))
    }

    pub async fn release(
        &self,
        provider_id: &str,
        allocation_id: &str,
    ) -> Result<(), PlacementError> {
        self.repository
            .release_allocation(provider_id, allocation_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn set_state(
        &self,
        provider_id: &str,
        state_value: ProviderState,
    ) -> Result<(), PlacementError> {
        self.repository
            .set_provider_state(provider_id, provider_state_as_str(state_value))
            .await
            .map_err(map_store_error)
    }

    pub async fn provider(&self, provider_id: &str) -> Result<ResourceProvider, PlacementError> {
        let record = self
            .repository
            .get_provider(provider_id)
            .await
            .map_err(map_store_error)?
            .ok_or(PlacementError::NotFound)?;
        provider_from_record(&record)
    }

    pub async fn providers(&self) -> Result<Vec<ResourceProvider>, PlacementError> {
        let records = self
            .repository
            .list_providers()
            .await
            .map_err(map_store_error)?;
        records.iter().map(provider_from_record).collect()
    }
}

fn provider_state_as_str(state: ProviderState) -> &'static str {
    match state {
        ProviderState::Enabled => "Enabled",
        ProviderState::Draining => "Draining",
        ProviderState::Unavailable => "Unavailable",
        ProviderState::Deleted => "Deleted",
    }
}

fn provider_state_from_str(value: &str) -> Option<ProviderState> {
    match value {
        "Enabled" => Some(ProviderState::Enabled),
        "Draining" => Some(ProviderState::Draining),
        "Unavailable" => Some(ProviderState::Unavailable),
        "Deleted" => Some(ProviderState::Deleted),
        _ => None,
    }
}

fn inventory_records(
    inventories: &BTreeMap<String, Inventory>,
) -> Vec<o3k_store::PlacementInventoryRecord> {
    inventories
        .iter()
        .map(|(class, inventory)| o3k_store::PlacementInventoryRecord {
            resource_class: class.clone(),
            total: inventory.total,
            reserved: inventory.reserved,
            allocation_ratio: inventory.allocation_ratio,
            used: inventory.used,
        })
        .collect()
}

fn inventory_map(records: &[o3k_store::PlacementInventoryRecord]) -> BTreeMap<String, Inventory> {
    records
        .iter()
        .map(|record| {
            (
                record.resource_class.clone(),
                Inventory {
                    total: record.total,
                    reserved: record.reserved,
                    allocation_ratio: record.allocation_ratio,
                    used: record.used,
                },
            )
        })
        .collect()
}

fn resource_records(resources: &BTreeMap<String, u64>) -> Vec<o3k_store::PlacementResourceRecord> {
    resources
        .iter()
        .map(|(class, amount)| o3k_store::PlacementResourceRecord {
            resource_class: class.clone(),
            amount: *amount,
        })
        .collect()
}

fn resource_map(records: &[o3k_store::PlacementResourceRecord]) -> BTreeMap<String, u64> {
    records
        .iter()
        .map(|record| (record.resource_class.clone(), record.amount))
        .collect()
}

fn provider_from_record(
    record: &o3k_store::PlacementProviderRecord,
) -> Result<ResourceProvider, PlacementError> {
    let state = provider_state_from_str(&record.state).ok_or_else(|| {
        PlacementError::Corrupt(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown placement provider state",
        )))
    })?;
    Ok(ResourceProvider {
        id: record.id.clone(),
        node_id: record.node_id.clone(),
        state,
        generation: record.generation,
        inventories: inventory_map(&record.inventories),
        allocations: record
            .allocations
            .iter()
            .map(|allocation| (allocation.id.clone(), allocation_from_record(allocation)))
            .collect(),
    })
}

fn allocation_from_record(record: &o3k_store::PlacementAllocationRecord) -> Allocation {
    Allocation {
        provider_id: record.provider_id.clone(),
        consumer_id: record.consumer_id.clone(),
        resources: resource_map(&record.resources),
    }
}

fn intent_from_record(record: &o3k_store::PlacementIntentRecord) -> AllocationIntent {
    AllocationIntent {
        provider_id: record.provider_id.clone(),
        allocation_id: record.id.clone(),
        consumer_id: record.consumer_id.clone(),
        resources: resource_map(&record.resources),
    }
}

fn provider_to_record(provider: &ResourceProvider) -> o3k_store::PlacementProviderRecord {
    o3k_store::PlacementProviderRecord {
        id: provider.id.clone(),
        node_id: provider.node_id.clone(),
        state: provider_state_as_str(provider.state).to_owned(),
        generation: provider.generation,
        inventories: inventory_records(&provider.inventories),
        allocations: provider
            .allocations
            .iter()
            .map(|(id, allocation)| o3k_store::PlacementAllocationRecord {
                id: id.clone(),
                provider_id: allocation.provider_id.clone(),
                consumer_id: allocation.consumer_id.clone(),
                resources: resource_records(&allocation.resources),
            })
            .collect(),
    }
}

fn intent_to_record(intent: &AllocationIntent) -> o3k_store::PlacementIntentRecord {
    o3k_store::PlacementIntentRecord {
        id: intent.allocation_id.clone(),
        provider_id: intent.provider_id.clone(),
        consumer_id: intent.consumer_id.clone(),
        resources: resource_records(&intent.resources),
    }
}

/// Imports the legacy file-backed journals into the repository once, then
/// renames the source files so a restart never reads them again. Every insert
/// is row-granular and skip-if-present, so a crash between inserts and the
/// renames re-imports without duplicating state. Any corrupt file fails the
/// import closed and leaves the files untouched.
async fn import_legacy_files(
    root: &Path,
    repository: &dyn o3k_store::PlacementRepository,
) -> Result<(), PlacementError> {
    let providers_path = root.join("placement.json");
    let providers: BTreeMap<String, ResourceProvider> = if providers_path.exists() {
        serde_json::from_slice(&fs::read(&providers_path).map_err(PlacementError::Storage)?)
            .map_err(PlacementError::Corrupt)?
    } else {
        BTreeMap::new()
    };
    let intents_path = root.join("allocation-intents.json");
    let intents: BTreeMap<String, AllocationIntent> = if intents_path.exists() {
        serde_json::from_slice(&fs::read(&intents_path).map_err(PlacementError::Storage)?)
            .map_err(PlacementError::Corrupt)?
    } else {
        BTreeMap::new()
    };
    for provider in providers.values() {
        repository
            .import_provider(&provider_to_record(provider))
            .await
            .map_err(map_store_error)?;
    }
    for intent in intents.values() {
        match repository.upsert_intent(&intent_to_record(intent)).await {
            Ok(_) => {}
            Err(o3k_store::StoreError::PlacementIntentConflict) => {
                return Err(PlacementError::InvalidAllocation);
            }
            Err(error) => return Err(map_store_error(error)),
        }
    }
    let _ = fs::rename(&providers_path, root.join("placement.json.imported"));
    let _ = fs::rename(&intents_path, root.join("allocation-intents.json.imported"));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

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

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("o3k-placement-{label}-{}", Uuid::now_v7()))
    }

    async fn test_ledger(root: &Path, store: &o3k_store::testkit::TestStore) -> PlacementLedger {
        let repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(store.clone());
        PlacementLedger::open(root, repository)
            .await
            .expect("ledger opens")
    }

    #[tokio::test]
    async fn allocation_is_atomic_idempotent_and_restartable() -> Result<(), PlacementError> {
        let root = test_root("allocation");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        let allocation = BTreeMap::from([(VCPU.to_owned(), 2)]);
        let first = ledger
            .allocate(
                "node-1",
                "alloc-1",
                "server-1",
                allocation.clone(),
                provider.generation,
            )
            .await?;
        assert_eq!(
            ledger
                .allocate(
                    "node-1",
                    "alloc-1",
                    "server-1",
                    allocation.clone(),
                    provider.generation
                )
                .await?,
            first
        );
        assert!(matches!(
            ledger
                .allocate(
                    "node-1",
                    "alloc-2",
                    "server-2",
                    BTreeMap::from([(VCPU.to_owned(), 3)]),
                    ledger.provider("node-1").await?.generation
                )
                .await,
            Err(PlacementError::OverCapacity)
        ));
        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        assert_eq!(reopened.provider("node-1").await?.allocations.len(), 1);
        reopened.release("node-1", "alloc-1").await?;
        reopened.release("node-1", "alloc-1").await?;
        drop(reopened);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn stale_and_unschedulable_updates_are_rejected() -> Result<(), PlacementError> {
        let root = test_root("state");
        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        assert!(matches!(
            ledger
                .refresh_inventory("node-1", provider.generation - 1, inventory())
                .await,
            Err(PlacementError::StaleGeneration)
        ));
        ledger
            .set_state("node-1", ProviderState::Unavailable)
            .await?;
        let current = ledger.provider("node-1").await?;
        assert!(matches!(
            ledger
                .allocate(
                    "node-1",
                    "a",
                    "s",
                    BTreeMap::from([(VCPU.to_owned(), 1)]),
                    current.generation
                )
                .await,
            Err(PlacementError::NotSchedulable)
        ));
        drop(ledger);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn refresh_inventory_preserves_durable_allocation_usage() -> Result<(), PlacementError> {
        let root = test_root("refresh");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        ledger
            .allocate(
                "node-1",
                "allocation-1",
                "server-1",
                BTreeMap::from([(VCPU.to_owned(), 2)]),
                provider.generation,
            )
            .await?;
        let current = ledger.provider("node-1").await?;
        let refreshed = ledger
            .refresh_inventory("node-1", current.generation, inventory())
            .await?;
        assert_eq!(refreshed.inventories[VCPU].used, 2);
        assert!(matches!(
            ledger
                .allocate(
                    "node-1",
                    "allocation-2",
                    "server-2",
                    BTreeMap::from([(VCPU.to_owned(), 3)]),
                    refreshed.generation
                )
                .await,
            Err(PlacementError::OverCapacity)
        ));
        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        assert_eq!(reopened.provider("node-1").await?.inventories[VCPU].used, 2);
        drop(reopened);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn reregister_reconciles_usage_and_preserves_capacity_after_reopen()
    -> Result<(), PlacementError> {
        let root = test_root("reregister");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        ledger
            .allocate(
                "node-1",
                "allocation-1",
                "server-1",
                BTreeMap::from([(VCPU.to_owned(), 2), (MEMORY_MB.to_owned(), 1024)]),
                provider.generation,
            )
            .await?;

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

        let reregistered = ledger.register_provider("node-1", reported).await?;
        assert_eq!(reregistered.inventories[VCPU].used, 2);
        assert_eq!(reregistered.inventories[MEMORY_MB].used, 1024);
        assert_eq!(reregistered.inventories["CUSTOM_RESOURCE"].used, 0);
        assert!(matches!(
            ledger
                .allocate(
                    "node-1",
                    "allocation-2",
                    "server-2",
                    BTreeMap::from([(VCPU.to_owned(), 3)]),
                    reregistered.generation
                )
                .await,
            Err(PlacementError::OverCapacity)
        ));

        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        let persisted = reopened.provider("node-1").await?;
        assert_eq!(persisted.inventories[VCPU].used, 2);
        assert_eq!(persisted.inventories[MEMORY_MB].used, 1024);
        assert_eq!(persisted.allocations.len(), 1);
        drop(reopened);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn sync_preserves_allocations_across_capacity_refresh_and_unavailability()
    -> Result<(), PlacementError> {
        let root = test_root("sync");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger
            .sync_provider("node-1", inventory(), ProviderState::Enabled)
            .await?;
        ledger
            .allocate(
                "node-1",
                "allocation-1",
                "server-1",
                BTreeMap::from([(VCPU.to_owned(), 2)]),
                provider.generation,
            )
            .await?;
        let refreshed = ledger
            .sync_provider(
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
            )
            .await?;
        assert_eq!(refreshed.inventories[VCPU].used, 2);
        assert_eq!(refreshed.allocations.len(), 1);
        assert_eq!(refreshed.state, ProviderState::Unavailable);
        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        assert_eq!(reopened.provider("node-1").await?.allocations.len(), 1);
        drop(reopened);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn allocation_intent_is_restart_safe_and_commit_is_idempotent()
    -> Result<(), PlacementError> {
        let root = test_root("intent");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        let resources = BTreeMap::from([(VCPU.to_owned(), 1)]);
        let intent = ledger
            .begin_allocation_intent("node-1", "allocation-1", "server-1", resources.clone())
            .await?;

        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        assert_eq!(
            reopened.allocation_intent("allocation-1").await?,
            Some(intent.clone())
        );
        let allocation = reopened
            .commit_allocation_intent(&intent, provider.generation)
            .await?;
        assert_eq!(allocation.consumer_id, "server-1");
        assert_eq!(reopened.allocation_intent("allocation-1").await?, None);
        assert_eq!(
            reopened
                .commit_allocation_intent(&intent, provider.generation)
                .await?,
            allocation
        );
        assert_eq!(reopened.provider("node-1").await?.allocations.len(), 1);

        drop(reopened);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let final_state = test_ledger(&root, &store).await;
        assert_eq!(final_state.provider("node-1").await?.allocations.len(), 1);
        assert_eq!(final_state.allocation_intent("allocation-1").await?, None);
        drop(final_state);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn reconciliation_releases_orphans_and_abandons_pending_intents_after_restart()
    -> Result<(), PlacementError> {
        let root = test_root("reconcile");
        let db_path = root.join("placement.db");
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let provider = ledger.register_provider("node-1", inventory()).await?;
        let resources = BTreeMap::from([(VCPU.to_owned(), 1)]);
        let retained = ledger
            .begin_allocation_intent(
                "node-1",
                "allocation-retained",
                "server-retained",
                resources.clone(),
            )
            .await?;
        ledger
            .commit_allocation_intent(&retained, provider.generation)
            .await?;
        let orphaned = ledger
            .begin_allocation_intent(
                "node-1",
                "allocation-orphaned",
                "server-orphaned",
                resources,
            )
            .await?;
        ledger
            .commit_allocation_intent(&orphaned, ledger.provider("node-1").await?.generation)
            .await?;
        let pending = ledger
            .begin_allocation_intent(
                "node-1",
                "allocation-pending",
                "server-pending",
                BTreeMap::from([(VCPU.to_owned(), 1)]),
            )
            .await?;

        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        let report = reopened
            .reconcile_consumers(["server-retained".to_owned()])
            .await?;
        assert_eq!(report.orphaned_allocations.len(), 1);
        assert_eq!(
            report.orphaned_allocations[0].allocation_id,
            "allocation-orphaned"
        );
        assert_eq!(report.abandoned_intents, vec![pending]);
        let current = reopened.provider("node-1").await?;
        assert_eq!(current.allocations.len(), 1);
        assert_eq!(current.inventories[VCPU].used, 1);
        assert_eq!(
            reopened.allocation_intent("allocation-pending").await?,
            None
        );

        drop(reopened);
        drop(store);
        let store = o3k_store::testkit::open_file(&db_path)
            .await
            .map_err(map_store_error)?;
        let final_state = test_ledger(&root, &store).await;
        assert_eq!(final_state.provider("node-1").await?.allocations.len(), 1);
        assert_eq!(
            final_state.provider("node-1").await?.inventories[VCPU].used,
            1
        );
        drop(final_state);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    struct FailingCommitRepository {
        inner: o3k_store::testkit::TestStore,
        fail_commits: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl o3k_store::PlacementRepository for FailingCommitRepository {
        async fn get_provider(
            &self,
            provider_id: &str,
        ) -> Result<Option<o3k_store::PlacementProviderRecord>, o3k_store::StoreError> {
            self.inner.get_provider(provider_id).await
        }
        async fn list_providers(
            &self,
        ) -> Result<Vec<o3k_store::PlacementProviderRecord>, o3k_store::StoreError> {
            self.inner.list_providers().await
        }
        async fn register_provider(
            &self,
            node_id: &str,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner.register_provider(node_id, inventories).await
        }
        async fn sync_provider(
            &self,
            node_id: &str,
            state: &str,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner.sync_provider(node_id, state, inventories).await
        }
        async fn refresh_inventories(
            &self,
            provider_id: &str,
            expected_generation: u64,
            inventories: &[o3k_store::PlacementInventoryRecord],
        ) -> Result<o3k_store::PlacementProviderRecord, o3k_store::StoreError> {
            self.inner
                .refresh_inventories(provider_id, expected_generation, inventories)
                .await
        }
        async fn set_provider_state(
            &self,
            provider_id: &str,
            state: &str,
        ) -> Result<(), o3k_store::StoreError> {
            self.inner.set_provider_state(provider_id, state).await
        }
        async fn commit_allocation(
            &self,
            provider_id: &str,
            expected_generation: u64,
            allocation: &o3k_store::PlacementAllocationRecord,
        ) -> Result<o3k_store::PlacementAllocationRecord, o3k_store::StoreError> {
            if self.fail_commits.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(o3k_store::StoreError::ResourceNotFound);
            }
            self.inner
                .commit_allocation(provider_id, expected_generation, allocation)
                .await
        }
        async fn release_allocation(
            &self,
            provider_id: &str,
            allocation_id: &str,
        ) -> Result<(), o3k_store::StoreError> {
            self.inner
                .release_allocation(provider_id, allocation_id)
                .await
        }
        async fn upsert_intent(
            &self,
            intent: &o3k_store::PlacementIntentRecord,
        ) -> Result<o3k_store::PlacementIntentRecord, o3k_store::StoreError> {
            self.inner.upsert_intent(intent).await
        }
        async fn get_intent(
            &self,
            allocation_id: &str,
        ) -> Result<Option<o3k_store::PlacementIntentRecord>, o3k_store::StoreError> {
            self.inner.get_intent(allocation_id).await
        }
        async fn list_intents(
            &self,
        ) -> Result<Vec<o3k_store::PlacementIntentRecord>, o3k_store::StoreError> {
            self.inner.list_intents().await
        }
        async fn delete_intent(&self, allocation_id: &str) -> Result<(), o3k_store::StoreError> {
            self.inner.delete_intent(allocation_id).await
        }
        async fn reconcile_consumers(
            &self,
            durable_consumer_ids: &[String],
        ) -> Result<o3k_store::PlacementReconcileRecord, o3k_store::StoreError> {
            self.inner.reconcile_consumers(durable_consumer_ids).await
        }
        async fn import_provider(
            &self,
            provider: &o3k_store::PlacementProviderRecord,
        ) -> Result<(), o3k_store::StoreError> {
            self.inner.import_provider(provider).await
        }
    }

    #[tokio::test]
    async fn failed_store_write_propagates_and_leaves_durable_state_unchanged()
    -> Result<(), PlacementError> {
        let root = test_root("store-failure");
        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let fail_commits = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let repository: Arc<dyn o3k_store::PlacementRepository> =
            Arc::new(FailingCommitRepository {
                inner: store.clone(),
                fail_commits: fail_commits.clone(),
            });
        let ledger = PlacementLedger::open(&root, repository).await?;
        let provider = ledger.register_provider("node-1", inventory()).await?;

        fail_commits.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            ledger
                .allocate(
                    "node-1",
                    "allocation-1",
                    "server-1",
                    BTreeMap::from([(VCPU.to_owned(), 1)]),
                    provider.generation,
                )
                .await,
            Err(PlacementError::Store(_))
        ));
        let stored = store
            .get_provider("node-1")
            .await
            .map_err(map_store_error)?
            .ok_or(PlacementError::NotFound)?;
        assert!(stored.allocations.is_empty());
        assert_eq!(stored.generation, provider.generation);

        fail_commits.store(false, std::sync::atomic::Ordering::SeqCst);
        let allocation = ledger
            .allocate(
                "node-1",
                "allocation-1",
                "server-1",
                BTreeMap::from([(VCPU.to_owned(), 1)]),
                provider.generation,
            )
            .await?;
        assert_eq!(allocation.consumer_id, "server-1");
        assert_eq!(ledger.provider("node-1").await?.allocations.len(), 1);
        drop(ledger);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_placement_files_are_imported_once_and_never_read_again()
    -> Result<(), PlacementError> {
        let root = test_root("legacy-import");
        fs::create_dir_all(&root).map_err(PlacementError::Storage)?;

        let mut inventories = inventory();
        inventories.insert(
            VCPU.to_owned(),
            Inventory {
                total: 4,
                reserved: 0,
                allocation_ratio: 1.0,
                used: 2,
            },
        );
        let allocations = BTreeMap::from([(
            "allocation-1".to_owned(),
            Allocation {
                provider_id: "node-a".to_owned(),
                consumer_id: "server-1".to_owned(),
                resources: BTreeMap::from([(VCPU.to_owned(), 2)]),
            },
        )]);
        let provider = ResourceProvider {
            id: "node-a".to_owned(),
            node_id: "node-a".to_owned(),
            state: ProviderState::Enabled,
            generation: 5,
            inventories,
            allocations,
        };
        let providers = BTreeMap::from([("node-a".to_owned(), provider)]);
        let intent = AllocationIntent {
            provider_id: "node-a".to_owned(),
            allocation_id: "intent-1".to_owned(),
            consumer_id: "server-pending".to_owned(),
            resources: BTreeMap::from([(VCPU.to_owned(), 1)]),
        };
        let intents = BTreeMap::from([("intent-1".to_owned(), intent.clone())]);
        fs::write(
            root.join("placement.json"),
            serde_json::to_vec_pretty(&providers).map_err(PlacementError::Corrupt)?,
        )
        .map_err(PlacementError::Storage)?;
        fs::write(
            root.join("allocation-intents.json"),
            serde_json::to_vec_pretty(&intents).map_err(PlacementError::Corrupt)?,
        )
        .map_err(PlacementError::Storage)?;

        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let imported = ledger.provider("node-a").await?;
        assert_eq!(imported.generation, 5);
        assert_eq!(imported.state, ProviderState::Enabled);
        assert_eq!(imported.allocations.len(), 1);
        let imported_allocation = imported
            .allocations
            .get("allocation-1")
            .ok_or(PlacementError::InvalidAllocation)?;
        assert_eq!(imported_allocation.consumer_id, "server-1");
        assert_eq!(imported.inventories[VCPU].used, 2);
        assert_eq!(imported.inventories[VCPU].available(), 2);
        assert_eq!(
            ledger.allocation_intent("intent-1").await?,
            Some(intent.clone())
        );
        assert!(root.join("placement.json.imported").exists());
        assert!(root.join("allocation-intents.json.imported").exists());
        assert!(!root.join("placement.json").exists());
        assert!(!root.join("allocation-intents.json").exists());

        drop(ledger);
        drop(store);
        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let reopened = test_ledger(&root, &store).await;
        assert!(reopened.providers().await?.is_empty());
        assert_eq!(reopened.allocation_intent("intent-1").await?, None);

        drop(reopened);
        drop(store);
        fs::write(root.join("placement.json"), b"not json").map_err(PlacementError::Storage)?;
        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let repository: Arc<dyn o3k_store::PlacementRepository> = Arc::new(store.clone());
        assert!(matches!(
            PlacementLedger::open(&root, repository).await,
            Err(PlacementError::Corrupt(_))
        ));
        assert!(root.join("placement.json").exists());
        assert!(
            store
                .get_provider("node-a")
                .await
                .map_err(map_store_error)?
                .is_none()
        );

        drop(store);
        fs::copy(
            root.join("placement.json.imported"),
            root.join("placement.json"),
        )
        .map_err(PlacementError::Storage)?;
        fs::copy(
            root.join("allocation-intents.json.imported"),
            root.join("allocation-intents.json"),
        )
        .map_err(PlacementError::Storage)?;
        let store = o3k_store::testkit::open_memory()
            .await
            .map_err(map_store_error)?;
        let ledger = test_ledger(&root, &store).await;
        let reimported = ledger.provider("node-a").await?;
        assert_eq!(reimported.generation, 5);
        assert_eq!(reimported.allocations.len(), 1);
        assert_eq!(
            ledger.allocation_intent("intent-1").await?,
            Some(intent.clone())
        );
        assert!(root.join("placement.json.imported").exists());
        assert!(!root.join("placement.json").exists());
        drop(ledger);
        drop(store);
        fs::remove_dir_all(&root).map_err(PlacementError::Storage)?;
        Ok(())
    }
}
