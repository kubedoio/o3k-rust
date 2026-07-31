//! Deterministic first-fit Nova scheduling backed by Placement allocations.

use o3k_placement::{
    Allocation, DISK_GB, MEMORY_MB, PlacementError, PlacementLedger, ProviderState, VCPU,
};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flavor {
    pub vcpus: u64,
    pub memory_mb: u64,
    pub disk_gb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleDecision {
    pub provider_id: String,
    pub allocation_id: String,
    pub allocation: Allocation,
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("no valid compute host is available")]
    NoValidHost,
    #[error("placement allocation conflict")]
    Conflict,
    #[error("invalid flavor")]
    InvalidFlavor,
    #[error("placement failure")]
    Placement(#[from] PlacementError),
}

#[derive(Clone)]
pub struct Scheduler {
    placement: PlacementLedger,
}

impl Scheduler {
    pub fn new(placement: PlacementLedger) -> Self {
        Self { placement }
    }

    pub fn schedule(
        &self,
        server_id: &str,
        flavor: Flavor,
    ) -> Result<ScheduleDecision, SchedulerError> {
        if server_id.is_empty() || flavor.vcpus == 0 || flavor.memory_mb == 0 {
            return Err(SchedulerError::InvalidFlavor);
        }
        let mut candidates = self
            .placement
            .providers()?
            .into_iter()
            .filter(|provider| {
                provider.state == ProviderState::Enabled
                    && provider
                        .inventories
                        .get(VCPU)
                        .is_some_and(|v| v.available() >= flavor.vcpus)
                    && provider
                        .inventories
                        .get(MEMORY_MB)
                        .is_some_and(|v| v.available() >= flavor.memory_mb)
                    && provider
                        .inventories
                        .get(DISK_GB)
                        .is_some_and(|v| v.available() >= flavor.disk_gb)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let left_free = left
                .inventories
                .values()
                .map(|v| v.available())
                .sum::<u64>();
            let right_free = right
                .inventories
                .values()
                .map(|v| v.available())
                .sum::<u64>();
            right_free
                .cmp(&left_free)
                .then_with(|| left.id.cmp(&right.id))
        });
        let resources = BTreeMap::from([
            (VCPU.to_owned(), flavor.vcpus),
            (MEMORY_MB.to_owned(), flavor.memory_mb),
            (DISK_GB.to_owned(), flavor.disk_gb),
        ]);
        for candidate in candidates {
            match self.placement.allocate(
                &candidate.id,
                &format!("allocation-{server_id}"),
                server_id,
                resources.clone(),
                candidate.generation,
            ) {
                Ok(allocation) => {
                    return Ok(ScheduleDecision {
                        provider_id: candidate.id,
                        allocation_id: format!("allocation-{server_id}"),
                        allocation,
                    });
                }
                Err(PlacementError::StaleGeneration | PlacementError::OverCapacity) => continue,
                Err(error) => return Err(SchedulerError::Placement(error)),
            }
        }
        Err(SchedulerError::NoValidHost)
    }

    pub fn release_terminal(&self, decision: &ScheduleDecision) -> Result<(), SchedulerError> {
        self.placement
            .release(&decision.provider_id, &decision.allocation_id)
            .map_err(SchedulerError::Placement)
    }
    pub fn retain_unknown(&self, _: &ScheduleDecision) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_placement::Inventory;
    fn inv(vcpu: u64) -> BTreeMap<String, Inventory> {
        BTreeMap::from([
            (
                VCPU.to_owned(),
                Inventory {
                    total: vcpu,
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
            (
                DISK_GB.to_owned(),
                Inventory {
                    total: 100,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
        ])
    }
    #[test]
    fn deterministic_selection_and_terminal_release() -> Result<(), SchedulerError> {
        let root = std::env::temp_dir().join(format!("o3k-scheduler-{}", std::process::id()));
        let placement = PlacementLedger::open(&root)?;
        placement.register_provider("node-b", inv(4))?;
        placement.register_provider("node-a", inv(4))?;
        let scheduler = Scheduler::new(placement.clone());
        let decision = scheduler.schedule(
            "server-1",
            Flavor {
                vcpus: 1,
                memory_mb: 512,
                disk_gb: 1,
            },
        )?;
        assert_eq!(decision.provider_id, "node-a");
        scheduler.release_terminal(&decision)?;
        assert_eq!(placement.provider("node-a")?.allocations.len(), 0);
        std::fs::remove_dir_all(root)
            .map_err(|error| SchedulerError::Placement(PlacementError::Storage(error)))?;
        Ok(())
    }
    #[test]
    fn unavailable_and_insufficient_hosts_are_skipped() -> Result<(), SchedulerError> {
        let root =
            std::env::temp_dir().join(format!("o3k-scheduler-nohost-{}", std::process::id()));
        let placement = PlacementLedger::open(&root)?;
        placement.register_provider("node-a", inv(1))?;
        placement.set_state("node-a", ProviderState::Unavailable)?;
        let scheduler = Scheduler::new(placement);
        assert!(matches!(
            scheduler.schedule(
                "server-1",
                Flavor {
                    vcpus: 2,
                    memory_mb: 512,
                    disk_gb: 1
                }
            ),
            Err(SchedulerError::NoValidHost)
        ));
        std::fs::remove_dir_all(root)
            .map_err(|error| SchedulerError::Placement(PlacementError::Storage(error)))?;
        Ok(())
    }
}
