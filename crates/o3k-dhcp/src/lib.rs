//! Durable, isolated dnsmasq configuration for O3K-managed flat networks.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Child, Command},
};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DhcpConfig {
    pub subnet: String,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub interface: String,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub port_id: String,
    pub mac: String,
    pub address: Ipv4Addr,
}

#[derive(Debug, Error)]
pub enum DhcpError {
    #[error("DHCP configuration is invalid")]
    InvalidConfig,
    #[error("DHCP binding conflicts with an existing address or MAC")]
    Conflict,
    #[error("DHCP storage failed")]
    Storage(#[source] io::Error),
    #[error("DHCP state is corrupt")]
    CorruptState(#[source] serde_json::Error),
    #[error("dnsmasq command failed")]
    CommandFailed,
}

/// Owns one managed dnsmasq process and provides restart/cleanup semantics.
///
/// The supervisor intentionally owns the child handle rather than relying on a
/// pid file alone. A pid file is an artifact produced by dnsmasq, not proof
/// that the process is still the one started by this service.
pub struct DnsmasqSupervisor {
    binary: PathBuf,
    config: PathBuf,
    pid_file: PathBuf,
    child: Child,
}

impl DnsmasqSupervisor {
    fn spawn(root: &Path, binary: &Path) -> Result<Self, DhcpError> {
        let config = root.join("dnsmasq.conf");
        let pid_file = root.join(format!("dnsmasq-{}.pid", uuid::Uuid::now_v7()));
        let mut child = Command::new(binary)
            // dnsmasq 2.90 rejects the space-separated form for these optional
            // long options ("junk found in command line"); use `--opt=value`.
            .arg(format!("--conf-file={}", config.display()))
            .arg(format!("--pid-file={}", pid_file.display()))
            .arg("--keep-in-foreground")
            .spawn()
            .map_err(|_| DhcpError::CommandFailed)?;
        if child
            .try_wait()
            .map_err(|_| DhcpError::CommandFailed)?
            .is_some()
        {
            let _ = fs::remove_file(&pid_file);
            return Err(DhcpError::CommandFailed);
        }
        Ok(Self {
            binary: binary.to_path_buf(),
            config,
            pid_file,
            child,
        })
    }

    /// Returns whether the owned process is still running.
    pub fn is_running(&mut self) -> Result<bool, DhcpError> {
        Ok(self
            .child
            .try_wait()
            .map_err(|_| DhcpError::CommandFailed)?
            .is_none())
    }

    /// Restart the owned process after the caller has published new config.
    pub fn restart(&mut self) -> Result<(), DhcpError> {
        self.stop()?;
        let mut child = Command::new(&self.binary)
            .arg(format!("--conf-file={}", self.config.display()))
            .arg(format!("--pid-file={}", self.pid_file.display()))
            .arg("--keep-in-foreground")
            .spawn()
            .map_err(|_| DhcpError::CommandFailed)?;
        if child
            .try_wait()
            .map_err(|_| DhcpError::CommandFailed)?
            .is_some()
        {
            let _ = fs::remove_file(&self.pid_file);
            return Err(DhcpError::CommandFailed);
        }
        self.child = child;
        Ok(())
    }

    /// Stop the owned process and remove only its managed pid file.
    pub fn stop(&mut self) -> Result<(), DhcpError> {
        if self
            .child
            .try_wait()
            .map_err(|_| DhcpError::CommandFailed)?
            .is_none()
        {
            self.child.kill().map_err(|_| DhcpError::CommandFailed)?;
        }
        self.child.wait().map_err(|_| DhcpError::CommandFailed)?;
        match fs::remove_file(&self.pid_file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(DhcpError::CommandFailed),
        }
    }
}

impl Drop for DnsmasqSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct State {
    config: Option<DhcpConfig>,
    bindings: BTreeMap<String, Binding>,
}

pub struct DhcpService {
    root: PathBuf,
    state: State,
}

impl DhcpService {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, DhcpError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(DhcpError::Storage)?;
        let path = root.join("state.json");
        let state = if path.exists() {
            serde_json::from_slice(&fs::read(path).map_err(DhcpError::Storage)?)
                .map_err(DhcpError::CorruptState)?
        } else {
            State::default()
        };
        Ok(Self { root, state })
    }

    pub fn configure(&mut self, config: DhcpConfig) -> Result<(), DhcpError> {
        validate_config(&config)?;
        for binding in self.state.bindings.values() {
            if !valid_host(&config.subnet, binding.address) || binding.address == config.gateway {
                return Err(DhcpError::InvalidConfig);
            }
        }
        self.state.config = Some(config);
        self.persist()
    }

    pub fn upsert_binding(&mut self, binding: Binding) -> Result<(), DhcpError> {
        let config = self.state.config.as_ref().ok_or(DhcpError::InvalidConfig)?;
        if binding.port_id.is_empty()
            || !valid_mac(&binding.mac)
            || !valid_host(&config.subnet, binding.address)
            || binding.address == config.gateway
        {
            return Err(DhcpError::InvalidConfig);
        }
        if self.state.bindings.values().any(|existing| {
            existing.port_id != binding.port_id
                && (existing.address == binding.address || existing.mac == binding.mac)
        }) {
            return Err(DhcpError::Conflict);
        }
        self.state.bindings.insert(binding.port_id.clone(), binding);
        self.persist()
    }

    pub fn remove_binding(&mut self, port_id: &str) -> Result<(), DhcpError> {
        self.state.bindings.remove(port_id);
        self.persist()
    }

    pub fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.state.bindings.values()
    }

