use super::NetworkError;
use o3k_domain::{Ipv4Prefix, NetworkProtocol, PolicyDirection};
use std::net::Ipv4Addr;
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(super) struct Ipv4Net {
    pub(super) network: Ipv4Addr,
    pub(super) broadcast: Ipv4Addr,
    pub(super) prefix: u8,
}

impl Ipv4Net {
    pub(super) fn parse(value: &str) -> Result<Self, NetworkError> {
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

    pub(super) fn canonical(self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }

    pub(super) fn contains(self, address: Ipv4Addr) -> bool {
        let raw = u32::from(address);
        raw >= u32::from(self.network) && raw <= u32::from(self.broadcast)
    }

    pub(super) fn first_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }

    pub(super) fn last_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast) - 1)
    }
}

pub(crate) fn parse_security_group_prefix(value: &str) -> Result<Ipv4Prefix, NetworkError> {
    let (address, length) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
    let address = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
    let length = length.parse().map_err(|_| NetworkError::InvalidRequest)?;
    Ipv4Prefix::new(address, length).ok_or(NetworkError::InvalidRequest)
}

pub(crate) fn parse_security_group_direction(value: &str) -> Result<PolicyDirection, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "ingress" => Ok(PolicyDirection::Ingress),
        "egress" => Ok(PolicyDirection::Egress),
        _ => Err(NetworkError::InvalidRequest),
    }
}

pub(crate) fn parse_security_group_protocol(value: &str) -> Result<NetworkProtocol, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "any" => Ok(NetworkProtocol::Any),
        "tcp" => Ok(NetworkProtocol::Tcp),
        "udp" => Ok(NetworkProtocol::Udp),
        "icmp" => Ok(NetworkProtocol::Icmp),
        _ => Err(NetworkError::InvalidRequest),
    }
}

pub(super) fn deterministic_port_mac(port_id: Uuid) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(port_id.as_bytes());
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    )
}
