use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, net::Ipv4Addr};
use uuid::Uuid;

/// A canonical IPv4 prefix. The stored address is always the network address;
/// host bits are rejected so every serialized value has one representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ipv4Prefix {
    pub network: Ipv4Addr,
    pub prefix_len: u8,
}

impl Ipv4Prefix {
    pub fn new(address: Ipv4Addr, prefix_len: u8) -> Option<Self> {
        if prefix_len > 32 {
            return None;
        }
        let mask = if prefix_len == 0 {
            0
        } else {
            u32::MAX << (32 - prefix_len)
        };
        let value = u32::from(address) & mask;
        (value == u32::from(address)).then_some(Self {
            network: Ipv4Addr::from(value),
            prefix_len,
        })
    }

    #[must_use]
    pub fn contains(self, address: Ipv4Addr) -> bool {
        let mask = if self.prefix_len == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix_len)
        };
        u32::from(address) & mask == u32::from(self.network)
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.contains(other.network) || other.contains(self.network)
    }
}

impl Ord for Ipv4Prefix {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.network, self.prefix_len).cmp(&(other.network, other.prefix_len))
    }
}

impl PartialOrd for Ipv4Prefix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A project-owned address-isolation namespace. P9's routed profile sets
/// `overlapping_prefixes` to false; the field keeps future VRF/overlay realms
/// explicit without making non-overlap a global O3K invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRealm {
    pub id: Uuid,
    pub project_id: String,
    pub prefix: Ipv4Prefix,
    pub overlapping_prefixes: bool,
}

/// A bounded, project-owned allocation range within an address realm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressPool {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub project_id: String,
    pub prefix: Ipv4Prefix,
    pub gateway: Option<Ipv4Addr>,
    pub first_usable: Ipv4Addr,
    pub last_usable: Ipv4Addr,
}

/// Canonical desired network state owned by the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkIntent {
    pub id: Uuid,
    pub project_id: String,
    pub realm: AddressRealm,
    pub address_pools: Vec<AddressPool>,
    pub endpoints: Vec<EndpointIntent>,
    pub routes: Vec<RouteIntent>,
    pub gateways: Vec<GatewayIntent>,
    pub egress: Vec<EgressIntent>,
    pub public_addresses: Vec<PublicAddressBindingIntent>,
    pub policies: Vec<PolicyIntent>,
    pub generation: u64,
    pub state: NetworkIntentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkIntentState {
    Requested,
    Active,
    Deleting,
    Error,
}

impl NetworkIntentState {
    pub fn transition(self, next: Self) -> Result<Self, &'static str> {
        let valid = matches!(
            (self, next),
            (Self::Requested, Self::Active)
                | (Self::Requested, Self::Deleting)
                | (Self::Requested, Self::Error)
                | (Self::Active, Self::Deleting)
                | (Self::Active, Self::Error)
                | (Self::Deleting, Self::Error)
        );
        valid
            .then_some(next)
            .ok_or("invalid network intent transition")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointIntent {
    pub id: Uuid,
    pub project_id: String,
    pub mac: String,
    pub fixed_ip: Ipv4Addr,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteIntent {
    pub destination: Ipv4Prefix,
    pub next_hop: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayIntent {
    pub destination: Ipv4Prefix,
    pub gateway: Ipv4Addr,
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressIntent {
    pub external_realm_id: Uuid,
    pub enabled: bool,
    pub nat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicAddressBindingIntent {
    pub id: Uuid,
    pub project_id: String,
    pub public_address: Ipv4Addr,
    pub endpoint_id: Uuid,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDirection {
    Ingress,
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkProtocol {
    Any,
    Tcp,
    Udp,
    Icmp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIntent {
    pub endpoint_id: Uuid,
    pub direction: PolicyDirection,
    pub protocol: NetworkProtocol,
    pub ports: Option<PortRange>,
    pub source: Option<Ipv4Prefix>,
    pub destination: Option<Ipv4Prefix>,
    pub action: PolicyAction,
}

/// Bounded capability vocabulary used before a plan can be dispatched to an
/// execution provider. These are semantic facts, not provider configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NetworkCapability {
    EndpointAttachment,
    Ipv4,
    Ipv6,
    L2AdjacencyScope,
    Routing,
    StatefulPolicy,
    Nat,
    PublicAddressRealization,
    OverlappingAddressRealms,
    EncapsulationModes,
    QosFeatures,
    RouteAdvertisementModes,
}

/// Provider-independent intent carried by a node plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPlanIntent {
    EndpointAttachment {
        endpoint_id: Uuid,
        mac: String,
        fixed_ip: Ipv4Addr,
        generation: u64,
    },
    AddressAssignment {
        endpoint_id: Uuid,
        address: Ipv4Addr,
        generation: u64,
    },
    Route(RouteIntent),
    Gateway(GatewayIntent),
    Egress(EgressIntent),
    PublicAddressBinding(PublicAddressBindingIntent),
    Policy(PolicyIntent),
}
