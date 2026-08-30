use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    path::PathBuf,
    sync::Arc,
};

use o3k_domain::Ipv4Prefix;
use o3k_kernel::{
    ActionId, AuditSink, Authorizer, LimitKey, LimitValue, OwnershipScope, ResourceId,
    ResourceType, ScopeId,
};
use thiserror::Error;
use uuid::Uuid;

use crate::NetworkRecord;

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
    #[error("unauthorized")]
    Unauthorized,
    #[error("network resource not found")]
    NotFound,
    #[error("network resource already exists or is still in use")]
    Conflict,
    #[error("network request is invalid")]
    InvalidRequest,
    #[error("quota exceeded for {key}: limit {limit}, used {used}, requested {requested}")]
    QuotaExceeded {
        key: LimitKey,
        limit: LimitValue,
        used: u64,
        requested: u64,
    },
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
        o3k_store::StoreError::NetworkNotFound | o3k_store::StoreError::ResourceNotFound => {
            NetworkError::NotFound
        }
        o3k_store::StoreError::NetworkInUse => NetworkError::Conflict,
        o3k_store::StoreError::OwnershipConflict => NetworkError::InvalidRequest,
        o3k_store::StoreError::QuotaExceeded {
            key,
            limit,
            used,
            requested,
        } => NetworkError::QuotaExceeded {
            key,
            limit,
            used,
            requested,
        },
        o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
        other => NetworkError::Store(other),
    }
}

fn realm_delete_operation(
    project_id: &str,
    realm_id: Uuid,
) -> Result<
    (
        o3k_store::OperationRecord,
        o3k_store::CanonicalOperationRecord,
        o3k_store::IdempotencyReservationRequest,
    ),
    NetworkError,
> {
    let action =
        ActionId::new("network", "DeleteRealm").map_err(|_| NetworkError::InvalidRequest)?;
    let resource_type =
        ResourceType::new("network", "address_realm").map_err(|_| NetworkError::InvalidRequest)?;
    let operation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:network:realm-delete:{project_id}:{realm_id}").as_bytes(),
    );
    let scope = OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
    let kernel = o3k_kernel::Operation::new(
        operation_id,
        "network",
        action.clone(),
        "o3k:network-service",
        scope,
        resource_type.clone(),
        Some(ResourceId::new_unchecked(realm_id.to_string())),
        None,
    );
    let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(&kernel)
        .map_err(map_store_error)?;
    let operation = o3k_store::OperationRecord {
        id: operation_id,
        resource_id: realm_id,
        kind: "lifecycle:realm-delete".to_owned(),
        state: o3k_store::OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let request = o3k_store::IdempotencyReservationRequest::from_semantics(
        project_id,
        action.to_string(),
        format!("canonical:realm-delete:{realm_id}"),
        &resource_type.to_string(),
        Some(&realm_id.to_string()),
        &serde_json::json!({"realm_id": realm_id}),
        operation_id,
    )
    .map_err(map_store_error)?;
    Ok((operation, canonical, request))
}

fn canonical_network_projection(network: o3k_store::CanonicalNetworkRecord) -> NetworkRecord {
    NetworkRecord {
        id: network.id,
        name: network.name,
        project_id: network.project_id,
        status: network.state.to_ascii_uppercase(),
    }
}

#[derive(Clone)]
pub struct NetworkService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn o3k_store::NetworkRepository>,
}

/// Canonical network reconstruction result.  Compatibility projections and
/// provider plans are derived from this durable graph; they are never used to
/// recover missing canonical children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalNetworkSnapshot {
    pub network: o3k_store::CanonicalNetworkRecord,
    pub realms: Vec<o3k_store::CanonicalAddressRealmRecord>,
    pub pools: BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
    pub endpoints: BTreeMap<Uuid, Vec<o3k_store::CanonicalEndpointRecord>>,
    /// Canonical L3 gateway authority relevant to this network's realms.
    /// Provider plans are derived from this graph; it is not compatibility
    /// state and does not redefine AddressRealm identity.
    pub l3_gateways: Vec<(
        o3k_store::CanonicalL3GatewayRecord,
        Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>,
    )>,
}

/// Compiles the canonical gateway graph into the existing provider-neutral
/// routing intents. AddressRealm remains the unit of address interpretation;
/// this function only derives connectivity from the gateway attachments.
pub type GatewayIntentMap = BTreeMap<
    Uuid,
    (
        Vec<o3k_domain::GatewayIntent>,
        Vec<o3k_domain::EgressIntent>,
    ),
>;

pub fn compile_l3_gateway_intents(
    gateway: &o3k_store::CanonicalL3GatewayRecord,
    attachments: &[o3k_store::CanonicalL3GatewayAttachmentRecord],
    realms: &[o3k_store::CanonicalAddressRealmRecord],
    pools: &BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
) -> Result<GatewayIntentMap, NetworkError> {
    if gateway.state != "active" || gateway.generation == 0 {
        return Err(NetworkError::InvalidRequest);
    }
    let mut realm_map = BTreeMap::new();
    for realm in realms {
        if realm.project_id != gateway.project_id || realm.state != "active" {
            continue;
        }
        let (network, prefix) = realm
            .prefix
            .split_once('/')
            .ok_or(NetworkError::InvalidRequest)?;
        let address = network.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix_len = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix = Ipv4Prefix::new(address, prefix_len).ok_or(NetworkError::InvalidRequest)?;
        realm_map.insert(realm.id, prefix);
    }
    let attached: BTreeSet<Uuid> = attachments
        .iter()
        .filter(|attachment| {
            attachment.project_id == gateway.project_id && attachment.state == "active"
        })
        .map(|attachment| attachment.realm_id)
        .collect();
    let mut result = BTreeMap::new();
    for realm_id in &attached {
        let local = realm_map.get(realm_id).ok_or(NetworkError::NotFound)?;
        let local_gateway = pools
            .get(realm_id)
            .and_then(|items| items.iter().find_map(|pool| pool.gateway))
            .or_else(|| u32::from(local.network).checked_add(1).map(Ipv4Addr::from))
            .ok_or(NetworkError::InvalidRequest)?;
        let mut routes = Vec::new();
        for remote_id in &attached {
            if remote_id != realm_id {
                routes.push(o3k_domain::GatewayIntent {
                    destination: *realm_map.get(remote_id).ok_or(NetworkError::NotFound)?,
                    gateway: local_gateway,
                    external: false,
                });
            }
        }
        let egress = gateway
            .external_realm_id
            .map(|external_realm_id| {
                vec![o3k_domain::EgressIntent {
                    external_realm_id,
                    enabled: true,
                    nat: gateway.enable_snat,
                }]
            })
            .unwrap_or_default();
        result.insert(*realm_id, (routes, egress));
    }
    Ok(result)
}

/// Result of observing one provider-owned Realm cleanup identity.  A Realm
/// remains canonical while the provider outcome is present or unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupObservation {
    Absent(o3k_store::CanonicalRealmBindingRecord),
    Present(o3k_store::CanonicalRealmBindingRecord),
    Unknown {
        binding: o3k_store::CanonicalRealmBindingRecord,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupProgress {
    Deleting { operation_id: Uuid, generation: u64 },
    AwaitingObservation { operation_id: Uuid, generation: u64 },
    Removed { operation_id: Uuid },
}

mod canonical;
mod compatibility;
mod helpers;
mod legacy_import;
mod port;
mod subnet;

pub(crate) use helpers::{
    parse_security_group_direction, parse_security_group_prefix, parse_security_group_protocol,
};

impl NetworkService {
    pub(super) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

#[cfg(test)]
mod tests;
