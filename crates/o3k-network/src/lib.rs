use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Poll interval while waiting for a freshly created TAP address to settle.
#[cfg(not(test))]
const TAP_ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(test)]
const TAP_ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// How long the kernel TAP address must continuously match the requested
/// address before it is considered stable. An asynchronously applied udev
/// MAC policy lands within tens of milliseconds of the device add event, so
/// a 200 ms observation window covers it with an order of magnitude margin.
#[cfg(not(test))]
const TAP_ADDRESS_SETTLE_WINDOW: Duration = Duration::from_millis(200);
#[cfg(test)]
const TAP_ADDRESS_SETTLE_WINDOW: Duration = Duration::ZERO;

/// Upper bound for address stabilization before the TAP is rolled back.
const TAP_ADDRESS_STABILIZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetworkConfig {
    pub bridge_name: String,
    pub uplink: Option<String>,
}

/// The address that O3K is allowed to add to its managed bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewaySpec {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

/// Durable ownership metadata for host-local network resources.
///
/// This is deliberately separate from Neutron metadata. It records only
/// resources that this host-network manager may mutate or remove.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NetworkOwnershipManifest {
    #[serde(default)]
    pub bridge: Option<BridgeOwnership>,
    #[serde(default)]
    pub taps: BTreeMap<String, TapOwnership>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeOwnership {
    pub name: String,
    pub uplink: Option<String>,
    pub created_by_o3k: bool,
    #[serde(default)]
    pub gateway: Option<GatewayOwnership>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayOwnership {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapOwnership {
    pub interface: String,
    pub instance_id: String,
    pub port_id: String,
    pub mac: String,
    pub bridge: String,
    pub created_by_o3k: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkCommandOutput {
    success: bool,
    stdout: String,
}

trait NetworkCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput>;
    fn status(&self, args: &[&str]) -> io::Result<bool>;
}

struct SystemNetworkCommand;

impl NetworkCommand for SystemNetworkCommand {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput> {
        let output = Command::new("ip").args(args).output()?;
        Ok(NetworkCommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }

    fn status(&self, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new("ip").args(args).status()?.success())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod host_network_tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn validates_names_and_generates_stable_interface_identity() -> Result<(), HostNetworkError> {
        let manager = HostNetworkManager::new(HostNetworkConfig {
            bridge_name: "o3k-br0".to_owned(),
            uplink: None,
        })?;
        assert_eq!(
            HostNetworkManager::tap_name("port-1")?,
            HostNetworkManager::tap_name("port-1")?
        );
        assert_eq!(
            HostNetworkManager::deterministic_mac("port-1")?,
            HostNetworkManager::deterministic_mac("port-1")?
        );
        assert!(matches!(
            HostNetworkManager::new(HostNetworkConfig {
                bridge_name: "../../escape".to_owned(),
                uplink: None
            }),
            Err(HostNetworkError::InvalidName)
        ));
        assert!(matches!(
            manager.create_tap(&TapSpec {
                instance_id: "instance-1".to_owned(),
                port_id: "port-1".to_owned(),
                mac: "bad".to_owned()
            }),
            Err(HostNetworkError::InvalidMac)
        ));
        assert!(matches!(
            manager.delete_tap(&TapSpec {
                instance_id: "instance-1".to_owned(),
                port_id: "port-1".to_owned(),
                mac: "bad".to_owned(),
            }),
            Err(HostNetworkError::InvalidMac)
        ));
        assert!(interface_output_is_owned(
            "2: o3ktap-abcd: <BROADCAST> mtu 1500 master o3k-br0 state UP\\n\\\ttun type tap\\n\\\tlink/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-abcd: <BROADCAST> mtu 1500 master o3k-br0 state UP\\n\\\tlink/ether 02:00:00:00:00:02 brd ff:ff:ff:ff:ff:ff",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        Ok(())
    }

    #[test]
    fn existing_uplink_must_be_up_and_attached_to_the_managed_bridge() {
        let output = "3: eth0: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP";
        assert!(interface_is_attached_to(output, "o3k-br0"));
        assert!(!interface_is_attached_to(
            "3: eth0: <BROADCAST,UP> mtu 1500 state UP",
            "o3k-br0"
        ));
        assert!(!interface_is_attached_to(
            "3: eth0: <BROADCAST> mtu 1500 master o3k-br0 state DOWN",
            "o3k-br0"
        ));
    }

    #[test]
    fn existing_link_must_be_a_bridge_before_it_is_reused() {
        assert!(interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500 hello_time 200 max_age 2000"
        ));
        assert!(!interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tlink/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff"
        ));
        assert!(!interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tbridge-helper foreign-name"
        ));
    }

    #[test]
    fn bridge_creation_failure_removes_only_the_new_bridge() {
        let command = FakeNetworkCommand::new([
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::status(false),
            Response::status(true),
        ]);
        let manager = test_manager(command.clone(), Some("eth0"));

        assert!(matches!(
            manager.ensure_bridge(),
            Err(HostNetworkError::CommandFailed)
        ));
        assert_eq!(
            command.calls(),
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", "o3k-br0", "type", "bridge"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "set", "dev", "eth0", "master", "o3k-br0"],
                vec!["link", "del", "dev", "o3k-br0"],
            ]
        );
    }

    #[test]
    fn tap_setup_failure_removes_new_tap_and_bridge() {
        let command = FakeNetworkCommand::new([
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::output(false, ""),
            Response::status(true),
            Response::status(false),
            Response::status(true),
            Response::status(true),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::CommandFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        assert_eq!(
            command.calls(),
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", "o3k-br0", "type", "bridge"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "show", "dev", &tap],
                vec!["tuntap", "add", "dev", &tap, "mode", "tap"],
                vec!["link", "set", "dev", &tap, "address", "02:00:00:00:00:01"],
                vec!["link", "del", "dev", &tap],
                vec!["link", "del", "dev", "o3k-br0"],
            ]
        );
    }

    #[test]
    fn foreign_existing_tap_is_never_deleted() {
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-abcd: <BROADCAST>"),
            Response::output(
                true,
                "2: o3ktap-abcd: <BROADCAST> master o3k-br0\\n\\ttun type tap\\n\\tlink/ether 02:00:00:00:00:02",
            ),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args == &["link", "del", "dev", "o3ktap-abcd"])
        );
    }

    #[test]
    fn discovery_only_returns_taps_attached_to_the_configured_bridge() {
        let command = FakeNetworkCommand::new([Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP\n\
             tun type tap\n\
             3: o3ktap-detached: <BROADCAST,UP> mtu 1500 state UP\n\
             4: o3ktap-foreign: <BROADCAST,UP> mtu 1500 master other-br0 state UP",
        )]);
        let manager = test_manager(command, None);

        assert_eq!(
            manager.discover_managed().expect("discovery succeeds"),
            vec!["o3ktap-owned"]
        );
    }

    #[test]
    fn ownership_tokens_are_matched_without_prefix_collisions() {
        assert!(interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP tun type tap link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br01 state UP tun type tap link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP tun type tap link/ether 02:00:00:00:00:010",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
    }

    #[test]
    fn tap_ownership_binds_instance_across_manager_restart() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-ownership-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "instance-a".to_owned(),
            port_id: "port-a".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        let name = manager.create_tap(&spec)?;
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert_eq!(manifest.taps[&name].instance_id, "instance-a");

        let reopened_command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
        ]);
        let reopened = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(reopened_command),
            &root,
        )?;
        assert_eq!(reopened.create_tap(&spec)?, name);
        assert!(matches!(
            reopened.create_tap(&TapSpec {
                instance_id: "instance-b".to_owned(),
                ..spec
            }),
            Err(HostNetworkError::ForeignInterface)
        ));
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn tap_address_is_reapplied_after_external_replacement() -> Result<(), HostNetworkError> {
        // A udev MAC policy write can land after the address was set during
        // TAP creation. The owner must observe the replacement, re-apply the
        // requested address, and only then record ownership.
        let command = FakeNetworkCommand::new([
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
            ),
            Response::status(true),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        let name = manager.create_tap(&spec)?;
        let calls = command.calls();
        let set_calls = calls
            .iter()
            .filter(|args| {
                args.as_slice() == ["link", "set", "dev", &name, "address", "02:00:00:00:00:01"]
            })
            .count();
        assert_eq!(set_calls, 2, "address must be re-applied after replacement");
        Ok(())
    }

    #[test]
    fn tap_address_reapply_failure_rolls_back_owned_resources() {
        let mut responses = vec![
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::status(true),
        ];
        // The kernel view never matches the requested address; the second
        // re-apply fails and the owned TAP and bridge are rolled back.
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
        ));
        responses.push(Response::status(true));
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
        ));
        responses.push(Response::status(false));
        // Rollback deletes the owned TAP and the owned bridge.
        responses.push(Response::status(true));
        responses.push(Response::status(true));
        let command = FakeNetworkCommand::new(responses);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::CommandFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let calls = command.calls();
        let reapplies = calls
            .iter()
            .filter(|args| {
                args.as_slice() == ["link", "set", "dev", &tap, "address", "02:00:00:00:00:01"]
            })
            .count();
        assert!(reapplies >= 2, "address must be re-applied while unstable");
        assert_eq!(calls[calls.len() - 2], vec!["link", "del", "dev", &tap]);
        assert_eq!(
            calls[calls.len() - 1],
            vec!["link", "del", "dev", "o3k-br0"]
        );
    }

    #[test]
    fn gateway_and_bridge_lifecycle_requires_owned_reverse_order() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-gateway-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::status(true),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command),
            &root,
        )?;
        let gateway = GatewaySpec {
            address: "192.0.2.1"
                .parse()
                .map_err(|_| HostNetworkError::InvalidConfiguration)?,
            prefix_len: 24,
        };
        manager.ensure_gateway(gateway)?;
        assert!(matches!(
            manager.delete_bridge(),
            Err(HostNetworkError::OwnershipConflict)
        ));
        manager.remove_gateway(gateway)?;
        manager.delete_bridge()?;
        assert_eq!(
            fs::read_to_string(root.join("ownership.json"))
                .map_err(|_| HostNetworkError::CommandFailed)?,
            "{\n  \"bridge\": null,\n  \"taps\": {}\n}"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn gateway_does_not_mutate_an_unowned_existing_bridge() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-foreign-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(matches!(
            manager.ensure_gateway(GatewaySpec {
                address: "192.0.2.1"
                    .parse()
                    .map_err(|_| HostNetworkError::InvalidConfiguration)?,
                prefix_len: 24,
            }),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert_eq!(command.calls().len(), 2);
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn manifest_accepts_multiple_taps_for_one_instance() -> Result<(), HostNetworkError> {
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                gateway: None,
            }),
            taps: [
                (
                    "o3ktap-a".to_owned(),
                    TapOwnership {
                        interface: "o3ktap-a".to_owned(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-a".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
                (
                    "o3ktap-b".to_owned(),
                    TapOwnership {
                        interface: "o3ktap-b".to_owned(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-b".to_owned(),
                        mac: "02:00:00:00:00:02".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        validate_manifest(
            &HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            &manifest,
        )
    }

    #[derive(Clone)]
    struct FakeNetworkCommand {
        responses: Arc<Mutex<VecDeque<Response>>>,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[derive(Clone)]
    enum Response {
        Output(bool, String),
        Status(bool),
    }

    impl Response {
        fn output(success: bool, stdout: &str) -> Self {
            Self::Output(success, stdout.to_owned())
        }

        fn status(success: bool) -> Self {
            Self::Status(success)
        }
    }

    impl FakeNetworkCommand {
        fn new(responses: impl IntoIterator<Item = Response>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn next(&self, args: &[&str]) -> Response {
            self.calls
                .lock()
                .expect("test calls mutex")
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            self.responses
                .lock()
                .expect("test responses mutex")
                .pop_front()
                .expect("test response for every command")
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("test calls mutex").clone()
        }
    }

    impl NetworkCommand for FakeNetworkCommand {
        fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput> {
            match self.next(args) {
                Response::Output(success, stdout) => Ok(NetworkCommandOutput { success, stdout }),
                Response::Status(_) => panic!("test output response expected"),
            }
        }

        fn status(&self, args: &[&str]) -> io::Result<bool> {
            match self.next(args) {
                Response::Status(success) => Ok(success),
                Response::Output(_, _) => panic!("test status response expected"),
            }
        }
    }

    fn test_manager(command: FakeNetworkCommand, uplink: Option<&str>) -> HostNetworkManager {
        HostNetworkManager::with_command(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: uplink.map(str::to_owned),
            },
            Arc::new(command),
        )
        .expect("valid test network configuration")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapSpec {
    pub instance_id: String,
    pub port_id: String,
    pub mac: String,
}

#[derive(Debug, Error)]
pub enum HostNetworkError {
    #[error("host network configuration is invalid")]
    InvalidConfiguration,
    #[error("host network operation failed")]
    CommandFailed,
    #[error("host network interface name is invalid")]
    InvalidName,
    #[error("host network MAC address is invalid")]
    InvalidMac,
    #[error("existing TAP interface is not owned by the requested O3K network")]
    ForeignInterface,
    #[error("host network rollback failed after an operation error")]
    RollbackFailed,
    #[error("host network ownership metadata is corrupt")]
    CorruptOwnership(#[source] serde_json::Error),
    #[error("host network ownership metadata could not be persisted")]
    OwnershipStorage(#[source] io::Error),
    #[error("host network ownership metadata conflicts with the requested resource")]
    OwnershipConflict,
}

impl HostNetworkConfig {
    pub fn validate(&self) -> Result<(), HostNetworkError> {
        validate_ifname(&self.bridge_name)?;
        if let Some(uplink) = &self.uplink {
            validate_ifname(uplink)?;
        }
        Ok(())
    }
}

pub struct HostNetworkManager {
    config: HostNetworkConfig,
    command: Arc<dyn NetworkCommand>,
    ownership: Option<Mutex<OwnershipStore>>,
}

struct OwnershipStore {
    path: PathBuf,
    manifest: NetworkOwnershipManifest,
}

impl HostNetworkManager {
    pub fn new(config: HostNetworkConfig) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            command: Arc::new(SystemNetworkCommand),
            ownership: None,
        })
    }

    /// Opens a manager with a durable, manager-owned host resource manifest.
    ///
    /// Existing links are still validated using read-only `ip` metadata. The
    /// manifest is required before O3K will mutate or remove a gateway or
    /// bridge, and it binds each reusable TAP to its instance and port.
    pub fn with_ownership_root(
        config: HostNetworkConfig,
        root: impl Into<PathBuf>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(HostNetworkError::OwnershipStorage)?;
        let path = root.join("ownership.json");
        let manifest = load_ownership(&path)?;
        validate_manifest(&config, &manifest)?;
        Ok(Self {
            config,
            command: Arc::new(SystemNetworkCommand),
            ownership: Some(Mutex::new(OwnershipStore { path, manifest })),
        })
    }

    #[cfg(test)]
    fn with_command(
        config: HostNetworkConfig,
        command: Arc<dyn NetworkCommand>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            command,
            ownership: None,
        })
    }

    #[cfg(test)]
    fn with_command_and_ownership(
        config: HostNetworkConfig,
        command: Arc<dyn NetworkCommand>,
        root: impl Into<PathBuf>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(HostNetworkError::OwnershipStorage)?;
        let path = root.join("ownership.json");
        let manifest = load_ownership(&path)?;
        validate_manifest(&config, &manifest)?;
        Ok(Self {
            config,
            command,
            ownership: Some(Mutex::new(OwnershipStore { path, manifest })),
        })
    }
    pub fn tap_name(port_id: &str) -> Result<String, HostNetworkError> {
        if port_id.trim().is_empty() {
            return Err(HostNetworkError::InvalidName);
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(port_id.as_bytes());
        let mut suffix = String::with_capacity(8);
        for byte in digest.iter().take(4) {
            use std::fmt::Write as _;
            let _ = write!(&mut suffix, "{byte:02x}");
        }
        Ok(format!("o3ktap-{suffix}"))
    }
    pub fn deterministic_mac(port_id: &str) -> Result<String, HostNetworkError> {
        if port_id.trim().is_empty() {
            return Err(HostNetworkError::InvalidName);
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(port_id.as_bytes());
        Ok(format!(
            "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            digest[0], digest[1], digest[2], digest[3], digest[4]
        ))
    }
    pub fn ensure_bridge(&self) -> Result<(), HostNetworkError> {
        self.ensure_bridge_with_ownership().map(|_| ())
    }

    /// Adds the managed gateway address after proving that the bridge is an
    /// O3K-owned bridge. A pre-existing bridge without a matching manifest is
    /// intentionally not mutated.
    pub fn ensure_gateway(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        validate_gateway(gateway)?;
        if let Some(recorded) = self.recorded_gateway()?
            && recorded != gateway
        {
            return Err(HostNetworkError::OwnershipConflict);
        }
        let bridge_created = self.ensure_bridge_with_ownership()?;
        if !bridge_created && !self.bridge_is_owned() {
            return Err(HostNetworkError::ForeignInterface);
        }
        let address = format!("{}/{}", gateway.address, gateway.prefix_len);
        if let Err(error) =
            self.run_ip(["addr", "replace", &address, "dev", &self.config.bridge_name])
        {
            let error = if bridge_created {
                self.rollback_bridge(error)
            } else {
                error
            };
            return Err(error);
        }
        if let Err(error) = self.set_gateway_ownership(gateway) {
            let rollback = self.run_ip(["addr", "del", &address, "dev", &self.config.bridge_name]);
            if rollback.is_err() {
                return Err(HostNetworkError::RollbackFailed);
            }
            if bridge_created {
                return Err(self.rollback_bridge(error));
            } else {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Removes only the gateway address recorded in the ownership manifest.
    pub fn remove_gateway(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        validate_gateway(gateway)?;
        let Some(recorded) = self.recorded_gateway()? else {
            return Ok(());
        };
        if recorded != gateway {
            return Err(HostNetworkError::OwnershipConflict);
        }
        let address = format!("{}/{}", gateway.address, gateway.prefix_len);
        self.run_ip(["addr", "del", &address, "dev", &self.config.bridge_name])?;
        self.clear_gateway_ownership()
    }

    /// Deletes the bridge only when O3K created it and no owned TAP remains.
    pub fn delete_bridge(&self) -> Result<(), HostNetworkError> {
        let Some(bridge) = self.recorded_bridge()? else {
            return Err(HostNetworkError::ForeignInterface);
        };
        if !bridge.created_by_o3k || bridge.gateway.is_some() || !self.recorded_taps_empty()? {
            return Err(HostNetworkError::OwnershipConflict);
        }
        if self.link_exists(&self.config.bridge_name) {
            let output =
                self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
            if !output.success || !interface_output_is_bridge(&output.stdout) {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.run_ip(["link", "del", "dev", &self.config.bridge_name])?;
        }
        self.clear_bridge_ownership()
    }

    fn ensure_bridge_with_ownership(&self) -> Result<bool, HostNetworkError> {
        if self.link_exists(&self.config.bridge_name) {
            let output =
                self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
            if !output.success || !interface_output_is_bridge(&output.stdout) {
                return Err(HostNetworkError::ForeignInterface);
            }
            if self.ownership.is_some() && !self.bridge_is_owned() {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.run_ip(["link", "set", "dev", &self.config.bridge_name, "up"])?;
            if let Some(uplink) = &self.config.uplink {
                let output = self.command_output(["-o", "link", "show", "dev", uplink])?;
                if !output.success {
                    return Err(HostNetworkError::CommandFailed);
                }
                if !interface_is_attached_to(&output.stdout, &self.config.bridge_name) {
                    return Err(HostNetworkError::ForeignInterface);
                }
            }
            return Ok(false);
        }
        self.run_ip([
            "link",
            "add",
            "name",
            &self.config.bridge_name,
            "type",
            "bridge",
        ])?;
        let setup = (|| {
            self.run_ip(["link", "set", "dev", &self.config.bridge_name, "up"])?;
            if let Some(uplink) = &self.config.uplink {
                self.run_ip([
                    "link",
                    "set",
                    "dev",
                    uplink,
                    "master",
                    &self.config.bridge_name,
                ])?;
            }
            Ok::<(), HostNetworkError>(())
        })();
        if let Err(error) = setup {
            return Err(self.rollback_bridge(error));
        }
        if let Err(error) = self.record_bridge_ownership() {
            return Err(self.rollback_bridge(error));
        }
        Ok(true)
    }

    pub fn create_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        self.ensure_tap(spec).map(|(name, _)| name)
    }

    /// Ensures one owned TAP exists and reports whether this call created it.
    /// Callers use the creation bit to make retries and rollback non-destructive.
    pub fn ensure_tap(&self, spec: &TapSpec) -> Result<(String, bool), HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        let bridge_created = self.ensure_bridge_with_ownership()?;
        let name = Self::tap_name(&spec.port_id)?;
        if self.link_exists(&name) {
            if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)?
            {
                if bridge_created {
                    return Err(self.rollback_bridge(HostNetworkError::ForeignInterface));
                }
                return Err(HostNetworkError::ForeignInterface);
            }
            self.validate_recorded_tap(&name, spec)?;
            return Ok((name, false));
        }
        let created_tap = self.run_ip(["tuntap", "add", "dev", &name, "mode", "tap"]);
        if let Err(error) = created_tap {
            return Err(if bridge_created {
                self.rollback_bridge(error)
            } else {
                error
            });
        }
        let setup = (|| {
            self.run_ip(["link", "set", "dev", &name, "address", &spec.mac])?;
            self.run_ip([
                "link",
                "set",
                "dev",
                &name,
                "master",
                &self.config.bridge_name,
            ])?;
            self.run_ip(["link", "set", "dev", &name, "up"])?;
            Ok::<(), HostNetworkError>(())
        })();
        if let Err(error) = setup {
            return Err(self.rollback_tap_and_bridge(&name, bridge_created, error));
        }
        if let Err(error) = self.stabilize_tap_address(&name, &spec.mac) {
            return Err(self.rollback_tap_and_bridge(&name, bridge_created, error));
        }
        if let Err(error) = self.record_tap_ownership(&name, spec) {
            return Err(self.rollback_tap_and_bridge(&name, bridge_created, error));
        }
        Ok((name, true))
    }
    /// Re-applies the requested TAP address until the kernel view stays
    /// stable across a settle window.
    ///
    /// A udev `net_setup_link` policy (for example the
    /// `MACAddressPolicy=persistent` shipped by `99-default.link`) is applied
    /// when the device add event is processed. That policy decision is based
    /// on attributes read when the worker starts, so the policy write can land
    /// after this process already set the address and silently replace it
    /// with a policy-derived address. The policy write happens once per add
    /// event, so observing the requested address across a settle window and
    /// re-applying it after any replacement converges before ownership is
    /// recorded.
    fn stabilize_tap_address(&self, name: &str, mac: &str) -> Result<(), HostNetworkError> {
        let started = Instant::now();
        let mut stable_since: Option<Instant> = None;
        loop {
            let output = self.command_output(["-d", "link", "show", "dev", name])?;
            if !output.success {
                return Err(HostNetworkError::CommandFailed);
            }
            if has_link_token(&output.stdout, "link/ether", mac) {
                let since = stable_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= TAP_ADDRESS_SETTLE_WINDOW {
                    return Ok(());
                }
            } else {
                stable_since = None;
                self.run_ip(["link", "set", "dev", name, "address", mac])?;
            }
            if started.elapsed() >= TAP_ADDRESS_STABILIZE_TIMEOUT {
                return Err(HostNetworkError::CommandFailed);
            }
            std::thread::sleep(TAP_ADDRESS_POLL_INTERVAL);
        }
    }

    /// Deletes a TAP only after proving its expected MAC and bridge ownership.
    pub fn delete_tap(&self, spec: &TapSpec) -> Result<(), HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        let name = Self::tap_name(&spec.port_id)?;
        if self.link_exists(&name) {
            if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)?
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.validate_recorded_tap(&name, spec)?;
            self.run_ip(["link", "del", "dev", &name])?;
        }
        self.clear_tap_ownership(&name, spec)?;
        Ok(())
    }

    /// Removes every TAP recorded as owned by one instance. Foreign or
    /// malformed ownership records are never selected for deletion.
    pub fn delete_taps_for_instance(&self, instance_id: &str) -> Result<(), HostNetworkError> {
        validate_reference(instance_id)?;
        let specs = self
            .ownership_snapshot(|manifest| {
                manifest
                    .taps
                    .values()
                    .filter(|record| record.created_by_o3k && record.instance_id == instance_id)
                    .map(|record| TapSpec {
                        instance_id: record.instance_id.clone(),
                        port_id: record.port_id.clone(),
                        mac: record.mac.clone(),
                    })
                    .collect::<Vec<_>>()
            })?
            .unwrap_or_default();
        let mut first_error = None;
        for spec in specs {
            if let Err(error) = self.delete_tap(&spec) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns durable port identities owned by an instance before its TAP
    /// records are removed. Coupled host services use these identities for
    /// fixed-lease cleanup.
    pub fn owned_port_ids_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<String>, HostNetworkError> {
        validate_reference(instance_id)?;
        let Some(ownership) = &self.ownership else {
            return Ok(Vec::new());
        };
        let store = ownership.lock().map_err(|_| {
            HostNetworkError::OwnershipStorage(io::Error::other("ownership lock poisoned"))
        })?;
        Ok(store
            .manifest
            .taps
            .values()
            .filter(|record| record.instance_id == instance_id)
            .map(|record| record.port_id.clone())
            .collect())
    }

    pub fn discover_managed(&self) -> Result<Vec<String>, HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        Ok(managed_tap_names(&output.stdout, &self.config.bridge_name))
    }

    /// Resolves a live TAP only when the durable ownership manifest and the
    /// current kernel interface both prove the requested instance/port/MAC
    /// binding. A deterministic TAP name alone is not sufficient evidence.
    pub fn resolve_owned_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        if self.ownership.is_none() {
            return Err(HostNetworkError::OwnershipConflict);
        }
        let name = Self::tap_name(&spec.port_id)?;
        if !self.link_exists(&name) {
            return Err(HostNetworkError::CommandFailed);
        }
        if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)? {
            return Err(HostNetworkError::ForeignInterface);
        }
        self.validate_recorded_tap(&name, spec)?;
        Ok(name)
    }

    /// Removes the managed gateway and bridge only when no owned TAP remains.
    /// A bridge without a durable O3K ownership record is never touched.
    pub fn cleanup_if_unused(&self) -> Result<(), HostNetworkError> {
        if !self.recorded_taps_empty()? {
            return Ok(());
        }
        if let Some(gateway) = self.recorded_gateway()? {
            self.remove_gateway(gateway)?;
        }
        if self.recorded_bridge()?.is_some() {
            self.delete_bridge()?
        }
        Ok(())
    }

    pub fn ownership_path(&self) -> Option<PathBuf> {
        self.ownership
            .as_ref()
            .and_then(|store| store.lock().ok().map(|guard| guard.path.clone()))
    }

    fn bridge_is_owned(&self) -> bool {
        self.recorded_bridge()
            .map(|bridge| bridge.is_some_and(|record| record.created_by_o3k))
            .unwrap_or(false)
    }

    fn recorded_bridge(&self) -> Result<Option<BridgeOwnership>, HostNetworkError> {
        self.ownership_snapshot(|manifest| manifest.bridge.clone())
            .map(|value| value.flatten())
    }

    fn recorded_gateway(&self) -> Result<Option<GatewaySpec>, HostNetworkError> {
        Ok(self
            .recorded_bridge()?
            .and_then(|bridge| bridge.gateway)
            .map(|gateway| GatewaySpec {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            }))
    }

    fn recorded_taps_empty(&self) -> Result<bool, HostNetworkError> {
        self.ownership_snapshot(|manifest| manifest.taps.is_empty())
            .map(|empty| empty.unwrap_or(true))
    }

    fn record_bridge_ownership(&self) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if let Some(existing) = &manifest.bridge
                && (existing.name != self.config.bridge_name
                    || existing.uplink != self.config.uplink)
            {
                return Err(HostNetworkError::OwnershipConflict);
            }
            manifest.bridge = Some(BridgeOwnership {
                name: self.config.bridge_name.clone(),
                uplink: self.config.uplink.clone(),
                created_by_o3k: true,
                gateway: manifest
                    .bridge
                    .as_ref()
                    .and_then(|bridge| bridge.gateway.clone()),
            });
            Ok(())
        })
    }

    fn set_gateway_ownership(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let bridge = manifest
                .bridge
                .as_mut()
                .ok_or(HostNetworkError::ForeignInterface)?;
            if bridge.name != self.config.bridge_name || !bridge.created_by_o3k {
                return Err(HostNetworkError::ForeignInterface);
            }
            bridge.gateway = Some(GatewayOwnership {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            });
            Ok(())
        })
    }

    fn clear_gateway_ownership(&self) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if let Some(bridge) = manifest.bridge.as_mut() {
                bridge.gateway = None;
            }
            Ok(())
        })
    }

    fn clear_bridge_ownership(&self) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if manifest.taps.is_empty() {
                manifest.bridge = None;
                Ok(())
            } else {
                Err(HostNetworkError::OwnershipConflict)
            }
        })
    }

    fn record_tap_ownership(
        &self,
        interface: &str,
        spec: &TapSpec,
    ) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let record = TapOwnership {
                interface: interface.to_owned(),
                instance_id: spec.instance_id.clone(),
                port_id: spec.port_id.clone(),
                mac: spec.mac.to_ascii_lowercase(),
                bridge: self.config.bridge_name.clone(),
                created_by_o3k: true,
            };
            if let Some(existing) = manifest.taps.get(interface)
                && existing != &record
            {
                return Err(HostNetworkError::OwnershipConflict);
            }
            manifest.taps.insert(interface.to_owned(), record);
            Ok(())
        })
    }

    fn validate_recorded_tap(
        &self,
        interface: &str,
        spec: &TapSpec,
    ) -> Result<(), HostNetworkError> {
        let Some(record) = self
            .ownership_snapshot(|manifest| manifest.taps.get(interface).cloned())?
            .flatten()
        else {
            return if self.ownership.is_some() {
                Err(HostNetworkError::ForeignInterface)
            } else {
                Ok(())
            };
        };
        if record.instance_id != spec.instance_id
            || record.port_id != spec.port_id
            || !record.mac.eq_ignore_ascii_case(&spec.mac)
            || record.bridge != self.config.bridge_name
            || !record.created_by_o3k
        {
            return Err(HostNetworkError::ForeignInterface);
        }
        Ok(())
    }

    fn clear_tap_ownership(&self, interface: &str, spec: &TapSpec) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let Some(record) = manifest.taps.get(interface) else {
                return Ok(());
            };
            if record.instance_id != spec.instance_id
                || record.port_id != spec.port_id
                || !record.mac.eq_ignore_ascii_case(&spec.mac)
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            manifest.taps.remove(interface);
            Ok(())
        })
    }

    fn ownership_snapshot<T>(
        &self,
        read: impl FnOnce(&NetworkOwnershipManifest) -> T,
    ) -> Result<Option<T>, HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(None);
        };
        let guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        Ok(Some(read(&guard.manifest)))
    }

    fn update_ownership(
        &self,
        update: impl FnOnce(&mut NetworkOwnershipManifest) -> Result<(), HostNetworkError>,
    ) -> Result<(), HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(());
        };
        let mut guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        let previous = guard.manifest.clone();
        update(&mut guard.manifest)?;
        if let Err(error) = persist_ownership(&guard.path, &guard.manifest) {
            guard.manifest = previous;
            return Err(error);
        }
        Ok(())
    }

    fn link_exists(&self, name: &str) -> bool {
        self.command
            .output(["link", "show", "dev", name].as_slice())
            .map(|output| output.success)
            .unwrap_or(false)
    }

    fn command_output<'a, I>(&self, args: I) -> Result<NetworkCommandOutput, HostNetworkError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        self.command
            .output(&args)
            .map_err(|_| HostNetworkError::CommandFailed)
    }

    fn run_ip<'a, I>(&self, args: I) -> Result<(), HostNetworkError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        match self.command.status(&args) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(HostNetworkError::CommandFailed),
        }
    }

    fn rollback_bridge(&self, original: HostNetworkError) -> HostNetworkError {
        if self
            .run_ip(["link", "del", "dev", &self.config.bridge_name])
            .is_ok()
        {
            match self.clear_bridge_ownership() {
                Ok(()) => original,
                Err(_) => HostNetworkError::RollbackFailed,
            }
        } else {
            HostNetworkError::RollbackFailed
        }
    }

    fn rollback_tap_and_bridge(
        &self,
        tap_name: &str,
        bridge_created: bool,
        original: HostNetworkError,
    ) -> HostNetworkError {
        if self.run_ip(["link", "del", "dev", tap_name]).is_err() {
            return HostNetworkError::RollbackFailed;
        }
        if bridge_created {
            return self.rollback_bridge(original);
        }
        original
    }
}

