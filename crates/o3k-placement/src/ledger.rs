//! Placement ledger — inventory and allocation management.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::types::{
    Allocation, AllocationIntent, Inventory, OrphanedAllocation, PlacementError, ProviderState,
    ReconciliationReport, ResourceProvider,
};

pub(crate) fn map_store_error(error: o3k_store::StoreError) -> PlacementError {
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

pub(crate) fn provider_state_as_str(state: ProviderState) -> &'static str {
    match state {
        ProviderState::Enabled => "Enabled",
        ProviderState::Draining => "Draining",
        ProviderState::Unavailable => "Unavailable",
        ProviderState::Deleted => "Deleted",
    }
}

pub(crate) fn provider_state_from_str(value: &str) -> Option<ProviderState> {
    match value {
        "Enabled" => Some(ProviderState::Enabled),
        "Draining" => Some(ProviderState::Draining),
        "Unavailable" => Some(ProviderState::Unavailable),
        "Deleted" => Some(ProviderState::Deleted),
        _ => None,
    }
}

pub(crate) fn inventory_records(
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

pub(crate) fn inventory_map(
    records: &[o3k_store::PlacementInventoryRecord],
) -> BTreeMap<String, Inventory> {
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

pub(crate) fn resource_records(
    resources: &BTreeMap<String, u64>,
) -> Vec<o3k_store::PlacementResourceRecord> {
    resources
        .iter()
        .map(|(class, amount)| o3k_store::PlacementResourceRecord {
            resource_class: class.clone(),
            amount: *amount,
        })
        .collect()
}

pub(crate) fn resource_map(
    records: &[o3k_store::PlacementResourceRecord],
) -> BTreeMap<String, u64> {
    records
        .iter()
        .map(|record| (record.resource_class.clone(), record.amount))
        .collect()
}

pub(crate) fn provider_from_record(
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

pub(crate) fn allocation_from_record(record: &o3k_store::PlacementAllocationRecord) -> Allocation {
    Allocation {
        provider_id: record.provider_id.clone(),
        consumer_id: record.consumer_id.clone(),
        resources: resource_map(&record.resources),
    }
}

pub(crate) fn intent_from_record(record: &o3k_store::PlacementIntentRecord) -> AllocationIntent {
    AllocationIntent {
        provider_id: record.provider_id.clone(),
        allocation_id: record.id.clone(),
        consumer_id: record.consumer_id.clone(),
        resources: resource_map(&record.resources),
    }
}

pub(crate) fn provider_to_record(
    provider: &ResourceProvider,
) -> o3k_store::PlacementProviderRecord {
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

pub(crate) fn intent_to_record(intent: &AllocationIntent) -> o3k_store::PlacementIntentRecord {
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
