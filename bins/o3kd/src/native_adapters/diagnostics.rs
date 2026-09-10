//! Production `o3kd` projection of canonical O3K diagnostics (#903).
//!
//! This adapter implements the bounded, read-only [`DiagnosticsReader`] port
//! by projecting canonical authority only:
//!
//! - services   -> shared lifecycle [`ManifestRegistry`];
//! - providers  -> agent [`AgentNodeRegistry`] + durable placement store;
//! - capacity   -> placement store capacity aggregate + agent observation clock;
//! - control-plane liveness -> coordination store `controller_sessions`;
//! - locations  -> canonical [`LocationRegistry`] (topology only).
//!
//! It never forwards node identity, agent epoch, capabilities, provider
//! secrets/connection strings, controller service principals, session ids,
//! manifest digests, or raw provider/controller error text. Freshness is a
//! first-class property: a stale or never-observed source is never reported
//! as `healthy`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use o3k_kernel::{ControllerState, ManifestRegistry};
use o3k_native_api::diagnostics::{
    CapacityDiagnostics, CapacityDimension, ComponentCounts, ControlPlaneStatus,
    ControllerDiagnostics, DIAGNOSTICS_VERSION, DiagnosticReason, DiagnosticStatus,
    DiagnosticsError, DiagnosticsPage, DiagnosticsReader, DiagnosticsSummary, LocationDiagnostics,
    MAX_CAPACITY_CLASSES, ProviderCapacityDimension, ProviderDiagnostics, ServiceDiagnostics,
    StatusCounts, encode_cursor, sort_dimensions,
};
use o3k_provider::{
    AgentAdministrativeState, AgentAvailability, AgentNodeRegistry, AgentNodeSnapshot,
};
use o3k_store::unified::O3kStore;
use o3k_store::{CoordinationRepository, PlacementRepository, StoreError};

/// Lease horizon for agent heartbeats. An agent whose last authenticated
/// heartbeat is older than this is projected as `stale`, never `healthy`.
pub const AGENT_LEASE_MS: i64 = 15_000;

/// Upper bound on the number of services projected in one pass. Service
/// collections are registry-backed (process-internal and small), but the bound
/// is retained for defense in depth.
pub const MAX_SERVICES: usize = 256;

/// Unix milliseconds since the UNIX epoch. Falls back to `0` only if the
/// system clock precedes the epoch, which cannot happen on supported hosts.
pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn availability_str(availability: AgentAvailability) -> &'static str {
    match availability {
        AgentAvailability::Available => "available",
        AgentAvailability::Unavailable => "unavailable",
    }
}

/// Provider status precedence, from most to least severe. This ordering is the
/// authority for both the per-provider list and the summary count and must
/// never report a provider as `healthy` from durable placement state alone:
///
/// 1. no agent snapshot        -> Unknown / NeverObserved (restart before
///    re-observation is never reported healthy from durable state);
/// 2. agent administratively Disabled -> Unavailable / AdministrativelyDisabled;
/// 3. durable state `Deleted`  -> Unavailable / AdministrativelyDisabled;
/// 4. durable `Draining` or agent Draining -> Degraded / Draining;
/// 5. agent Unavailable        -> Stale / HeartbeatLost when the last
///    heartbeat is older than the lease, else Unavailable / HeartbeatLost;
/// 6. last heartbeat stale     -> Stale / ObservationStale;
/// 7. otherwise                -> Healthy.
#[allow(clippy::needless_pass_by_value)]
fn provider_status(
    snap: Option<&AgentNodeSnapshot>,
    record_state: &str,
    observed: Option<i64>,
    now: i64,
) -> (DiagnosticStatus, Option<DiagnosticReason>) {
    let Some(snap) = snap else {
        return (
            DiagnosticStatus::Unknown,
            Some(DiagnosticReason::NeverObserved),
        );
    };
    if snap.administrative_state == AgentAdministrativeState::Disabled {
        return (
            DiagnosticStatus::Unavailable,
            Some(DiagnosticReason::AdministrativelyDisabled),
        );
    }
    if record_state == "Deleted" {
        return (
            DiagnosticStatus::Unavailable,
            Some(DiagnosticReason::AdministrativelyDisabled),
        );
    }
    if record_state == "Draining" || snap.administrative_state == AgentAdministrativeState::Draining
    {
        return (DiagnosticStatus::Degraded, Some(DiagnosticReason::Draining));
    }
    if snap.availability == AgentAvailability::Unavailable {
        if observed.is_some_and(|observed| now - observed > AGENT_LEASE_MS) {
            return (
                DiagnosticStatus::Stale,
                Some(DiagnosticReason::HeartbeatLost),
            );
        }
        return (
            DiagnosticStatus::Unavailable,
            Some(DiagnosticReason::HeartbeatLost),
        );
    }
    if observed.is_some_and(|observed| now - observed > AGENT_LEASE_MS) {
        return (
            DiagnosticStatus::Stale,
            Some(DiagnosticReason::ObservationStale),
        );
    }
    (DiagnosticStatus::Healthy, None)
}

