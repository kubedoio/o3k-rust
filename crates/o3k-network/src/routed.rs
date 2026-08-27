//! Linux-native routed egress/SNAT realization.
//!
//! The provider owns one explicitly named nftables table and one marked route
//! per configured external realm. It never flushes global routes or firewall
//! state, and it refuses to adopt an unmarked pre-existing table.

use o3k_domain::{EgressIntent, Ipv4Prefix, NetworkPlanIntent};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

const TABLE: &str = "o3k_p9";
const CHAIN: &str = "postrouting";
const MARKER: &str = "o3k-p9-managed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedExternalConfig {
    pub external_realm_id: Uuid,
    pub uplink: String,
    pub bridge: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Ownership {
    realm_id: Uuid,
    prefix: Ipv4Prefix,
    uplink: String,
    bridge: String,
    #[serde(default)]
    forwarding_enabled_by_o3k: bool,
}

trait RoutedCommand: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool>;
}

struct SystemRoutedCommand;

impl RoutedCommand for SystemRoutedCommand {
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

#[derive(Debug, Error)]
pub enum RoutedNetworkError {
    #[error("routed external configuration is invalid")]
    InvalidConfiguration,
    #[error("routed plan does not authorize the configured external realm")]
    UnauthorizedExternalRealm,
    #[error("routed plan has no enabled egress intent")]
    MissingEgress,
    #[error("internal Realm gateway routes require the namespaced fabric provider")]
    InternalRealmRoutingUnsupported,
    #[error("routed host command failed")]
    CommandFailed,
    #[error("routed provider state storage failed: {0}")]
    Storage(#[from] io::Error),
    #[error("routed provider state is corrupt")]
    CorruptState,
    #[error("pre-existing nftables state is not O3K-owned")]
    ForeignFirewallState,
    #[error("owned routed state does not match the requested plan")]
    OwnershipConflict,
}

pub struct LinuxRoutedProvider {
    config: RoutedExternalConfig,
    root: PathBuf,
    command: Arc<dyn RoutedCommand>,
    ownership: Option<Ownership>,
}

impl LinuxRoutedProvider {
    pub fn open(
        config: RoutedExternalConfig,
        root: impl Into<PathBuf>,
    ) -> Result<Self, RoutedNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ownership = load_ownership(&root.join("routed.json"))?;
        Ok(Self {
            config,
            root,
            command: Arc::new(SystemRoutedCommand),
            ownership,
        })
    }

