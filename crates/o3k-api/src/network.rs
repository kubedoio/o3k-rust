//! Neutron-compatible network protocol adapter: network/subnet/port
//! handlers, wire models, and error mapping.

use std::{net::Ipv4Addr, sync::Arc};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use o3k_network::{
    NetworkError, NetworkRecord, NetworkService, PortRecord, PublicAddressAllocator,
    PublicAddressBinding, PublicAddressError, SubnetRecord,
};
use uuid::Uuid;

use crate::{AppState, auth::require_auth_context, error::keystone_error};

#[derive(serde::Deserialize)]
pub(crate) struct NetworkRequestBody {
    network: CreateNetworkRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct CreateNetworkRequest {
    name: String,
}
#[derive(serde::Serialize)]
pub(crate) struct NetworkEnvelope {
    network: NetworkResponse,
}
#[derive(serde::Serialize)]
pub(crate) struct NetworkList {
    networks: Vec<NetworkResponse>,
}
#[derive(serde::Serialize)]
pub(crate) struct NetworkResponse {
    id: String,
    name: String,
    project_id: String,
    status: String,
}

pub(crate) fn network_response(value: NetworkRecord) -> NetworkResponse {
    NetworkResponse {
        id: value.id.to_string(),
        name: value.name,
        project_id: value.project_id,
        status: value.status,
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SubnetRequestBody {
    subnet: CreateSubnetRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct CreateSubnetRequest {
    name: String,
    network_id: uuid::Uuid,
    cidr: String,
    gateway_ip: Option<Ipv4Addr>,
    allocation_pools: Option<Vec<AllocationPool>>,
}
#[derive(serde::Deserialize)]
pub(crate) struct AllocationPool {
    start: Ipv4Addr,
    end: Ipv4Addr,
}
#[derive(serde::Serialize)]
pub(crate) struct SubnetEnvelope {
    subnet: SubnetResponse,
}
#[derive(serde::Serialize)]
pub(crate) struct SubnetList {
    subnets: Vec<SubnetResponse>,
}
#[derive(serde::Serialize)]
pub(crate) struct SubnetResponse {
    id: String,
    network_id: String,
    name: String,
    project_id: String,
    cidr: String,
    gateway_ip: Ipv4Addr,
    allocation_pools: Vec<AllocationPoolResponse>,
}
#[derive(serde::Serialize)]
pub(crate) struct AllocationPoolResponse {
    start: Ipv4Addr,
    end: Ipv4Addr,
}

pub(crate) fn subnet_response(value: SubnetRecord) -> SubnetResponse {
    SubnetResponse {
        id: value.id.to_string(),
        network_id: value.network_id.to_string(),
        name: value.name,
        project_id: value.project_id,
        cidr: value.cidr,
        gateway_ip: value.gateway_ip,
        allocation_pools: vec![AllocationPoolResponse {
            start: value.allocation_start,
            end: value.allocation_end,
        }],
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct PortRequestBody {
    port: CreatePortRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct CreatePortRequest {
    name: String,
    network_id: uuid::Uuid,
}
#[derive(serde::Serialize)]
pub(crate) struct PortEnvelope {
    port: PortResponse,
}
#[derive(serde::Serialize)]
pub(crate) struct PortList {
    ports: Vec<PortResponse>,
}
#[derive(serde::Serialize)]
pub(crate) struct PortResponse {
    id: String,
    network_id: String,
    project_id: String,
    name: String,
    mac_address: String,
    fixed_ips: Vec<FixedIpResponse>,
    status: String,
}
#[derive(serde::Serialize)]
pub(crate) struct FixedIpResponse {
    subnet_id: String,
    ip_address: Ipv4Addr,
}

pub(crate) fn port_response(value: PortRecord) -> PortResponse {
    PortResponse {
        id: value.id.to_string(),
        network_id: value.network_id.to_string(),
        project_id: value.project_id,
        name: value.name,
        mac_address: value.mac_address,
        fixed_ips: value
            .subnet_id
            .map(|subnet_id| FixedIpResponse {
                subnet_id: subnet_id.to_string(),
                ip_address: value.fixed_ip,
            })
            .into_iter()
            .collect(),
        status: value.status,
    }
}

pub(crate) fn network_error(error: NetworkError) -> axum::response::Response {
    match error {
        NetworkError::Unauthorized => keystone_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The request has not been authenticated.",
        ),
        NetworkError::NotFound => keystone_error(
            StatusCode::NOT_FOUND,
            "Not Found",
            "network resource was not found",
        ),
        NetworkError::Conflict | NetworkError::PoolExhausted => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "network operation is not allowed",
        ),
        NetworkError::InvalidRequest => keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid network request",
        ),
        NetworkError::QuotaExceeded {
            ref key,
            limit,
            used,
            requested,
        } => {
            let message = format!(
                "Quota exceeded for {key}: limit {limit}, used {used}, requested {requested}"
            );
            keystone_error(StatusCode::CONFLICT, "Conflict", message)
        }
        NetworkError::Store(_) | NetworkError::CorruptMetadata(_) => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "network storage is unavailable",
        ),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct FloatingIpRequestBody {
    floatingip: FloatingIpRequest,
}

#[derive(serde::Deserialize)]
pub(crate) struct FloatingIpRequest {
    #[serde(default)]
    floating_network_id: Option<uuid::Uuid>,
    #[serde(default)]
    port_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize)]
pub(crate) struct FloatingIpEnvelope {
    floatingip: FloatingIpResponse,
}

#[derive(serde::Serialize)]
pub(crate) struct FloatingIpList {
    floatingips: Vec<FloatingIpResponse>,
}

#[derive(serde::Serialize)]
pub(crate) struct FloatingIpResponse {
    id: String,
    project_id: String,
    floating_ip_address: Ipv4Addr,
    port_id: Option<String>,
    status: &'static str,
}

fn floating_ip_response(binding: PublicAddressBinding) -> FloatingIpResponse {
    FloatingIpResponse {
        id: binding.allocation_id.to_string(),
        project_id: binding.project_id,
        floating_ip_address: binding.public_address,
        port_id: binding.endpoint_id.map(|id| id.to_string()),
        // Allocation/association is control-plane state only. Host realization
        // is a separate execution operation, so this projection must not claim
        // ACTIVE before an agent observation exists.
        status: "DOWN",
    }
}

fn public_error(error: PublicAddressError) -> axum::response::Response {
    let (status, title) = match error {
        PublicAddressError::NotFound => (StatusCode::NOT_FOUND, "Not Found"),
        PublicAddressError::NotOwner
        | PublicAddressError::AssociationConflict
        | PublicAddressError::InUse
        | PublicAddressError::Exhausted => (StatusCode::CONFLICT, "Conflict"),
        PublicAddressError::InvalidPool | PublicAddressError::MissingEndpoint => {
            (StatusCode::BAD_REQUEST, "Bad Request")
        }
        PublicAddressError::CorruptState
        | PublicAddressError::Storage(_)
        | PublicAddressError::ForeignProviderState
        | PublicAddressError::ProviderCommandFailed => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    };
    keystone_error(status, title, "floating IP operation failed")
}

#[allow(clippy::result_large_err)]
fn public_allocator(
    state: &AppState,
) -> Result<&Arc<PublicAddressAllocator>, axum::response::Response> {
    state.public_allocator.as_ref().ok_or_else(|| {
        keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "floating IP service is not configured",
        )
    })
}

pub(crate) async fn list_floating_ips(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let allocator = match public_allocator(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match allocator.list(auth.effective_scope().id().as_str()) {
        Ok(values) => Json(FloatingIpList {
            floatingips: values.into_iter().map(floating_ip_response).collect(),
        })
        .into_response(),
        Err(error) => public_error(error),
    }
}

pub(crate) async fn create_floating_ip(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<FloatingIpRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let allocator = match public_allocator(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid floating IP request",
        );
    };
    let _ = body.floatingip.floating_network_id;
    let operation_id = headers
        .get("x-openstack-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let project_id = auth.effective_scope().id().as_str();
    let mut binding = match allocator.allocate(project_id, &operation_id) {
        Ok(value) => value,
        Err(error) => return public_error(error),
    };
    if let Some(port_id) = body.floatingip.port_id {
        let service = match network_service(&state) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if service
            .get_port_for_project(project_id, port_id)
            .await
            .is_err()
        {
            return public_error(PublicAddressError::MissingEndpoint);
        }
        binding = match allocator.associate(project_id, binding.allocation_id, port_id) {
            Ok(value) => value,
            Err(error) => return public_error(error),
        };
    }
    (
        StatusCode::CREATED,
        Json(FloatingIpEnvelope {
            floatingip: floating_ip_response(binding),
        }),
    )
        .into_response()
}

pub(crate) async fn show_floating_ip(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let allocator = match public_allocator(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match allocator.get(auth.effective_scope().id().as_str(), id) {
        Ok(value) => Json(FloatingIpEnvelope {
            floatingip: floating_ip_response(value),
        })
        .into_response(),
        Err(error) => public_error(error),
    }
}

pub(crate) async fn update_floating_ip(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    request: Result<Json<FloatingIpRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let allocator = match public_allocator(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid floating IP request",
        );
    };
    let project_id = auth.effective_scope().id().as_str();
    let result = match body.floatingip.port_id {
        Some(port_id) => {
            let service = match network_service(&state) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if service
                .get_port_for_project(project_id, port_id)
                .await
                .is_err()
            {
                return public_error(PublicAddressError::MissingEndpoint);
            }
            allocator.associate(project_id, id, port_id)
        }
        None => allocator.disassociate(project_id, id),
    };
    match result {
        Ok(value) => Json(FloatingIpEnvelope {
            floatingip: floating_ip_response(value),
        })
        .into_response(),
        Err(error) => public_error(error),
    }
}

pub(crate) async fn delete_floating_ip(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let allocator = match public_allocator(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match allocator.release(auth.effective_scope().id().as_str(), id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => public_error(error),
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn network_service(
    state: &AppState,
) -> Result<&Arc<NetworkService>, axum::response::Response> {
    state.network.as_ref().ok_or_else(|| {
        keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "network service is not configured",
        )
    })
}

pub(crate) async fn list_extensions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(response) = require_auth_context(&state, &headers) {
        return response;
    }
    if let Err(response) = network_service(&state) {
        return response;
    }
    Json(serde_json::json!({"extensions": []})).into_response()
}

pub(crate) async fn create_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<NetworkRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid network request",
        );
    };
    match service.create_network(&auth, body.network.name).await {
        Ok(value) => (
            StatusCode::CREATED,
            Json(NetworkEnvelope {
                network: network_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn list_networks(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_networks(&auth).await {
        Ok(values) => Json(NetworkList {
            networks: values.into_iter().map(network_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn show_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_network(&auth, id).await {
        Ok(value) => Json(NetworkEnvelope {
            network: network_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn delete_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_network(&auth, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn create_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<SubnetRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid subnet request",
        );
    };
    if body
        .subnet
        .allocation_pools
        .as_ref()
        .is_some_and(|values| values.len() > 1)
    {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "multiple allocation pools are not supported by this profile",
        );
    }
    let pool = body
        .subnet
        .allocation_pools
        .as_ref()
        .and_then(|values| values.first());
    match service
        .create_subnet(
            &auth,
            body.subnet.network_id,
            body.subnet.name,
            body.subnet.cidr,
            body.subnet.gateway_ip,
            pool.map(|v| v.start),
            pool.map(|v| v.end),
        )
        .await
    {
        Ok(value) => (
            StatusCode::CREATED,
            Json(SubnetEnvelope {
                subnet: subnet_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn list_subnets(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_subnets(&auth).await {
        Ok(values) => Json(SubnetList {
            subnets: values.into_iter().map(subnet_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn show_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_subnet(&auth, id).await {
        Ok(value) => Json(SubnetEnvelope {
            subnet: subnet_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn delete_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_subnet(&auth, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn create_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<PortRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid port request",
        );
    };
    match service
        .create_port(&auth, body.port.network_id, body.port.name)
        .await
    {
        Ok(value) => (
            StatusCode::CREATED,
            Json(PortEnvelope {
                port: port_response(value),
            }),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn list_ports(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_ports(&auth).await {
        Ok(values) => Json(PortList {
            ports: values.into_iter().map(port_response).collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn show_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_port(&auth, id).await {
        Ok(value) => Json(PortEnvelope {
            port: port_response(value),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn delete_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_port(&auth, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}
