//! Fail-closed Linux realization for the accepted P11 v2 fabric contract.
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

const STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxP11Config {
    pub root: PathBuf,
    pub fabric_namespace: String,
    pub fabric_interface: String,
    pub wireguard_port: u16,
    pub geneve_port: u16,
}

impl LinuxP11Config {
    #[must_use]
    pub fn for_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fabric_namespace: "o3k-fabric".to_owned(),
            fabric_interface: "wg-o3k".to_owned(),
            wireguard_port: 51_820,
            geneve_port: 6_081,
        }
    }

    fn validate(&self) -> Result<(), LinuxP11Error> {
        if self.root == Path::new("/")
            || self.root.as_os_str().is_empty()
            || !valid_name(&self.fabric_namespace)
            || !valid_name(&self.fabric_interface)
            || self.wireguard_port == 0
            || self.geneve_port == 0
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
    fabric_transport_ip: std::net::Ipv4Addr,
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
    #[serde(default)]
    geneve: BTreeMap<String, GeneveOwnership>,
    /// One isolated L2 attachment exists for every remote target host.  The
    /// shared fabric namespace therefore never needs a tenant-IP route table;
    /// overlapping realms are selected by their attachment and Geneve VNI.
    #[serde(default)]
    attachments: BTreeMap<String, FabricAttachmentOwnership>,
    #[serde(default)]
    endpoint_taps: BTreeMap<Uuid, EndpointTapOwnership>,
    #[serde(default)]
    pending_endpoint_taps: BTreeMap<Uuid, EndpointTapOwnership>,
    directory_generation: u64,
    local_fabric_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EndpointTapOwnership {
    endpoint_id: Uuid,
    interface: String,
    mac: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GeneveOwnership {
    target_host: String,
    interface: String,
    remote_transport_ip: std::net::Ipv4Addr,
    vni: u32,
    binding_generation: u64,
    local_tunnel_mac: String,
    remote_tunnel_mac: String,
    bridge: String,
    realm_veth: String,
    fabric_veth: String,
    #[serde(default)]
    realized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FabricAttachmentOwnership {
    target_host: String,
    bridge: String,
    realm_veth: String,
    fabric_veth: String,
    local_tunnel_mac: String,
    remote_tunnel_mac: String,
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
                || fabric.fabric_transport_ip.is_unspecified()
                || fabric.fabric_transport_ip.is_loopback()
                || fabric.fabric_generation == 0
                || Path::new(&fabric.private_key_path).parent() != Some(self.config.root.as_path()))
        {
            return Err(LinuxP11Error::CorruptState);
        }
        if let Some(fabric) = &self.state.fabric {
            validate_private_key_file(Path::new(&fabric.private_key_path))?;
        }
        for (realm_id, ownership) in &self.state.realms {
            let Some(plan) = self.plans.get(realm_id) else {
                return Err(LinuxP11Error::CorruptState);
            };
            if realm_id != &ownership.realm_id
                || plan.realm_id != *realm_id
                || plan.directory_generation != ownership.directory_generation
                || plan.local_fabric_generation != ownership.local_fabric_generation
            {
                return Err(LinuxP11Error::CorruptState);
            }
            for (target_host, geneve) in &ownership.geneve {
                if target_host != &geneve.target_host
                    || !valid_name(&geneve.interface)
                    || geneve.remote_transport_ip.is_unspecified()
                    || geneve.remote_transport_ip.is_loopback()
                    || geneve.vni == 0
                    || geneve.vni > 0x000f_ffff
                    || geneve.binding_generation == 0
                    || geneve.vni != plan.encapsulation.provider_segment_id
                    || geneve.binding_generation != plan.encapsulation.binding_generation
                    || !valid_mac(&geneve.local_tunnel_mac)
                    || !valid_mac(&geneve.remote_tunnel_mac)
                    || !valid_name(&geneve.bridge)
                    || !valid_name(&geneve.realm_veth)
                    || !valid_name(&geneve.fabric_veth)
                    || !plan.peers.iter().any(|peer| {
                        peer.host_id == geneve.target_host
                            && peer.fabric_transport_ip == geneve.remote_transport_ip
                    })
                {
                    return Err(LinuxP11Error::CorruptState);
                }
            }
            for (target_host, attachment) in &ownership.attachments {
                if target_host != &attachment.target_host
                    || !valid_name(&attachment.bridge)
                    || !valid_name(&attachment.realm_veth)
                    || !valid_name(&attachment.fabric_veth)
                    || !valid_mac(&attachment.local_tunnel_mac)
                    || !valid_mac(&attachment.remote_tunnel_mac)
                    || !ownership.geneve.contains_key(target_host)
                {
                    return Err(LinuxP11Error::CorruptState);
                }
            }
            for (endpoint_id, tap) in &ownership.endpoint_taps {
                if endpoint_id != &tap.endpoint_id
                    || !valid_name(&tap.interface)
                    || !valid_mac(&tap.mac)
                    || !tap.interface.starts_with("o3k-t-")
                {
                    return Err(LinuxP11Error::CorruptState);
                }
            }
            for (endpoint_id, tap) in &ownership.pending_endpoint_taps {
                if endpoint_id != &tap.endpoint_id
                    || !valid_name(&tap.interface)
                    || !valid_mac(&tap.mac)
                    || !tap.interface.starts_with("o3k-t-")
                {
                    return Err(LinuxP11Error::CorruptState);
                }
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
            geneve: BTreeMap::new(),
            attachments: BTreeMap::new(),
            endpoint_taps: BTreeMap::new(),
            pending_endpoint_taps: BTreeMap::new(),
            directory_generation: plan.directory_generation,
            local_fabric_generation: plan.local_fabric_generation,
        }
    }

    fn ensure_fabric(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        if let Some(fabric) = &self.state.fabric {
            if plan.local_fabric_generation != fabric.fabric_generation {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            if plan.local_fabric_transport_ip != fabric.fabric_transport_ip {
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
        let (interface_exists, _) = self
            .command
            .output(
                "ip",
                &["link", "show", "dev", &self.config.fabric_interface],
            )
            .map_err(LinuxP11Error::Storage)?;
        if interface_exists {
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
            fabric_transport_ip: plan.local_fabric_transport_ip,
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
        let transport_ip = format!("{}/32", plan.local_fabric_transport_ip);
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    &self.config.fabric_namespace,
                    "ip",
                    "addr",
                    "replace",
                    &transport_ip,
                    "dev",
                    &self.config.fabric_interface,
                ],
            )
            .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        Ok(())
    }

    fn ensure_realm(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        let mut ownership = self.realm_ownership(plan);
        if let Some(existing) = self.state.realms.get(&plan.realm_id) {
            ownership.geneve = existing.geneve.clone();
            ownership.attachments = existing.attachments.clone();
            ownership.endpoint_taps = existing.endpoint_taps.clone();
            ownership.pending_endpoint_taps = existing.pending_endpoint_taps.clone();
        }
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
            for interface in [
                &ownership.bridge,
                &ownership.host_veth,
                &ownership.realm_veth,
                &ownership.fabric_veth,
                &ownership.fabric_realm_veth,
            ] {
                let (interface_exists, _) = self
                    .command
                    .output("ip", &["link", "show", "dev", interface])
                    .map_err(LinuxP11Error::Storage)?;
                if interface_exists {
                    return Err(LinuxP11Error::ForeignState);
                }
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
        let tenant_mtu = plan.tenant_mtu.to_string();
        let fabric_mtu = plan.local_fabric_mtu.to_string();
        for interface in [&ownership.bridge, &ownership.host_veth] {
            if !self
                .command
                .run("ip", &["link", "set", "dev", interface, "mtu", &tenant_mtu])
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for (namespace, interface, mtu) in [
            (
                ownership.namespace.as_str(),
                ownership.realm_veth.as_str(),
                tenant_mtu.as_str(),
            ),
            (
                ownership.namespace.as_str(),
                ownership.fabric_veth.as_str(),
                fabric_mtu.as_str(),
            ),
            (
                self.config.fabric_namespace.as_str(),
                ownership.fabric_realm_veth.as_str(),
                fabric_mtu.as_str(),
            ),
        ] {
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns", "exec", namespace, "ip", "link", "set", "dev", interface, "mtu",
                        mtu,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }

    /// Creates one provider-owned known-unicast Geneve device and one isolated
    /// L2 attachment per current remote host for this realm. The attachment
    /// is deliberately one-to-one: the shared fabric namespace forwards by a
    /// provider tunnel MAC, while all tenant IP routes remain in the realm
    /// namespace. This is what keeps overlapping realms out of one shared
    /// tenant-IP route table.
    fn ensure_geneve(&mut self, plan: &NamespacedRoutedFabricPlan) -> Result<(), LinuxP11Error> {
        if self.plans.values().any(|other| {
            other.realm_id != plan.realm_id
                && other.encapsulation.fabric_domain_id == plan.encapsulation.fabric_domain_id
                && other.encapsulation.provider_segment_id == plan.encapsulation.provider_segment_id
        }) {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        let Some(current) = self.state.realms.get(&plan.realm_id).cloned() else {
            return Err(LinuxP11Error::CorruptState);
        };
        let mut desired = BTreeMap::new();
        for peer in &plan.peers {
            if !plan
                .routes
                .iter()
                .any(|route| route.target_host == peer.host_id)
            {
                continue;
            }
            let interface = geneve_name(plan.realm_id, &peer.host_id);
            let bridge = geneve_bridge_name(plan.realm_id, &peer.host_id);
            let realm_veth = geneve_realm_veth_name(plan.realm_id, &peer.host_id);
            let fabric_veth = geneve_fabric_veth_name(plan.realm_id, &peer.host_id);
            desired.insert(
                peer.host_id.clone(),
                GeneveOwnership {
                    target_host: peer.host_id.clone(),
                    interface,
                    remote_transport_ip: peer.fabric_transport_ip,
                    vni: plan.encapsulation.provider_segment_id,
                    binding_generation: plan.encapsulation.binding_generation,
                    local_tunnel_mac: tunnel_mac(plan.realm_id, &plan.local_host),
                    remote_tunnel_mac: tunnel_mac(plan.realm_id, &peer.host_id),
                    bridge,
                    realm_veth,
                    fabric_veth,
                    realized: current
                        .geneve
                        .get(&peer.host_id)
                        .is_some_and(|existing| existing.realized),
                },
            );
        }
        for (target_host, old) in &current.geneve {
            if desired.contains_key(target_host) {
                continue;
            }
            self.remove_geneve_attachment(old, &current.namespace)?;
            let (exists, output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &old.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if exists {
                if !geneve_link_matches(&output, old, self.config.geneve_port) {
                    return Err(LinuxP11Error::ForeignState);
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
                            "del",
                            &old.interface,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
        }
        for (target_host, wanted) in &mut desired {
            if let Some(existing) = current.geneve.get(target_host)
                && (existing.target_host != wanted.target_host
                    || existing.interface != wanted.interface
                    || existing.remote_transport_ip != wanted.remote_transport_ip
                    || existing.vni != wanted.vni
                    || existing.binding_generation != wanted.binding_generation
                    || existing.local_tunnel_mac != wanted.local_tunnel_mac
                    || existing.remote_tunnel_mac != wanted.remote_tunnel_mac
                    || existing.bridge != wanted.bridge
                    || existing.realm_veth != wanted.realm_veth
                    || existing.fabric_veth != wanted.fabric_veth)
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            let (exists, output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &wanted.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if exists && !geneve_link_matches(&output, wanted, self.config.geneve_port) {
                return Err(LinuxP11Error::ForeignState);
            }
            if !exists {
                wanted.realized = false;
            }
        }
        let mut next = current.clone();
        next.geneve = desired.clone();
        next.attachments = desired
            .values()
            .map(|geneve| {
                (
                    geneve.target_host.clone(),
                    FabricAttachmentOwnership {
                        target_host: geneve.target_host.clone(),
                        bridge: geneve.bridge.clone(),
                        realm_veth: geneve.realm_veth.clone(),
                        fabric_veth: geneve.fabric_veth.clone(),
                        local_tunnel_mac: geneve.local_tunnel_mac.clone(),
                        remote_tunnel_mac: geneve.remote_tunnel_mac.clone(),
                    },
                )
            })
            .collect();
        if next != current {
            self.state.realms.insert(plan.realm_id, next);
            store_state(&self.state_path, &self.state)?;
        }
        for (target_host, wanted) in &desired {
            let vni = wanted.vni.to_string();
            let remote = wanted.remote_transport_ip.to_string();
            let port = self.config.geneve_port.to_string();
            let (already_exists, _) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &wanted.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if !already_exists
                && !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &self.config.fabric_namespace,
                            "ip",
                            "link",
                            "add",
                            &wanted.interface,
                            "type",
                            "geneve",
                            "id",
                            &vni,
                            "remote",
                            &remote,
                            "dstport",
                            &port,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            let tenant_mtu = plan.tenant_mtu.to_string();
            for args in [
                vec![
                    "netns",
                    "exec",
                    self.config.fabric_namespace.as_str(),
                    "ip",
                    "link",
                    "set",
                    "dev",
                    wanted.interface.as_str(),
                    "address",
                    wanted.local_tunnel_mac.as_str(),
                ],
                vec![
                    "netns",
                    "exec",
                    self.config.fabric_namespace.as_str(),
                    "ip",
                    "link",
                    "set",
                    "dev",
                    wanted.interface.as_str(),
                    "mtu",
                    tenant_mtu.as_str(),
                ],
            ] {
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
                        &self.config.fabric_namespace,
                        "ip",
                        "link",
                        "set",
                        &wanted.interface,
                        "up",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            self.ensure_geneve_attachment(plan, wanted)?;
            if let Some(realized) = self
                .state
                .realms
                .get_mut(&plan.realm_id)
                .and_then(|realm| realm.geneve.get_mut(target_host))
            {
                realized.realized = true;
            }
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }

    fn ensure_geneve_attachment(
        &self,
        plan: &NamespacedRoutedFabricPlan,
        geneve: &GeneveOwnership,
    ) -> Result<(), LinuxP11Error> {
        let fabric_ns = self.config.fabric_namespace.as_str();
        let realm_ns = self
            .state
            .realms
            .get(&plan.realm_id)
            .ok_or(LinuxP11Error::CorruptState)?
            .namespace
            .clone();
        let (bridge_exists, bridge_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.bridge,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if bridge_exists && !bridge_output.contains("bridge") {
            return Err(LinuxP11Error::ForeignState);
        }
        if !bridge_exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "link",
                        "add",
                        &geneve.bridge,
                        "type",
                        "bridge",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        for args in [
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                geneve.bridge.as_str(),
                "up",
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                geneve.interface.as_str(),
                "master",
                geneve.bridge.as_str(),
            ],
        ] {
            if !self
                .command
                .run("ip", &args)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        let (realm_exists, realm_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    &realm_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.realm_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if realm_exists && !realm_output.contains("veth") {
            return Err(LinuxP11Error::ForeignState);
        }
        let (fabric_exists, fabric_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "-d",
                    "link",
                    "show",
                    "dev",
                    &geneve.fabric_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if fabric_exists && !fabric_output.contains("veth") {
            return Err(LinuxP11Error::ForeignState);
        }
        if !realm_exists
            && (!self
                .command
                .run(
                    "ip",
                    &[
                        "link",
                        "add",
                        &geneve.realm_veth,
                        "type",
                        "veth",
                        "peer",
                        "name",
                        &geneve.fabric_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &["link", "set", &geneve.realm_veth, "netns", &realm_ns],
                    )
                    .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &["link", "set", &geneve.fabric_veth, "netns", fabric_ns],
                    )
                    .map_err(LinuxP11Error::Storage)?)
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        for args in [
            vec![
                "netns",
                "exec",
                &realm_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.realm_veth,
                "address",
                &geneve.local_tunnel_mac,
            ],
            vec![
                "netns",
                "exec",
                &realm_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.realm_veth,
                "up",
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "address",
                &geneve.local_tunnel_mac,
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "master",
                &geneve.bridge,
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "up",
            ],
        ] {
            if !self
                .command
                .run("ip", &args)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for mac in [
            geneve.remote_tunnel_mac.as_str(),
            geneve.local_tunnel_mac.as_str(),
        ] {
            let device = if mac == geneve.remote_tunnel_mac {
                geneve.interface.as_str()
            } else {
                geneve.fabric_veth.as_str()
            };
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "bridge",
                        "fdb",
                        "replace",
                        mac,
                        "dev",
                        device,
                        "master",
                        "permanent",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }

    fn remove_geneve_attachment(
        &self,
        geneve: &GeneveOwnership,
        realm_ns: &str,
    ) -> Result<(), LinuxP11Error> {
        let fabric_ns = self.config.fabric_namespace.as_str();
        let (exists, output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "-d",
                    "link",
                    "show",
                    "dev",
                    &geneve.bridge,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if exists && !output.contains("bridge") {
            return Err(LinuxP11Error::ForeignState);
        }
        if exists {
            let (ports_exist, ports) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "-o",
                        "link",
                        "show",
                        "master",
                        &geneve.bridge,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if ports_exist && !bridge_ports_are_owned(&ports, geneve) {
                return Err(LinuxP11Error::ForeignState);
            }
        }
        if exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "link",
                        "del",
                        &geneve.bridge,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        let (realm_veth_exists, _) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    realm_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.realm_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if realm_veth_exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        realm_ns,
                        "ip",
                        "link",
                        "del",
                        &geneve.realm_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        Ok(())
    }

    fn realize_routes(
        &self,
        plan: &NamespacedRoutedFabricPlan,
        ownership: &RealmOwnership,
    ) -> Result<(), LinuxP11Error> {
        for route in &plan.routes {
            let attachment = ownership
                .attachments
                .get(&route.target_host)
                .ok_or(LinuxP11Error::CorruptState)?;
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
                        &attachment.realm_veth,
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
                            &attachment.remote_tunnel_mac,
                            "nud",
                            "permanent",
                            "dev",
                            &attachment.realm_veth,
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
                            "proxy",
                            &route.destination.network.to_string(),
                            "dev",
                            &ownership.realm_veth,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }

    fn ensure_endpoint_taps(
        &mut self,
        plan: &NamespacedRoutedFabricPlan,
    ) -> Result<(), LinuxP11Error> {
        let Some(current) = self.state.realms.get(&plan.realm_id).cloned() else {
            return Err(LinuxP11Error::CorruptState);
        };
        let desired = plan
            .directory
            .entries
            .iter()
            .filter(|entry| entry.selected_host == plan.local_host)
            .map(|entry| {
                (
                    entry.endpoint_id,
                    EndpointTapOwnership {
                        endpoint_id: entry.endpoint_id,
                        interface: endpoint_tap_name(plan.realm_id, entry.endpoint_id),
                        mac: entry.mac.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if !current.pending_endpoint_taps.is_empty() && current.pending_endpoint_taps != desired {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        if current.endpoint_taps != desired && current.pending_endpoint_taps.is_empty() {
            let mut pending = current.clone();
            pending.pending_endpoint_taps = desired.clone();
            self.state.realms.insert(plan.realm_id, pending);
            store_state(&self.state_path, &self.state)?;
        }
        for (endpoint_id, old) in &current.endpoint_taps {
            if desired.contains_key(endpoint_id) {
                continue;
            }
            self.remove_endpoint_tap(old, &current.bridge)?;
        }
        for wanted in desired.values() {
            if let Some(existing) = current.endpoint_taps.get(&wanted.endpoint_id)
                && existing != wanted
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            let (exists, output) = self
                .command
                .output("ip", &["-d", "link", "show", "dev", &wanted.interface])
                .map_err(LinuxP11Error::Storage)?;
            if exists && !tap_link_matches(&output, wanted, &current.bridge) {
                return Err(LinuxP11Error::ForeignState);
            }
            if exists
                && !current.endpoint_taps.contains_key(&wanted.endpoint_id)
                && !current
                    .pending_endpoint_taps
                    .contains_key(&wanted.endpoint_id)
            {
                return Err(LinuxP11Error::ForeignState);
            }
            if !exists {
                for args in [
                    vec![
                        "tuntap",
                        "add",
                        "dev",
                        wanted.interface.as_str(),
                        "mode",
                        "tap",
                    ],
                    vec![
                        "link",
                        "set",
                        "dev",
                        wanted.interface.as_str(),
                        "address",
                        wanted.mac.as_str(),
                    ],
                    vec![
                        "link",
                        "set",
                        "dev",
                        wanted.interface.as_str(),
                        "master",
                        current.bridge.as_str(),
                    ],
                    vec!["link", "set", "dev", wanted.interface.as_str(), "up"],
                ] {
                    if !self
                        .command
                        .run("ip", &args)
                        .map_err(LinuxP11Error::Storage)?
                    {
                        return Err(LinuxP11Error::CommandFailed);
                    }
                }
            }
        }
        if current.endpoint_taps != desired || !current.pending_endpoint_taps.is_empty() {
            let mut next = self
                .state
                .realms
                .get(&plan.realm_id)
                .cloned()
                .ok_or(LinuxP11Error::CorruptState)?;
            next.endpoint_taps = desired;
            next.pending_endpoint_taps.clear();
            self.state.realms.insert(plan.realm_id, next);
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }

    fn remove_endpoint_tap(
        &self,
        tap: &EndpointTapOwnership,
        bridge: &str,
    ) -> Result<(), LinuxP11Error> {
        let (exists, output) = self
            .command
            .output("ip", &["-d", "link", "show", "dev", &tap.interface])
            .map_err(LinuxP11Error::Storage)?;
        if exists && !tap_link_matches(&output, tap, bridge) {
            return Err(LinuxP11Error::ForeignState);
        }
        if exists
            && !self
                .command
                .run("ip", &["link", "del", "dev", &tap.interface])
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
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
                        || existing.fabric_transport_ip != peer.fabric_transport_ip
                        || existing.fabric_generation != peer.fabric_generation
                    {
                        return Err(LinuxP11Error::OwnershipConflict);
                    }
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
                || peer.fabric_transport_ip.is_unspecified()
                || peer.fabric_transport_ip.is_loopback()
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
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
            args.extend([
                "allowed-ips".to_owned(),
                format!("{}/32", peer.fabric_transport_ip),
            ]);
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            if !self
                .command
                .run("ip", &refs)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            let route = format!("{}/32", peer.fabric_transport_ip);
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
        self.ensure_geneve(plan)?;
        self.ensure_endpoint_taps(plan)?;
        let ownership = self
            .state
            .realms
            .get(&plan.realm_id)
            .cloned()
            .ok_or(LinuxP11Error::CorruptState)?;
        self.realize_routes(plan, &ownership)?;
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
        for tap in ownership.endpoint_taps.values() {
            self.remove_endpoint_tap(tap, &ownership.bridge)?;
        }
        for tap in ownership.pending_endpoint_taps.values() {
            if !ownership.endpoint_taps.contains_key(&tap.endpoint_id) {
                self.remove_endpoint_tap(tap, &ownership.bridge)?;
            }
        }
        for geneve in ownership.geneve.values() {
            self.remove_geneve_attachment(geneve, &ownership.namespace)?;
            let (exists, output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &geneve.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if exists {
                if !geneve_link_matches(&output, geneve, self.config.geneve_port) {
                    return Err(P11FabricError::Backend(
                        LinuxP11Error::ForeignState.to_string(),
                    ));
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
                            "del",
                            &geneve.interface,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(P11FabricError::Backend(
                        LinuxP11Error::CommandFailed.to_string(),
                    ));
                }
            }
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

fn valid_mac(value: &str) -> bool {
    let octets = value.split(':').collect::<Vec<_>>();
    octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn geneve_name(realm_id: Uuid, target_host: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in realm_id
        .as_bytes()
        .iter()
        .copied()
        .chain(target_host.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("o3k-g-{:08x}", hash as u32)
}

fn provider_name(prefix: &str, realm_id: Uuid, target_host: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in prefix
        .as_bytes()
        .iter()
        .copied()
        .chain(realm_id.as_bytes().iter().copied())
        .chain(target_host.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("o3k-{}-{:08x}", prefix, hash as u32)
}

fn geneve_bridge_name(realm_id: Uuid, target_host: &str) -> String {
    provider_name("c", realm_id, target_host)
}

fn geneve_realm_veth_name(realm_id: Uuid, target_host: &str) -> String {
    provider_name("e", realm_id, target_host)
}

fn geneve_fabric_veth_name(realm_id: Uuid, target_host: &str) -> String {
    provider_name("i", realm_id, target_host)
}

fn endpoint_tap_name(realm_id: Uuid, endpoint_id: Uuid) -> String {
    let bytes = realm_id
        .as_bytes()
        .iter()
        .copied()
        .chain(endpoint_id.as_bytes().iter().copied())
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
        .to_be_bytes();
    format!(
        "o3k-t-{:02x}{:02x}{:02x}{:02x}",
        bytes[4], bytes[5], bytes[6], bytes[7]
    )
}

fn tunnel_mac(realm_id: Uuid, host_id: &str) -> String {
    let bytes = realm_id
        .as_bytes()
        .iter()
        .copied()
        .chain(host_id.as_bytes().iter().copied())
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
        .to_be_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[2], bytes[3], bytes[4], bytes[5], bytes[6]
    )
}

fn bridge_ports_are_owned(output: &str, geneve: &GeneveOwnership) -> bool {
    let names = output
        .lines()
        .filter_map(|line| line.split_once(": ").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|name| name.trim_end_matches(':').split('@').next().unwrap_or(name))
        .collect::<BTreeSet<_>>();
    names == BTreeSet::from([geneve.interface.as_str(), geneve.fabric_veth.as_str()])
}

fn geneve_link_matches(output: &str, ownership: &GeneveOwnership, port: u16) -> bool {
    output.contains("geneve")
        && output.contains(&format!("id {}", ownership.vni))
        && output.contains(&format!("remote {}", ownership.remote_transport_ip))
        && output.contains(&format!("dstport {}", port))
}

fn tap_link_matches(output: &str, ownership: &EndpointTapOwnership, bridge: &str) -> bool {
    output.contains("tun")
        && output.contains(&format!("link/ether {}", ownership.mac))
        && output.contains(&format!("master {}", bridge))
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
        AddressRealm, EndpointLocation, FabricHostIdentity, FabricProviderKind, Ipv4Prefix,
        RealmEncapsulationBinding, RealmEndpointDirectory,
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
            fabric_transport_ip: "198.18.0.1".parse().expect("transport ip"),
            provider_version: "wireguard-v1".to_owned(),
            fabric_generation: 3,
            underlay_mtu: 1500,
            fabric_mtu: 1420,
        };
        let remote = FabricHostIdentity {
            host_id: "host-b".to_owned(),
            public_key: "B".repeat(43) + "=",
            underlay_endpoint: "192.0.2.2:51820".to_owned(),
            fabric_transport_ip: "198.18.0.2".parse().expect("transport ip"),
            provider_version: "wireguard-v1".to_owned(),
            fabric_generation: 3,
            underlay_mtu: 1500,
            fabric_mtu: 1420,
        };
        let binding = RealmEncapsulationBinding {
            fabric_domain_id: Uuid::from_u128(100),
            realm_id: realm.id,
            provider_kind: FabricProviderKind::Geneve,
            provider_segment_id: 101,
            binding_generation: 3,
        };
        directory
            .compile_fabric_plan(&local, &[local.clone(), remote], 1400, &binding)
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
        let interface = geneve_name(plan().realm_id, "host-b");
        assert!(calls.iter().any(|(program, args)| {
            program == "ip"
                && args.windows(8).any(|window| {
                    window
                        == [
                            "type",
                            "geneve",
                            "id",
                            "101",
                            "remote",
                            "198.18.0.2",
                            "dstport",
                            "6081",
                        ]
                })
                && args.iter().any(|arg| arg == &interface)
        }));
        assert!(calls.iter().any(|(program, args)| {
            program == "ip"
                && args
                    .windows(2)
                    .any(|window| window == ["allowed-ips", "198.18.0.2/32"])
        }));
        assert!(!calls.iter().any(|(_, args)| {
            args.windows(2)
                .any(|window| window == ["allowed-ips", "10.40.1.12/32"])
        }));
        let remote_tunnel_mac = tunnel_mac(plan().realm_id, "host-b");
        let attachment = provider
            .state
            .realms
            .get(&plan().realm_id)
            .and_then(|realm| realm.attachments.get("host-b"))
            .expect("remote attachment");
        assert!(calls.iter().any(|(program, args)| {
            program == "ip"
                && args.windows(4).any(|window| {
                    window == ["lladdr", remote_tunnel_mac.as_str(), "nud", "permanent"]
                })
                && args.last() == Some(&attachment.realm_veth)
        }));
        assert!(calls.iter().any(|(program, args)| {
            program == "ip"
                && args.contains(&"bridge".to_owned())
                && args
                    .windows(2)
                    .any(|window| window == ["replace", remote_tunnel_mac.as_str()])
        }));
        assert!(calls.iter().any(|(program, args)| {
            program == "ip" && args.windows(2).any(|window| window == ["mtu", "1400"])
        }));
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