fn validate_ifname(name: &str) -> Result<(), HostNetworkError> {
    if name.is_empty()
        || name.len() > 15
        || name
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    {
        return Err(HostNetworkError::InvalidName);
    }
    Ok(())
}

fn validate_reference(value: &str) -> Result<(), HostNetworkError> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '/' | '\\')
        })
    {
        return Err(HostNetworkError::InvalidName);
    }
    Ok(())
}

fn validate_mac(mac: &str) -> Result<(), HostNetworkError> {
    if mac.len() != 17
        || mac.split(':').count() != 6
        || !mac
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(HostNetworkError::InvalidMac);
    }
    Ok(())
}

fn validate_gateway(gateway: GatewaySpec) -> Result<(), HostNetworkError> {
    if gateway.prefix_len > 30 {
        return Err(HostNetworkError::InvalidConfiguration);
    }
    Ok(())
}

fn load_ownership(path: &Path) -> Result<NetworkOwnershipManifest, HostNetworkError> {
    if !path.exists() {
        return Ok(NetworkOwnershipManifest::default());
    }
    serde_json::from_slice(&fs::read(path).map_err(HostNetworkError::OwnershipStorage)?)
        .map_err(HostNetworkError::CorruptOwnership)
}

fn validate_manifest(
    config: &HostNetworkConfig,
    manifest: &NetworkOwnershipManifest,
) -> Result<(), HostNetworkError> {
    if let Some(bridge) = &manifest.bridge {
        if bridge.name != config.bridge_name || bridge.uplink != config.uplink {
            return Err(HostNetworkError::OwnershipConflict);
        }
        if let Some(gateway) = bridge.gateway.as_ref() {
            validate_gateway(GatewaySpec {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            })?;
        }
    }
    let mut ports = HashSet::new();
    for (interface, tap) in &manifest.taps {
        validate_ifname(interface)?;
        validate_ifname(&tap.interface)?;
        validate_reference(&tap.instance_id)?;
        validate_reference(&tap.port_id)?;
        validate_mac(&tap.mac)?;
        if interface != &tap.interface
            || tap.bridge != config.bridge_name
            || !tap.created_by_o3k
            || !ports.insert(tap.port_id.clone())
        {
            return Err(HostNetworkError::OwnershipConflict);
        }
    }
    Ok(())
}