/// Service status mapping from the canonical controller lifecycle state.
fn service_status(state: ControllerState) -> (DiagnosticStatus, Option<DiagnosticReason>) {
    match state {
        ControllerState::Ready => (DiagnosticStatus::Healthy, None),
        ControllerState::NotReady => (
            DiagnosticStatus::Unavailable,
            Some(DiagnosticReason::ReadinessFailed),
        ),
        ControllerState::Incompatible => (
            DiagnosticStatus::Degraded,
            Some(DiagnosticReason::ProtocolIncompatible),
        ),
        ControllerState::Disabled => (
            DiagnosticStatus::Unavailable,
            Some(DiagnosticReason::AdministrativelyDisabled),
        ),
        ControllerState::Declared => (
            DiagnosticStatus::Unknown,
            Some(DiagnosticReason::NeverObserved),
        ),
    }
}

/// Tolerant parsing of a control-plane heartbeat timestamp: RFC3339 first,
/// then the SQLite `datetime('now')` `%Y-%m-%d %H:%M:%S` (UTC) form. On
/// failure returns `None` (the heartbeat is not projected).
fn parse_timestamp(value: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Some(naive.and_utc().timestamp_millis());
    }
    None
}

/// Capacity unit label for a resource class.
fn unit_for(resource_class: &str) -> &'static str {
    match resource_class {
        "VCPU" => "count",
        "MEMORY_MB" => "mib",
        "DISK_GB" => "gib",
        _ => "count",
    }
}

/// Maps a durable store error to the bounded diagnostics error vocabulary.
fn map_store_error(error: StoreError) -> DiagnosticsError {
    match error {
        StoreError::Corrupt(_) => DiagnosticsError::Corrupt,
        _ => DiagnosticsError::Unavailable,
    }
}

