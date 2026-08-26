//! Bounded stateful L3/L4 policy realization using Linux nftables/conntrack.
//!
//! The canonical execution input is [`PolicyIntent`]. This provider owns one
//! marked table and scopes every rule to an O3K endpoint address, leaving
//! unrelated firewall state untouched. Default behavior is explicitly
//! stateful realization: canonical rules and per-Endpoint unmatched actions
//! are compiled into an owned nftables table; established/related return
//! traffic is accepted by conntrack.

use o3k_domain::{
    NetworkPlanIntent, NetworkProtocol, PolicyAction, PolicyDefaultIntent, PolicyDirection,
    PolicyIntent, PolicyStatefulMode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, ErrorKind, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

const TABLE: &str = "o3k_policy";
const CHAIN: &str = "forward";
const MARKER: &str = "o3k-p9-policy";
const STATE_FILE: &str = "policy.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEndpoint {
    pub endpoint_id: Uuid,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Ownership {
    fingerprint: String,
    endpoint_ids: Vec<Uuid>,
    #[serde(default)]
    policies: Vec<PolicyIntent>,
    #[serde(default)]
    defaults: Vec<PolicyDefaultIntent>,
    #[serde(default)]
    endpoints: Vec<PolicyEndpoint>,
    /// Provider-owned endpoint realization evidence. This is never used to
    /// reconstruct canonical policy state.
    #[serde(default)]
    endpoint_fingerprints: BTreeMap<Uuid, String>,
    /// Aggregate provider fingerprint associated with each endpoint evidence
    /// record. Endpoint evidence is valid only while the aggregate nftables
    /// ownership marker still has this value.
    #[serde(default)]
    endpoint_aggregate_fingerprints: BTreeMap<Uuid, String>,
}

trait PolicyCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, args: &[&str]) -> io::Result<bool>;
}

struct SystemPolicyCommand {
    namespace: Option<String>,
}

impl PolicyCommand for SystemPolicyCommand {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)> {
        let mut command = if let Some(namespace) = &self.namespace {
            let mut command = Command::new("ip");
            command.args(["netns", "exec", namespace, "nft"]);
            command
        } else {
            Command::new("nft")
        };
        let output = command.args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, args: &[&str]) -> io::Result<bool> {
        let mut command = if let Some(namespace) = &self.namespace {
            let mut command = Command::new("ip");
            command.args(["netns", "exec", namespace, "nft"]);
            command
        } else {
            Command::new("nft")
        };
        Ok(command.args(args).status()?.success())
    }
}

#[derive(Debug, Error)]
pub enum PolicyNetworkError {
    #[error("policy endpoint is not present in the accepted plan")]
    UnknownEndpoint,
    #[error("policy rule has an invalid port range or protocol combination")]
    InvalidRule,
    #[error("policy provider state is corrupt")]
    CorruptState,
    #[error("policy provider storage failed: {0}")]
    Storage(#[from] io::Error),
    #[error("policy host command failed")]
    CommandFailed,
    #[error("pre-existing policy table is not O3K-owned")]
    ForeignState,
    #[error("policy provider ownership conflicts with the accepted plan")]
    OwnershipConflict,
    #[error("policy default action is invalid or unsupported")]
    InvalidDefault,
    #[error("policy defaults conflict for one endpoint")]
    ConflictingDefault,
}

pub struct StatefulPolicyProvider {
    root: PathBuf,
    command: Arc<dyn PolicyCommand>,
    ownership: Option<Ownership>,
}

impl StatefulPolicyProvider {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PolicyNetworkError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ownership = load_state(&root.join(STATE_FILE))?;
        Ok(Self {
            root,
            command: Arc::new(SystemPolicyCommand { namespace: None }),
            ownership,
        })
    }