fn persist_ownership(
    path: &Path,
    manifest: &NetworkOwnershipManifest,
) -> Result<(), HostNetworkError> {
    let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| {
        HostNetworkError::OwnershipStorage(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership metadata serialization failed",
        ))
    })?;
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(HostNetworkError::OwnershipStorage(error));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(HostNetworkError::OwnershipStorage(error));
    }
    Ok(())
}

fn interface_is_owned_with(
    command: &dyn NetworkCommand,
    name: &str,
    expected_mac: &str,
    bridge_name: &str,
) -> Result<bool, HostNetworkError> {
    let output = command
        .output(["-d", "link", "show", "dev", name].as_slice())
        .map_err(|_| HostNetworkError::CommandFailed)?;
    if !output.success {
        return Err(HostNetworkError::CommandFailed);
    }
    Ok(interface_output_is_owned(
        &output.stdout,
        expected_mac,
        bridge_name,
    ))
}

fn interface_output_is_owned(output: &str, expected_mac: &str, bridge_name: &str) -> bool {
    interface_output_is_tap(output)
        && has_link_token(output, "link/ether", expected_mac)
        && has_link_token(output, "master", bridge_name)
}

fn interface_output_is_tap(output: &str) -> bool {
    output.contains("tun type tap")
        || output.lines().any(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            tokens
                .windows(3)
                .any(|window| window == ["tun", "type", "tap"])
        })
}

