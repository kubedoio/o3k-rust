//! Fail-closed Linux realization for the accepted P11 v1 fabric contract.
//!
//! Provider-native objects are bounded by an ownership manifest. WireGuard
//! private-key bytes are generated and retained locally and never occur in
//! plans, protocol messages, observations, or ordinary logs.

use crate::p11::{P11FabricBackend, P11FabricError};
use o3k_domain::{FabricPeer, NamespacedRoutedFabricPlan};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxP11Config {
    pub root: PathBuf,
    pub fabric_namespace: String,
    pub fabric_interface: String,
    pub wireguard_port: u16,
}

impl LinuxP11Config {
    #[must_use]
    pub fn for_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fabric_namespace: "o3k-fabric".to_owned(),
            fabric_interface: "wg-o3k".to_owned(),
            wireguard_port: 51_820,
        }
    }

    fn validate(&self) -> Result<(), LinuxP11Error> {
        if self.root == Path::new("/")
            || self.root.as_os_str().is_empty()
            || !valid_name(&self.fabric_namespace)
            || !valid_name(&self.fabric_interface)
            || self.wireguard_port == 0
        {
            return Err(LinuxP11Error::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LinuxP11Error {
    #[error("Linux P11 configuration is invalid")]
    InvalidConfiguration,
    #[error("Linux P11 provider state is corrupt")]
    CorruptState,
    #[error("Linux P11 provider state is foreign or ambiguous")]
    ForeignState,
    #[error("Linux P11 provider state conflicts with the requested plan")]
    OwnershipConflict,
    #[error("Linux P11 provider command failed")]
    CommandFailed,
    #[error("Linux P11 provider state storage failed: {0}")]
    Storage(#[from] io::Error),
}

impl From<LinuxP11Error> for P11FabricError {
    fn from(error: LinuxP11Error) -> Self {
        Self::Backend(error.to_string())
    }
}

trait LinuxP11Command: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool>;
}

struct SystemLinuxP11Command;

impl LinuxP11Command for SystemLinuxP11Command {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
        let output = Command::new(program).args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new(program).args(args).status()?.success())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FabricOwnership {
    namespace: String,
    interface: String,
    private_key_path: String,
    fabric_generation: u64,
    #[serde(default)]
    managed_peers: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RealmOwnership {
    realm_id: Uuid,
    namespace: String,
    bridge: String,
    host_veth: String,
    realm_veth: String,
    fabric_veth: String,
    fabric_realm_veth: String,
    directory_generation: u64,
    local_fabric_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProviderState {
    version: u32,
    #[serde(default)]
    fabric: Option<FabricOwnership>,
    #[serde(default)]
    realms: BTreeMap<Uuid, RealmOwnership>,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            fabric: None,
            realms: BTreeMap::new(),
        }
    }
}

pub struct LinuxP11FabricBackend {
    config: LinuxP11Config,
    state_path: PathBuf,
    plans_path: PathBuf,
    command: Arc<dyn LinuxP11Command>,
    state: ProviderState,
    plans: BTreeMap<Uuid, NamespacedRoutedFabricPlan>,
}

impl LinuxP11FabricBackend {
    pub fn open(config: LinuxP11Config) -> Result<Self, LinuxP11Error> {
        config.validate()?;
        let state_path = config.root.join("ownership.json");
        let plans_path = config.root.join("plans");
        fs::create_dir_all(&plans_path)?;
        let backend = Self {
            config,
            state_path: state_path.clone(),
            plans_path: plans_path.clone(),
            command: Arc::new(SystemLinuxP11Command),
            state: load_state(&state_path)?,
            plans: load_plans(&plans_path)?,
        };
        backend.validate_loaded_state()?;
        Ok(backend)
    }

    #[cfg(test)]
    fn with_command(
        config: LinuxP11Config,
        command: Arc<dyn LinuxP11Command>,
    ) -> Result<Self, LinuxP11Error> {
        config.validate()?;
        let state_path = config.root.join("ownership.json");
        let plans_path = config.root.join("plans");
        fs::create_dir_all(&plans_path)?;
        let backend = Self {
            config,
            state_path: state_path.clone(),
            plans_path: plans_path.clone(),
            command,
            state: load_state(&state_path)?,
            plans: load_plans(&plans_path)?,
        };
        backend.validate_loaded_state()?;
        Ok(backend)
    }

    fn validate_loaded_state(&self) -> Result<(), LinuxP11Error> {
        if self.state.version != STATE_VERSION {
            return Err(LinuxP11Error::CorruptState);
        }
        if let Some(fabric) = &self.state.fabric
            && (fabric.namespace != self.config.fabric_namespace
                || fabric.interface != self.config.fabric_interface
                || fabric.fabric_generation == 0
                || Path::new(&fabric.private_key_path).parent() != Some(self.config.root.as_path()))
        {
            return Err(LinuxP11Error::CorruptState);
        }
        if let Some(fabric) = &self.state.fabric {
            validate_private_key_file(Path::new(&fabric.private_key_path))?;
        }
        for (realm_id, ownership) in &self.state.realms {
            if realm_id != &ownership.realm_id
                || self.plans.get(realm_id).is_none_or(|plan| {
                    plan.realm_id != *realm_id
                        || plan.directory_generation != ownership.directory_generation
                        || plan.local_fabric_generation != ownership.local_fabric_generation
                })
            {
                return Err(LinuxP11Error::CorruptState);
            }
        }
        Ok(())
    }

    fn realm_ownership(&self, plan: &NamespacedRoutedFabricPlan) -> RealmOwnership {
        let suffix = plan.realm_id.simple().to_string();
        RealmOwnership {
            realm_id: plan.realm_id,
            namespace: format!("o3k-r-{}", &suffix[..8]),
            bridge: format!("o3k-b-{}", &suffix[..8]),
            host_veth: format!("o3k-h-{}", &suffix[..8]),
            realm_veth: format!("o3k-n-{}", &suffix[..8]),
            fabric_veth: format!("o3k-f-{}", &suffix[..8]),
            fabric_realm_veth: format!("o3k-x-{}", &suffix[..8]),
            directory_generation: plan.directory_generation,
            local_fabric_generation: plan.local_fabric_generation,
        }
    }

    fn ensure_fabric(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        if let Some(fabric) = &self.state.fabric {
            if plan.local_fabric_generation != fabric.fabric_generation {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            return Ok(());
        }
        let private_key_path = self.config.root.join("wireguard-private.key");
        let (exists, _) = self
            .command
            .output(
                "ip",
                &["netns", "exec", &self.config.fabric_namespace, "true"],
            )
            .map_err(LinuxP11Error::Storage)?;
        if exists {
            return Err(LinuxP11Error::ForeignState);
        }
        if private_key_path.exists() {
            return Err(LinuxP11Error::ForeignState);
        }
        write_private_key(&private_key_path, &self.command)?;
        self.state.fabric = Some(FabricOwnership {
            namespace: self.config.fabric_namespace.clone(),
            interface: self.config.fabric_interface.clone(),
            private_key_path: private_key_path.display().to_string(),
            fabric_generation: plan.local_fabric_generation,
            managed_peers: BTreeSet::new(),
        });
        store_state(&self.state_path, &self.state)?;
        if !self
            .command
            .run("ip", &["netns", "add", &self.config.fabric_namespace])
            .map_err(LinuxP11Error::Storage)?
            || !self
                .command
                .run(
                    "ip",
                    &[
                        "link",
                        "add",
                        &self.config.fabric_interface,
                        "type",
                        "wireguard",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            || !self
                .command
                .run(
                    "ip",
                    &[
                        "link",
                        "set",
                        &self.config.fabric_interface,
                        "netns",
                        &self.config.fabric_namespace,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    &self.config.fabric_namespace,
                    "wg",
                    "set",
                    &self.config.fabric_interface,
                    "private-key",
                    private_key_path
                        .to_str()
                        .ok_or(LinuxP11Error::CorruptState)?,
                    "listen-port",
                    &self.config.wireguard_port.to_string(),
                ],
            )
            .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    &self.config.fabric_namespace,
                    "ip",
                    "link",
                    "set",
                    &self.config.fabric_interface,
                    "up",
                ],
            )
            .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        Ok(())
    }

    fn ensure_realm(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        let ownership = self.realm_ownership(plan);
        if let Some(existing) = self.state.realms.get(&plan.realm_id)
            && (existing.namespace != ownership.namespace
                || existing.bridge != ownership.bridge
                || existing.host_veth != ownership.host_veth
                || existing.realm_veth != ownership.realm_veth
                || existing.fabric_veth != ownership.fabric_veth
                || existing.fabric_realm_veth != ownership.fabric_realm_veth
                || plan.directory_generation < existing.directory_generation
                || plan.local_fabric_generation < existing.local_fabric_generation)
        {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        if !self.state.realms.contains_key(&plan.realm_id) {
            let (exists, _) = self
                .command
                .output("ip", &["netns", "exec", &ownership.namespace, "true"])
                .map_err(LinuxP11Error::Storage)?;
            if exists {
                return Err(LinuxP11Error::ForeignState);
            }
            self.state.realms.insert(plan.realm_id, ownership.clone());
            store_state(&self.state_path, &self.state)?;
            let commands = [
                vec!["netns", "add", ownership.namespace.as_str()],
                vec!["link", "add", ownership.bridge.as_str(), "type", "bridge"],
                vec!["link", "set", ownership.bridge.as_str(), "up"],
                vec![
                    "link",
                    "add",
                    ownership.host_veth.as_str(),
                    "type",
                    "veth",
                    "peer",
                    "name",
                    ownership.realm_veth.as_str(),
                ],
                vec![
                    "link",
                    "add",
                    ownership.fabric_veth.as_str(),
                    "type",
                    "veth",
                    "peer",
                    "name",
                    ownership.fabric_realm_veth.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.realm_veth.as_str(),
                    "netns",
                    ownership.namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.fabric_veth.as_str(),
                    "netns",
                    ownership.namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.fabric_realm_veth.as_str(),
                    "netns",
                    self.config.fabric_namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.host_veth.as_str(),
                    "master",
                    ownership.bridge.as_str(),
                ],
                vec!["link", "set", ownership.host_veth.as_str(), "up"],
            ];
            for args in commands {
                if !self
                    .command
                    .run("ip", &args)
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &ownership.namespace,
                        "ip",
                        "link",
                        "set",
                        &ownership.realm_veth,
                        "up",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            for (namespace, interface) in [
                (&ownership.namespace, &ownership.fabric_veth),
                (&self.config.fabric_namespace, &ownership.fabric_realm_veth),
            ] {
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns", "exec", namespace, "ip", "link", "set", interface, "up",
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &ownership.namespace,
                        "sysctl",
                        "-w",
                        "net.ipv4.ip_forward=1",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        } else if self.state.realms.get(&plan.realm_id) != Some(&ownership) {
            self.state.realms.insert(plan.realm_id, ownership.clone());
            store_state(&self.state_path, &self.state)?;
        }
        self.realize_routes(plan, &ownership)
    }

    fn realize_routes(
        &self,
        plan: &NamespacedRoutedFabricPlan,
        ownership: &RealmOwnership,
    ) -> Result<(), LinuxP11Error> {
        for route in &plan.routes {
            let destination = format!("{}/32", route.destination.network);
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &ownership.namespace,
                        "ip",
                        "route",
                        "replace",
                        &destination,
                        "dev",
                        &ownership.fabric_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &ownership.namespace,
                            "ip",
                            "neigh",
                            "replace",
                            &route.destination.network.to_string(),
                            "lladdr",
                            &plan.proxy_mac,
                            "nud",
                            "permanent",
                            "dev",
                            &ownership.realm_veth,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for entry in &plan.directory.entries {
            if entry.selected_host == plan.local_host {
                let destination = format!("{}/32", entry.fixed_ip);
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &self.config.fabric_namespace,
                            "ip",
                            "route",
                            "replace",
                            &destination,
                            "dev",
                            &ownership.fabric_realm_veth,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
        }
        Ok(())
    }

    fn configure_peers(&mut self) -> Result<(), LinuxP11Error> {
        let Some(fabric) = self.state.fabric.clone() else {
            return Err(LinuxP11Error::CorruptState);
        };
        let mut peers = BTreeMap::<String, FabricPeer>::new();
        for plan in self
            .plans
            .values()
            .filter(|plan| self.state.realms.contains_key(&plan.realm_id))
        {
            for peer in &plan.peers {
                if let Some(existing) = peers.get_mut(&peer.host_id) {
                    if existing.public_key != peer.public_key
                        || existing.underlay_endpoint != peer.underlay_endpoint
                        || existing.fabric_generation != peer.fabric_generation
                    {
                        return Err(LinuxP11Error::OwnershipConflict);
                    }
                    existing
                        .allowed_destinations
                        .extend(peer.allowed_destinations.iter().copied());
                } else {
                    peers.insert(peer.host_id.clone(), peer.clone());
                }
            }
        }
        let current_keys = peers
            .values()
            .map(|peer| peer.public_key.clone())
            .collect::<BTreeSet<_>>();
        for stale_key in fabric.managed_peers.difference(&current_keys) {
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &fabric.namespace,
                        "wg",
                        "set",
                        &fabric.interface,
                        "peer",
                        stale_key,
                        "remove",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for peer in peers.values_mut() {
            if !valid_wireguard_key(&peer.public_key)
                || peer.host_id.is_empty()
                || peer.host_id.len() > 63
                || !peer
                    .host_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || peer
                    .underlay_endpoint
                    .parse::<std::net::SocketAddr>()
                    .is_err()
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            peer.allowed_destinations.sort();
            peer.allowed_destinations.dedup();
            let mut args = vec![
                "netns".to_owned(),
                "exec".to_owned(),
                fabric.namespace.clone(),
                "wg".to_owned(),
                "set".to_owned(),
                fabric.interface.clone(),
                "peer".to_owned(),
                peer.public_key.clone(),
                "endpoint".to_owned(),
                peer.underlay_endpoint.clone(),
            ];
            let allowed_ips = peer
                .allowed_destinations
                .iter()
                .map(|destination| format!("{}/32", destination.network))
                .collect::<Vec<_>>()
                .join(",");
            if allowed_ips.is_empty() {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            args.extend(["allowed-ips".to_owned(), allowed_ips]);
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            if !self
                .command
                .run("ip", &refs)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            for destination in &peer.allowed_destinations {
                let route = format!("{}/32", destination.network);
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &fabric.namespace,
                            "ip",
                            "route",
                            "replace",
                            &route,
                            "dev",
                            &fabric.interface,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
        }
        if let Some(stored) = self.state.fabric.as_mut() {
            stored.managed_peers = current_keys;
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }

    fn persist_plan(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        store_plan(
            &self.plans_path.join(format!("{}.json", plan.realm_id)),
            plan,
        )?;
        self.plans.insert(plan.realm_id, plan.clone());
        Ok(())
    }

    fn remove_plan(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        self.plans.remove(&plan.realm_id);
        let _ = fs::remove_file(self.plans_path.join(format!("{}.json", plan.realm_id)));
        if self.state.realms.remove(&plan.realm_id).is_some() {
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }

    fn remove_fabric_if_unused(&mut self, generation: u64) -> Result<(), LinuxP11Error> {
        if !self.state.realms.is_empty() {
            return Ok(());
        }
        let Some(fabric) = self.state.fabric.clone() else {
            return Ok(());
        };
        if generation < fabric.fabric_generation {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        let (namespace_exists, _) = self
            .command
            .output("ip", &["netns", "exec", &fabric.namespace, "true"])
            .map_err(LinuxP11Error::Storage)?;
        if namespace_exists {
            let (interface_exists, _) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &fabric.namespace,
                        "ip",
                        "link",
                        "show",
                        "dev",
                        &fabric.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if interface_exists
                && !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &fabric.namespace,
                            "ip",
                            "link",
                            "del",
                            &fabric.interface,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            if !self
                .command
                .run("ip", &["netns", "del", &fabric.namespace])
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        fs::remove_file(&fabric.private_key_path).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        self.state.fabric = None;
        store_state(&self.state_path, &self.state)?;
        Ok(())
    }
}

impl P11FabricBackend for LinuxP11FabricBackend {
    fn apply(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), P11FabricError> {
        self.ensure_fabric(plan)?;
        self.persist_plan(plan)?;
        self.ensure_realm(plan)?;
        self.configure_peers()?;
        Ok(())
    }

    fn remove(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), P11FabricError> {
        let Some(ownership) = self.state.realms.get(&plan.realm_id).cloned() else {
            self.remove_fabric_if_unused(plan.local_fabric_generation)?;
            return Ok(());
        };
        if plan.directory_generation < ownership.directory_generation
            || plan.local_fabric_generation < ownership.local_fabric_generation
        {
            return Err(P11FabricError::StaleGeneration);
        }
        let commands = [
            vec![
                "netns",
                "exec",
                ownership.namespace.as_str(),
                "ip",
                "route",
                "flush",
                "table",
                "main",
            ],
            vec!["link", "del", ownership.host_veth.as_str()],
            vec!["link", "del", ownership.bridge.as_str()],
            vec!["netns", "del", ownership.namespace.as_str()],
        ];
        for args in commands {
            if !self
                .command
                .run("ip", &args)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed.into());
            }
        }
        self.remove_plan(plan)?;
        if !self.state.realms.is_empty() {
            self.configure_peers()?;
        }
        self.remove_fabric_if_unused(plan.local_fabric_generation)?;
        Ok(())
    }

    fn observe(&self, plan: &NamespacedRoutedFabricPlan) -> Result<bool, P11FabricError> {
        let Some(ownership) = self.state.realms.get(&plan.realm_id) else {
            return Ok(false);
        };
        let (success, _) = self
            .command
            .output("ip", &["netns", "exec", &ownership.namespace, "true"])
            .map_err(LinuxP11Error::Storage)?;
        Ok(success && self.plans.get(&plan.realm_id) == Some(plan))
    }

    fn observe_removed(&self, plan: &NamespacedRoutedFabricPlan) -> Result<bool, P11FabricError> {
        Ok(!self.state.realms.contains_key(&plan.realm_id)
            && !self.plans.contains_key(&plan.realm_id))
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn load_state(path: &Path) -> Result<ProviderState, LinuxP11Error> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| LinuxP11Error::CorruptState),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ProviderState::default()),
        Err(error) => Err(LinuxP11Error::Storage(error)),
    }
}

fn load_plans(path: &Path) -> Result<BTreeMap<Uuid, NamespacedRoutedFabricPlan>, LinuxP11Error> {
    let mut plans = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() || !entry.file_type()?.is_file() {
            return Err(LinuxP11Error::ForeignState);
        }
        let plan: NamespacedRoutedFabricPlan = serde_json::from_slice(&fs::read(entry.path())?)
            .map_err(|_| LinuxP11Error::CorruptState)?;
        plans.insert(plan.realm_id, plan);
    }
    Ok(plans)
}

fn store_state(path: &Path, state: &ProviderState) -> Result<(), LinuxP11Error> {
    store_json(path, state)
}

fn store_plan(path: &Path, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
    store_json(path, plan)
}

fn store_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LinuxP11Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| LinuxP11Error::CorruptState)?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn write_private_key(path: &Path, command: &Arc<dyn LinuxP11Command>) -> Result<(), LinuxP11Error> {
    if path.exists() {
        return validate_private_key_file(path);
    }
    let (success, key) = command
        .output("wg", &["genkey"])
        .map_err(LinuxP11Error::Storage)?;
    if !success || !valid_wireguard_key(key.trim()) || key.lines().count() != 1 {
        return Err(LinuxP11Error::CommandFailed);
    }
    let mut file = fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    let mut file = file.open(path)?;
    file.write_all(key.trim().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn validate_private_key_file(path: &Path) -> Result<(), LinuxP11Error> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(LinuxP11Error::ForeignState);
    }
    let key = fs::read_to_string(path)?;
    if !valid_wireguard_key(key.trim()) {
        return Err(LinuxP11Error::ForeignState);
    }
    Ok(())
}

fn valid_wireguard_key(value: &str) -> bool {
    value.len() == 44
        && value.ends_with('=')
        && value[..43]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use o3k_domain::{
        AddressRealm, EndpointLocation, FabricHostIdentity, Ipv4Prefix, RealmEndpointDirectory,
    };
    use std::{os::unix::fs::PermissionsExt, sync::Mutex};

    struct FakeCommand {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        namespace_exists: bool,
    }

    impl LinuxP11Command for FakeCommand {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if args.starts_with(&["netns", "exec"]) && self.namespace_exists {
                return Ok((true, String::new()));
            }
            if program == "wg" {
                return Ok((true, format!("{}\n", "A".repeat(43) + "=")));
            }
            Ok((false, String::new()))
        }

        fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            Ok(true)
        }
    }

    fn plan() -> NamespacedRoutedFabricPlan {
        let realm = AddressRealm {
            id: Uuid::from_u128(11),
            project_id: "project-a".to_owned(),
            prefix: Ipv4Prefix::new("10.40.1.0".parse().expect("ip"), 24).expect("prefix"),
            overlapping_prefixes: false,
        };
        let directory = RealmEndpointDirectory::build(
            &realm,
            vec![EndpointLocation {
                endpoint_id: Uuid::from_u128(12),
                project_id: realm.project_id.clone(),
                realm_id: realm.id,
                fixed_ip: "10.40.1.12".parse().expect("ip"),
                mac: "02:00:00:00:00:12".to_owned(),
                selected_host: "host-b".to_owned(),
                endpoint_generation: 1,
                placement_generation: 1,
            }],
            &[],
            2,
        )
        .expect("directory");
        let local = FabricHostIdentity {
            host_id: "host-a".to_owned(),
            public_key: "public-a".to_owned(),
            underlay_endpoint: "192.0.2.1:51820".to_owned(),
            provider_version: "wireguard-v1".to_owned(),
            fabric_generation: 3,
            underlay_mtu: 1500,
            fabric_mtu: 1420,
        };
        let remote = FabricHostIdentity {
            host_id: "host-b".to_owned(),
            public_key: "B".repeat(43) + "=",
            underlay_endpoint: "192.0.2.2:51820".to_owned(),
            provider_version: "wireguard-v1".to_owned(),
            fabric_generation: 3,
            underlay_mtu: 1500,
            fabric_mtu: 1420,
        };
        directory
            .compile_fabric_plan(&local, &[local.clone(), remote], 1400)
            .expect("plan")
    }

    #[test]
    fn provider_refuses_foreign_fabric_namespace() {
        let root = std::env::temp_dir().join(format!("o3k-p11-linux-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            namespace_exists: true,
        });
        let mut provider =
            LinuxP11FabricBackend::with_command(LinuxP11Config::for_root(&root), command)
                .expect("provider");
        assert!(matches!(
            provider.apply(&plan()),
            Err(P11FabricError::Backend(message)) if message.contains("foreign")
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_records_key_path_but_never_plan_key_material() {
        let root = std::env::temp_dir().join(format!("o3k-p11-linux-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            namespace_exists: false,
        });
        let command_for_assertion = Arc::clone(&command);
        let mut provider =
            LinuxP11FabricBackend::with_command(LinuxP11Config::for_root(&root), command)
                .expect("provider");
        provider.apply(&plan()).expect("apply");
        let state = fs::read_to_string(root.join("ownership.json")).expect("state");
        let serialized =
            fs::read_to_string(root.join("plans").join(format!("{}.json", plan().realm_id)))
                .expect("plan");
        assert!(!state.contains(&"A".repeat(43)));
        assert!(!serialized.contains(&"A".repeat(43)));
        assert!(state.contains("wireguard-private.key"));
        assert_eq!(
            fs::metadata(root.join("wireguard-private.key"))
                .expect("key")
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let calls = command_for_assertion.calls.lock().expect("calls");
        assert!(
            calls
                .iter()
                .any(|(program, args)| program == "wg" && args == &["genkey"])
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_fences_generation_changes_and_cleans_owned_key() {
        let root = std::env::temp_dir().join(format!("o3k-p11-linux-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand {
            calls: Mutex::new(Vec::new()),
            namespace_exists: false,
        });
        let mut provider =
            LinuxP11FabricBackend::with_command(LinuxP11Config::for_root(&root), command)
                .expect("provider");
        let current = plan();
        provider.apply(&current).expect("apply");
        let mut changed = current.clone();
        changed.local_fabric_generation += 1;
        assert!(matches!(
            provider.apply(&changed),
            Err(P11FabricError::Backend(message)) if message.contains("conflicts")
        ));
        provider.remove(&current).expect("remove");
        assert!(provider.observe_removed(&current).expect("removed"));
        assert!(!root.join("wireguard-private.key").exists());
        let _ = fs::remove_dir_all(root);
    }
}
