use std::{
    fs, io,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
        let data = if path.exists() {
            serde_json::from_slice(&fs::read(path).map_err(NetworkError::Storage)?)
                .map_err(NetworkError::CorruptMetadata)?
        } else {
            Persisted::default()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner { root, data })),
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
                let port = PortRecord {
                    id: Uuid::now_v7(),
                    network_id,
                    project_id: project_id.to_owned(),
                    name,
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
    let temporary = inner
        .root
        .join(format!("metadata.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(&inner.data).map_err(|_| NetworkError::Conflict)?;
    fs::write(&temporary, bytes).map_err(NetworkError::Storage)?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(temporary);
        return Err(NetworkError::Storage(error));
    }
    Ok(())
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
        assert_eq!(first.fixed_ip, subnet.allocation_start);
        let reopened = NetworkService::open(&path)?;
        assert_eq!(reopened.get_port("project-a", first.id)?, first);
        fs::remove_dir_all(path)?;
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