fn managed_tap_names(output: &str, bridge_name: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current_name = None;
    let mut current_output = String::new();
    let finish = |name: &mut Option<String>, block: &mut String, names: &mut Vec<String>| {
        if let Some(name) = name.take()
            && name.starts_with("o3ktap-")
            && interface_output_is_tap(block)
            && interface_is_attached_to(block, bridge_name)
        {
            names.push(name);
        }
        block.clear();
    };
    for line in output.lines() {
        if let Some((_, rest)) = line.split_once(": ")
            && line
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
            && rest.split(':').next().is_some_and(|name| !name.is_empty())
        {
            finish(&mut current_name, &mut current_output, &mut names);
            current_name = rest.split(':').next().map(str::to_owned);
        }
        if current_name.is_some() {
            current_output.push_str(line);
            current_output.push('\n');
        }
    }
    finish(&mut current_name, &mut current_output, &mut names);
    names
}

fn interface_is_attached_to(output: &str, bridge_name: &str) -> bool {
    output.lines().any(|line| {
        has_link_token(line, "state", "UP") && has_link_token(line, "master", bridge_name)
    })
}

fn has_link_token(output: &str, key: &str, expected: &str) -> bool {
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == key && pair[1].eq_ignore_ascii_case(expected))
}

fn interface_output_is_bridge(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim_start().starts_with("bridge "))
}