    /// Returns the durable binding for one port, if present.
    pub fn binding(&self, port_id: &str) -> Option<&Binding> {
        self.state.bindings.get(port_id)
    }

    /// Returns the persisted network configuration for restart reconciliation.
    pub fn configuration(&self) -> Option<&DhcpConfig> {
        self.state.config.as_ref()
    }

    pub fn render_config(&self) -> Result<String, DhcpError> {
        let config = self.state.config.as_ref().ok_or(DhcpError::InvalidConfig)?;
        validate_config(config)?;
        let (network, broadcast) = subnet_bounds(&config.subnet).ok_or(DhcpError::InvalidConfig)?;
        let dhcp_start = Ipv4Addr::from(u32::from(network) + 1);
        let dhcp_end = Ipv4Addr::from(u32::from(broadcast) - 1);
        let mut lines = vec![
            "# Managed by o3k-dhcp; do not edit.".to_owned(),
            format!("interface={}", config.interface),
            "bind-interfaces".to_owned(),
            format!(
                "dhcp-leasefile={}",
                self.root.join("dnsmasq.leases").display()
            ),
            format!(
                "dhcp-range={},{},static,{}",
                dhcp_start, dhcp_end, config.lease_seconds
            ),
            format!("dhcp-option=3,{}", config.gateway),
        ];
        if !config.dns.is_empty() {
            lines.push(format!(
                "dhcp-option=6,{}",
                config
                    .dns
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        lines.extend(
            self.state
                .bindings
                .values()
                .map(|binding| format!("dhcp-host={},{}", binding.mac, binding.address)),
        );
        Ok(lines.join("\n") + "\n")
    }

    pub fn write_config(&self) -> Result<PathBuf, DhcpError> {
        let content = self.render_config()?;
        let path = self.root.join("dnsmasq.conf");
        atomic_write(&path, content.as_bytes())?;
        Ok(path)
    }

    pub fn start(&self, binary: &Path) -> Result<DnsmasqSupervisor, DhcpError> {
        self.write_config()?;
        DnsmasqSupervisor::spawn(&self.root, binary)
    }

    /// Publish the current state and restart the owned process.
    pub fn reload(&self, supervisor: &mut DnsmasqSupervisor) -> Result<(), DhcpError> {
        self.write_config()?;
        supervisor.restart()
    }

    pub fn managed_config_path(&self) -> PathBuf {
        self.root.join("dnsmasq.conf")
    }

    pub fn managed_lease_path(&self) -> PathBuf {
        self.root.join("dnsmasq.leases")
    }

    fn persist(&self) -> Result<(), DhcpError> {
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|_| DhcpError::InvalidConfig)?;
        atomic_write(&self.root.join("state.json"), &bytes)
    }
}

fn validate_config(config: &DhcpConfig) -> Result<(), DhcpError> {
    if config.interface.is_empty()
        || config.interface.len() > 15
        || !config
            .interface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || config.lease_seconds == 0
        || !valid_host(&config.subnet, config.gateway)
        || config
            .dns
            .iter()
            .any(|address| !valid_host(&config.subnet, *address))
    {
        return Err(DhcpError::InvalidConfig);
    }
    Ok(())
}

fn valid_host(cidr: &str, address: Ipv4Addr) -> bool {
    let Some((network, broadcast)) = subnet_bounds(cidr) else {
        return false;
    };
    u32::from(address) > u32::from(network) && u32::from(address) < u32::from(broadcast)
}

fn subnet_bounds(cidr: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address = address.parse::<Ipv4Addr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    if prefix > 30 {
        return None;
    }
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(address) & mask;
    Some((Ipv4Addr::from(network), Ipv4Addr::from(network | !mask)))
}

fn valid_mac(mac: &str) -> bool {
    mac.len() == 17
        && mac.split(':').count() == 6
        && mac
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DhcpError> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(DhcpError::Storage(error));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(DhcpError::Storage(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Result<DhcpService, DhcpError> {
        DhcpService::open(std::env::temp_dir().join("o3k-dhcp-tests"))
    }
    fn config() -> Result<DhcpConfig, DhcpError> {
        Ok(DhcpConfig {
            subnet: "192.0.2.0/24".into(),
            gateway: "192.0.2.1".parse().map_err(|_| DhcpError::InvalidConfig)?,
            dns: vec!["192.0.2.1".parse().map_err(|_| DhcpError::InvalidConfig)?],
            interface: "o3k-br0".into(),
            lease_seconds: 3600,
        })
    }
    #[test]
    fn renders_fixed_bindings_and_rejects_collisions() -> Result<(), DhcpError> {
        let mut service = service()?;
        service.configure(config()?)?;
        service.upsert_binding(Binding {
            port_id: "p1".into(),
            mac: "02:00:00:00:00:01".into(),
            address: "192.0.2.10".parse().map_err(|_| DhcpError::InvalidConfig)?,
        })?;
        assert!(matches!(
            service.upsert_binding(Binding {
                port_id: "p2".into(),
                mac: "02:00:00:00:00:02".into(),
                address: "192.0.2.10".parse().map_err(|_| DhcpError::InvalidConfig)?
            }),
            Err(DhcpError::Conflict)
        ));
        let rendered = service.render_config()?;
        assert!(rendered.contains("dhcp-host=02:00:00:00:00:01,192.0.2.10"));
        assert!(rendered.contains("dhcp-range=192.0.2.1,192.0.2.254,static,3600"));
        assert!(rendered.contains("dhcp-leasefile="));
        assert!(
            service
                .managed_lease_path()
                .ends_with("o3k-dhcp-tests/dnsmasq.leases")
        );
        Ok(())
    }

    #[test]
    fn gateway_reconfiguration_rejects_existing_binding() -> Result<(), DhcpError> {
        let root = std::env::temp_dir().join(format!("o3k-dhcp-gateway-{}", uuid::Uuid::now_v7()));
        let mut service = DhcpService::open(&root)?;
        service.configure(config()?)?;
        let binding_address = "192.0.2.10".parse().map_err(|_| DhcpError::InvalidConfig)?;
        service.upsert_binding(Binding {
            port_id: "gateway-conflict".into(),
            mac: "02:00:00:00:00:10".into(),
            address: binding_address,
        })?;

        let mut conflicting = config()?;
        conflicting.gateway = binding_address;
        assert!(matches!(
            service.configure(conflicting),
            Err(DhcpError::InvalidConfig)
        ));
        assert!(service.render_config()?.contains("dhcp-option=3,192.0.2.1"));
        fs::remove_dir_all(root).map_err(DhcpError::Storage)?;
        Ok(())
    }
    #[test]
    fn foreign_paths_are_not_used() -> Result<(), DhcpError> {
        let service = service()?;
        assert!(
            service
                .managed_config_path()
                .ends_with("o3k-dhcp-tests/dnsmasq.conf")
        );
        Ok(())
    }

    #[test]
    fn configuration_survives_reopen() -> Result<(), DhcpError> {
        let root = std::env::temp_dir().join(format!("o3k-dhcp-reopen-{}", uuid::Uuid::now_v7()));
        let mut service = DhcpService::open(&root)?;
        let expected = config()?;
        service.configure(expected.clone())?;
        let reopened = DhcpService::open(&root)?;
        assert_eq!(reopened.configuration(), Some(&expected));
        fs::remove_dir_all(root).map_err(DhcpError::Storage)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_is_nonblocking_restartable_and_owned() -> Result<(), DhcpError> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("o3k-dhcp-supervisor-{}", std::process::id()));
        fs::create_dir_all(&root).map_err(DhcpError::Storage)?;
        let binary = root.join("fake-dnsmasq.sh");
        fs::write(&binary, "#!/bin/sh\ntrap 'exit 0' TERM INT HUP\nsleep 30\n")
            .map_err(DhcpError::Storage)?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .map_err(DhcpError::Storage)?;

        let mut service = DhcpService::open(&root)?;
        service.configure(config()?)?;
        let mut supervisor = service.start(&binary)?;
        assert!(supervisor.is_running()?);
        service.upsert_binding(Binding {
            port_id: "p1".into(),
            mac: "02:00:00:00:00:01".into(),
            address: "192.0.2.10".parse().map_err(|_| DhcpError::InvalidConfig)?,
        })?;
        service.reload(&mut supervisor)?;
        assert!(supervisor.is_running()?);
        assert!(
            fs::read_to_string(service.managed_config_path())
                .map_err(DhcpError::Storage)?
                .contains("192.0.2.10")
        );
        supervisor.stop()?;
        assert!(!supervisor.is_running()?);

        let failing_binary = root.join("missing-dnsmasq");
        assert!(matches!(
            service.start(&failing_binary),
            Err(DhcpError::CommandFailed)
        ));

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn spawn_uses_equals_form_for_path_options() -> Result<(), DhcpError> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("o3k-dhcp-argv-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(DhcpError::Storage)?;
        let argv_file = root.join("argv.txt");
        let binary = root.join("fake-dnsmasq-argv.sh");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ntrap 'exit 0' TERM INT HUP\nsleep 30\n",
                argv_file.display()
            ),
        )
        .map_err(DhcpError::Storage)?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .map_err(DhcpError::Storage)?;

        let mut service = DhcpService::open(&root)?;
        service.configure(config()?)?;
        let mut supervisor = service.start(&binary)?;
        // The child records argv then holds; startup is asynchronous, so poll
        // for the recording with a bounded wait before asserting on it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !argv_file.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "fake dnsmasq never recorded its argv within 5s"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let recorded = fs::read_to_string(&argv_file).map_err(DhcpError::Storage)?;
        let args: Vec<&str> = recorded.lines().collect();
        let expected_config = format!("--conf-file={}", service.managed_config_path().display());
        assert!(
            args.contains(&expected_config.as_str()),
            "expected {expected_config} in argv, got {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg.starts_with("--pid-file=")),
            "expected --pid-file=<path> in argv, got {args:?}"
        );
        supervisor.stop()?;

        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
