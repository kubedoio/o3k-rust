//! Durable, isolated dnsmasq configuration for O3K-managed flat networks.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
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
            if !valid_host(&config.subnet, binding.address) {
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

    pub fn render_config(&self) -> Result<String, DhcpError> {
        let config = self.state.config.as_ref().ok_or(DhcpError::InvalidConfig)?;
        validate_config(config)?;
        let mut lines = vec![
            "# Managed by o3k-dhcp; do not edit.".to_owned(),
            format!("interface={}", config.interface),
            "bind-interfaces".to_owned(),
            format!(
                "dhcp-range={},static,{}",
                config.subnet, config.lease_seconds
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

    pub fn start(&self, binary: &Path) -> Result<(), DhcpError> {
        let config = self.write_config()?;
        let status = Command::new(binary)
            .args([
                "--conf-file",
                config.to_str().ok_or(DhcpError::CommandFailed)?,
                "--pid-file",
                self.root
                    .join("dnsmasq.pid")
                    .to_str()
                    .ok_or(DhcpError::CommandFailed)?,
            ])
            .status()
            .map_err(|_| DhcpError::CommandFailed)?;
        if status.success() {
            Ok(())
        } else {
            Err(DhcpError::CommandFailed)
        }
    }

    pub fn managed_config_path(&self) -> PathBuf {
        self.root.join("dnsmasq.conf")
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
}