pub use o3k_store::{NetworkRecord, PortRecord, SubnetRecord};

/// Canonical binding state of a port on its selected host.
///
/// The durable store persists the string projections (persistence
/// projection); this service is the only authority that transitions between
/// states. `None` in the store means no host was ever selected and no
/// observation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortBindingState {
    /// A create dispatch selected a host but realization is not yet observed.
    Binding,
    /// The host observed the binding as realized.
    Bound,
    /// The host observed the binding as not realized.
    Down,
    /// The host observed a terminal failure.
    Error,
}

impl PortBindingState {
    /// The durable string projection.
    pub fn as_str(self) -> &'static str {
        match self {
            PortBindingState::Binding => "binding",
            PortBindingState::Bound => "bound",
            PortBindingState::Down => "down",
            PortBindingState::Error => "error",
        }
    }

    /// Parses the durable string projection. Unknown values are rejected so
    /// free-form state can never be persisted through the service.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "binding" => Some(PortBindingState::Binding),
            "bound" => Some(PortBindingState::Bound),
            "down" => Some(PortBindingState::Down),
            "error" => Some(PortBindingState::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("network resource not found")]
    NotFound,
    #[error("network resource already exists or is still in use")]
    Conflict,
    #[error("network request is invalid")]
    InvalidRequest,
    #[error("subnet allocation pool is exhausted")]
    PoolExhausted,
    #[error("network store error")]
    Store(#[source] o3k_store::StoreError),
    #[error("network metadata is corrupt")]
    CorruptMetadata(#[source] serde_json::Error),
}

fn map_store_error(error: o3k_store::StoreError) -> NetworkError {
    match error {
        o3k_store::StoreError::ResourceAlreadyExists => NetworkError::Conflict,
        o3k_store::StoreError::NetworkNotFound => NetworkError::NotFound,
        o3k_store::StoreError::NetworkInUse => NetworkError::Conflict,
        other => NetworkError::Store(other),
    }
}

#[derive(Clone)]
pub struct NetworkService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn o3k_store::NetworkRepository>,
}

