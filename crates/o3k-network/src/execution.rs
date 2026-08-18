//! The node-local network execution boundary.
//!
//! This module deliberately stops at plan admission.  The control plane owns
//! the semantic [`NodeNetworkPlan`]; a network provider owns host mutation.
//! Admission is journaled before a provider is called so a reconnect can
//! replay the accepted identity instead of issuing a second mutation.

use crate::NodeNetworkPlan;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use thiserror::Error;
use uuid::Uuid;

const JOURNAL_FILE: &str = "accepted-network-plans.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAgentIdentity {
    pub agent_id: String,
    pub agent_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkControllerLease {
    pub controller_id: String,
    pub controller_epoch: String,
    pub fencing_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPlanCommand {
    pub command_id: Uuid,
    pub operation_id: Uuid,
    pub idempotency_key: String,
    pub target: NetworkAgentIdentity,
    pub controller: NetworkControllerLease,
    pub deadline_unix_ms: u64,
    pub plan: NodeNetworkPlan,
}

impl NetworkPlanCommand {
    pub fn fingerprint(&self) -> &str {
        &self.plan.fingerprint_sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AcceptedPlan {
    command_id: Uuid,
    operation_id: Uuid,
    idempotency_key: String,
    plan: NodeNetworkPlan,
    target: NetworkAgentIdentity,
    controller: NetworkControllerLease,
    status: NetworkPlanStatus,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Journal {
    plans: Vec<AcceptedPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAdmission {
    Accepted,
    Replayed,
    RequiresObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPlanStatus {
    Accepted,
    Applying,
    Succeeded,
    Unknown,
}

#[derive(Debug, Error)]
pub enum NetworkExecutionError {
    #[error("network execution journal failed: {0}")]
    Io(#[from] io::Error),
    #[error("network execution journal is corrupt")]
    CorruptJournal,
    #[error("network plan command is invalid")]
    InvalidCommand,
    #[error("network plan command targets a fenced agent epoch")]
    StaleAgentEpoch,
    #[error("network plan command has a stale controller lease")]
    StaleControllerLease,
    #[error("network plan command deadline has expired")]
    DeadlineExpired,
    #[error("network plan identity was replayed with a different payload")]
    ConflictingReplay,
    #[error("network mutation outcome is unknown and requires observation")]
    MutationOutcomeUnknown,
    #[error("network command is not present in the durable journal")]
    UnknownCommand,
}

/// Durable admission boundary for a node-local network executor.
#[derive(Debug)]
pub struct NetworkPlanExecutor {
    root: PathBuf,
    agent: NetworkAgentIdentity,
    lease: NetworkControllerLease,
    journal_lock: Mutex<()>,
}

impl NetworkPlanExecutor {
    pub fn open(
        root: impl Into<PathBuf>,
        agent: NetworkAgentIdentity,
        lease: NetworkControllerLease,
    ) -> Result<Self, NetworkExecutionError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let executor = Self {
            root,
            agent,
            lease,
            journal_lock: Mutex::new(()),
        };
        let _ = executor.load()?;
        Ok(executor)
    }

    pub fn admit(
        &self,
        command: &NetworkPlanCommand,
        now_unix_ms: u64,
    ) -> Result<PlanAdmission, NetworkExecutionError> {
        self.validate(command, now_unix_ms)?;
        let _guard = self
            .journal_lock
            .lock()
            .map_err(|_| NetworkExecutionError::CorruptJournal)?;
        let mut journal = self.load()?;
        if let Some(existing) = journal
            .plans
            .iter()
            .find(|existing| existing.command_id == command.command_id)
        {
            if existing.plan.fingerprint_sha256 == command.fingerprint()
                && existing.operation_id == command.operation_id
                && existing.idempotency_key == command.idempotency_key
                && existing.plan.plan_id == command.plan.plan_id
                && existing.target == command.target
                && existing.controller == command.controller
            {
                return Ok(match existing.status {
                    NetworkPlanStatus::Accepted | NetworkPlanStatus::Applying => {
                        PlanAdmission::RequiresObservation
                    }
                    NetworkPlanStatus::Succeeded | NetworkPlanStatus::Unknown => {
                        PlanAdmission::Replayed
                    }
                });
            }
            return Err(NetworkExecutionError::ConflictingReplay);
        }
        if journal.plans.iter().any(|existing| {
            existing.operation_id == command.operation_id
                && existing.plan.plan_id == command.plan.plan_id
        }) {
            return Err(NetworkExecutionError::ConflictingReplay);
        }
        if command.plan.operation_id != command.operation_id
            || command.plan.deadline_unix_ms != command.deadline_unix_ms
        {
            return Err(NetworkExecutionError::InvalidCommand);
        }
        if command.deadline_unix_ms < now_unix_ms {
            return Err(NetworkExecutionError::DeadlineExpired);
        }
        journal.plans.push(AcceptedPlan {
            command_id: command.command_id,
            operation_id: command.operation_id,
            idempotency_key: command.idempotency_key.clone(),
            plan: command.plan.clone(),
            target: command.target.clone(),
            controller: command.controller.clone(),
            status: NetworkPlanStatus::Accepted,
        });
        self.store(&journal)?;
        Ok(PlanAdmission::Accepted)
    }

    pub fn accepted(&self, command_id: Uuid) -> Result<bool, NetworkExecutionError> {
        let _guard = self
            .journal_lock
            .lock()
            .map_err(|_| NetworkExecutionError::CorruptJournal)?;
        Ok(self
            .load()?
            .plans
            .iter()
            .any(|plan| plan.command_id == command_id))
    }

    pub fn execute<R: NetworkPlanRealizer>(
        &self,
        command: &NetworkPlanCommand,
        now_unix_ms: u64,
        realizer: &mut R,
    ) -> Result<PlanAdmission, NetworkExecutionError> {
        let admission = self.admit(command, now_unix_ms)?;
        if admission != PlanAdmission::Accepted {
            return Ok(admission);
        }
        self.set_status(command.command_id, NetworkPlanStatus::Applying)?;
        match realizer.realize(&command.plan) {
            Ok(()) => {
                self.set_status(command.command_id, NetworkPlanStatus::Succeeded)?;
                Ok(PlanAdmission::Accepted)
            }
            Err(_) => {
                self.set_status(command.command_id, NetworkPlanStatus::Unknown)?;
                Err(NetworkExecutionError::MutationOutcomeUnknown)
            }
        }
    }

    pub fn reconcile<R: NetworkPlanRealizer>(
        &self,
        command_id: Uuid,
        realizer: &mut R,
    ) -> Result<NetworkPlanStatus, NetworkExecutionError> {
        let _guard = self
            .journal_lock
            .lock()
            .map_err(|_| NetworkExecutionError::CorruptJournal)?;
        let mut journal = self.load()?;
        let status = {
            let record = journal
                .plans
                .iter_mut()
                .find(|plan| plan.command_id == command_id)
                .ok_or(NetworkExecutionError::UnknownCommand)?;
            if record.status == NetworkPlanStatus::Succeeded {
                return Ok(record.status);
            }
            record.status = match realizer.observe(&record.plan) {
                Ok(true) => NetworkPlanStatus::Succeeded,
                Ok(false) | Err(_) => NetworkPlanStatus::Unknown,
            };
            record.status
        };
        self.store(&journal)?;
        Ok(status)
    }

    fn set_status(
        &self,
        command_id: Uuid,
        status: NetworkPlanStatus,
    ) -> Result<(), NetworkExecutionError> {
        let _guard = self
            .journal_lock
            .lock()
            .map_err(|_| NetworkExecutionError::CorruptJournal)?;
        let mut journal = self.load()?;
        let record = journal
            .plans
            .iter_mut()
            .find(|plan| plan.command_id == command_id)
            .ok_or(NetworkExecutionError::UnknownCommand)?;
        record.status = status;
        self.store(&journal)
    }

    fn validate(
        &self,
        command: &NetworkPlanCommand,
        _now_unix_ms: u64,
    ) -> Result<(), NetworkExecutionError> {
        if command.idempotency_key.trim().is_empty()
            || command.target.agent_id.trim().is_empty()
            || command.target.agent_epoch.trim().is_empty()
            || command.controller.controller_id.trim().is_empty()
            || command.controller.controller_epoch.trim().is_empty()
            || command.controller.fencing_token == 0
            || command.plan.node_id.trim().is_empty()
            || command.plan.node_id != command.target.agent_id
            || command.plan.fingerprint_sha256.len() != 64
            || !command
                .plan
                .fingerprint_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(NetworkExecutionError::InvalidCommand);
        }
        if command.target != self.agent {
            return Err(NetworkExecutionError::StaleAgentEpoch);
        }
        if command.controller != self.lease {
            return Err(NetworkExecutionError::StaleControllerLease);
        }
        Ok(())
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE)
    }

    fn load(&self) -> Result<Journal, NetworkExecutionError> {
        let path = self.journal_path();
        match fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| NetworkExecutionError::CorruptJournal)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn store(&self, journal: &Journal) -> Result<(), NetworkExecutionError> {
        let path = self.journal_path();
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(journal)
            .map_err(|_| NetworkExecutionError::CorruptJournal)?;
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

/// A provider-specific realization is intentionally a narrow seam.  It does
/// not receive tenant authorization or allocate public identities.
pub trait NetworkPlanRealizer {
    type Error;

    fn realize(&mut self, plan: &NodeNetworkPlan) -> Result<(), Self::Error>;

    fn observe(&mut self, _plan: &NodeNetworkPlan) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

pub fn journal_path(root: &Path) -> PathBuf {
    root.join(JOURNAL_FILE)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::compile_node_network_plan;
    use o3k_domain::{
        AddressRealm, EndpointIntent, Ipv4Prefix, NetworkCapability, NetworkIntent,
        NetworkProtocol, PolicyAction, PolicyDirection, PolicyIntent, PortRange, RouteIntent,
    };
    use std::{collections::HashSet, net::Ipv4Addr};

    fn plan() -> NodeNetworkPlan {
        let prefix =
            |ip: &str, length| Ipv4Prefix::new(ip.parse().expect("ip"), length).expect("prefix");
        let intent = NetworkIntent {
            id: Uuid::from_u128(1),
            project_id: "project-a".into(),
            realm: AddressRealm {
                id: Uuid::from_u128(2),
                project_id: "project-a".into(),
                prefix: prefix("10.0.0.0", 24),
                overlapping_prefixes: false,
            },
            address_pools: vec![],
            endpoints: vec![EndpointIntent {
                id: Uuid::from_u128(3),
                project_id: "project-a".into(),
                mac: "02:00:00:00:00:03".into(),
                fixed_ip: Ipv4Addr::new(10, 0, 0, 3),
                generation: 1,
            }],
            routes: vec![RouteIntent {
                destination: prefix("0.0.0.0", 0),
                next_hop: Some(Ipv4Addr::new(10, 0, 0, 1)),
            }],
            gateways: vec![],
            egress: vec![],
            public_addresses: vec![],
            policies: vec![PolicyIntent {
                endpoint_id: Uuid::from_u128(3),
                direction: PolicyDirection::Egress,
                protocol: NetworkProtocol::Tcp,
                ports: Some(PortRange {
                    start: 443,
                    end: 443,
                }),
                source: None,
                destination: Some(prefix("0.0.0.0", 0)),
                action: PolicyAction::Allow,
            }],
            generation: 1,
            state: o3k_domain::NetworkIntentState::Requested,
        };
        let capabilities = [
            NetworkCapability::Ipv4,
            NetworkCapability::EndpointAttachment,
            NetworkCapability::Routing,
            NetworkCapability::StatefulPolicy,
        ]
        .into_iter()
        .collect::<HashSet<_>>();
        compile_node_network_plan(
            &intent,
            "node-a",
            Uuid::from_u128(4),
            100,
            &capabilities,
            &[],
        )
        .expect("plan")
    }

    fn command() -> NetworkPlanCommand {
        NetworkPlanCommand {
            command_id: Uuid::from_u128(10),
            operation_id: Uuid::from_u128(4),
            idempotency_key: "idempotency-1".into(),
            target: NetworkAgentIdentity {
                agent_id: "node-a".into(),
                agent_epoch: "epoch-1".into(),
            },
            controller: NetworkControllerLease {
                controller_id: "controller-a".into(),
                controller_epoch: "epoch-1".into(),
                fencing_token: 7,
            },
            deadline_unix_ms: 100,
            plan: plan(),
        }
    }

    #[derive(Default)]
    struct RecordingRealizer {
        calls: usize,
        observed: bool,
    }

    impl NetworkPlanRealizer for RecordingRealizer {
        type Error = &'static str;

        fn realize(&mut self, _plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
            self.calls += 1;
            Ok(())
        }

        fn observe(&mut self, _plan: &NodeNetworkPlan) -> Result<bool, Self::Error> {
            Ok(self.observed)
        }
    }

    #[test]
    fn equivalent_command_replays_after_executor_restart() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile_path("replay");
        let command = command();
        let executor =
            NetworkPlanExecutor::open(&root, command.target.clone(), command.controller.clone())?;
        let mut realizer = RecordingRealizer::default();
        assert_eq!(
            executor.execute(&command, 1, &mut realizer)?,
            PlanAdmission::Accepted
        );
        assert_eq!(realizer.calls, 1);
        drop(executor);
        let restarted =
            NetworkPlanExecutor::open(&root, command.target.clone(), command.controller.clone())?;
        assert_eq!(restarted.admit(&command, 2)?, PlanAdmission::Replayed);
        assert!(restarted.accepted(command.command_id)?);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn conflicting_payload_and_stale_identity_fail_closed() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile_path("fences");
        let command = command();
        let executor =
            NetworkPlanExecutor::open(&root, command.target.clone(), command.controller.clone())?;
        executor.admit(&command, 1)?;
        let mut conflict = command.clone();
        conflict.plan.fingerprint_sha256.replace_range(..1, "0");
        assert!(matches!(
            executor.admit(&conflict, 1),
            Err(NetworkExecutionError::ConflictingReplay)
        ));
        let mut stale = command.clone();
        stale.target.agent_epoch = "epoch-0".into();
        assert!(matches!(
            executor.admit(&stale, 1),
            Err(NetworkExecutionError::StaleAgentEpoch)
        ));
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn deadline_is_checked_before_journaling() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile_path("deadline");
        let command = command();
        let executor =
            NetworkPlanExecutor::open(&root, command.target.clone(), command.controller.clone())?;
        assert!(matches!(
            executor.admit(&command, 101),
            Err(NetworkExecutionError::DeadlineExpired)
        ));
        assert!(!executor.accepted(command.command_id)?);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn failed_realization_is_unknown_until_observation_resolves_it()
    -> Result<(), Box<dyn std::error::Error>> {
        struct FailingRealizer {
            observed: bool,
        }

        impl NetworkPlanRealizer for FailingRealizer {
            type Error = &'static str;

            fn realize(&mut self, _plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
                Err("transport interrupted")
            }

            fn observe(&mut self, _plan: &NodeNetworkPlan) -> Result<bool, Self::Error> {
                Ok(self.observed)
            }
        }

        let root = tempfile_path("unknown");
        let command = command();
        let executor =
            NetworkPlanExecutor::open(&root, command.target.clone(), command.controller.clone())?;
        let mut realizer = FailingRealizer { observed: false };
        assert!(matches!(
            executor.execute(&command, 1, &mut realizer),
            Err(NetworkExecutionError::MutationOutcomeUnknown)
        ));
        assert_eq!(
            executor.admit(&command, 2)?,
            PlanAdmission::Replayed,
            "a duplicate must not mutate while the outcome is unknown"
        );
        realizer.observed = true;
        assert_eq!(
            executor.reconcile(command.command_id, &mut realizer)?,
            NetworkPlanStatus::Succeeded
        );
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    fn tempfile_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("o3k-network-executor-{label}-{}", Uuid::new_v4()))
    }
}