/// Worst-wins combination of two component-class aggregate statuses.
fn worst_status(left: DiagnosticStatus, right: DiagnosticStatus) -> DiagnosticStatus {
    fn rank(status: DiagnosticStatus) -> u8 {
        match status {
            DiagnosticStatus::Unavailable => 5,
            DiagnosticStatus::Degraded => 4,
            DiagnosticStatus::Stale => 3,
            DiagnosticStatus::Unknown => 2,
            DiagnosticStatus::Healthy => 1,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

/// Production adapter that projects canonical O3K authority into the operator
/// diagnostics contract.
pub struct DiagnosticsReaderAdapter {
    registry: Arc<RwLock<ManifestRegistry>>,
    agents: Arc<dyn AgentNodeRegistry>,
    store: Arc<O3kStore>,
    locations: o3k_kernel::LocationRegistry,
    started_at_unix_ms: i64,
    observations: Arc<RwLock<HashMap<String, i64>>>,
}

impl DiagnosticsReaderAdapter {
    /// Creates a new adapter. Observations start empty; every service is
    /// treated as observed at process start until the composition probe task
    /// records a fresh timestamp.
    #[must_use]
    pub fn new(
        registry: Arc<RwLock<ManifestRegistry>>,
        agents: Arc<dyn AgentNodeRegistry>,
        store: Arc<O3kStore>,
        locations: o3k_kernel::LocationRegistry,
    ) -> Self {
        Self {
            registry,
            agents,
            store,
            locations,
            started_at_unix_ms: now_unix_ms(),
            observations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Records the observation time of a service. Used by the composition
    /// controller probe task to mark when a controller was last probed.
    pub fn record_service_observation(&self, service_id: &str, unix_ms: i64) {
        if let Ok(mut observations) = self.observations.write() {
            observations.insert(service_id.to_owned(), unix_ms);
        }
    }

    /// Exposes the shared service-observation map to the composition probe
    /// task.
    pub fn observations(&self) -> Arc<RwLock<HashMap<String, i64>>> {
        self.observations.clone()
    }

    fn service_observed_at(&self, service_id: &str) -> i64 {
        self.observations
            .read()
            .ok()
            .and_then(|observations| observations.get(service_id).copied())
            .unwrap_or(self.started_at_unix_ms)
    }

    async fn control_plane_status(&self) -> ControlPlaneStatus {
        let sessions = match self.store.list_active_controller_sessions().await {
            Ok(sessions) => sessions,
            Err(_) => {
                return ControlPlaneStatus {
                    status: DiagnosticStatus::Unavailable,
                    active_sessions: 0,
                    observed_at_unix_ms: None,
                    reason: Some(DiagnosticReason::NeverObserved),
                };
            }
        };
        let active_sessions = sessions.len() as u64;
        let observed_at_unix_ms = sessions
            .iter()
            .filter_map(|session| parse_timestamp(&session.heartbeat_at))
            .max();
        if active_sessions > 0 {
            ControlPlaneStatus {
                status: DiagnosticStatus::Healthy,
                active_sessions,
                observed_at_unix_ms,
                reason: None,
            }
        } else {
            ControlPlaneStatus {
                status: DiagnosticStatus::Unavailable,
                active_sessions,
                observed_at_unix_ms,
                reason: Some(DiagnosticReason::NeverObserved),
            }
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticsReader for DiagnosticsReaderAdapter {
    async fn summary(&self) -> Result<DiagnosticsSummary, DiagnosticsError> {
        let now = now_unix_ms();

        // Services: iterate the registry directly (bounded, process-internal)
        // and count by projected status. This deliberately does not call
        // `services()` with pagination.
        let mut services = ComponentCounts::default();
        {
            let reg = self
                .registry
                .read()
                .map_err(|_| DiagnosticsError::Corrupt)?;
            for manifest in reg.all() {
                let (status, _) = reg
                    .controller(&manifest.service_id)
                    .map(|registration| service_status(registration.state))
                    .unwrap_or((
                        DiagnosticStatus::Unknown,
                        Some(DiagnosticReason::NeverObserved),
                    ));
                services.total += 1;
                match status {
                    DiagnosticStatus::Healthy => services.healthy += 1,
                    DiagnosticStatus::Degraded => services.degraded += 1,
                    DiagnosticStatus::Unavailable => services.unavailable += 1,
                    DiagnosticStatus::Stale => services.stale += 1,
                    DiagnosticStatus::Unknown => services.unknown += 1,
                }
            }
        }

        // Providers: the agent registry is the observed set (in-memory and
        // fleet-bounded). Durable placement providers without a live agent
        // snapshot surface as `unknown` in the providers list, but are not
        // counted here — the summary reports the observed agent population.
        let agents = self.agents.all().await;
        let mut providers = ComponentCounts::default();
        for agent in &agents {
            let observed = self.agents.observed_at_unix_ms(&agent.agent_id).await;
            let (status, _) = provider_status(Some(agent), "Enabled", observed, now);
            providers.total += 1;
            match status {
                DiagnosticStatus::Healthy => providers.healthy += 1,
                DiagnosticStatus::Degraded => providers.degraded += 1,
                DiagnosticStatus::Unavailable => providers.unavailable += 1,
                DiagnosticStatus::Stale => providers.stale += 1,
                DiagnosticStatus::Unknown => providers.unknown += 1,
            }
        }

        let control_plane = self.control_plane_status().await;
        let locations = LocationDiagnostics {
            configured: !self.locations.is_empty(),
            regions: self.locations.len() as u64,
            availability_domains: self
                .locations
                .regions()
                .iter()
                .map(|region| region.availability_domains.len() as u64)
                .sum(),
        };

        let status = worst_status(services.aggregate(), providers.aggregate());

        Ok(DiagnosticsSummary {
            version: DIAGNOSTICS_VERSION.to_owned(),
            evaluated_at_unix_ms: now,
            status,
            counts: StatusCounts {
                services,
                providers,
            },
            control_plane: Some(control_plane),
            locations,
        })
    }

    async fn services(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<DiagnosticsPage<ServiceDiagnostics>, DiagnosticsError> {
        let reg = self
            .registry
            .read()
            .map_err(|_| DiagnosticsError::Corrupt)?;
        let mut manifests = reg.all();
        manifests.sort_by(|left, right| left.service_id.cmp(&right.service_id));

        let mut items: Vec<ServiceDiagnostics> = Vec::with_capacity(limit + 1);
        for manifest in manifests
            .into_iter()
            .filter(|manifest| match after {
                Some(after) => manifest.service_id.as_str() > after,
                None => true,
            })
            .take(limit + 1)
        {
            let service_id = manifest.service_id.clone();
            let registration = reg.controller(&service_id);
            let lifecycle_state = registration
                .map(|registration| registration.state.to_string())
                .unwrap_or_else(|| "declared".to_owned());
            let (status, reason) = registration
                .map(|registration| service_status(registration.state))
                .unwrap_or((
                    DiagnosticStatus::Unknown,
                    Some(DiagnosticReason::NeverObserved),
                ));
            let controller = registration.map(|registration| {
                let manifest_controller = manifest.controller.as_ref();
                let protocol_version = registration
                    .health
                    .as_ref()
                    .map(|health| health.protocol_version.to_string())
                    .or_else(|| {
                        manifest_controller.map(|controller| controller.protocol_version.clone())
                    })
                    .unwrap_or_default();
                let healthy = registration
                    .health
                    .as_ref()
                    .map(|health| health.healthy)
                    .unwrap_or(registration.state == ControllerState::Ready);
                ControllerDiagnostics {
                    mode: manifest_controller
                        .map(|controller| controller.mode.clone())
                        .unwrap_or_default(),
                    protocol: manifest_controller
                        .map(|controller| controller.protocol.clone())
                        .unwrap_or_default(),
                    protocol_version,
                    healthy,
                    session_generation: registration
                        .session
                        .as_ref()
                        .map(|session| session.session_generation),
                }
            });
            items.push(ServiceDiagnostics {
                service_id,
                namespace: manifest.namespace.clone(),
                service_version: manifest.service_version.clone(),
                ownership: manifest.ownership.to_string(),
                lifecycle_state,
                status,
                observed_at_unix_ms: Some(self.service_observed_at(&manifest.service_id)),
                reason,
                controller,
            });
        }

        let has_more = items.len() > limit;
        if has_more {
            items.truncate(limit);
        }
        let next_cursor = if has_more {
            items.last().map(|item| encode_cursor(&item.service_id))
        } else {
            None
        };

        Ok(DiagnosticsPage {
            items,
            has_more,
            next_cursor,
        })
    }

    async fn providers(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<DiagnosticsPage<ProviderDiagnostics>, DiagnosticsError> {
        let records = self
            .store
            .list_providers_bounded(after, limit + 1)
            .await
            .map_err(map_store_error)?;
        let now = now_unix_ms();
        let has_more = records.len() > limit;
        let take = if has_more { limit } else { records.len() };

        let mut items = Vec::with_capacity(take);
        for record in records.into_iter().take(take) {
            let provider_id = record.id.clone();
            let snap = self.agents.snapshot(&provider_id).await;
            let observed = self.agents.observed_at_unix_ms(&provider_id).await;
            let (status, reason) = provider_status(snap.as_ref(), &record.state, observed, now);
            let availability = snap
                .as_ref()
                .map(|snap| availability_str(snap.availability))
                .unwrap_or("unobserved")
                .to_owned();
            let capacity = record
                .inventories
                .iter()
                .map(|inventory| {
                    let allocatable =
                        ((inventory.total as f64) * inventory.allocation_ratio).floor() as u64;
                    ProviderCapacityDimension {
                        resource_class: inventory.resource_class.clone(),
                        total: inventory.total,
                        reserved: inventory.reserved,
                        allocated: inventory.used,
                        available: allocatable
                            .saturating_sub(inventory.reserved)
                            .saturating_sub(inventory.used),
                    }
                })
                .collect();
            items.push(ProviderDiagnostics {
                provider_id,
                state: record.state,
                availability,
                status,
                observed_at_unix_ms: observed,
                reason,
                capacity,
            });
        }

        let next_cursor = if has_more {
            items.last().map(|item| encode_cursor(&item.provider_id))
        } else {
            None
        };

        Ok(DiagnosticsPage {
            items,
            has_more,
            next_cursor,
        })
    }

    async fn capacity(&self) -> Result<CapacityDiagnostics, DiagnosticsError> {
        let summary = self
            .store
            .capacity_summary(MAX_CAPACITY_CLASSES)
            .await
            .map_err(map_store_error)?;
        let agents = self.agents.all().await;
        let now = now_unix_ms();

        let mut max_observed: Option<i64> = None;
        let mut fleet = ComponentCounts::default();
        for agent in &agents {
            let observed = self.agents.observed_at_unix_ms(&agent.agent_id).await;
            if let Some(observed) = observed {
                max_observed = Some(max_observed.map_or(observed, |max| max.max(observed)));
            }
            let (status, _) = provider_status(Some(agent), "Enabled", observed, now);
            fleet.total += 1;
            match status {
                DiagnosticStatus::Healthy => fleet.healthy += 1,
                DiagnosticStatus::Degraded => fleet.degraded += 1,
                DiagnosticStatus::Unavailable => fleet.unavailable += 1,
                DiagnosticStatus::Stale => fleet.stale += 1,
                DiagnosticStatus::Unknown => fleet.unknown += 1,
            }
        }

        let provider_total = summary.providers_enabled
            + summary.providers_draining
            + summary.providers_unavailable
            + summary.providers_deleted;
        let (status, reason) = if provider_total == 0 && agents.is_empty() {
            // Capacity authority is not populated at all.
            (
                DiagnosticStatus::Unknown,
                Some(DiagnosticReason::NeverObserved),
            )
        } else if max_observed.is_none() || now - max_observed.unwrap_or(now) > AGENT_LEASE_MS {
            // Durable last-known values without any fresh agent observation are
            // never reported healthy (covers process restart before
            // re-observation, and a fleet that has stopped reporting).
            (
                DiagnosticStatus::Stale,
                Some(DiagnosticReason::ObservationStale),
            )
        } else if fleet.degraded > 0 || fleet.unavailable > 0 || fleet.stale > 0 {
            // Partial provider failure: at least one provider is down or stale
            // even though another produced a fresh observation.
            (DiagnosticStatus::Degraded, None)
        } else {
            (DiagnosticStatus::Healthy, None)
        };

        let mut dimensions: Vec<CapacityDimension> = summary
            .classes
            .iter()
            .map(|class| CapacityDimension {
                resource_class: class.resource_class.clone(),
                unit: unit_for(&class.resource_class).to_owned(),
                allocatable: class.allocatable,
                reserved: class.reserved,
                allocated: class.allocated,
                available: class
                    .allocatable
                    .saturating_sub(class.reserved)
                    .saturating_sub(class.allocated),
            })
            .collect();
        sort_dimensions(&mut dimensions);

        Ok(CapacityDiagnostics {
            version: DIAGNOSTICS_VERSION.to_owned(),
            status,
            observed_at_unix_ms: max_observed,
            reason,
            providers_enabled: summary.providers_enabled,
            providers_draining: summary.providers_draining,
            providers_unavailable: summary.providers_unavailable,
            providers_deleted: summary.providers_deleted,
            dimensions,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use o3k_provider::{AgentCapabilities, AgentEpochLease};

    #[allow(clippy::type_complexity)]
    struct FakeAgents {
        nodes: Arc<tokio::sync::Mutex<HashMap<String, (AgentNodeSnapshot, Option<i64>)>>>,
    }

    #[async_trait::async_trait]
    impl AgentNodeRegistry for FakeAgents {
        async fn all(&self) -> Vec<AgentNodeSnapshot> {
            self.nodes
                .lock()
                .await
                .values()
                .map(|(snapshot, _)| snapshot.clone())
                .collect()
        }
        async fn snapshot(&self, agent_id: &str) -> Option<AgentNodeSnapshot> {
            self.nodes
                .lock()
                .await
                .get(agent_id)
                .map(|(snapshot, _)| snapshot.clone())
        }
        async fn lease_current_epoch(
            &self,
            _agent_id: &str,
            _agent_epoch: &str,
        ) -> Option<Box<dyn AgentEpochLease>> {
            None
        }
        fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<o3k_provider::AgentEvent> {
            tokio::sync::broadcast::channel(1).1
        }
        async fn observed_at_unix_ms(&self, agent_id: &str) -> Option<i64> {
            self.nodes
                .lock()
                .await
                .get(agent_id)
                .and_then(|(_, observed)| *observed)
        }
    }

    fn capabilities() -> AgentCapabilities {
        AgentCapabilities {
            agent_provider_name: "test".to_owned(),
            agent_provider_version: "1".to_owned(),
            max_vcpus: 8,
            max_memory_mib: 8192,
            max_disk_gb: 100,
            lifecycle_actions: vec![],
            console_log: false,
            flags: vec![],
        }
    }

    fn snapshot(
        agent_id: &str,
        availability: AgentAvailability,
        administrative_state: AgentAdministrativeState,
    ) -> AgentNodeSnapshot {
        AgentNodeSnapshot {
            agent_id: agent_id.to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            availability,
            administrative_state,
            capabilities: capabilities(),
        }
    }

    fn fake_agents(nodes: HashMap<String, (AgentNodeSnapshot, Option<i64>)>) -> Arc<FakeAgents> {
        Arc::new(FakeAgents {
            nodes: Arc::new(tokio::sync::Mutex::new(nodes)),
        })
    }

    fn inventory(
        resource_class: &str,
        total: u64,
        reserved: u64,
        allocation_ratio: f64,
        used: u64,
    ) -> o3k_store::PlacementInventoryRecord {
        o3k_store::PlacementInventoryRecord {
            resource_class: resource_class.to_owned(),
            total,
            reserved,
            allocation_ratio,
            used,
        }
    }

    #[tokio::test]
    async fn provider_never_observed_is_unknown_even_when_durable_state_enabled() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            fake_agents(HashMap::new()),
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].provider_id, "provider-a");
        assert_eq!(page.items[0].status, DiagnosticStatus::Unknown);
        assert_eq!(page.items[0].reason, Some(DiagnosticReason::NeverObserved));
        assert_eq!(page.items[0].availability, "unobserved");
    }

    #[tokio::test]
    async fn provider_healthy_when_agent_observed_fresh() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        let observed = now_unix_ms();
        let agents = fake_agents(HashMap::from([(
            "provider-a".to_owned(),
            (
                snapshot(
                    "provider-a",
                    AgentAvailability::Available,
                    AgentAdministrativeState::Enabled,
                ),
                Some(observed),
            ),
        )]));
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            agents,
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, DiagnosticStatus::Healthy);
        assert_eq!(page.items[0].reason, None);
        assert_eq!(page.items[0].availability, "available");
    }

    #[tokio::test]
    async fn provider_stale_when_heartbeat_older_than_lease() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        let observed = now_unix_ms() - AGENT_LEASE_MS - 1_000;
        let agents = fake_agents(HashMap::from([(
            "provider-a".to_owned(),
            (
                snapshot(
                    "provider-a",
                    AgentAvailability::Available,
                    AgentAdministrativeState::Enabled,
                ),
                Some(observed),
            ),
        )]));
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            agents,
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        assert_eq!(page.items[0].status, DiagnosticStatus::Stale);
        assert_eq!(
            page.items[0].reason,
            Some(DiagnosticReason::ObservationStale)
        );
    }

    #[tokio::test]
    async fn provider_draining_is_degraded() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        store
            .set_provider_state("provider-a", "Draining")
            .await
            .expect("set state");
        let agents = fake_agents(HashMap::from([(
            "provider-a".to_owned(),
            (
                snapshot(
                    "provider-a",
                    AgentAvailability::Available,
                    AgentAdministrativeState::Enabled,
                ),
                Some(now_unix_ms()),
            ),
        )]));
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            agents,
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        assert_eq!(page.items[0].status, DiagnosticStatus::Degraded);
        assert_eq!(page.items[0].reason, Some(DiagnosticReason::Draining));
    }

    #[tokio::test]
    async fn provider_disabled_is_unavailable() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        let agents = fake_agents(HashMap::from([(
            "provider-a".to_owned(),
            (
                snapshot(
                    "provider-a",
                    AgentAvailability::Available,
                    AgentAdministrativeState::Disabled,
                ),
                Some(now_unix_ms()),
            ),
        )]));
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            agents,
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        assert_eq!(page.items[0].status, DiagnosticStatus::Unavailable);
        assert_eq!(
            page.items[0].reason,
            Some(DiagnosticReason::AdministrativelyDisabled)
        );
    }

    #[tokio::test]
    async fn capacity_unknown_when_no_providers() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            fake_agents(HashMap::new()),
            store,
            o3k_kernel::LocationRegistry::default(),
        );
        let capacity = adapter.capacity().await.expect("capacity");
        assert_eq!(capacity.status, DiagnosticStatus::Unknown);
        assert_eq!(capacity.reason, Some(DiagnosticReason::NeverObserved));
    }

    #[tokio::test]
    async fn capacity_stale_when_providers_exist_but_no_fresh_observation() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider("provider-a", &[inventory("VCPU", 8, 1, 1.0, 2)])
            .await
            .expect("register provider");
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            fake_agents(HashMap::new()),
            store,
            o3k_kernel::LocationRegistry::default(),
        );
        let capacity = adapter.capacity().await.expect("capacity");
        assert_eq!(capacity.status, DiagnosticStatus::Stale);
        assert_eq!(capacity.reason, Some(DiagnosticReason::ObservationStale));
    }

    #[tokio::test]
    async fn capacity_arithmetic_is_saturating() {
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        store
            .register_provider(
                "provider-a",
                &[
                    inventory("VCPU", 8, 1, 1.0, 2),
                    // Over-allocated class must clamp to zero, never wrap.
                    inventory("MEMORY_MB", 2, 5, 1.0, 9),
                ],
            )
            .await
            .expect("register provider");
        let observed = now_unix_ms();
        let agents = fake_agents(HashMap::from([(
            "provider-a".to_owned(),
            (
                snapshot(
                    "provider-a",
                    AgentAvailability::Available,
                    AgentAdministrativeState::Enabled,
                ),
                Some(observed),
            ),
        )]));
        let adapter = DiagnosticsReaderAdapter::new(
            Arc::new(RwLock::new(ManifestRegistry::new())),
            agents,
            store,
            o3k_kernel::LocationRegistry::default(),
        );

        let page = adapter.providers(10, None).await.expect("providers");
        let capacity = adapter.capacity().await.expect("capacity");
        assert_eq!(capacity.status, DiagnosticStatus::Healthy);

        let vcpu = capacity
            .dimensions
            .iter()
            .find(|dimension| dimension.resource_class == "VCPU")
            .expect("VCPU dimension");
        // register_provider recomputes `used` from durable allocations, so the
        // input `used` is zeroed: allocatable = floor(8 * 1.0) = 8, reserved = 1,
        // available = 8 - 1 - 0 = 7.
        assert_eq!(vcpu.allocatable, 8);
        assert_eq!(vcpu.available, 7);
        assert_eq!(vcpu.unit, "count");

        let memory = capacity
            .dimensions
            .iter()
            .find(|dimension| dimension.resource_class == "MEMORY_MB")
            .expect("MEMORY_MB dimension");
        // Over-allocated class (total 2, reserved 5) must clamp to zero.
        assert_eq!(memory.available, 0, "negative remainder must clamp to zero");
        assert_eq!(memory.unit, "mib");

        let provider_vcpu = page.items[0]
            .capacity
            .iter()
            .find(|dimension| dimension.resource_class == "VCPU")
            .expect("provider VCPU dimension");
        assert_eq!(provider_vcpu.available, 7);
    }

    #[test]
    fn service_declared_is_unknown_ready_is_healthy() {
        let (status, reason) = service_status(ControllerState::Declared);
        assert_eq!(status, DiagnosticStatus::Unknown);
        assert_eq!(reason, Some(DiagnosticReason::NeverObserved));

        let (status, reason) = service_status(ControllerState::Ready);
        assert_eq!(status, DiagnosticStatus::Healthy);
        assert_eq!(reason, None);

        let (status, reason) = service_status(ControllerState::NotReady);
        assert_eq!(status, DiagnosticStatus::Unavailable);
        assert_eq!(reason, Some(DiagnosticReason::ReadinessFailed));
    }

    #[test]
    fn provider_status_unknown_when_no_snapshot_despite_enabled_durable_state() {
        let (status, reason) = provider_status(None, "Enabled", Some(now_unix_ms()), now_unix_ms());
        assert_eq!(status, DiagnosticStatus::Unknown);
        assert_eq!(reason, Some(DiagnosticReason::NeverObserved));
    }

    #[test]
    fn parse_timestamp_accepts_rfc3339_and_sqlite_datetime() {
        assert!(parse_timestamp("2026-09-11T10:00:00Z").is_some());
        assert!(parse_timestamp("2026-09-11 10:00:00").is_some());
        assert!(parse_timestamp("garbage").is_none());
    }
}
