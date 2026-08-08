//! Neutron-compatible network protocol adapter: network/subnet/port
//! handlers, wire models, and error mapping.

use std::{net::Ipv4Addr, sync::Arc};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use o3k_network::{NetworkError, NetworkRecord, NetworkService, PortRecord, SubnetRecord};

use crate::{AppState, auth::require_token, error::keystone_error};

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
        NetworkError::Store(_) | NetworkError::CorruptMetadata(_) => keystone_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "network storage is unavailable",
        ),
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
    if let Err(response) = require_token(&state, &headers) {
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
    let token = match require_token(&state, &headers) {
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
    match service
        .create_network(&token.project_id, body.network.name)
        .await
    {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_networks(&token.project_id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_network(&token.project_id, id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_network(&token.project_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn create_subnet(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<SubnetRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
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
            &token.project_id,
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_subnets(&token.project_id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_subnet(&token.project_id, id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_subnet(&token.project_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn create_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<PortRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let token = match require_token(&state, &headers) {
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
        .create_port(&token.project_id, body.port.network_id, body.port.name)
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.list_ports(&token.project_id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.get_port(&token.project_id, id).await {
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
    let token = match require_token(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.delete_port(&token.project_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}
