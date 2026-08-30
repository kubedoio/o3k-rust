#[allow(clippy::wildcard_imports)]
use super::*;

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

/// Optional kernel TAP access identity for consumers such as libvirt that
/// open a pre-created `managed="no"` interface themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapAccess {
    pub user: String,
    pub group: String,
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
    pub identity: Option<String>,
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

use linux_fabric::network_execution::{NetworkCommand, NetworkCommandOutput, SystemNetworkCommand};

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
    fn managed_bridge_mac_is_stable_and_locally_administered() -> Result<(), HostNetworkError> {
        let first = HostNetworkManager::deterministic_bridge_mac("o3k-b87654403")?;
        let second = HostNetworkManager::deterministic_bridge_mac("o3k-b87654403")?;
        assert_eq!(first, second);
        assert_eq!(first.len(), 17);
        assert!(first.starts_with("02:"));
        assert_ne!(
            first,
            HostNetworkManager::deterministic_bridge_mac("o3k-b87654404")?
        );
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
    fn bridge_creation_failure_removes_only_the_new_bridge() -> Result<(), HostNetworkError> {
        // The bridge is created under a provisional random name and renamed
        // only after the durable record is written (issue #608); an uplink
        // attach failure after the rename must remove only the newly created
        // bridge — never a foreign or record-less link.
        let root = std::env::temp_dir().join(format!("o3k-network-bridge-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <temp> down
            Response::status(true),      // link set dev <temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::status(false),     // link set dev eth0 master o3k-br0: FAILS
            Response::status(true),      // link del dev o3k-br0 (rollback)
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: Some("eth0".to_owned()),
            },
            Arc::new(command.clone()),
            &root,
        )?;

        assert!(matches!(
            manager.ensure_bridge(),
            Err(HostNetworkError::CommandFailed)
        ));
        let calls = command.calls();
        let temp = calls[1][3].clone();
        assert!(temp.starts_with("o3kbm-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &temp, "type", "bridge"],
                vec!["link", "set", "dev", &temp, "up"],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["link", "set", "dev", &temp, "down"],
                vec!["link", "set", "dev", &temp, "name", "o3k-br0"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "set", "dev", "eth0", "master", "o3k-br0"],
                vec!["link", "del", "dev", "o3k-br0"],
            ]
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn provisional_bridge_failure_removes_only_the_provisional_link() {
        // Issue #608: a failure before the rename (identity probe here) must
        // delete the provisional `o3kbm-*` bridge it created and never touch
        // the deterministic name.
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(false, ""), // identity probe: command failed
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // rollback probe of the provisional bridge
            Response::status(true),      // link del dev <temp>
        ]);
        let manager = test_manager(command.clone(), None);

        assert!(matches!(
            manager.ensure_bridge(),
            Err(HostNetworkError::ForeignInterface)
        ));
        let calls = command.calls();
        let temp = calls[1][3].clone();
        assert!(temp.starts_with("o3kbm-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &temp, "type", "bridge"],
                vec!["link", "set", "dev", &temp, "up"],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["link", "del", "dev", &temp],
            ]
        );
    }

    #[test]
    fn tap_setup_failure_removes_new_tap_and_bridge() {
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(false),     // link set dev <tap_temp> master: FAILS
            Response::output(
                true,
                "2: o3ktap-abcd: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // rollback probe of the provisional tap
            Response::status(true),      // link del dev <tap_temp>
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::RollbackFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let calls = command.calls();
        // Both the bridge and the TAP are created under provisional random
        // names; the deterministic names are only assigned by the final
        // renames (issues #602, #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &bridge_temp, "type", "bridge"],
                vec!["link", "set", "dev", &bridge_temp, "up"],
                vec!["-d", "link", "show", "dev", &bridge_temp],
                vec!["link", "set", "dev", &bridge_temp, "down"],
                vec!["link", "set", "dev", &bridge_temp, "name", "o3k-br0"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "show", "dev", &tap],
                vec!["tuntap", "add", "dev", &tap_temp, "mode", "tap"],
                vec![
                    "link",
                    "set",
                    "dev",
                    &tap_temp,
                    "address",
                    "02:00:00:00:00:01"
                ],
                vec!["link", "set", "dev", &tap_temp, "master", "o3k-br0"],
                vec!["-d", "link", "show", "dev", &tap_temp],
                vec!["link", "del", "dev", &tap_temp],
            ]
        );
    }

    #[test]
    fn provisional_tap_residue_is_reaped_without_a_manifest_proof() {
        // Issue #602: a create that dies before the ownership record is
        // durable leaves a provisional `o3ktmp-*` link. It is self-identifying
        // residue, so the startup reap deletes it without a manifest proof
        // while deterministic `o3ktap-*` and foreign links stay untouched.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3ktmp-1a2b3c4d: <BROADCAST> mtu 1500 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:09\n\
                 3: o3ktap-live000: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01\n\
                 4: eth0: <BROADCAST,UP> state UP\n\tlink/ether 02:00:00:00:00:02",
            ),
            Response::status(true), // link del dev o3ktmp-1a2b3c4d
        ]);
        let manager = test_manager(command.clone(), None);
        manager.reap_partial_links().expect("partial reap");
        assert_eq!(
            command.calls(),
            vec![
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3ktmp-1a2b3c4d"],
            ]
        );
    }

    #[test]
    fn provisional_bridge_residue_is_reaped_without_a_manifest_proof() {
        // Issue #608: a create that dies before the ownership record is
        // durable leaves a provisional `o3kbm-*` bridge. It is self-
        // identifying residue, so the startup reap deletes it without a
        // manifest proof while deterministic `o3k-b-*` bridges, foreign
        // links, and `o3kbm-*`-named non-bridge interfaces stay untouched.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500\n\
                 3: o3kbm-5e6f7788: <BROADCAST,UP> mtu 1500 state UP\n\tlink/ether 02:00:00:00:00:09\n\
                 4: o3k-b-2770749: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500\n\
                 5: o3ktmp-9a8b7c6d: <BROADCAST> mtu 1500 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:0a",
            ),
            Response::status(true), // link del dev o3kbm-1a2b3c4d
            Response::status(true), // link del dev o3ktmp-9a8b7c6d
        ]);
        let manager = test_manager(command.clone(), None);
        manager.reap_partial_links().expect("partial reap");
        assert_eq!(
            command.calls(),
            vec![
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3kbm-1a2b3c4d"],
                vec!["link", "del", "dev", "o3ktmp-9a8b7c6d"],
            ]
        );
    }

    #[test]
    fn crash_between_record_and_rename_is_fully_reaped() -> Result<(), HostNetworkError> {
        // Issue #602 crash window: the create died after record_tap_ownership
        // but before the rename, so the durable record references the final
        // (never created) deterministic name while the provisional link still
        // exists. The dangling record must be cleared without a kernel delete
        // and the provisional link must be reaped; neither half may survive.
        let root = std::env::temp_dir().join(format!("o3k-network-partial-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev <final>: absent
            Response::output(
                true,
                "2: o3ktmp-5e6f7788: <BROADCAST> master o3k-br0 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::status(true), // link del dev o3ktmp-5e6f7788
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        manager.delete_taps_for_instance("server-1")?;
        manager.reap_partial_links()?;
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(manifest.taps.is_empty(), "dangling record must be cleared");
        assert_eq!(
            command.calls(),
            vec![
                vec!["link", "show", "dev", &tap],
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3ktmp-5e6f7788"],
            ]
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
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
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // address stabilization probe
            Response::status(true),      // rename to the deterministic name
            Response::status(true),      // set up
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
    fn ensure_tap_recreates_a_recorded_but_absent_tap_and_reuses_a_present_one()
    -> Result<(), HostNetworkError> {
        // Issue #613 blocker A (host reboot): the durable record survives but
        // the ephemeral TAP is gone, while the persisted domain XML still
        // references the deterministic name. The first `ensure_tap` must
        // re-create the TAP under the recorded name (one `tuntap add`, no
        // duplicate record); the second call must verify and reuse the live
        // TAP without creating another one. The same manager serves both
        // calls, exactly like the startup restoration followed by the next
        // retry pass.
        let root = std::env::temp_dir().join(format!("o3k-network-restore-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        // First call: bridge exists (recorded identity matches), TAP absent,
        // so the create path runs under the provisional name and renames to
        // the deterministic one.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add (provisional name)
            Response::status(true),      // link set dev <temp> address
            Response::status(true),      // link set dev <temp> master
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // address stabilization probe
            Response::status(true),      // rename to the deterministic name
            Response::status(true),      // link set dev <tap> up
            // Second call: TAP present and owned, so no creation happens.
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true), // link set dev o3k-br0 up
            Response::output(true, "2: o3ktap-92bdccea: <BROADCAST>"), // tap exists
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // owned-tap probe
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        assert_eq!(
            manager.owned_tap_specs_for_instance("server-1")?,
            vec![spec.clone()],
            "the durable record must drive the restoration"
        );
        assert!(
            manager
                .owned_tap_specs_for_instance("server-other")?
                .is_empty(),
            "another instance's records must never be selected"
        );
        assert_eq!(
            manager.ensure_tap(&spec)?,
            (tap.clone(), true),
            "the absent recorded TAP must be re-created"
        );
        assert_eq!(
            manager.ensure_tap(&spec)?,
            (tap.clone(), false),
            "the present owned TAP must be verified and reused, not re-created"
        );
        assert_eq!(
            command
                .calls()
                .iter()
                .filter(|args| args[..2] == ["tuntap", "add"])
                .count(),
            1,
            "exactly one TAP creation may ever be issued for the recorded TAP"
        );
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert_eq!(
            manifest.taps.len(),
            1,
            "the restoration must never duplicate the ownership record"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn ensure_tap_fails_closed_on_a_foreign_link_at_the_recorded_name()
    -> Result<(), HostNetworkError> {
        // Issue #613 blocker A restore path: a FOREIGN link exists at the
        // recorded deterministic TAP name (a TAP attached to the bridge but
        // with a different MAC). `ensure_tap` must fail closed with
        // `ForeignInterface` and issue ZERO mutation commands — no
        // `tuntap add`, no `link del` — so the startup restoration holds
        // the instance's domain start back instead of touching the foreign
        // interface.
        let root = std::env::temp_dir().join(format!("o3k-network-foreign-tap-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true), // link set dev o3k-br0 up
            Response::output(true, "2: o3ktap-92bdccea: <BROADCAST>"), // tap exists
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:02",
            ), // owned-tap probe: foreign MAC at the recorded name
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        assert!(matches!(
            manager.ensure_tap(&spec),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args[..2] == ["tuntap", "add"]),
            "a foreign link must never trigger a TAP creation"
        );
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args[..2] == ["link", "del"]),
            "a foreign link must never be deleted"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn crash_residue_is_enumerated_and_reaped_across_restart() -> Result<(), HostNetworkError> {
        // Issue-87 S3 rerun #5: the create prepared the host network (bridge,
        // TAP, DHCP bindings) and the agent died before defining the domain.
        // The control-plane delete converges through local completion and
        // never dispatches an agent delete, so the residue survives until the
        // agent restart reconciliation enumerates the durable manifest and
        // reaps the recorded network state of the absent instance.
        let root = std::env::temp_dir().join(format!("o3k-network-reap-{}", Uuid::now_v7()));
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let first = FakeNetworkCommand::new([
            Response::output(false, ""), // bridge absent
            Response::status(true),      // bridge add (provisional name)
            Response::status(true),      // bridge up (provisional name)
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // bridge down (provisional name)
            Response::status(true),      // rename to the deterministic bridge name
            Response::status(true),      // bridge up (deterministic name)
            Response::output(false, ""), // tap absent
            Response::status(true),      // tuntap add (provisional name)
            Response::status(true),      // set address
            Response::status(true),      // set master
            Response::output(
                true,
                &format!(
                    "2: {tap}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // rename to the deterministic name
            Response::status(true), // set up
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(first),
            &root,
        )?;
        assert_eq!(manager.create_tap(&spec)?, tap);
        // The agent process is killed here; the kernel and the ownership
        // manifest keep the bridge and TAP with no delete command in flight.

        // On restart the same ownership root is reopened and the kernel still
        // reports the TAP attached to the managed bridge.
        let reopened_command = FakeNetworkCommand::new([
            Response::output(
                true,
                &format!("2: {tap}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // link del tap
            Response::output(true, "2: o3k-br0: <BROADCAST,UP> mtu 1500 state UP"),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true), // link del bridge
        ]);
        let reopened = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(reopened_command.clone()),
            &root,
        )?;
        // The restart reconciliation enumerates the residue through the
        // durable manifest and tears down the recorded network state.
        assert_eq!(reopened.owned_instance_ids()?, vec!["server-1".to_owned()]);
        reopened.delete_taps_for_instance("server-1")?;
        reopened.cleanup_if_unused()?;
        let calls = reopened_command.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", &tap])
                .count(),
            1,
            "the TAP must be deleted exactly once"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", "o3k-br0"])
                .count(),
            1,
            "the owned bridge must be deleted exactly once"
        );
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(manifest.bridge.is_none() && manifest.taps.is_empty());
        // A repeat of the reap after the teardown is a no-op: the manifest is
        // the authority, so no further host command may be issued.
        let calls_before = reopened_command.calls().len();
        reopened.delete_taps_for_instance("server-1")?;
        reopened.cleanup_if_unused()?;
        assert_eq!(reopened_command.calls().len(), calls_before);
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn reaping_one_instance_keeps_the_shared_bridge_until_the_last_tap_is_gone()
    -> Result<(), HostNetworkError> {
        // Issue-87: the managed bridge is shared; reaping one absent instance
        // must never remove it while another recorded instance still uses it.
        let root = std::env::temp_dir().join(format!("o3k-network-shared-{}", Uuid::now_v7()));
        let tap_a = HostNetworkManager::tap_name("port-a").expect("valid test tap name");
        let tap_b = HostNetworkManager::tap_name("port-b").expect("valid test tap name");
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [
                (
                    tap_a.clone(),
                    TapOwnership {
                        interface: tap_a.clone(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-a".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
                (
                    tap_b.clone(),
                    TapOwnership {
                        interface: tap_b.clone(),
                        instance_id: "server-2".to_owned(),
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
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                &format!("2: {tap_a}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap_a}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // link del tap-a
            Response::output(
                true,
                &format!("2: {tap_b}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap_b}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:02"
                ),
            ),
            Response::status(true), // link del tap-b
            Response::output(true, "2: o3k-br0: <BROADCAST,UP> mtu 1500 state UP"),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true), // link del bridge
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        manager.delete_taps_for_instance("server-1")?;
        manager.cleanup_if_unused()?;
        let mid: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(
            mid.bridge.is_some(),
            "the shared bridge must survive the first reap"
        );
        assert_eq!(mid.taps.len(), 1);
        manager.delete_taps_for_instance("server-2")?;
        manager.cleanup_if_unused()?;
        let end: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(end.bridge.is_none() && end.taps.is_empty());
        assert_eq!(
            command
                .calls()
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", "o3k-br0"])
                .count(),
            1,
            "the bridge must be deleted exactly once, after the last TAP"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn reaping_a_never_prepared_instance_is_a_noop() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-noop-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(manager.owned_instance_ids()?.is_empty());
        manager.delete_taps_for_instance("never-prepared")?;
        manager.cleanup_if_unused()?;
        assert!(
            command.calls().is_empty(),
            "a never-prepared instance must not touch the host network"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn tap_address_is_reapplied_after_external_replacement() -> Result<(), HostNetworkError> {
        // A udev MAC policy write can land after the address was set during
        // TAP creation. The owner must observe the replacement, re-apply the
        // requested address, and only then record ownership.
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
            ),
            Response::status(true),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::status(true),
            Response::status(true),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        let name = manager.create_tap(&spec)?;
        let calls = command.calls();
        // Address setup and stabilization happen under the provisional tap
        // name; the bridge too is created under a provisional name and
        // renamed only after its durable record is written (issues #602,
        // #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        let set_calls = calls
            .iter()
            .filter(|args| {
                args.as_slice()
                    == [
                        "link",
                        "set",
                        "dev",
                        &tap_temp,
                        "address",
                        "02:00:00:00:00:01",
                    ]
            })
            .count();
        assert_eq!(set_calls, 2, "address must be re-applied after replacement");
        assert!(
            calls
                .iter()
                .any(|args| args.as_slice() == ["link", "set", "dev", &tap_temp, "name", &name]),
            "the provisional link must be renamed to the deterministic name"
        );
        assert!(
            calls
                .iter()
                .any(|args| args.as_slice()
                    == ["link", "set", "dev", &bridge_temp, "name", "o3k-br0"]),
            "the provisional bridge must be renamed to the deterministic name"
        );
        Ok(())
    }

    #[test]
    fn tap_address_reapply_failure_rolls_back_owned_resources() {
        let mut responses = vec![
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
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
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
        ));
        // Without a durable bridge identity, rollback preserves the bridge for
        // reconciliation instead of guessing that a same-name replacement is
        // still O3K-owned; the newly-created TAP is still removed.
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
            Err(HostNetworkError::RollbackFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let calls = command.calls();
        // Setup and stabilization run under the provisional names (issues
        // #602, #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        let reapplies = calls
            .iter()
            .filter(|args| {
                args.as_slice()
                    == [
                        "link",
                        "set",
                        "dev",
                        &tap_temp,
                        "address",
                        "02:00:00:00:00:01",
                    ]
            })
            .count();
        assert!(reapplies >= 2, "address must be re-applied while unstable");
        assert_eq!(
            calls.last(),
            Some(&vec![
                "link".to_owned(),
                "del".to_owned(),
                "dev".to_owned(),
                tap_temp.clone()
            ])
        );
        assert!(
            !calls.iter().any(|args| args.len() > 3
                && args[3] == tap
                && (args[0] == "tuntap" || (args[0] == "link" && args[1] != "show"))),
            "the deterministic name must not be mutated before the rename"
        );
    }

    #[test]
    fn gateway_and_bridge_lifecycle_requires_owned_reverse_order() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-gateway-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <temp> down
            Response::status(true),      // link set dev <temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::status(true),      // addr replace 192.0.2.1/24 dev o3k-br0
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // remove_gateway ownership probe
            Response::status(true),      // addr del 192.0.2.1/24 dev o3k-br0
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // delete_bridge link_exists
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // delete_bridge ownership probe
            Response::status(true),      // link del dev o3k-br0
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
    fn cleanup_preserves_same_name_foreign_bridge_replacement() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-replaced-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let gateway = GatewaySpec {
            address: "192.0.2.1"
                .parse()
                .map_err(|_| HostNetworkError::InvalidConfiguration)?,
            prefix_len: 24,
        };
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: Some(GatewayOwnership {
                    address: gateway.address,
                    prefix_len: gateway.prefix_len,
                }),
            }),
            taps: BTreeMap::new(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([Response::output(
            true,
            "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
        )]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(matches!(
            manager.remove_gateway(gateway),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            command
                .calls()
                .iter()
                .all(|args| args.as_slice() != ["addr", "del", "192.0.2.1/24", "dev", "o3k-br0"])
        );
        Ok(())
    }

    #[test]
    fn manifest_accepts_multiple_taps_for_one_instance() -> Result<(), HostNetworkError> {
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
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
            let identity_probe = args == ["-d", "link", "show", "dev", "o3k-br0"]
                && self
                    .calls
                    .lock()
                    .expect("test calls mutex")
                    .last()
                    .is_some_and(|previous| {
                        previous == &["link", "set", "dev", "o3k-br0", "up"]
                            || (previous.len() >= 2
                                && previous[previous.len() - 2..] == ["master", "o3k-br0"])
                    });
            if identity_probe {
                return Ok(NetworkCommandOutput {
                    success: true,
                    stdout: "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500".to_owned(),
                });
            }
            match self.next(args) {
                Response::Output(success, stdout) => Ok(NetworkCommandOutput { success, stdout }),
                Response::Status(_) => panic!("test output response expected"),
            }
        }

        fn status(&self, args: &[&str]) -> io::Result<bool> {
            if args.len() == 6
                && args[..4] == ["link", "set", "dev", "o3k-br0"]
                && args[4] == "address"
            {
                return Ok(true);
            }
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
    set_stable_bridge_mac: bool,
    tap_access: Option<TapAccess>,
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
            set_stable_bridge_mac: true,
            tap_access: None,
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
            set_stable_bridge_mac: true,
            tap_access: None,
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
            set_stable_bridge_mac: false,
            tap_access: None,
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
            set_stable_bridge_mac: false,
            tap_access: None,
        })
    }

    /// Configures the kernel identity allowed to open newly created TAPs.
    /// This is intentionally explicit and optional; ordinary host consumers
    /// retain the historical root-owned TAP behavior.
    pub fn with_tap_access(mut self, access: Option<TapAccess>) -> Result<Self, HostNetworkError> {
        if access
            .as_ref()
            .is_some_and(|value| value.user.trim().is_empty() || value.group.trim().is_empty())
        {
            return Err(HostNetworkError::InvalidConfiguration);
        }
        self.tap_access = access;
        Ok(self)
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
    /// Provisional name for a TAP whose ownership record is not yet durable.
    /// The random suffix makes the name self-identifying crash residue: no
    /// manifest record ever references it, no domain ever attaches it, and it
    /// never collides with a deterministic `o3ktap-` name, so startup
    /// reconciliation may delete it without a manifest proof (issue #602).
    fn partial_tap_name() -> String {
        format!("o3ktmp-{}", partial_suffix())
    }
    /// Provisional name for a bridge whose ownership record is not yet
    /// durable. Same self-identifying residue contract as [`Self::partial_tap_name`]:
    /// no manifest record ever references it and it never collides with a
    /// deterministic `o3k-b*` bridge, so startup reconciliation may delete it
    /// without a manifest proof (issue #608).
    fn partial_bridge_name() -> String {
        format!("o3kbm-{}", partial_suffix())
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

    /// Returns the stable, locally-administered MAC used by a managed bridge.
    ///
    /// Linux may otherwise change a bridge's automatically selected MAC when
    /// the first TAP is enslaved.  Ownership is recorded only after this
    /// address is set, so the identity remains stable across TAP attach and
    /// detach operations and cannot be confused with a same-name replacement.
    pub fn deterministic_bridge_mac(bridge_name: &str) -> Result<String, HostNetworkError> {
        validate_ifname(bridge_name)?;
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bridge_name.as_bytes());
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
        if !bridge_created && !self.bridge_is_owned_live()? {
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
        if !self.bridge_is_owned_live()? {
            return Err(HostNetworkError::ForeignInterface);
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
            if !output.success
                || !interface_output_is_bridge(&output.stdout)
                || !self.bridge_is_owned_output(&output)
            {
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
            if self.ownership.is_some() && !self.bridge_is_owned_output(&output) {
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
        // Create under a provisional random name and rename only after the
        // ownership record is durable. A crash before the rename leaves an
        // `o3kbm-*` bridge that no manifest record ever references by that
        // name and that never collides with a deterministic `o3k-b*` bridge,
        // so startup reconciliation can delete it without weakening the
        // foreign-interface fence (issue #608: a crash between link creation
        // and ownership recording otherwise orphaned a deterministic-name
        // bridge that the ownership fence permanently refused and no reap
        // covered).
        let temp_name = Self::partial_bridge_name();
        self.run_ip(["link", "add", "name", &temp_name, "type", "bridge"])?;
        let setup = (|| {
            if self.set_stable_bridge_mac {
                let bridge_mac = Self::deterministic_bridge_mac(&self.config.bridge_name)?;
                self.run_ip(["link", "set", "dev", &temp_name, "address", &bridge_mac])?;
            }
            self.run_ip(["link", "set", "dev", &temp_name, "up"])
        })();
        if let Err(error) = setup {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        let identity = self
            .command_output(["-d", "link", "show", "dev", &temp_name])
            .ok()
            .filter(|output| output.success && interface_output_is_bridge(&output.stdout))
            .and_then(|output| interface_identity(&output.stdout));
        let Some(identity) = identity else {
            return Err(
                self.rollback_provisional_bridge(&temp_name, HostNetworkError::ForeignInterface)
            );
        };
        // The record is keyed by the deterministic name, so a crash after
        // this point converges on retry exactly like the TAP path.
        if let Err(error) = self.record_bridge_ownership(identity) {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        // The bridge must be DOWN for the rename; a failure before the
        // rename still removes only the provisional link.
        let renamed = (|| {
            self.run_ip(["link", "set", "dev", &temp_name, "down"])?;
            self.run_ip([
                "link",
                "set",
                "dev",
                &temp_name,
                "name",
                &self.config.bridge_name,
            ])
        })();
        if let Err(error) = renamed {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        // The uplink is attached only after the rename by the final name, so
        // the recorded master reference is stable. Failures here hit the
        // deterministic rollback: the durable record exists and the live
        // identity is verified before deletion.
        let bring_up = (|| {
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
        if let Err(error) = bring_up {
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
        // Create under a provisional random name and rename only after the
        // ownership record is durable. A crash before the rename leaves an
        // `o3ktmp-*` link that no manifest record ever references and that
        // never collides with a deterministic `o3ktap-` name, so startup
        // reconciliation can delete it without weakening the foreign-interface
        // fence (issue #602: a crash between link creation and ownership
        // recording otherwise orphaned a deterministic-name TAP that wedged
        // every later create on the network).
        let temp_name = Self::partial_tap_name();
        let mut tuntap_args = vec!["tuntap", "add", "dev", &temp_name, "mode", "tap"];
        if let Some(access) = &self.tap_access {
            tuntap_args.extend(["user", access.user.as_str(), "group", access.group.as_str()]);
        }
        let created_tap = self.run_ip(tuntap_args);
        if let Err(error) = created_tap {
            return Err(if bridge_created {
                self.rollback_bridge(error)
            } else {
                error
            });
        }
        let setup = (|| {
            self.run_ip(["link", "set", "dev", &temp_name, "address", &spec.mac])?;
            self.run_ip([
                "link",
                "set",
                "dev",
                &temp_name,
                "master",
                &self.config.bridge_name,
            ])?;
            Ok::<(), HostNetworkError>(())
        })();
        if let Err(error) = setup {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        if let Err(error) = self.stabilize_tap_address(&temp_name, &spec.mac) {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        if let Err(error) = self.record_tap_ownership(&name, spec) {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        // The link was never brought up, so the rename is accepted; ownership
        // is already recorded under the final name. From here the recorded
        // startup reap covers a crash exactly as before.
        if let Err(error) = self.run_ip(["link", "set", "dev", &temp_name, "name", &name]) {
            let mut rollback =
                self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error);
            if self.clear_tap_ownership(&name, spec).is_err() {
                rollback = HostNetworkError::RollbackFailed;
            }
            return Err(rollback);
        }
        if let Err(error) = self.run_ip(["link", "set", "dev", &name, "up"]) {
            let mut rollback =
                self.rollback_tap_and_bridge(&name, &spec.mac, bridge_created, error);
            if self.clear_tap_ownership(&name, spec).is_err() {
                rollback = HostNetworkError::RollbackFailed;
            }
            return Err(rollback);
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

    /// Returns the create-time specs of the TAPs recorded as O3K-owned for
    /// one instance. The startup domain restoration (issue #613 blocker A)
    /// re-creates these TAPs after a host reboot: the ephemeral devices are
    /// gone while the persisted domain XML still references them. Foreign or
    /// malformed records are never selected for mutation; `ensure_tap`
    /// re-verifies every returned spec against the manifest and the kernel
    /// before creating or reusing anything.
    pub fn owned_tap_specs_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<TapSpec>, HostNetworkError> {
        validate_reference(instance_id)?;
        Ok(self
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
            .unwrap_or_default())
    }

    /// Returns the distinct instance identities recorded in the ownership
    /// manifest. The agent's restart reconciliation enumerates these to find
    /// host artifacts that may be stale after a crash.
    pub fn owned_instance_ids(&self) -> Result<Vec<String>, HostNetworkError> {
        self.ownership_snapshot(|manifest| {
            let mut ids: Vec<String> = manifest
                .taps
                .values()
                .filter(|record| record.created_by_o3k)
                .map(|record| record.instance_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        })
        .map(|ids| ids.unwrap_or_default())
    }

    pub fn discover_managed(&self) -> Result<Vec<String>, HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        Ok(managed_tap_names(&output.stdout, &self.config.bridge_name))
    }

    /// Deletes provisional `o3ktmp-*` TAPs and `o3kbm-*` bridges. Such a link
    /// is by construction residue of a create that died before the ownership
    /// record became durable: manifest records use the final deterministic
    /// name, so the manifest never references a provisional name, no running
    /// domain ever attaches one, and the random suffix never collides with a
    /// legitimate interface. The deterministic `o3ktap-`/`o3k-b*` foreign-
    /// interface fences are unchanged (issues #602, #608).
    pub fn reap_partial_links(&self) -> Result<(), HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        let mut first_error = None;
        for name in partial_link_names(&output.stdout) {
            if let Err(error) = self.run_ip(["link", "del", "dev", &name]) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Resolves a live TAP only when the durable ownership manifest and the
    /// current kernel interface both prove the requested instance/port/MAC
    /// binding. A deterministic TAP name alone is not sufficient evidence.
    pub fn resolve_owned_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        // The ownership manifest may be written by the bounded network
        // executor after a compute agent has opened its manager. Refresh the
        // durable snapshot before a cross-process read so a valid externally
        // realized TAP is not mistaken for a foreign interface.
        self.refresh_ownership()?;
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

    /// Reloads the manager-owned manifest after another O3K process has
    /// durably changed it. The atomic manifest replacement makes this read
    /// safe across the network executor and compute agent boundary.
    pub fn refresh_ownership(&self) -> Result<(), HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(());
        };
        let path = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?
            .path
            .clone();
        let manifest = load_ownership(&path)?;
        validate_manifest(&self.config, &manifest)?;
        let mut guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        guard.manifest = manifest;
        Ok(())
    }

    /// Returns the configured bridge identity for bounded execution adapters.
    pub fn bridge_name(&self) -> Option<String> {
        Some(self.config.bridge_name.clone())
    }

    fn bridge_is_owned_output(&self, output: &NetworkCommandOutput) -> bool {
        let Some(identity) = interface_identity(&output.stdout) else {
            return false;
        };
        self.recorded_bridge().ok().flatten().is_some_and(|record| {
            record.name == self.config.bridge_name
                && record.created_by_o3k
                && record.identity.as_deref() == Some(identity.as_str())
        })
    }

    fn bridge_is_owned_live(&self) -> Result<bool, HostNetworkError> {
        let output =
            self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
        Ok(output.success
            && interface_output_is_bridge(&output.stdout)
            && self.bridge_is_owned_output(&output))
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

    fn record_bridge_ownership(&self, identity: String) -> Result<(), HostNetworkError> {
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
                identity: Some(identity.clone()),
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
        // A bridge that never reached the durable ownership manifest has no
        // current identity to verify.  Preserve it for reconciliation rather
        // than deleting a same-name replacement during rollback.
        if self.recorded_bridge().ok().flatten().is_none() {
            return HostNetworkError::RollbackFailed;
        }
        let owned_now = self
            .command_output(["-d", "link", "show", "dev", &self.config.bridge_name])
            .ok()
            .is_some_and(|output| {
                output.success
                    && interface_output_is_bridge(&output.stdout)
                    && self.bridge_is_owned_output(&output)
            });
        if owned_now
            && self
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

    /// Removes a bridge that never reached the durable deterministic name.
    /// The provisional name is O3K-created by construction, so the deletion
    /// guard only has to prove the link is still the bridge we made (its
    /// stable MAC when one was set); a failed deletion leaves a record-less
    /// `o3kbm-*` bridge that the startup reap removes on the next restart
    /// (issue #608).
    fn rollback_provisional_bridge(
        &self,
        temp_name: &str,
        original: HostNetworkError,
    ) -> HostNetworkError {
        let output = self
            .command_output(["-d", "link", "show", "dev", temp_name])
            .ok()
            .filter(|output| output.success && interface_output_is_bridge(&output.stdout));
        let Some(output) = output else {
            return HostNetworkError::RollbackFailed;
        };
        if self.set_stable_bridge_mac {
            let Ok(expected) = Self::deterministic_bridge_mac(&self.config.bridge_name) else {
                return HostNetworkError::RollbackFailed;
            };
            if !has_link_token(&output.stdout, "link/ether", &expected) {
                return HostNetworkError::RollbackFailed;
            }
        }
        if self.run_ip(["link", "del", "dev", temp_name]).is_err() {
            return HostNetworkError::RollbackFailed;
        }
        original
    }

    fn rollback_tap_and_bridge(
        &self,
        tap_name: &str,
        expected_mac: &str,
        bridge_created: bool,
        original: HostNetworkError,
    ) -> HostNetworkError {
        let owned_now = self
            .command_output(["-d", "link", "show", "dev", tap_name])
            .ok()
            .is_some_and(|output| {
                output.success
                    && interface_output_is_owned(
                        &output.stdout,
                        expected_mac,
                        &self.config.bridge_name,
                    )
            });
        if !owned_now || self.run_ip(["link", "del", "dev", tap_name]).is_err() {
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

/// Random 8-hex-character suffix from the random tail of a v7 UUID, shared by
/// the provisional TAP (`o3ktmp-`) and bridge (`o3kbm-`) names. 8 hex chars
/// keep either prefixed name inside the 15-byte kernel interface-name limit.
fn partial_suffix() -> String {
    let id = Uuid::now_v7().simple().to_string();
    id[id.len() - 8..].to_owned()
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

fn partial_link_names(output: &str) -> Vec<String> {
    // A provisional link is residue regardless of bridge attachment: a crash
    // can land before `set master`, so no bridge condition applies here. The
    // kernel output proves the link kind: an `o3ktmp-*` name must still be a
    // TAP and an `o3kbm-*` name must still be a bridge. Names come from the
    // kernel; keep only syntactically valid interface names with a
    // provisional prefix.
    let mut names = Vec::new();
    let mut current_name = None;
    let mut current_output = String::new();
    let finish = |name: &mut Option<String>, block: &mut String, names: &mut Vec<String>| {
        if let Some(name) = name.take()
            && validate_ifname(&name).is_ok()
            && ((name.starts_with("o3ktmp-") && interface_output_is_tap(block))
                || (name.starts_with("o3kbm-") && interface_output_is_bridge(block)))
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

/// Returns a stable live-link identity from `ip -d link show`: the kernel
/// ifindex plus the link-layer address when present. A missing identity is
/// treated as unowned for destructive operations.
fn interface_identity(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let index = first.split_once(':')?.0.trim();
    if !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mac = output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "link/ether")
        .map(|pair| pair[1].to_ascii_lowercase());
    Some(mac.map_or_else(|| index.to_owned(), |mac| format!("{index}:{mac}")))
}