    /// Open the production provider against an explicitly isolated network
    /// namespace. The namespace is part of the execution boundary and is
    /// never inferred from canonical policy state.
    pub fn open_in_namespace(
        root: impl Into<PathBuf>,
        namespace: impl Into<String>,
    ) -> Result<Self, PolicyNetworkError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ownership = load_state(&root.join(STATE_FILE))?;
        Ok(Self {
            root,
            command: Arc::new(SystemPolicyCommand {
                namespace: Some(namespace.into()),
            }),
            ownership,
        })
    }

    #[cfg(test)]
    fn with_command(
        root: impl Into<PathBuf>,
        command: Arc<dyn PolicyCommand>,
    ) -> Result<Self, PolicyNetworkError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ownership = load_state(&root.join(STATE_FILE))?;
        Ok(Self {
            root,
            command,
            ownership,
        })
    }

    pub fn apply(
        &mut self,
        intents: &[NetworkPlanIntent],
        endpoints: &[PolicyEndpoint],
    ) -> Result<(), PolicyNetworkError> {
        let policies: Vec<PolicyIntent> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::Policy(policy) => Some(policy.clone()),
                _ => None,
            })
            .collect();
        let defaults: Vec<PolicyDefaultIntent> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::PolicyDefault(default) => Some(default.clone()),
                _ => None,
            })
            .collect();
        let current_addresses: std::collections::HashMap<Uuid, Ipv4Addr> = endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint_id, endpoint.address))
            .collect();
        for policy in &policies {
            if !current_addresses.contains_key(&policy.endpoint_id) {
                return Err(PolicyNetworkError::UnknownEndpoint);
            }
            validate_policy(policy)?;
        }
        for default in &defaults {
            if default.endpoint_id == Uuid::nil()
                || default.policy_id == Uuid::nil()
                || !current_addresses.contains_key(&default.endpoint_id)
            {
                return Err(PolicyNetworkError::InvalidDefault);
            }
            validate_default(default)?;
        }
        let current_endpoint_ids: std::collections::HashSet<Uuid> = endpoints
            .iter()
            .map(|endpoint| endpoint.endpoint_id)
            .collect();
        let mut all_policies: Vec<PolicyIntent> = self
            .ownership
            .as_ref()
            .map(|ownership| {
                ownership
                    .policies
                    .iter()
                    .filter(|policy| !current_endpoint_ids.contains(&policy.endpoint_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        all_policies.extend(policies);
        all_policies.sort_by_key(|policy| (policy.endpoint_id, policy.id));
        let mut all_defaults: Vec<PolicyDefaultIntent> = self
            .ownership
            .as_ref()
            .map(|ownership| {
                ownership
                    .defaults
                    .iter()
                    .filter(|default| !current_endpoint_ids.contains(&default.endpoint_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        all_defaults.extend(defaults);
        all_defaults.sort_by_key(|default| default.endpoint_id);
        if all_defaults
            .windows(2)
            .any(|pair| pair[0].endpoint_id == pair[1].endpoint_id)
        {
            return Err(PolicyNetworkError::ConflictingDefault);
        }
        let mut all_endpoints = self
            .ownership
            .as_ref()
            .map(|ownership| ownership.endpoints.clone())
            .unwrap_or_default();
        for endpoint in endpoints {
            if let Some(existing) = all_endpoints
                .iter_mut()
                .find(|existing| existing.endpoint_id == endpoint.endpoint_id)
            {
                *existing = endpoint.clone();
            } else {
                all_endpoints.push(endpoint.clone());
            }
        }
        let addresses: std::collections::HashMap<Uuid, Ipv4Addr> = all_endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint_id, endpoint.address))
            .collect();
        for policy in &all_policies {
            if !addresses.contains_key(&policy.endpoint_id) {
                return Err(PolicyNetworkError::UnknownEndpoint);
            }
        }
        let fingerprint = fingerprint(&all_policies, &all_defaults)?;
        let ownership = Ownership {
            fingerprint,
            endpoint_ids: all_endpoints
                .iter()
                .map(|endpoint| endpoint.endpoint_id)
                .collect(),
            policies: all_policies,
            defaults: all_defaults,
            endpoints: all_endpoints,
            endpoint_fingerprints: self
                .ownership
                .as_ref()
                .map(|value| value.endpoint_fingerprints.clone())
                .unwrap_or_default(),
            endpoint_aggregate_fingerprints: self
                .ownership
                .as_ref()
                .map(|value| value.endpoint_aggregate_fingerprints.clone())
                .unwrap_or_default(),
        };
        let table_exists = self.ensure_foreign_safe()?;
        if self
            .ownership
            .as_ref()
            .is_some_and(|existing| existing == &ownership)
        {
            return Ok(());
        }
        if table_exists && self.ownership.is_none() {
            return Err(PolicyNetworkError::ForeignState);
        }
        // Persist the exact accepted scope before mutation so an interrupted
        // replacement is recoverable and never mistaken for no state.
        store_state(&self.root.join(STATE_FILE), &ownership)?;
        self.ownership = Some(ownership.clone());
        if table_exists
            && !self
                .command
                .run(&["delete", "table", "ip", TABLE])
                .map_err(PolicyNetworkError::Storage)?
        {
            return Err(PolicyNetworkError::CommandFailed);
        }
        if !self
            .command
            .run(&[
                "add",
                "table",
                "ip",
                TABLE,
                "{",
                "comment",
                &format!(
                    "\"{}:{}\"",
                    MARKER,
                    self.ownership
                        .as_ref()
                        .map_or("unknown", |value| value.fingerprint.as_str())
                ),
                ";",
                "}",
            ])
            .map_err(PolicyNetworkError::Storage)?
            || !self
                .command
                .run(&[
                    "add", "chain", "ip", TABLE, CHAIN, "{", "type", "filter", "hook", "forward",
                    "priority", "-100", ";", "policy", "accept", ";", "}",
                ])
                .map_err(PolicyNetworkError::Storage)?
        {
            return Err(PolicyNetworkError::CommandFailed);
        }
        if !self
            .command
            .run(&[
                "add",
                "rule",
                "ip",
                TABLE,
                CHAIN,
                "ct",
                "state",
                "established,related",
                "accept",
                "comment",
                MARKER,
            ])
            .map_err(PolicyNetworkError::Storage)?
        {
            return Err(PolicyNetworkError::CommandFailed);
        }
        let mut sorted_policies = ownership.policies.clone();
        sorted_policies.sort_by_key(|policy| (policy.action != PolicyAction::Deny, policy.id));
        for (index, policy) in sorted_policies.iter().enumerate() {
            let endpoint_address = addresses[&policy.endpoint_id];
            let mut args = vec!["add", "rule", "ip", TABLE, CHAIN];
            let endpoint_value = endpoint_address.to_string();
            let prefix_value = policy
                .source
                .or(policy.destination)
                .map(|prefix| format!("{}/{}", prefix.network, prefix.prefix_len));
            let protocol = protocol_name(policy.protocol);
            let port = policy
                .ports
                .map(|ports| format!("{}-{}", ports.start, ports.end));
            if matches!(policy.direction, PolicyDirection::Ingress) {
                args.extend(["ip", "daddr"]);
                args.push(&endpoint_value);
            } else {
                args.extend(["ip", "saddr"]);
                args.push(&endpoint_value);
            }
            if let Some(prefix) = prefix_value.as_deref() {
                if matches!(policy.direction, PolicyDirection::Ingress) {
                    args.extend(["ip", "saddr"]);
                } else {
                    args.extend(["ip", "daddr"]);
                }
                args.push(prefix);
            }
            if let Some(protocol) = protocol {
                args.push(protocol);
                if let Some(port) = port.as_deref() {
                    args.extend(["dport", port]);
                }
            }
            args.push("counter");
            args.push(if policy.action == PolicyAction::Allow {
                "accept"
            } else {
                "drop"
            });
            let comment = format!("\"{}:{}\"", MARKER, index);
            args.extend(["comment", &comment]);
            if !self
                .command
                .run(&args)
                .map_err(PolicyNetworkError::Storage)?
            {
                return Err(PolicyNetworkError::CommandFailed);
            }
        }
        for default in &ownership.defaults {
            let endpoint_value = addresses[&default.endpoint_id].to_string();
            let mut args = vec!["add", "rule", "ip", TABLE, CHAIN];
            if default.unmatched_action == PolicyAction::Deny {
                args.extend(["ip", "daddr", &endpoint_value, "drop", "comment", MARKER]);
                if !self
                    .command
                    .run(&args)
                    .map_err(PolicyNetworkError::Storage)?
                {
                    return Err(PolicyNetworkError::CommandFailed);
                }
                let egress = vec![
                    "add",
                    "rule",
                    "ip",
                    TABLE,
                    CHAIN,
                    "ip",
                    "saddr",
                    &endpoint_value,
                    "drop",
                    "comment",
                    MARKER,
                ];
                if !self
                    .command
                    .run(&egress)
                    .map_err(PolicyNetworkError::Storage)?
                {
                    return Err(PolicyNetworkError::CommandFailed);
                }
            }
        }
        Ok(())
    }

    /// Replace exactly one Endpoint's effective snapshot while rebuilding the
    /// provider-owned aggregate table from the complete known inventory. The
    /// inventory is not the replacement scope: policies for every other
    /// Endpoint are retained, and their provider evidence is rebound only
    /// after the aggregate realization succeeds.
    pub fn apply_endpoint_snapshot(
        &mut self,
        endpoint_id: Uuid,
        intents: &[NetworkPlanIntent],
        known_endpoints: &[PolicyEndpoint],
    ) -> Result<(), PolicyNetworkError> {
        if !known_endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint_id == endpoint_id)
        {
            return Err(PolicyNetworkError::UnknownEndpoint);
        }
        let mut inventory = self
            .ownership
            .as_ref()
            .map(|ownership| ownership.endpoints.clone())
            .unwrap_or_default();
        for endpoint in known_endpoints {
            if let Some(existing) = inventory
                .iter_mut()
                .find(|existing| existing.endpoint_id == endpoint.endpoint_id)
            {
                *existing = endpoint.clone();
            } else {
                inventory.push(endpoint.clone());
            }
        }

        let mut aggregate = self
            .ownership
            .as_ref()
            .map(|ownership| {
                ownership
                    .policies
                    .iter()
                    .filter(|policy| policy.endpoint_id != endpoint_id)
                    .cloned()
                    .map(NetworkPlanIntent::Policy)
                    .chain(
                        ownership
                            .defaults
                            .iter()
                            .filter(|default| default.endpoint_id != endpoint_id)
                            .cloned()
                            .map(NetworkPlanIntent::PolicyDefault),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        aggregate.extend(intents.iter().cloned());
        self.apply(&aggregate, &inventory)?;

        let Some(mut ownership) = self.ownership.clone() else {
            return Err(PolicyNetworkError::CorruptState);
        };
        ownership.endpoint_fingerprints.remove(&endpoint_id);
        ownership
            .endpoint_aggregate_fingerprints
            .remove(&endpoint_id);
        for bound_aggregate in ownership.endpoint_aggregate_fingerprints.values_mut() {
            *bound_aggregate = ownership.fingerprint.clone();
        }
        store_state(&self.root.join(STATE_FILE), &ownership)?;
        self.ownership = Some(ownership);
        Ok(())
    }

    pub fn observe(&self) -> Result<bool, PolicyNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(true);
        };
        let (success, output) = self
            .command
            .output(&["list", "table", "ip", TABLE])
            .map_err(PolicyNetworkError::Storage)?;
        Ok(success && output.contains(MARKER) && output.contains(&ownership.fingerprint))
    }

    /// Record exact endpoint-scoped realization evidence after a successful
    /// provider application. The record is provider-owned derived state and
    /// is intentionally not a source for canonical policy reconstruction.
    pub fn record_endpoint_fingerprint(
        &mut self,
        endpoint_id: Uuid,
        fingerprint: &str,
    ) -> Result<(), PolicyNetworkError> {
        let Some(mut ownership) = self.ownership.clone() else {
            return Err(PolicyNetworkError::UnknownEndpoint);
        };
        if !ownership.endpoint_ids.contains(&endpoint_id) {
            return Err(PolicyNetworkError::UnknownEndpoint);
        }
        ownership
            .endpoint_fingerprints
            .insert(endpoint_id, fingerprint.to_owned());
        ownership
            .endpoint_aggregate_fingerprints
            .insert(endpoint_id, ownership.fingerprint.clone());
        store_state(&self.root.join(STATE_FILE), &ownership)?;
        self.ownership = Some(ownership);
        Ok(())
    }

    /// Observe exact endpoint-scoped provider evidence after a fresh provider
    /// instance is opened. The nftables table must still be O3K-owned.
    pub fn observe_endpoint_fingerprint(
        &self,
        endpoint_id: Uuid,
    ) -> Result<Option<String>, PolicyNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(None);
        };
        let (success, output) = self
            .command
            .output(&["list", "table", "ip", TABLE])
            .map_err(PolicyNetworkError::Storage)?;
        if !success {
            return Ok(None);
        }
        if !output.contains(MARKER) {
            return Err(PolicyNetworkError::ForeignState);
        }
        if !output.contains(&ownership.fingerprint) {
            return Err(PolicyNetworkError::CorruptState);
        }
        let endpoint_fingerprint = match ownership.endpoint_fingerprints.get(&endpoint_id) {
            Some(fingerprint) => fingerprint.clone(),
            None if ownership
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint_id == endpoint_id) =>
            {
                return Ok(None);
            }
            None => return Err(PolicyNetworkError::CorruptState),
        };
        let endpoint_aggregate = ownership
            .endpoint_aggregate_fingerprints
            .get(&endpoint_id)
            .ok_or(PolicyNetworkError::CorruptState)?;
        if endpoint_aggregate != &ownership.fingerprint {
            return Err(PolicyNetworkError::CorruptState);
        }
        Ok(Some(endpoint_fingerprint))
    }

    pub fn remove(&mut self) -> Result<(), PolicyNetworkError> {
        let Some(ownership) = self.ownership.take() else {
            return Ok(());
        };
        let (success, output) = self
            .command
            .output(&["list", "table", "ip", TABLE])
            .map_err(PolicyNetworkError::Storage)?;
        if success && !output.contains(MARKER) {
            self.ownership = Some(ownership);
            return Err(PolicyNetworkError::ForeignState);
        }
        if success
            && !self
                .command
                .run(&["delete", "table", "ip", TABLE])
                .map_err(PolicyNetworkError::Storage)?
        {
            self.ownership = Some(ownership);
            return Err(PolicyNetworkError::CommandFailed);
        }
        let _ = fs::remove_file(self.root.join(STATE_FILE));
        Ok(())
    }

    pub fn remove_for_plan(
        &mut self,
        intents: &[NetworkPlanIntent],
        endpoints: &[PolicyEndpoint],
    ) -> Result<(), PolicyNetworkError> {
        let targets: std::collections::HashSet<Uuid> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::Policy(policy) => Some(policy.endpoint_id),
                NetworkPlanIntent::PolicyDefault(default) => Some(default.endpoint_id),
                _ => None,
            })
            .chain(endpoints.iter().map(|endpoint| endpoint.endpoint_id))
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        let Some(ownership) = &self.ownership else {
            return Ok(());
        };
        if ownership
            .policies
            .iter()
            .all(|policy| targets.contains(&policy.endpoint_id))
        {
            return self.remove();
        }
        self.apply(&[], endpoints)
    }

    fn ensure_foreign_safe(&self) -> Result<bool, PolicyNetworkError> {
        let (success, output) = self
            .command
            .output(&["list", "table", "ip", TABLE])
            .map_err(PolicyNetworkError::Storage)?;
        if success && !output.contains(MARKER) {
            return Err(PolicyNetworkError::ForeignState);
        }
        // A provider instance may outlive the process that opened it while a
        // different instance advances the aggregate table.  Refuse to
        // replace that newer realization from stale in-memory ownership.
        if success
            && self.ownership.as_ref().is_some_and(|ownership| {
                !output.contains(&format!("{MARKER}:{}", ownership.fingerprint))
            })
        {
            return Err(PolicyNetworkError::OwnershipConflict);
        }
        Ok(success)
    }
}