    #[cfg(test)]
    fn with_command(
        config: RoutedExternalConfig,
        root: impl Into<PathBuf>,
        command: Arc<dyn RoutedCommand>,
    ) -> Result<Self, RoutedNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root)?;
        let ownership = load_ownership(&root.join("routed.json"))?;
        Ok(Self {
            config,
            root,
            command,
            ownership,
        })
    }

    pub fn apply(&mut self, intents: &[NetworkPlanIntent]) -> Result<(), RoutedNetworkError> {
        if intents.iter().any(
            |intent| matches!(intent, NetworkPlanIntent::Gateway(gateway) if !gateway.external),
        ) {
            return Err(RoutedNetworkError::InternalRealmRoutingUnsupported);
        }
        let prefix = realm_prefix(intents).ok_or(RoutedNetworkError::MissingEgress)?;
        let egress = intents.iter().find_map(|intent| match intent {
            NetworkPlanIntent::Egress(egress) if egress.enabled => Some(egress),
            _ => None,
        });
        let Some(EgressIntent {
            external_realm_id,
            nat,
            ..
        }) = egress
        else {
            return Err(RoutedNetworkError::MissingEgress);
        };
        if *external_realm_id != self.config.external_realm_id || !nat {
            return Err(RoutedNetworkError::UnauthorizedExternalRealm);
        }
        let (firewall_present, firewall_output) = self
            .command
            .output("nft", &["list", "table", "ip", TABLE])
            .map_err(RoutedNetworkError::Storage)?;
        if firewall_present && !firewall_output.contains(MARKER) {
            return Err(RoutedNetworkError::ForeignFirewallState);
        }
        let forwarding_enabled_by_o3k = if let Some(existing) = &self.ownership {
            existing.forwarding_enabled_by_o3k
        } else {
            let (success, output) = self
                .command
                .output("sysctl", &["-n", "net.ipv4.ip_forward"])
                .map_err(RoutedNetworkError::Storage)?;
            if !success {
                return Err(RoutedNetworkError::CommandFailed);
            }
            output.trim() != "1"
        };
        let ownership = Ownership {
            realm_id: self.config.external_realm_id,
            prefix,
            uplink: self.config.uplink.clone(),
            bridge: self.config.bridge.clone(),
            forwarding_enabled_by_o3k,
        };
        if let Some(existing) = &self.ownership
            && existing != &ownership
        {
            return Err(RoutedNetworkError::OwnershipConflict);
        }
        // Record the exact owned target before host mutation. If the process
        // dies after one command, restart reconciliation can observe or remove
        // this bounded state instead of treating the mutation as nonexistent.
        store_ownership(&self.root.join("routed.json"), &ownership)?;
        self.ownership = Some(ownership.clone());
        if ownership.forwarding_enabled_by_o3k
            && !self
                .command
                .run("sysctl", &["-w", "net.ipv4.ip_forward=1"])
                .map_err(RoutedNetworkError::Storage)?
        {
            return Err(RoutedNetworkError::CommandFailed);
        }
        self.ensure_firewall(&ownership)?;
        if !self
            .command
            .run(
                "ip",
                &[
                    "route",
                    "replace",
                    &prefix_string(prefix),
                    "dev",
                    &self.config.bridge,
                ],
            )
            .map_err(RoutedNetworkError::Storage)?
        {
            return Err(RoutedNetworkError::CommandFailed);
        }
        Ok(())
    }

    pub fn observe(&self) -> Result<bool, RoutedNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(true);
        };
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", TABLE])
            .map_err(RoutedNetworkError::Storage)?;
        if !success {
            return Ok(false);
        }
        if !output.contains(MARKER) {
            return Err(RoutedNetworkError::ForeignFirewallState);
        }
        let (success, _) = self
            .command
            .output(
                "ip",
                &[
                    "route",
                    "show",
                    &prefix_string(ownership.prefix),
                    "dev",
                    &ownership.bridge,
                ],
            )
            .map_err(RoutedNetworkError::Storage)?;
        if !success {
            return Ok(false);
        }
        let (success, output) = self
            .command
            .output("sysctl", &["-n", "net.ipv4.ip_forward"])
            .map_err(RoutedNetworkError::Storage)?;
        Ok(success && output.trim() == "1")
    }

    pub fn remove(&mut self) -> Result<(), RoutedNetworkError> {
        let Some(ownership) = self.ownership.take() else {
            return Ok(());
        };
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", TABLE])
            .map_err(RoutedNetworkError::Storage)?;
        if success && !output.contains(MARKER) {
            self.ownership = Some(ownership);
            return Err(RoutedNetworkError::ForeignFirewallState);
        }
        if success
            && !self
                .command
                .run("nft", &["delete", "table", "ip", TABLE])
                .map_err(RoutedNetworkError::Storage)?
        {
            self.ownership = Some(ownership);
            return Err(RoutedNetworkError::CommandFailed);
        }
        let route = prefix_string(ownership.prefix);
        if !self
            .command
            .run("ip", &["route", "del", &route, "dev", &ownership.bridge])
            .map_err(RoutedNetworkError::Storage)?
        {
            self.ownership = Some(ownership);
            return Err(RoutedNetworkError::CommandFailed);
        }
        if ownership.forwarding_enabled_by_o3k
            && !self
                .command
                .run("sysctl", &["-w", "net.ipv4.ip_forward=0"])
                .map_err(RoutedNetworkError::Storage)?
        {
            self.ownership = Some(ownership);
            return Err(RoutedNetworkError::CommandFailed);
        }
        let _ = fs::remove_file(self.root.join("routed.json"));
        Ok(())
    }

    fn ensure_firewall(&self, ownership: &Ownership) -> Result<(), RoutedNetworkError> {
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", TABLE])
            .map_err(RoutedNetworkError::Storage)?;
        if success && !output.contains(MARKER) {
            return Err(RoutedNetworkError::ForeignFirewallState);
        }
        if !success
            && !self
                .command
                .run(
                    "nft",
                    &[
                        "add",
                        "table",
                        "ip",
                        TABLE,
                        "{",
                        "comment",
                        &format!("\"{}\"", MARKER),
                        ";",
                        "}",
                    ],
                )
                .map_err(RoutedNetworkError::Storage)?
        {
            return Err(RoutedNetworkError::CommandFailed);
        }
        if !self
            .command
            .run(
                "nft",
                &[
                    "add",
                    "chain",
                    "ip",
                    TABLE,
                    CHAIN,
                    "{",
                    "type",
                    "nat",
                    "hook",
                    "postrouting",
                    "priority",
                    "100",
                    ";",
                    "policy",
                    "accept",
                    ";",
                    "}",
                ],
            )
            .map_err(RoutedNetworkError::Storage)?
        {
            // A chain that already exists is acceptable only in the marked
            // O3K table; the probe above established that ownership.
            let (_, chain) = self
                .command
                .output("nft", &["list", "chain", "ip", TABLE, CHAIN])
                .map_err(RoutedNetworkError::Storage)?;
            if !chain.contains(MARKER) {
                return Err(RoutedNetworkError::ForeignFirewallState);
            }
        }
        let source = prefix_string(ownership.prefix);
        let (_, chain) = self
            .command
            .output("nft", &["list", "chain", "ip", TABLE, CHAIN])
            .map_err(RoutedNetworkError::Storage)?;
        if chain.contains(MARKER) && chain.contains(&source) && chain.contains(&ownership.uplink) {
            return Ok(());
        }
        if !self
            .command
            .run(
                "nft",
                &[
                    "add",
                    "rule",
                    "ip",
                    TABLE,
                    CHAIN,
                    "ip",
                    "saddr",
                    &source,
                    "oifname",
                    &format!("\"{}\"", ownership.uplink),
                    "masquerade",
                    "comment",
                    MARKER,
                ],
            )
            .map_err(RoutedNetworkError::Storage)?
        {
            return Err(RoutedNetworkError::CommandFailed);
        }
        Ok(())
    }
}

