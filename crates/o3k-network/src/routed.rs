//! Provider-independent routed-egress contract.
//!
//! Linux realization lives under [`crate::linux_fabric`].

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedExternalConfig {
    pub external_realm_id: Uuid,
    pub uplink: String,
    pub bridge: String,
}

#[derive(Debug, thiserror::Error)]
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
    Storage(#[from] std::io::Error),
    #[error("routed provider state is corrupt")]
    CorruptState,
    #[error("pre-existing nftables state is not O3K-owned")]
    ForeignFirewallState,
    #[error("owned routed state does not match the requested plan")]
    OwnershipConflict,
}

pub use crate::linux_fabric::routed::LinuxRoutedProvider;