fn validate_policy(policy: &PolicyIntent) -> Result<(), PolicyNetworkError> {
    if policy.ports.is_some_and(|ports| ports.start > ports.end)
        || policy.ports.is_some_and(|_| {
            matches!(
                policy.protocol,
                NetworkProtocol::Any | NetworkProtocol::Icmp
            )
        })
    {
        return Err(PolicyNetworkError::InvalidRule);
    }
    // The endpoint address is the destination of ingress and the source of
    // egress. Accepting the opposite endpoint-side prefix would produce an
    // ambiguous nft rule, so reject it before any host mutation.
    if matches!(policy.direction, PolicyDirection::Ingress) && policy.destination.is_some()
        || matches!(policy.direction, PolicyDirection::Egress) && policy.source.is_some()
    {
        return Err(PolicyNetworkError::InvalidRule);
    }
    Ok(())
}

fn validate_default(default: &PolicyDefaultIntent) -> Result<(), PolicyNetworkError> {
    (default.stateful_mode == PolicyStatefulMode::Stateful && default.generation > 0)
        .then_some(())
        .ok_or(PolicyNetworkError::InvalidDefault)
}

fn protocol_name(protocol: NetworkProtocol) -> Option<&'static str> {
    match protocol {
        NetworkProtocol::Any => None,
        NetworkProtocol::Tcp => Some("tcp"),
        NetworkProtocol::Udp => Some("udp"),
        NetworkProtocol::Icmp => Some("icmp"),
    }
}