impl NetworkService {
    pub async fn open(
        root: impl Into<PathBuf>,
        repository: Arc<dyn o3k_store::NetworkRepository>,
    ) -> Result<Self, NetworkError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| {
            NetworkError::Store(o3k_store::StoreError::CreateDataDirectory {
                path: root.clone(),
                source,
            })
        })?;
        let inner = Arc::new(Inner { root, repository });
        if inner.root.join("metadata.json").exists() {
            import_legacy_metadata(&inner.root, inner.repository.as_ref()).await?;
        }
        Ok(Self {
            inner,
            lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub async fn create_network(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .list_networks(project_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|network| network.name == name)
        {
            return Err(NetworkError::Conflict);
        }
        let network = NetworkRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "ACTIVE".to_owned(),
        };
        match self.inner.repository.insert_network(&network).await {
            Ok(()) => Ok(network),
            Err(o3k_store::StoreError::ResourceAlreadyExists) => Err(NetworkError::Conflict),
            Err(error) => Err(map_store_error(error)),
        }
    }

    pub async fn list_networks(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkRecord>, NetworkError> {
        self.inner
            .repository
            .list_networks(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn get_network(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<NetworkRecord, NetworkError> {
        self.inner
            .repository
            .get_network(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn delete_network(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_network(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subnet(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
        cidr: String,
        gateway_ip: Option<Ipv4Addr>,
        allocation_start: Option<Ipv4Addr>,
        allocation_end: Option<Ipv4Addr>,
    ) -> Result<SubnetRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let net = Ipv4Net::parse(&cidr)?;
        let cidr = net.canonical();
        let gateway = gateway_ip.unwrap_or(net.first_host());
        if !net.contains(gateway) || gateway == net.network || gateway == net.broadcast {
            return Err(NetworkError::InvalidRequest);
        }
        let start = allocation_start.unwrap_or(Ipv4Addr::from(u32::from(net.first_host()) + 1));
        let end = allocation_end.unwrap_or(net.last_host());
        if !net.contains(start)
            || !net.contains(end)
            || start > end
            || (u32::from(start)..=u32::from(end)).contains(&u32::from(gateway))
        {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        if self
            .inner
            .repository
            .list_subnets_for_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|subnet| subnet.cidr == cidr)
        {
            return Err(NetworkError::Conflict);
        }
        let subnet = SubnetRecord {
            id: Uuid::now_v7(),
            network_id,
            name,
            project_id: project_id.to_owned(),
            cidr,
            gateway_ip: gateway,
            allocation_start: start,
            allocation_end: end,
        };
        match self.inner.repository.insert_subnet(&subnet).await {
            Ok(()) => Ok(subnet),
            Err(o3k_store::StoreError::ResourceAlreadyExists) => Err(NetworkError::Conflict),
            Err(error) => Err(map_store_error(error)),
        }
    }

    pub async fn list_subnets(&self, project_id: &str) -> Result<Vec<SubnetRecord>, NetworkError> {
        self.inner
            .repository
            .list_subnets(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn get_subnet(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<SubnetRecord, NetworkError> {
        self.inner
            .repository
            .get_subnet(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn delete_subnet(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_subnet(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn create_port(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let subnet = self
            .inner
            .repository
            .list_subnets_for_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::NotFound)?;
        let used: HashSet<Ipv4Addr> = self
            .inner
            .repository
            .list_ports_for_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .map(|port| port.fixed_ip)
            .collect();
        let mut candidate = u32::from(subnet.allocation_start);
        let end = u32::from(subnet.allocation_end);
        let gateway = subnet.gateway_ip;
        while candidate <= end {
            let address = Ipv4Addr::from(candidate);
            if address != gateway && !used.contains(&address) {
                let id = Uuid::now_v7();
                let port = PortRecord {
                    id,
                    network_id,
                    subnet_id: Some(subnet.id),
                    project_id: project_id.to_owned(),
                    name: name.clone(),
                    mac_address: deterministic_port_mac(id),
                    fixed_ip: address,
                    status: "ACTIVE".to_owned(),
                    binding_host: None,
                    binding_state: None,
                };
                match self.inner.repository.insert_port(&port).await {
                    Ok(()) => return Ok(port),
                    Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
                    Err(error) => return Err(map_store_error(error)),
                }
            }
            candidate = candidate.saturating_add(1);
        }
        Err(NetworkError::PoolExhausted)
    }

    pub async fn list_ports(&self, project_id: &str) -> Result<Vec<PortRecord>, NetworkError> {
        self.inner
            .repository
            .list_ports(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn get_port(&self, project_id: &str, id: Uuid) -> Result<PortRecord, NetworkError> {
        self.inner
            .repository
            .get_port(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn delete_port(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_port(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn record_binding_intent(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
    ) -> Result<PortRecord, NetworkError> {
        if host.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port
            .binding_host
            .as_deref()
            .is_some_and(|current| current != host)
        {
            return Err(NetworkError::Conflict);
        }
        // A create dispatch is underway: transitions from unbound, binding,
        // down, and error to binding. A completed `bound` observation is kept:
        // idempotent dispatch replays of an already-succeeded create must not
        // downgrade durable observed state.
        let next = match port
            .binding_state
            .as_deref()
            .and_then(PortBindingState::parse)
        {
            Some(PortBindingState::Bound) => PortBindingState::Bound,
            _ => PortBindingState::Binding,
        };
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(next.as_str()))
            .await
            .map_err(map_store_error)
    }

    pub async fn project_binding_observation(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
        state: &str,
    ) -> Result<PortRecord, NetworkError> {
        let state = PortBindingState::parse(state).ok_or(NetworkError::InvalidRequest)?;
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port.binding_host.as_deref() != Some(host) {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(state.as_str()))
            .await
            .map_err(map_store_error)
    }

    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

#[derive(Clone, Copy)]
struct Ipv4Net {
    network: Ipv4Addr,
    broadcast: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    fn parse(value: &str) -> Result<Self, NetworkError> {
        let (address, prefix) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
        let address: Ipv4Addr = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix: u8 = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        if prefix > 30 {
            return Err(NetworkError::InvalidRequest);
        }
        let raw = u32::from(address);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let network = Ipv4Addr::from(raw & mask);
        let broadcast = Ipv4Addr::from((raw & mask) | !mask);
        Ok(Self {
            network,
            broadcast,
            prefix,
        })
    }

    fn canonical(self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }

    fn contains(self, address: Ipv4Addr) -> bool {
        let raw = u32::from(address);
        raw >= u32::from(self.network) && raw <= u32::from(self.broadcast)
    }
    fn first_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }
    fn last_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast) - 1)
    }
}

/// The legacy `metadata.json` shape written by previous versions. It is
/// parsed once, imported into the durable store, and the file is renamed so
/// it is never read again.
#[derive(serde::Deserialize)]
struct LegacyFile {
    networks: Vec<LegacyNetwork>,
    subnets: Vec<LegacySubnet>,
    ports: Vec<LegacyPort>,
}

#[derive(serde::Deserialize)]
struct LegacyNetwork {
    id: Uuid,
    name: String,
    project_id: String,
    status: String,
}

#[derive(serde::Deserialize)]
struct LegacySubnet {
    id: Uuid,
    network_id: Uuid,
    name: String,
    project_id: String,
    cidr: String,
    gateway_ip: Ipv4Addr,
    allocation_start: Ipv4Addr,
    allocation_end: Ipv4Addr,
}

#[derive(serde::Deserialize)]
struct LegacyPort {
    id: Uuid,
    network_id: Uuid,
    #[serde(default)]
    subnet_id: Uuid,
    project_id: String,
    name: String,
    #[serde(default)]
    mac_address: String,
    fixed_ip: Ipv4Addr,
    status: String,
}

/// Imports the legacy `metadata.json` file exactly once, in dependency order
/// (networks, then subnets, then ports), and renames it so `open` never reads
/// it again. The rename is best-effort: when it fails, the next `open`
/// re-reads the file, but the import is idempotent (records already present
/// are skipped), so the file can never double-import. Inserts skip records
/// that are already present, which makes a partially completed previous
/// import crash-resume safe. A corrupt file, duplicate MACs, or any
/// non-already-exists insert error fails the import closed and leaves the
/// file in place.
async fn import_legacy_metadata(
    root: &Path,
    repository: &dyn o3k_store::NetworkRepository,
) -> Result<(), NetworkError> {
    let path = root.join("metadata.json");
    let file = fs::File::open(&path)
        .map_err(|error| NetworkError::CorruptMetadata(serde_json::Error::io(error)))?;
    let mut legacy: LegacyFile =
        serde_json::from_reader(file).map_err(NetworkError::CorruptMetadata)?;
    let mut macs = HashSet::new();
    for port in &mut legacy.ports {
        if port.mac_address.is_empty() {
            port.mac_address = deterministic_port_mac(port.id);
        }
        if port.subnet_id.is_nil()
            && let Some(subnet) = legacy.subnets.iter().find(|subnet| {
                subnet.network_id == port.network_id && subnet.project_id == port.project_id
            })
        {
            port.subnet_id = subnet.id;
        }
        if !macs.insert(port.mac_address.to_ascii_lowercase()) {
            return Err(NetworkError::Conflict);
        }
    }
    for network in &legacy.networks {
        let record = NetworkRecord {
            id: network.id,
            name: network.name.clone(),
            project_id: network.project_id.clone(),
            status: network.status.clone(),
        };
        match repository.insert_network(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for subnet in &legacy.subnets {
        let record = SubnetRecord {
            id: subnet.id,
            network_id: subnet.network_id,
            name: subnet.name.clone(),
            project_id: subnet.project_id.clone(),
            cidr: subnet.cidr.clone(),
            gateway_ip: subnet.gateway_ip,
            allocation_start: subnet.allocation_start,
            allocation_end: subnet.allocation_end,
        };
        match repository.insert_subnet(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for port in &legacy.ports {
        let record = PortRecord {
            id: port.id,
            network_id: port.network_id,
            subnet_id: (!port.subnet_id.is_nil()).then_some(port.subnet_id),
            project_id: port.project_id.clone(),
            name: port.name.clone(),
            mac_address: port.mac_address.clone(),
            fixed_ip: port.fixed_ip,
            status: port.status.clone(),
            binding_host: None,
            binding_state: None,
        };
        match repository.insert_port(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    let _ = fs::rename(&path, root.join("metadata.json.imported"));
    Ok(())
}

fn deterministic_port_mac(port_id: Uuid) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(port_id.as_bytes());
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/o3k-network-{label}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn allocation_is_deterministic_collision_safe_and_restartable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("allocation");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network("project-a", "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                "project-a",
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let first = service
            .create_port("project-a", network.id, "one".to_owned())
            .await?;
        let second = service
            .create_port("project-a", network.id, "two".to_owned())
            .await?;
        assert_ne!(first.fixed_ip, second.fixed_ip);
        assert_ne!(first.mac_address, second.mac_address);
        assert_eq!(first.mac_address, deterministic_port_mac(first.id));
        assert_eq!(first.fixed_ip, subnet.allocation_start);
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        assert_eq!(reopened.get_port("project-a", first.id).await?, first);
        reopened.delete_port("project-a", first.id).await?;
        let replacement = reopened
            .create_port("project-a", network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, first.fixed_ip);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            name.contains("metadata.tmp-") || name.contains("metadata.json")
        }));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn legacy_metadata_file_is_imported_once_and_never_read_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("legacy-import");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        let network_id = Uuid::now_v7();
        let subnet_id = Uuid::now_v7();
        let port_with_mac = Uuid::now_v7();
        let port_without_mac = Uuid::now_v7();
        let port_without_subnet = Uuid::now_v7();
        let legacy = serde_json::json!({
            "networks": [{
                "id": network_id,
                "name": "flat",
                "project_id": "project-a",
                "status": "ACTIVE"
            }],
            "subnets": [{
                "id": subnet_id,
                "network_id": network_id,
                "name": "lab",
                "project_id": "project-a",
                "cidr": "192.0.2.0/29",
                "gateway_ip": "192.0.2.1",
                "allocation_start": "192.0.2.2",
                "allocation_end": "192.0.2.14"
            }],
            "ports": [
                {
                    "id": port_with_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "with-mac",
                    "mac_address": "02:00:00:00:00:99",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "no-mac",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_subnet,
                    "network_id": network_id,
                    "project_id": "project-a",
                    "name": "no-subnet",
                    "mac_address": "02:00:00:00:00:98",
                    "fixed_ip": "192.0.2.4",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(path.join("metadata.json"), serde_json::to_vec(&legacy)?)?;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        assert_eq!(service.list_networks("project-a").await?.len(), 1);
        assert_eq!(service.list_subnets("project-a").await?.len(), 1);
        assert_eq!(service.list_ports("project-a").await?.len(), 3);
        let network = service.get_network("project-a", network_id).await?;
        assert_eq!(network.id, network_id);
        let subnet = service.get_subnet("project-a", subnet_id).await?;
        assert_eq!(subnet.id, subnet_id);
        let first = service.get_port("project-a", port_with_mac).await?;
        assert_eq!(first.mac_address, "02:00:00:00:00:99");
        assert_eq!(first.subnet_id, Some(subnet_id));
        let migrated_mac = service.get_port("project-a", port_without_mac).await?;
        assert_eq!(
            migrated_mac.mac_address,
            deterministic_port_mac(port_without_mac)
        );
        assert_eq!(migrated_mac.subnet_id, Some(subnet_id));
        let migrated_subnet = service.get_port("project-a", port_without_subnet).await?;
        assert_eq!(migrated_subnet.subnet_id, Some(subnet_id));
        assert_eq!(migrated_subnet.mac_address, "02:00:00:00:00:98");
        assert!(!path.join("metadata.json").exists());
        assert!(path.join("metadata.json.imported").exists());
        let second = NetworkService::open(&path, store).await?;
        assert_eq!(second.list_networks("project-a").await?.len(), 1);
        assert_eq!(second.list_subnets("project-a").await?.len(), 1);
        assert_eq!(second.list_ports("project-a").await?.len(), 3);
        drop(second);
        fs::remove_dir_all(path)?;

        let corrupt_path = root("legacy-import-corrupt");
        let _ = fs::remove_dir_all(&corrupt_path);
        fs::create_dir_all(&corrupt_path)?;
        fs::write(corrupt_path.join("metadata.json"), b"not-json")?;
        let corrupt_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&corrupt_path, corrupt_store).await,
            Err(NetworkError::CorruptMetadata(_))
        ));
        assert!(corrupt_path.join("metadata.json").exists());
        fs::remove_dir_all(corrupt_path)?;

        let duplicate_path = root("legacy-import-duplicate-mac");
        let _ = fs::remove_dir_all(&duplicate_path);
        fs::create_dir_all(&duplicate_path)?;
        let duplicated = serde_json::json!({
            "networks": [],
            "subnets": [],
            "ports": [
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "one",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "two",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(
            duplicate_path.join("metadata.json"),
            serde_json::to_vec(&duplicated)?,
        )?;
        let duplicate_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&duplicate_path, duplicate_store).await,
            Err(NetworkError::Conflict)
        ));
        assert!(duplicate_path.join("metadata.json").exists());
        fs::remove_dir_all(duplicate_path)?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_port_creation_never_allocates_duplicate_ips_or_macs()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-network-concurrent-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let setup_store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let setup = NetworkService::open(&path, setup_store.clone()).await?;
        let network = setup.create_network("project-a", "flat".to_owned()).await?;
        let subnet = setup
            .create_subnet(
                "project-a",
                network.id,
                "lab".to_owned(),
                "192.0.2.0/28".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(subnet.cidr, "192.0.2.0/28");
        drop(setup);
        drop(setup_store);

        let store_a = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let mut handles = Vec::new();
        for index in 0..12 {
            let service = if index % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            let network_id = network.id;
            handles.push(tokio::spawn(async move {
                service
                    .create_port("project-a", network_id, format!("port-{index}"))
                    .await
            }));
        }
        let mut ports = Vec::new();
        for handle in handles {
            match handle.await? {
                Ok(port) => ports.push(port),
                Err(NetworkError::PoolExhausted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(ports.len(), 12);
        let ips: HashSet<Ipv4Addr> = ports.iter().map(|port| port.fixed_ip).collect();
        let macs: HashSet<String> = ports
            .iter()
            .map(|port| port.mac_address.to_ascii_lowercase())
            .collect();
        assert_eq!(ports.len(), ips.len());
        assert_eq!(ports.len(), macs.len());
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn binding_state_strings_round_trip_through_canonical_parsing() {
        for state in [
            PortBindingState::Binding,
            PortBindingState::Bound,
            PortBindingState::Down,
            PortBindingState::Error,
        ] {
            assert_eq!(PortBindingState::parse(state.as_str()), Some(state));
        }
        assert_eq!(PortBindingState::parse("unbound"), None);
        assert_eq!(PortBindingState::parse("banana"), None);
        assert_eq!(PortBindingState::parse(""), None);
    }

    #[tokio::test]
    async fn binding_intent_and_observation_projection_are_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("binding");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network("project-a", "flat".to_owned())
            .await?;
        let _subnet = service
            .create_subnet(
                "project-a",
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port("project-a", network.id, "one".to_owned())
            .await?;
        let intended = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(intended.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(intended.binding_state.as_deref(), Some("binding"));
        let observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(observed.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(observed.binding_state.as_deref(), Some("bound"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-1", "banana")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        // An idempotent dispatch replay of the same create must not downgrade
        // the completed `bound` observation back to `binding`.
        let replayed = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(replayed.binding_state.as_deref(), Some("bound"));
        // A fresh dispatch after an observed failure resets to `binding`.
        let down = service
            .project_binding_observation("project-a", port.id, "compute-1", "down")
            .await?;
        assert_eq!(down.binding_state.as_deref(), Some("down"));
        let retried = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(retried.binding_state.as_deref(), Some("binding"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-2", "bound")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "compute-2")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .project_binding_observation("project-a", Uuid::now_v7(), "compute-1", "bound")
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "  ")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let final_observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(final_observed.binding_state.as_deref(), Some("bound"));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_port("project-a", port.id).await?;
        assert_eq!(restored.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(restored.binding_state.as_deref(), Some("bound"));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn delete_cleanup_and_ip_reuse_after_restart() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("delete-reuse");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network("project-a", "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                "project-a",
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port("project-a", network.id, "one".to_owned())
            .await?;
        service.delete_port("project-a", port.id).await?;
        assert!(matches!(
            service.get_port("project-a", port.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let replacement = reopened
            .create_port("project-a", network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, port.fixed_ip);
        assert_ne!(replacement.mac_address, port.mac_address);
        reopened.delete_port("project-a", replacement.id).await?;
        reopened.delete_subnet("project-a", subnet.id).await?;
        reopened.delete_network("project-a", network.id).await?;
        assert!(matches!(
            reopened.get_network("project-a", network.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_cidr_exhaustion_and_project_isolation_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("validation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store).await?;
        let network = service
            .create_network("project-a", "flat".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_subnet(
                    "project-a",
                    network.id,
                    "bad".to_owned(),
                    "192.0.2.1/31".to_owned(),
                    None,
                    None,
                    None
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let _ = service
            .create_subnet(
                "project-a",
                network.id,
                "tiny".to_owned(),
                "192.0.2.0/30".to_owned(),
                None,
                Some(Ipv4Addr::new(192, 0, 2, 2)),
                Some(Ipv4Addr::new(192, 0, 2, 2)),
            )
            .await?;
        let _ = service
            .create_port("project-a", network.id, "one".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_port("project-a", network.id, "two".to_owned())
                .await,
            Err(NetworkError::PoolExhausted)
        ));
        assert!(matches!(
            service
                .create_subnet(
                    "project-a",
                    network.id,
                    "gateway-overlap".to_owned(),
                    "198.51.100.0/29".to_owned(),
                    Some(Ipv4Addr::new(198, 51, 100, 3)),
                    Some(Ipv4Addr::new(198, 51, 100, 2)),
                    Some(Ipv4Addr::new(198, 51, 100, 4)),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            service.get_network("project-b", network.id).await,
            Err(NetworkError::NotFound)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
