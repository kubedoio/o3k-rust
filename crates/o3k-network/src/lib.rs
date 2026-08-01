use std::{
    collections::HashSet,
    fs, io,
    net::Ipv4Addr,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetworkConfig {
    pub bridge_name: String,
    pub uplink: Option<String>,
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
}

impl HostNetworkManager {
    pub fn new(config: HostNetworkConfig) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            command: Arc::new(SystemNetworkCommand),
        })
    }

    #[cfg(test)]
    fn with_command(
        config: HostNetworkConfig,
        command: Arc<dyn NetworkCommand>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self { config, command })
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

    fn ensure_bridge_with_ownership(&self) -> Result<bool, HostNetworkError> {
        if self.link_exists(&self.config.bridge_name) {
            let output =
                self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
            if !output.success || !interface_output_is_bridge(&output.stdout) {
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
        Ok(true)
    }

    pub fn create_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        validate_reference(&spec.instance_id)?;
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
            return Ok(name);
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
        Ok(name)
    }
    /// Deletes a TAP only after proving its expected MAC and bridge ownership.
    pub fn delete_tap(&self, spec: &TapSpec) -> Result<(), HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_mac(&spec.mac)?;
        let name = Self::tap_name(&spec.port_id)?;
        if self.link_exists(&name) {
            if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)?
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.run_ip(["link", "del", "dev", &name])?;
        }
        Ok(())
    }
    pub fn discover_managed(&self) -> Result<Vec<String>, HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        Ok(managed_tap_names(&output.stdout, &self.config.bridge_name))
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
            original
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
        if let Some(name) = name.take() {
            if name.starts_with("o3ktap-")
                && interface_output_is_tap(block)
                && interface_is_attached_to(block, bridge_name)
            {
                names.push(name);
            }
        }
        block.clear();
    };
    for line in output.lines() {
        if let Some((_, rest)) = line.split_once(": ") {
            if line
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
                && rest.split(':').next().is_some_and(|name| !name.is_empty())
            {
                finish(&mut current_name, &mut current_output, &mut names);
                current_name = rest.split(':').next().map(str::to_owned);
            }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkRecord {
    pub id: Uuid,
    pub name: String,
    pub project_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubnetRecord {
    pub id: Uuid,
    pub network_id: Uuid,
    pub name: String,
    pub project_id: String,
    pub cidr: String,
    pub gateway_ip: Ipv4Addr,
    pub allocation_start: Ipv4Addr,
    pub allocation_end: Ipv4Addr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortRecord {
    pub id: Uuid,
    pub network_id: Uuid,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub mac_address: String,
    pub fixed_ip: Ipv4Addr,
    pub status: String,
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
    #[error("network storage error")]
    Storage(#[source] io::Error),
    #[error("network metadata is corrupt")]
    CorruptMetadata(#[source] serde_json::Error),
}

#[derive(Clone)]
pub struct NetworkService {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Serialize, Deserialize, Default)]
struct Persisted {
    networks: Vec<NetworkRecord>,
    subnets: Vec<SubnetRecord>,
    ports: Vec<PortRecord>,
}

struct Inner {
    root: PathBuf,
    data: Persisted,
}

impl NetworkService {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, NetworkError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(NetworkError::Storage)?;
        let path = root.join("metadata.json");
        let mut data = if path.exists() {
            serde_json::from_slice(&fs::read(path).map_err(NetworkError::Storage)?)
                .map_err(NetworkError::CorruptMetadata)?
        } else {
            Persisted::default()
        };
        let mut migrated = false;
        for port in &mut data.ports {
            if port.mac_address.is_empty() {
                port.mac_address = deterministic_port_mac(port.id);
                migrated = true;
            }
        }
        let mut macs = HashSet::new();
        if data
            .ports
            .iter()
            .any(|port| !macs.insert(port.mac_address.to_ascii_lowercase()))
        {
            return Err(NetworkError::Conflict);
        }
        let inner = Inner { root, data };
        if migrated {
            persist(&inner)?;
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    pub fn create_network(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let mut inner = self.lock()?;
        if inner
            .data
            .networks
            .iter()
            .any(|network| network.project_id == project_id && network.name == name)
        {
            return Err(NetworkError::Conflict);
        }
        let network = NetworkRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "ACTIVE".to_owned(),
        };
        inner.data.networks.push(network.clone());
        persist(&inner)?;
        Ok(network)
    }

    pub fn list_networks(&self, project_id: &str) -> Result<Vec<NetworkRecord>, NetworkError> {
        let inner = self.lock()?;
        Ok(inner
            .data
            .networks
            .iter()
            .filter(|item| item.project_id == project_id)
            .cloned()
            .collect())
    }

    pub fn get_network(&self, project_id: &str, id: Uuid) -> Result<NetworkRecord, NetworkError> {
        let inner = self.lock()?;
        inner
            .data
            .networks
            .iter()
            .find(|item| item.id == id && item.project_id == project_id)
            .cloned()
            .ok_or(NetworkError::NotFound)
    }

    pub fn delete_network(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let mut inner = self.lock()?;
        let position = inner
            .data
            .networks
            .iter()
            .position(|item| item.id == id && item.project_id == project_id)
            .ok_or(NetworkError::NotFound)?;
        if inner.data.subnets.iter().any(|item| item.network_id == id)
            || inner.data.ports.iter().any(|item| item.network_id == id)
        {
            return Err(NetworkError::Conflict);
        }
        inner.data.networks.remove(position);
        persist(&inner)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_subnet(
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
            || start == gateway
            || end == gateway
        {
            return Err(NetworkError::InvalidRequest);
        }
        let mut inner = self.lock()?;
        if !inner
            .data
            .networks
            .iter()
            .any(|item| item.id == network_id && item.project_id == project_id)
        {
            return Err(NetworkError::NotFound);
        }
        if inner
            .data
            .subnets
            .iter()
            .any(|item| item.network_id == network_id && item.cidr == cidr)
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
        inner.data.subnets.push(subnet.clone());
        persist(&inner)?;
        Ok(subnet)
    }

    pub fn list_subnets(&self, project_id: &str) -> Result<Vec<SubnetRecord>, NetworkError> {
        let inner = self.lock()?;
        Ok(inner
            .data
            .subnets
            .iter()
            .filter(|item| item.project_id == project_id)
            .cloned()
            .collect())
    }

    pub fn get_subnet(&self, project_id: &str, id: Uuid) -> Result<SubnetRecord, NetworkError> {
        let inner = self.lock()?;
        inner
            .data
            .subnets
            .iter()
            .find(|item| item.id == id && item.project_id == project_id)
            .cloned()
            .ok_or(NetworkError::NotFound)
    }

    pub fn delete_subnet(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let mut inner = self.lock()?;
        let position = inner
            .data
            .subnets
            .iter()
            .position(|item| item.id == id && item.project_id == project_id)
            .ok_or(NetworkError::NotFound)?;
        if inner
            .data
            .ports
            .iter()
            .any(|item| item.network_id == inner.data.subnets[position].network_id)
        {
            return Err(NetworkError::Conflict);
        }
        inner.data.subnets.remove(position);
        persist(&inner)
    }

    pub fn create_port(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        let mut inner = self.lock()?;
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        if !inner
            .data
            .networks
            .iter()
            .any(|item| item.id == network_id && item.project_id == project_id)
        {
            return Err(NetworkError::NotFound);
        }
        let subnet = inner
            .data
            .subnets
            .iter()
            .find(|item| item.network_id == network_id && item.project_id == project_id)
            .cloned()
            .ok_or(NetworkError::NotFound)?;
        let used: std::collections::HashSet<Ipv4Addr> = inner
            .data
            .ports
            .iter()
            .filter(|item| item.network_id == network_id)
            .map(|item| item.fixed_ip)
            .collect();
        let mut candidate = u32::from(subnet.allocation_start);
        let end = u32::from(subnet.allocation_end);
        let gateway = subnet.gateway_ip;
        while candidate <= end {
            let address = Ipv4Addr::from(candidate);
            if address != gateway && !used.contains(&address) {
                let id = Uuid::now_v7();
                let mac_address = deterministic_port_mac(id);
                if inner
                    .data
                    .ports
                    .iter()
                    .any(|port| port.mac_address.eq_ignore_ascii_case(&mac_address))
                {
                    return Err(NetworkError::Conflict);
                }
                let port = PortRecord {
                    id,
                    network_id,
                    project_id: project_id.to_owned(),
                    name,
                    mac_address,
                    fixed_ip: address,
                    status: "ACTIVE".to_owned(),
                };
                inner.data.ports.push(port.clone());
                persist(&inner)?;
                return Ok(port);
            }
            candidate = candidate.saturating_add(1);
        }
        Err(NetworkError::PoolExhausted)
    }

    pub fn list_ports(&self, project_id: &str) -> Result<Vec<PortRecord>, NetworkError> {
        let inner = self.lock()?;
        Ok(inner
            .data
            .ports
            .iter()
            .filter(|item| item.project_id == project_id)
            .cloned()
            .collect())
    }

    pub fn get_port(&self, project_id: &str, id: Uuid) -> Result<PortRecord, NetworkError> {
        let inner = self.lock()?;
        inner
            .data
            .ports
            .iter()
            .find(|item| item.id == id && item.project_id == project_id)
            .cloned()
            .ok_or(NetworkError::NotFound)
    }

    pub fn delete_port(&self, project_id: &str, id: Uuid) -> Result<(), NetworkError> {
        let mut inner = self.lock()?;
        let position = inner
            .data
            .ports
            .iter()
            .position(|item| item.id == id && item.project_id == project_id)
            .ok_or(NetworkError::NotFound)?;
        inner.data.ports.remove(position);
        persist(&inner)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, NetworkError> {
        self.inner.lock().map_err(|_| NetworkError::Conflict)
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

fn persist(inner: &Inner) -> Result<(), NetworkError> {
    let path = inner.root.join("metadata.json");
    let temporary = inner.root.join(format!("metadata.tmp-{}", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(&inner.data).map_err(|_| NetworkError::Conflict)?;
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(NetworkError::Storage(error));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(temporary);
        return Err(NetworkError::Storage(error));
    }
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

    #[test]
    fn allocation_is_deterministic_collision_safe_and_restartable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("allocation");
        let _ = fs::remove_dir_all(&path);
        let service = NetworkService::open(&path)?;
        let network = service.create_network("project-a", "flat".to_owned())?;
        let subnet = service.create_subnet(
            "project-a",
            network.id,
            "lab".to_owned(),
            "192.0.2.0/29".to_owned(),
            None,
            None,
            None,
        )?;
        let first = service.create_port("project-a", network.id, "one".to_owned())?;
        let second = service.create_port("project-a", network.id, "two".to_owned())?;
        assert_ne!(first.fixed_ip, second.fixed_ip);
        assert_ne!(first.mac_address, second.mac_address);
        assert_eq!(first.mac_address, deterministic_port_mac(first.id));
        assert_eq!(first.fixed_ip, subnet.allocation_start);
        let reopened = NetworkService::open(&path)?;
        assert_eq!(reopened.get_port("project-a", first.id)?, first);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("metadata.tmp-")
        }));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn opening_legacy_ports_migrates_the_deterministic_mac() -> Result<(), NetworkError> {
        let path = root("port-mac-migration");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).map_err(NetworkError::Storage)?;
        let port_id = Uuid::now_v7();
        let network_id = Uuid::now_v7();
        let legacy = serde_json::json!({
            "networks": [],
            "subnets": [],
            "ports": [{
                "id": port_id,
                "network_id": network_id,
                "project_id": "project-a",
                "name": "legacy",
                "fixed_ip": "192.0.2.2",
                "status": "ACTIVE"
            }]
        });
        fs::write(
            path.join("metadata.json"),
            serde_json::to_vec(&legacy).map_err(|_| NetworkError::Conflict)?,
        )
        .map_err(NetworkError::Storage)?;

        let service = NetworkService::open(&path)?;
        let port = service.get_port("project-a", port_id)?;
        assert_eq!(port.mac_address, deterministic_port_mac(port_id));
        let persisted =
            fs::read_to_string(path.join("metadata.json")).map_err(NetworkError::Storage)?;
        assert!(persisted.contains(&port.mac_address));
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    #[test]
    fn invalid_cidr_exhaustion_and_project_isolation_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("validation");
        let _ = fs::remove_dir_all(&path);
        let service = NetworkService::open(&path)?;
        let network = service.create_network("project-a", "flat".to_owned())?;
        assert!(matches!(
            service.create_subnet(
                "project-a",
                network.id,
                "bad".to_owned(),
                "192.0.2.1/31".to_owned(),
                None,
                None,
                None
            ),
            Err(NetworkError::InvalidRequest)
        ));
        let _ = service.create_subnet(
            "project-a",
            network.id,
            "tiny".to_owned(),
            "192.0.2.0/30".to_owned(),
            None,
            Some(Ipv4Addr::new(192, 0, 2, 2)),
            Some(Ipv4Addr::new(192, 0, 2, 2)),
        )?;
        let _ = service.create_port("project-a", network.id, "one".to_owned())?;
        assert!(matches!(
            service.create_port("project-a", network.id, "two".to_owned()),
            Err(NetworkError::PoolExhausted)
        ));
        assert!(matches!(
            service.get_network("project-b", network.id),
            Err(NetworkError::NotFound)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }
}
