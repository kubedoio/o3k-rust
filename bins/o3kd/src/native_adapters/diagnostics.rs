use async_trait::async_trait;
use o3k_native_api::diagnostics::{CapacityDimension, CapacityReader, CapacityStatus};
use o3k_placement::PlacementLedger;

/// Projects the durable Cloud Kernel Placement ledger into the bounded,
/// provider-neutral diagnostics contract.  Provider IDs and node identity are
/// intentionally discarded at this boundary.
pub struct CapacityReaderAdapter {
    pub placement: PlacementLedger,
}

#[async_trait]
impl CapacityReader for CapacityReaderAdapter {
    async fn read(&self) -> Result<CapacityStatus, ()> {
        const MAX_DIMENSIONS: usize = 64;
        let (records, enabled, degraded) = self
            .placement
            .capacity_summary(MAX_DIMENSIONS + 1)
            .await
            .map_err(|_| ())?;
        if records.len() > MAX_DIMENSIONS {
            return Err(());
        }
        if enabled == 0 && degraded == 0 {
            return Ok(CapacityStatus {
                available: false,
                reason: "capacity authority is configured".to_owned(),
                dimensions: Vec::new(),
            });
        }
        let dimensions = records
            .into_iter()
            .map(|record| CapacityDimension {
                resource_class: record.resource_class,
                total: record.total,
                reserved: record.reserved,
                used: record.used,
                available: record.available,
            })
            .collect::<Vec<_>>();
        Ok(CapacityStatus {
            available: enabled > 0 && dimensions.iter().any(|dimension| dimension.available > 0),
            reason: if degraded > 0 {
                "capacity authority is configured with degraded providers".to_owned()
            } else {
                "capacity authority is configured".to_owned()
            },
            dimensions,
        })
    }
}

#[cfg(test)]
fn aggregate_capacity(
    providers: Vec<o3k_placement::ResourceProvider>,
) -> Result<CapacityStatus, ()> {
    use o3k_placement::ProviderState;
    use std::collections::BTreeMap;
    let mut dimensions: BTreeMap<String, CapacityDimension> = BTreeMap::new();
    let mut schedulable = false;
    let mut degraded = false;
    for provider in providers {
        match provider.state {
            ProviderState::Enabled => {
                schedulable = true;
            }
            ProviderState::Draining | ProviderState::Unavailable => degraded = true,
            ProviderState::Deleted => continue,
        }
        for (resource_class, inventory) in provider.inventories {
            let entry =
                dimensions
                    .entry(resource_class.clone())
                    .or_insert_with(|| CapacityDimension {
                        resource_class,
                        total: 0,
                        reserved: 0,
                        used: 0,
                        available: 0,
                    });
            entry.total = entry.total.saturating_add(inventory.total);
            entry.reserved = entry.reserved.saturating_add(inventory.reserved);
            entry.used = entry.used.saturating_add(inventory.used);
            entry.available = entry.available.saturating_add(inventory.available());
        }
    }
    if dimensions.len() > 64 {
        return Err(());
    }
    let dimensions = dimensions.into_values().collect::<Vec<_>>();
    Ok(CapacityStatus {
        available: schedulable && dimensions.iter().any(|dimension| dimension.available > 0),
        reason: if degraded {
            "capacity authority is configured with degraded providers".to_owned()
        } else {
            "capacity authority is configured".to_owned()
        },
        dimensions,
    })
}

#[cfg(test)]
mod tests {
    use super::aggregate_capacity;
    use o3k_placement::{Inventory, ProviderState, ResourceProvider, VCPU};
    use std::collections::BTreeMap;

    #[test]
    fn aggregate_is_provider_neutral_and_uses_allocation_derived_available() {
        let mut inventories = BTreeMap::new();
        inventories.insert(
            VCPU.to_owned(),
            Inventory {
                total: 8,
                reserved: 2,
                allocation_ratio: 1.0,
                used: 1,
            },
        );
        let status_result = aggregate_capacity(vec![ResourceProvider {
            id: "private-provider-id".into(),
            node_id: "private-node-id".into(),
            state: ProviderState::Enabled,
            generation: 1,
            inventories,
            allocations: BTreeMap::new(),
        }]);
        assert!(status_result.is_ok());
        let Some(status) = status_result.ok() else {
            return;
        };
        assert!(status.available);
        assert_eq!(status.dimensions[0].available, 5);
        let encoded_result = serde_json::to_string(&status);
        assert!(encoded_result.is_ok());
        let Some(encoded) = encoded_result.ok() else {
            return;
        };
        assert!(!encoded.contains("private-provider-id"));
        assert!(!encoded.contains("private-node-id"));
    }
}
