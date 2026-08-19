//! Bounded stateful L3/L4 policy realization using Linux nftables/conntrack.
//!
//! The canonical policy remains [`PolicyIntent`]. This provider owns one
//! marked table and scopes every rule to an O3K endpoint address, leaving
//! unrelated firewall state untouched. Default behavior is explicitly
//! stateful allow: rules add targeted denies/allows and established/related
//! return traffic is accepted by conntrack.

use o3k_domain::{NetworkPlanIntent, NetworkProtocol, PolicyAction, PolicyDirection, PolicyIntent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
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
    endpoints: Vec<PolicyEndpoint>,
}

trait PolicyCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, args: &[&str]) -> io::Result<bool>;
}

struct SystemPolicyCommand;

impl PolicyCommand for SystemPolicyCommand {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)> {
        let output = Command::new("nft").args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new("nft").args(args).status()?.success())
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
            command: Arc::new(SystemPolicyCommand),
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
        let fingerprint = fingerprint(&all_policies);
        let ownership = Ownership {
            fingerprint,
            endpoint_ids: all_endpoints
                .iter()
                .map(|endpoint| endpoint.endpoint_id)
                .collect(),
            policies: all_policies,
            endpoints: all_endpoints,
        };
        if self
            .ownership
            .as_ref()
            .is_some_and(|existing| existing == &ownership)
        {
            return Ok(());
        }
        let table_exists = self.ensure_foreign_safe()?;
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
        for (index, policy) in ownership.policies.iter().enumerate() {
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

fn protocol_name(protocol: NetworkProtocol) -> Option<&'static str> {
    match protocol {
        NetworkProtocol::Any => None,
        NetworkProtocol::Tcp => Some("tcp"),
        NetworkProtocol::Udp => Some("udp"),
        NetworkProtocol::Icmp => Some("icmp"),
    }
}

fn fingerprint(policies: &[PolicyIntent]) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(policies).unwrap_or_default())
    )
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
        listing: String,
    }

    impl PolicyCommand for FakeCommand {
        fn output(&self, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls
                .lock()
                .expect("calls")
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            Ok((!self.listing.is_empty(), self.listing.clone()))
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

    #[test]
    fn unknown_endpoint_is_rejected_before_host_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
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
            listing: "table ip o3k_policy { comment foreign; }".to_owned(),
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
            listing: String::new(),
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
    fn invalid_port_rule_is_rejected_before_host_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
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
    fn ingress_and_egress_ports_target_destination() {
        let root = std::env::temp_dir().join(format!("o3k-policy-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
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
            listing: String::new(),
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
}
