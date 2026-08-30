use std::{path::PathBuf, sync::Arc};

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
mod canonical;
mod compatibility;
mod helpers;
mod legacy_import;
mod port;
mod subnet;

pub use canonical::{
    CanonicalNetworkSnapshot, GatewayIntentMap, RealmCleanupObservation, RealmCleanupProgress,
    compile_l3_gateway_intents,
};
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