impl RoutedExternalConfig {
    fn validate(&self) -> Result<(), RoutedNetworkError> {
        if self.external_realm_id == Uuid::nil()
            || !valid_ifname(&self.uplink)
            || !valid_ifname(&self.bridge)
            || self.uplink == self.bridge
        {
            return Err(RoutedNetworkError::InvalidConfiguration);
        }
        Ok(())
    }
}

fn realm_prefix(intents: &[NetworkPlanIntent]) -> Option<Ipv4Prefix> {
    intents.iter().find_map(|intent| match intent {
        NetworkPlanIntent::AddressRealm { prefix, .. } => Some(*prefix),
        _ => None,
    })
}

fn prefix_string(prefix: Ipv4Prefix) -> String {
    format!("{}/{}", prefix.network, prefix.prefix_len)
}

fn valid_ifname(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn load_ownership(path: &Path) -> Result<Option<Ownership>, RoutedNetworkError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| RoutedNetworkError::CorruptState),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RoutedNetworkError::Storage(error)),
    }
}

fn store_ownership(path: &Path, ownership: &Ownership) -> Result<(), RoutedNetworkError> {
    let bytes =
        serde_json::to_vec_pretty(ownership).map_err(|_| RoutedNetworkError::CorruptState)?;
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
    use std::{net::Ipv4Addr, sync::Mutex};

    struct FakeCommand {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        table_output: Mutex<(bool, String)>,
        fail_route_delete: Mutex<bool>,
    }

    impl FakeCommand {
        fn new(table_output: (bool, &str)) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                table_output: Mutex::new((table_output.0, table_output.1.to_owned())),
                fail_route_delete: Mutex::new(false),
            }
        }
    }

    impl RoutedCommand for FakeCommand {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if args.starts_with(&["list", "table"]) {
                return Ok(self.table_output.lock().expect("table").clone());
            }
            if program == "sysctl" {
                return Ok((true, "0\n".to_owned()));
            }
            Ok((true, String::new()))
        }

        fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if program == "ip"
                && args.starts_with(&["route", "del"])
                && *self.fail_route_delete.lock().expect("route failure")
            {
                return Ok(false);
            }
            Ok(true)
        }
    }

    fn config() -> RoutedExternalConfig {
        RoutedExternalConfig {
            external_realm_id: Uuid::from_u128(9),
            uplink: "eth0".to_owned(),
            bridge: "o3k-br0".to_owned(),
        }
    }

    fn intents(enabled: bool, external_realm_id: Uuid) -> Vec<NetworkPlanIntent> {
        vec![
            NetworkPlanIntent::AddressRealm {
                realm_id: Uuid::from_u128(1),
                prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("prefix"),
                gateway: Ipv4Addr::new(10, 0, 0, 1),
            },
            NetworkPlanIntent::Egress(EgressIntent {
                external_realm_id,
                enabled,
                nat: true,
            }),
        ]
    }

    #[test]
    fn tenant_cannot_select_an_unconfigured_external_realm() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider =
            LinuxRoutedProvider::with_command(config(), &root, command).expect("provider");
        assert!(matches!(
            provider.apply(&intents(true, Uuid::from_u128(10))),
            Err(RoutedNetworkError::UnauthorizedExternalRealm)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn disabled_egress_fails_before_any_host_command() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        assert!(matches!(
            provider.apply(&intents(false, Uuid::from_u128(9))),
            Err(RoutedNetworkError::MissingEgress)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn internal_gateway_routes_fail_closed_before_external_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        let mut plan = intents(true, Uuid::from_u128(9));
        plan.push(NetworkPlanIntent::Gateway(o3k_domain::GatewayIntent {
            destination: Ipv4Prefix::new(Ipv4Addr::new(10, 1, 0, 0), 24).expect("prefix"),
            gateway: Ipv4Addr::new(10, 0, 0, 1),
            external: false,
        }));
        assert!(matches!(
            provider.apply(&plan),
            Err(RoutedNetworkError::InternalRealmRoutingUnsupported)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn foreign_existing_table_is_never_adopted_or_mutated() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((
            true,
            "table ip o3k_p9 { comment foreign; }",
        )));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        assert!(matches!(
            provider.apply(&intents(true, Uuid::from_u128(9))),
            Err(RoutedNetworkError::ForeignFirewallState)
        ));
        assert_eq!(command.calls.lock().expect("calls").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snat_chain_is_a_postrouting_nat_hook() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        provider
            .apply(&intents(true, Uuid::from_u128(9)))
            .expect("routed apply");
        let calls = command.calls.lock().expect("calls");
        assert!(calls.iter().any(|call| {
            call.1
                .windows(3)
                .any(|window| window == ["hook", "postrouting", "priority"])
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn forwarding_is_enabled_and_restored_as_owned_state() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        provider
            .apply(&intents(true, Uuid::from_u128(9)))
            .expect("routed apply");
        assert!(
            command
                .calls
                .lock()
                .expect("calls")
                .iter()
                .any(|call| { call.0 == "sysctl" && call.1 == ["-w", "net.ipv4.ip_forward=1"] })
        );
        provider.remove().expect("routed remove");
        assert!(
            command
                .calls
                .lock()
                .expect("calls")
                .iter()
                .any(|call| { call.0 == "sysctl" && call.1 == ["-w", "net.ipv4.ip_forward=0"] })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn route_delete_failure_keeps_owned_state_retryable() {
        let root = std::env::temp_dir().join(format!("o3k-routed-{}", Uuid::now_v7()));
        let command = Arc::new(FakeCommand::new((false, "")));
        let mut provider = LinuxRoutedProvider::with_command(
            config(),
            &root,
            Arc::clone(&command) as Arc<dyn RoutedCommand>,
        )
        .expect("provider");
        provider
            .apply(&intents(true, Uuid::from_u128(9)))
            .expect("routed apply");
        *command.fail_route_delete.lock().expect("route failure") = true;
        assert!(matches!(
            provider.remove(),
            Err(RoutedNetworkError::CommandFailed)
        ));
        *command.fail_route_delete.lock().expect("route failure") = false;
        provider.remove().expect("retry cleanup");
        let _ = fs::remove_dir_all(root);
    }
}