fn fingerprint(
    policies: &[PolicyIntent],
    defaults: &[PolicyDefaultIntent],
) -> Result<String, PolicyNetworkError> {
    let bytes =
        serde_json::to_vec(&(policies, defaults)).map_err(|_| PolicyNetworkError::CorruptState)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn load_state(path: &Path) -> Result<Option<Ownership>, PolicyNetworkError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| PolicyNetworkError::CorruptState),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn store_state(path: &Path, ownership: &Ownership) -> Result<(), PolicyNetworkError> {
    let bytes =
        serde_json::to_vec_pretty(ownership).map_err(|_| PolicyNetworkError::CorruptState)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeCommand {
        calls: Mutex<Vec<Vec<String>>>,
        listing: Mutex<String>,
    }

    impl PolicyCommand for FakeCommand {
        fn output(&self, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls
                .lock()
                .expect("calls")
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            let listing = self.listing.lock().expect("listing").clone();
            Ok((!listing.is_empty(), listing))
        }

        fn run(&self, args: &[&str]) -> io::Result<bool> {
            self.calls
                .lock()
                .expect("calls")
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            Ok(true)
        }
    }

    fn policy(endpoint_id: Uuid) -> NetworkPlanIntent {
        NetworkPlanIntent::Policy(PolicyIntent {
            id: Uuid::from_u128(10),
            endpoint_id,
            direction: PolicyDirection::Ingress,
            protocol: NetworkProtocol::Tcp,
            ports: Some(o3k_domain::PortRange { start: 22, end: 22 }),
            source: None,
            destination: None,
            action: PolicyAction::Deny,
        })
    }

    fn policy_with(
        endpoint_id: Uuid,
        direction: PolicyDirection,
        protocol: NetworkProtocol,
        ports: Option<o3k_domain::PortRange>,
    ) -> NetworkPlanIntent {
        NetworkPlanIntent::Policy(PolicyIntent {
            id: Uuid::from_u128(11),
            endpoint_id,
            direction,
            protocol,
            ports,
            source: None,
            destination: None,
            action: PolicyAction::Allow,
        })
    }

    fn default(endpoint_id: Uuid, action: PolicyAction) -> NetworkPlanIntent {
        NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
            policy_id: Uuid::from_u128(12),
            endpoint_id,
            unmatched_action: action,
            stateful_mode: PolicyStatefulMode::Stateful,
            generation: 1,
        })
    }

    #[test]
    fn unknown_endpoint_is_rejected_before_host_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        assert!(matches!(
            provider.apply(&[policy(Uuid::from_u128(1))], &[]),
            Err(PolicyNetworkError::UnknownEndpoint)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn foreign_table_is_never_adopted() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new("table ip o3k_policy { comment foreign; }".to_owned()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(1);
        assert!(matches!(
            provider.apply(
                &[policy(endpoint)],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2)
                }]
            ),
            Err(PolicyNetworkError::ForeignState)
        ));
        assert_eq!(command.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn policy_realization_preserves_unrelated_endpoint_rules() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let first_endpoint = PolicyEndpoint {
            endpoint_id: first,
            address: Ipv4Addr::new(10, 0, 0, 2),
        };
        let second_endpoint = PolicyEndpoint {
            endpoint_id: second,
            address: Ipv4Addr::new(10, 0, 0, 3),
        };
        provider
            .apply(&[policy(first)], std::slice::from_ref(&first_endpoint))
            .expect("first policy");
        provider
            .apply(&[policy(second)], std::slice::from_ref(&second_endpoint))
            .expect("second policy");
        assert_eq!(
            provider
                .ownership
                .as_ref()
                .expect("ownership")
                .policies
                .len(),
            2
        );
        provider
            .remove_for_plan(&[policy(first)], std::slice::from_ref(&first_endpoint))
            .expect("remove first policy");
        let ownership = provider.ownership.as_ref().expect("remaining ownership");
        assert_eq!(ownership.policies.len(), 1);
        assert_eq!(ownership.policies[0].endpoint_id, second);
    }

    #[test]
    fn stale_provider_instance_cannot_replace_newer_aggregate() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let endpoint = PolicyEndpoint {
            endpoint_id: Uuid::from_u128(1),
            address: Ipv4Addr::new(10, 0, 0, 2),
        };
        let first_command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut first = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&first_command) as Arc<dyn PolicyCommand>,
        )
        .expect("first provider");
        first
            .apply_endpoint_snapshot(
                endpoint.endpoint_id,
                &[default(endpoint.endpoint_id, PolicyAction::Allow)],
                std::slice::from_ref(&endpoint),
            )
            .expect("initial aggregate");
        let first_fingerprint = first
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();

        let stale_command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(format!(
                "table ip {TABLE} {{ comment {MARKER}:{first_fingerprint}; }}"
            )),
        });
        let mut stale = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&stale_command) as Arc<dyn PolicyCommand>,
        )
        .expect("stale provider");
        let newer_command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(format!(
                "table ip {TABLE} {{ comment {MARKER}:{first_fingerprint}; }}"
            )),
        });
        let mut newer = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&newer_command) as Arc<dyn PolicyCommand>,
        )
        .expect("newer provider");
        newer
            .apply_endpoint_snapshot(
                endpoint.endpoint_id,
                &[default(endpoint.endpoint_id, PolicyAction::Deny)],
                std::slice::from_ref(&endpoint),
            )
            .expect("newer aggregate");

        let newer_fingerprint = newer
            .ownership
            .as_ref()
            .expect("new ownership")
            .fingerprint
            .clone();
        assert_ne!(first_fingerprint, newer_fingerprint);
        *stale_command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{newer_fingerprint}; }}");
        // The stale instance observes the newer provider marker and must
        // refuse replacement before issuing a destructive nftables command.
        let result = stale.apply_endpoint_snapshot(
            endpoint.endpoint_id,
            &[default(endpoint.endpoint_id, PolicyAction::Allow)],
            std::slice::from_ref(&endpoint),
        );
        assert!(
            matches!(result, Err(PolicyNetworkError::OwnershipConflict)),
            "unexpected stale writer result: {result:?}"
        );
        assert_eq!(stale_command.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn invalid_port_rule_is_rejected_before_host_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(1);
        assert!(matches!(
            provider.apply(
                &[policy_with(
                    endpoint,
                    PolicyDirection::Ingress,
                    NetworkProtocol::Icmp,
                    Some(o3k_domain::PortRange { start: 1, end: 1 }),
                )],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2),
                }]
            ),
            Err(PolicyNetworkError::InvalidRule)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn deny_default_realizes_endpoint_scoped_terminal_drops() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(1);
        provider
            .apply(
                &[default(endpoint, PolicyAction::Deny)],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2),
                }],
            )
            .expect("deny default");
        let calls = command.calls.lock().expect("calls");
        let drops = calls
            .iter()
            .filter(|call| call.iter().any(|value| value == "drop"))
            .count();
        assert_eq!(drops, 2);
        assert!(calls.iter().any(|call| {
            call.windows(3)
                .any(|window| window == ["ip", "daddr", "10.0.0.2"])
        }));
        assert!(calls.iter().any(|call| {
            call.windows(3)
                .any(|window| window == ["ip", "saddr", "10.0.0.2"])
        }));
    }

    #[test]
    fn ingress_and_egress_ports_target_destination() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(1);
        provider
            .apply(
                &[
                    policy_with(
                        endpoint,
                        PolicyDirection::Ingress,
                        NetworkProtocol::Tcp,
                        Some(o3k_domain::PortRange { start: 22, end: 22 }),
                    ),
                    policy_with(
                        endpoint,
                        PolicyDirection::Egress,
                        NetworkProtocol::Tcp,
                        Some(o3k_domain::PortRange {
                            start: 443,
                            end: 443,
                        }),
                    ),
                ],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2),
                }],
            )
            .expect("policy apply");
        let calls = command.calls.lock().expect("calls");
        assert!(
            calls
                .iter()
                .any(|call| { call.windows(2).any(|pair| pair == ["dport", "22-22"]) })
        );
        assert!(
            calls
                .iter()
                .any(|call| { call.windows(2).any(|pair| pair == ["dport", "443-443"]) })
        );
    }

    #[test]
    fn cleanup_reaps_durable_policy_state_when_table_is_already_absent() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(1);
        provider
            .apply(
                &[policy(endpoint)],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2),
                }],
            )
            .expect("policy apply");
        assert!(root.join(STATE_FILE).exists());

        provider.remove().expect("policy cleanup");
        assert!(!root.join(STATE_FILE).exists());
    }

    #[test]
    fn endpoint_fingerprint_observation_survives_provider_restart() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let endpoint = Uuid::from_u128(1);
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        provider
            .apply(
                &[policy(endpoint)],
                &[PolicyEndpoint {
                    endpoint_id: endpoint,
                    address: Ipv4Addr::new(10, 0, 0, 2),
                }],
            )
            .expect("policy apply");
        let aggregate = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        let endpoint_fingerprint = "canonical-f1";
        provider
            .record_endpoint_fingerprint(endpoint, endpoint_fingerprint)
            .expect("record endpoint evidence");
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate}; }}");
        drop(provider);

        let reopened = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("reopened provider");
        assert_eq!(
            reopened
                .observe_endpoint_fingerprint(endpoint)
                .expect("observation"),
            Some(endpoint_fingerprint.to_owned())
        );
    }

    #[test]
    fn endpoint_evidence_is_rejected_when_aggregate_realization_changes() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let endpoint_a = Uuid::from_u128(1);
        let endpoint_b = Uuid::from_u128(2);
        let endpoints = vec![
            PolicyEndpoint {
                endpoint_id: endpoint_a,
                address: Ipv4Addr::new(10, 0, 0, 2),
            },
            PolicyEndpoint {
                endpoint_id: endpoint_b,
                address: Ipv4Addr::new(10, 0, 0, 3),
            },
        ];
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        provider
            .apply(&[policy(endpoint_a)], &endpoints)
            .expect("apply A");
        let aggregate_a = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        provider
            .record_endpoint_fingerprint(endpoint_a, "endpoint-a-f1")
            .expect("record endpoint evidence");
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate_a}; }}");

        provider
            .apply(&[policy(endpoint_b)], &endpoints)
            .expect("apply B");
        let aggregate_b = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        assert_ne!(aggregate_a, aggregate_b);
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate_b}; }}");

        assert!(matches!(
            provider.observe_endpoint_fingerprint(endpoint_a),
            Err(PolicyNetworkError::CorruptState)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn endpoint_replacement_preserves_unaffected_endpoint_evidence() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: Mutex::new(String::new()),
        });
        let endpoint_a = Uuid::from_u128(1);
        let endpoint_b = Uuid::from_u128(2);
        let endpoints = vec![
            PolicyEndpoint {
                endpoint_id: endpoint_a,
                address: Ipv4Addr::new(10, 0, 0, 2),
            },
            PolicyEndpoint {
                endpoint_id: endpoint_b,
                address: Ipv4Addr::new(10, 0, 0, 3),
            },
        ];
        let mut provider = StatefulPolicyProvider::with_command(
            &root,
            Arc::clone(&command) as Arc<dyn PolicyCommand>,
        )
        .expect("provider");
        provider
            .apply(
                &[
                    NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
                        policy_id: Uuid::from_u128(11),
                        endpoint_id: endpoint_a,
                        unmatched_action: PolicyAction::Deny,
                        stateful_mode: PolicyStatefulMode::Stateful,
                        generation: 1,
                    }),
                    NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
                        policy_id: Uuid::from_u128(12),
                        endpoint_id: endpoint_b,
                        unmatched_action: PolicyAction::Deny,
                        stateful_mode: PolicyStatefulMode::Stateful,
                        generation: 1,
                    }),
                ],
                &endpoints,
            )
            .expect("initial aggregate");
        provider
            .record_endpoint_fingerprint(endpoint_a, "FA1")
            .expect("record A");
        provider
            .record_endpoint_fingerprint(endpoint_b, "FB1")
            .expect("record B");
        let aggregate = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate}; }}");

        provider
            .apply_endpoint_snapshot(
                endpoint_a,
                &[NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
                    policy_id: Uuid::from_u128(11),
                    endpoint_id: endpoint_a,
                    unmatched_action: PolicyAction::Allow,
                    stateful_mode: PolicyStatefulMode::Stateful,
                    generation: 2,
                })],
                &endpoints,
            )
            .expect("replace A");
        provider
            .record_endpoint_fingerprint(endpoint_a, "FA2")
            .expect("record A2");
        let aggregate = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate}; }}");
        assert_eq!(
            provider.observe_endpoint_fingerprint(endpoint_a).ok(),
            Some(Some("FA2".into()))
        );
        assert_eq!(
            provider.observe_endpoint_fingerprint(endpoint_b).ok(),
            Some(Some("FB1".into()))
        );

        provider
            .apply_endpoint_snapshot(endpoint_a, &[], &endpoints)
            .expect("remove A");
        let aggregate = provider
            .ownership
            .as_ref()
            .expect("ownership")
            .fingerprint
            .clone();
        *command.listing.lock().expect("listing") =
            format!("table ip {TABLE} {{ comment {MARKER}:{aggregate}; }}");
        assert_eq!(
            provider.observe_endpoint_fingerprint(endpoint_a).ok(),
            Some(None)
        );
        assert_eq!(
            provider.observe_endpoint_fingerprint(endpoint_b).ok(),
            Some(Some("FB1".into()))
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
