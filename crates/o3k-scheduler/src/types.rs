//! Scheduling domain types: flavor, decision, errors.

use o3k_placement::Allocation;
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
    #[error("placement allocation is not owned by the requested server")]
    AllocationMismatch,
    #[error("invalid flavor")]
    InvalidFlavor,
    #[error("placement failure")]
    Placement(#[from] o3k_placement::PlacementError),
}
