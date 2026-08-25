//! Neutron-compatible network protocol adapter: network/subnet/port
//! handlers, wire models, and error mapping.

use std::{net::Ipv4Addr, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use o3k_domain::{NetworkProtocol, PolicyAction, PolicyDirection, PolicyIntent, PortRange};
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
#[derive(serde::Deserialize)]
pub(crate) struct UpdateNetworkRequestBody {
    network: UpdateNetworkRequest,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateNetworkRequest {
    name: Option<String>,
    #[serde(default)]
    admin_state_up: Option<bool>,
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
    tenant_id: String,
    status: String,
    admin_state_up: bool,
    mtu: u32,
    subnets: Vec<String>,
}

pub(crate) fn network_response(
    value: NetworkRecord,
    admin_state_up: bool,
    subnet_ids: Vec<Uuid>,
) -> NetworkResponse {
    NetworkResponse {
        id: value.id.to_string(),
        name: value.name,
        tenant_id: value.project_id.clone(),
        project_id: value.project_id,
        status: value.status,
        admin_state_up,
        mtu: 1500,
        subnets: subnet_ids.into_iter().map(|id| id.to_string()).collect(),
    }
}

async fn canonical_network_response(
    service: &NetworkService,
    value: NetworkRecord,
) -> Result<NetworkResponse, NetworkError> {
    let snapshot = service
        .reconstruct_canonical_network(&value.project_id, value.id)
        .await?;
    Ok(network_response(
        value,
        snapshot.network.admin_state_up,
        snapshot.realms.into_iter().map(|realm| realm.id).collect(),
    ))
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
pub(crate) struct UpdatePortRequestBody {
    port: UpdatePortRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct UpdatePortRequest {
    #[serde(default)]
    security_groups: Vec<uuid::Uuid>,
}
#[derive(serde::Deserialize)]
pub(crate) struct CreatePortRequest {
    name: String,
    network_id: uuid::Uuid,
    #[serde(default)]
    security_groups: Vec<uuid::Uuid>,
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
    security_groups: Vec<String>,
}
#[derive(serde::Serialize)]
pub(crate) struct FixedIpResponse {
    subnet_id: String,
    ip_address: Ipv4Addr,
}

#[derive(serde::Deserialize)]
pub(crate) struct NetworkPolicyQuery {
    network_id: Uuid,
}

#[derive(serde::Deserialize)]
pub(crate) struct NetworkPolicyRequestBody {
    policy: NetworkPolicyRequest,
}

#[derive(serde::Deserialize)]
pub(crate) struct NetworkPolicyRequest {
    network_id: Uuid,
    endpoint_id: Uuid,
    direction: String,
    protocol: String,
    ports: Option<PolicyPortRange>,
    source: Option<String>,
    destination: Option<String>,
    action: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct PolicyPortRange {
    start: u16,
    end: u16,
}

#[derive(serde::Deserialize)]
pub(crate) struct SecurityGroupRequestBody {
    security_group: SecurityGroupRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct SecurityGroupRequest {
    name: String,
    #[serde(default)]
    description: String,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupEnvelope {
    security_group: SecurityGroupResponse,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupList {
    security_groups: Vec<SecurityGroupResponse>,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupResponse {
    id: String,
    project_id: String,
    name: String,
    description: String,
    security_group_rules: Vec<SecurityGroupRuleResponse>,
}
#[derive(serde::Deserialize)]
pub(crate) struct SecurityGroupRuleRequestBody {
    security_group_rule: SecurityGroupRuleRequest,
}
#[derive(serde::Deserialize)]
pub(crate) struct SecurityGroupRuleRequest {
    security_group_id: Uuid,
    direction: String,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    port_range_min: Option<u16>,
    #[serde(default)]
    port_range_max: Option<u16>,
    #[serde(default)]
    remote_ip_prefix: Option<String>,
    #[serde(default)]
    ethertype: Option<String>,
    #[serde(default)]
    remote_group_id: Option<Uuid>,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupRuleResponse {
    id: String,
    project_id: String,
    security_group_id: String,
    direction: String,
    ethertype: &'static str,
    protocol: Option<String>,
    port_range_min: Option<u16>,
    port_range_max: Option<u16>,
    remote_ip_prefix: Option<String>,
    remote_group_id: Option<String>,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupRuleEnvelope {
    security_group_rule: SecurityGroupRuleResponse,
}
#[derive(serde::Serialize)]
pub(crate) struct SecurityGroupRuleList {
    security_group_rules: Vec<SecurityGroupRuleResponse>,
}
#[derive(serde::Deserialize)]
pub(crate) struct SecurityGroupRuleQuery {
    #[serde(default)]
    security_group_id: Option<Uuid>,
}

#[derive(serde::Serialize)]
pub(crate) struct NetworkPolicyEnvelope {
    policy: NetworkPolicyResponse,
}

#[derive(serde::Serialize)]
pub(crate) struct NetworkPolicyList {
    policies: Vec<NetworkPolicyResponse>,
}

#[derive(serde::Serialize)]
pub(crate) struct NetworkPolicyResponse {
    id: String,
    network_id: String,
    endpoint_id: String,
    direction: &'static str,
    protocol: &'static str,
    ports: Option<PolicyPortRange>,
    source: Option<String>,
    destination: Option<String>,
    action: &'static str,
    status: &'static str,
}

fn policy_response(network_id: Uuid, policy: PolicyIntent) -> NetworkPolicyResponse {
    NetworkPolicyResponse {
        id: policy.id.to_string(),
        network_id: network_id.to_string(),
        endpoint_id: policy.endpoint_id.to_string(),
        direction: match policy.direction {
            PolicyDirection::Ingress => "ingress",
            PolicyDirection::Egress => "egress",
        },
        protocol: match policy.protocol {
            NetworkProtocol::Any => "any",
            NetworkProtocol::Tcp => "tcp",
            NetworkProtocol::Udp => "udp",
            NetworkProtocol::Icmp => "icmp",
        },
        ports: policy.ports.map(|ports| PolicyPortRange {
            start: ports.start,
            end: ports.end,
        }),
        source: policy
            .source
            .map(|prefix| format!("{}/{}", prefix.network, prefix.prefix_len)),
        destination: policy
            .destination
            .map(|prefix| format!("{}/{}", prefix.network, prefix.prefix_len)),
        action: match policy.action {
            PolicyAction::Allow => "allow",
            PolicyAction::Deny => "deny",
        },
        // The API mutation is durable intent. Host realization is reported by
        // the agent observation path, so this projection must not claim active
        // before that evidence exists.
        status: "pending",
    }
}

fn parse_policy_prefix(value: Option<String>) -> Result<Option<o3k_domain::Ipv4Prefix>, ()> {
    value
        .map(|value| {
            let (address, length) = value.split_once('/').ok_or(())?;
            let address = address.parse().map_err(|_| ())?;
            let length = length.parse().map_err(|_| ())?;
            o3k_domain::Ipv4Prefix::new(address, length).ok_or(())
        })
        .transpose()
}

fn parse_policy_request(
    request: NetworkPolicyRequest,
    id: Uuid,
) -> Result<(Uuid, PolicyIntent), ()> {
    let direction = match request.direction.as_str() {
        "ingress" => PolicyDirection::Ingress,
        "egress" => PolicyDirection::Egress,
        _ => return Err(()),
    };
    let protocol = match request.protocol.as_str() {
        "any" => NetworkProtocol::Any,
        "tcp" => NetworkProtocol::Tcp,
        "udp" => NetworkProtocol::Udp,
        "icmp" => NetworkProtocol::Icmp,
        _ => return Err(()),
    };
    let action = match request.action.as_str() {
        "allow" => PolicyAction::Allow,
        "deny" => PolicyAction::Deny,
        _ => return Err(()),
    };
    let source = parse_policy_prefix(request.source)?;
    let destination = parse_policy_prefix(request.destination)?;
    Ok((
        request.network_id,
        PolicyIntent {
            id,
            endpoint_id: request.endpoint_id,
            direction,
            protocol,
            ports: request.ports.map(|ports| PortRange {
                start: ports.start,
                end: ports.end,
            }),
            source,
            destination,
            action,
        },
    ))
}

async fn security_group_response(
    service: &NetworkService,
    project_id: &str,
    group: o3k_store::SecurityGroupRecord,
) -> Result<SecurityGroupResponse, NetworkError> {
    let rules = service
        .list_security_group_rules_for_project(project_id, group.id)
        .await?
        .into_iter()
        .map(security_group_rule_response)
        .collect();
    Ok(SecurityGroupResponse {
        id: group.id.to_string(),
        project_id: group.project_id,
        name: group.name,
        description: group.description,
        security_group_rules: rules,
    })
}

fn security_group_rule_response(
    rule: o3k_store::SecurityGroupRuleRecord,
) -> SecurityGroupRuleResponse {
    SecurityGroupRuleResponse {
        id: rule.id.to_string(),
        project_id: rule.project_id,
        security_group_id: rule.security_group_id.to_string(),
        direction: rule.direction,
        ethertype: "IPv4",
        protocol: (rule.protocol != "any").then_some(rule.protocol),
        port_range_min: rule.port_min,
        port_range_max: rule.port_max,
        remote_ip_prefix: rule.remote_ip_prefix,
        remote_group_id: None,
    }
}

pub(crate) async fn list_security_groups(
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
    let project = auth.effective_scope().id().as_str();
    let groups = match service.list_security_groups_for_project(project).await {
        Ok(groups) => groups,
        Err(error) => return network_error(error),
    };
    let mut responses = Vec::with_capacity(groups.len());
    for group in groups {
        match security_group_response(service, project, group).await {
            Ok(value) => responses.push(value),
            Err(error) => return network_error(error),
        }
    }
    Json(SecurityGroupList {
        security_groups: responses,
    })
    .into_response()
}

pub(crate) async fn create_security_group(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<SecurityGroupRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid security group request",
        );
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    match service
        .create_security_group_for_project(
            project,
            body.security_group.name,
            body.security_group.description,
        )
        .await
    {
        Ok(group) => match security_group_response(service, project, group).await {
            Ok(value) => (
                StatusCode::CREATED,
                Json(SecurityGroupEnvelope {
                    security_group: value,
                }),
            )
                .into_response(),
            Err(error) => network_error(error),
        },
        Err(error) => network_error(error),
    }
}

pub(crate) async fn show_security_group(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let group = match service.get_security_group_for_project(project, id).await {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    match security_group_response(service, project, group).await {
        Ok(value) => Json(SecurityGroupEnvelope {
            security_group: value,
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn update_security_group(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<SecurityGroupRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid security group request",
        );
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let group = match service
        .update_security_group_for_project(
            project,
            id,
            body.security_group.name,
            body.security_group.description,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    match security_group_response(service, project, group).await {
        Ok(value) => Json(SecurityGroupEnvelope {
            security_group: value,
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn delete_security_group(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .delete_security_group_for_project(auth.effective_scope().id().as_str(), id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn list_security_group_rules(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<SecurityGroupRuleQuery>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let groups = match service.list_security_groups_for_project(project).await {
        Ok(groups) => groups,
        Err(error) => return network_error(error),
    };
    let mut rules = Vec::new();
    for group in groups {
        if query.security_group_id.is_none() || query.security_group_id == Some(group.id) {
            match service
                .list_security_group_rules_for_project(project, group.id)
                .await
            {
                Ok(values) => rules.extend(values.into_iter().map(security_group_rule_response)),
                Err(error) => return network_error(error),
            }
        }
    }
    Json(SecurityGroupRuleList {
        security_group_rules: rules,
    })
    .into_response()
}

pub(crate) async fn create_security_group_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<SecurityGroupRuleRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid security group rule request",
        );
    };
    let input = body.security_group_rule;
    if input
        .ethertype
        .as_deref()
        .is_some_and(|value| value != "IPv4")
        || input.remote_group_id.is_some()
    {
        return network_error(NetworkError::InvalidRequest);
    }
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let rule = match service
        .create_security_group_rule_for_project(
            project,
            input.security_group_id,
            input.direction,
            input.protocol.unwrap_or_else(|| "any".to_owned()),
            input.port_range_min,
            input.port_range_max,
            input.remote_ip_prefix,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    if let Err(response) =
        dispatch_security_group_endpoints(&state, project, rule.security_group_id).await
    {
        return response;
    }
    (
        StatusCode::CREATED,
        Json(SecurityGroupRuleEnvelope {
            security_group_rule: security_group_rule_response(rule),
        }),
    )
        .into_response()
}

pub(crate) async fn show_security_group_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .get_security_group_rule_for_project(auth.effective_scope().id().as_str(), id)
        .await
    {
        Ok(rule) => Json(SecurityGroupRuleEnvelope {
            security_group_rule: security_group_rule_response(rule),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn delete_security_group_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let rule = match service
        .get_security_group_rule_for_project(project, id)
        .await
    {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    if let Err(error) = service
        .delete_security_group_rule_for_project(project, id)
        .await
    {
        return network_error(error);
    }
    if let Err(response) =
        dispatch_security_group_endpoints(&state, project, rule.security_group_id).await
    {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn dispatch_security_group_endpoints(
    state: &AppState,
    project_id: &str,
    group_id: Uuid,
) -> Result<(), axum::response::Response> {
    let service = network_service(state)?;
    let bindings = service
        .list_security_group_bindings_for_project(project_id, None)
        .await
        .map_err(network_error)?;
    for binding in bindings
        .into_iter()
        .filter(|binding| binding.security_group_id == group_id)
    {
        let port = service
            .get_port_for_project(project_id, binding.endpoint_id)
            .await
            .map_err(network_error)?;
        dispatch_policy_network(state, project_id, port.network_id, port.id).await?;
    }
    Ok(())
}

pub(crate) async fn list_network_policies(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<NetworkPolicyQuery>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .list_policies_for_project(auth.effective_scope().id().as_str(), query.network_id)
        .await
    {
        Ok(policies) => Json(NetworkPolicyList {
            policies: policies
                .into_iter()
                .map(|policy| policy_response(query.network_id, policy))
                .collect(),
        })
        .into_response(),
        Err(error) => network_error(error),
    }
}

pub(crate) async fn create_network_policy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<NetworkPolicyRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid policy request",
        );
    };
    let request_identity = headers
        .get("idempotency-key")
        .or_else(|| headers.get("x-openstack-request-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let policy_input = body.policy;
    let policy_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!(
            "o3k:network:policy:{}:{}:{}:{}",
            auth.effective_scope().id(),
            policy_input.network_id,
            policy_input.endpoint_id,
            request_identity
        )
        .as_bytes(),
    );
    let (network_id, policy) = match parse_policy_request(policy_input, policy_id) {
        Ok(value) => value,
        Err(()) => {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "invalid policy shape",
            );
        }
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project_id = auth.effective_scope().id().as_str();
    let endpoint_id = policy.endpoint_id;
    let policy = match service
        .upsert_policy_for_project(project_id, network_id, policy)
        .await
    {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    if let Err(response) =
        dispatch_policy_network(&state, project_id, network_id, endpoint_id).await
    {
        return response;
    }
    (
        StatusCode::CREATED,
        Json(NetworkPolicyEnvelope {
            policy: policy_response(network_id, policy),
        }),
    )
        .into_response()
}

pub(crate) async fn show_network_policy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<NetworkPolicyQuery>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .list_policies_for_project(auth.effective_scope().id().as_str(), query.network_id)
        .await
    {
        Ok(policies) => match policies.into_iter().find(|policy| policy.id == id) {
            Some(policy) => Json(NetworkPolicyEnvelope {
                policy: policy_response(query.network_id, policy),
            })
            .into_response(),
            None => network_error(o3k_network::NetworkError::NotFound),
        },
        Err(error) => network_error(error),
    }
}

pub(crate) async fn update_network_policy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<NetworkPolicyRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Ok(Json(body)) = request else {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid policy request",
        );
    };
    let (network_id, policy) = match parse_policy_request(body.policy, id) {
        Ok(value) => value,
        Err(()) => {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "invalid policy shape",
            );
        }
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project_id = auth.effective_scope().id().as_str();
    let endpoint_id = policy.endpoint_id;
    let policy = match service
        .upsert_policy_for_project(project_id, network_id, policy)
        .await
    {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    if let Err(response) =
        dispatch_policy_network(&state, project_id, network_id, endpoint_id).await
    {
        return response;
    }
    Json(NetworkPolicyEnvelope {
        policy: policy_response(network_id, policy),
    })
    .into_response()
}

pub(crate) async fn delete_network_policy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<NetworkPolicyQuery>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project_id = auth.effective_scope().id().as_str();
    let endpoint_id = match service
        .list_policies_for_project(project_id, query.network_id)
        .await
    {
        Ok(policies) => match policies.into_iter().find(|policy| policy.id == id) {
            Some(policy) => policy.endpoint_id,
            None => return network_error(o3k_network::NetworkError::NotFound),
        },
        Err(error) => return network_error(error),
    };
    if let Err(error) = service
        .delete_policy_for_project(project_id, query.network_id, id)
        .await
    {
        return network_error(error);
    }
    if let Err(response) =
        dispatch_policy_network(&state, project_id, query.network_id, endpoint_id).await
    {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn dispatch_policy_network(
    state: &AppState,
    project_id: &str,
    network_id: Uuid,
    endpoint_id: Uuid,
) -> Result<(), axum::response::Response> {
    let Some(dispatcher) = state.network_dispatcher.as_ref() else {
        return Ok(());
    };
    let Some(controller) = state.network_controller.as_ref() else {
        return Ok(());
    };
    let network = network_service(state)?;
    let ports = network
        .list_ports_for_project(project_id)
        .await
        .map_err(network_error)?;
    let port = ports
        .into_iter()
        .find(|port| port.network_id == network_id && port.id == endpoint_id)
        .ok_or_else(|| network_error(o3k_network::NetworkError::NotFound))?;
    // Policy intent is durable before an endpoint is scheduled.  Leave the
    // provider projection pending until the normal binding lifecycle can
    // dispatch the complete attachment plan.
    let Some(host) = port.binding_host.clone() else {
        return Ok(());
    };
    let agent = if let Some(registry) = state.agent_registry.as_ref()
        && let Some(agent) = registry.snapshot(&host).await
    {
        o3k_network::NetworkAgentIdentity {
            agent_id: agent.agent_id,
            agent_epoch: agent.agent_epoch,
        }
    } else if let Some(agent) = state.network_agent.as_ref()
        && agent.agent_id == host
    {
        agent.clone()
    } else {
        return Err(keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "selected network agent is unavailable",
        ));
    };
    let subnet_id = port.subnet_id.ok_or_else(|| {
        keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "endpoint has no fixed subnet",
        )
    })?;
    let subnet = network
        .get_subnet_for_project(project_id, subnet_id)
        .await
        .map_err(network_error)?;
    let policies = network
        .list_policies_for_project(project_id, network_id)
        .await
        .map_err(network_error)?
        .into_iter()
        .filter(|policy| policy.endpoint_id == port.id)
        .collect();
    let deadline_unix_ms = unix_time_millis().saturating_add(30_000);
    let operation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        serde_json::to_string(&policies)
            .unwrap_or_default()
            .as_bytes(),
    );
    let plan = o3k_network::compile_attachment_plan(o3k_network::AttachmentPlanInput {
        endpoint_id: port.id,
        realm_id: port.network_id,
        project_id,
        mac: &port.mac_address,
        fixed_ip: port.fixed_ip,
        subnet_cidr: &subnet.cidr,
        node_id: &host,
        operation_id,
        deadline_unix_ms,
        public_address: None,
        external_realm_id: state.network_external_realm_id,
        policies,
    })
    .map_err(|error| keystone_error(StatusCode::BAD_REQUEST, "Bad Request", error.to_string()))?;
    let status = dispatcher
        .dispatch(o3k_network::NetworkPlanCommand {
            command_id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("policy:{operation_id}").as_bytes(),
            ),
            operation_id,
            idempotency_key: format!("o3k:network:policy:{project_id}:{network_id}:{operation_id}"),
            action: o3k_network::NetworkPlanAction::Apply,
            target: agent,
            controller: controller.clone(),
            deadline_unix_ms,
            plan,
        })
        .await
        .map_err(|error| {
            keystone_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                error.to_string(),
            )
        })?;
    if status != o3k_network::NetworkPlanStatus::Succeeded {
        return Err(keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "policy realization requires observation",
        ));
    }
    network
        .mark_network_intent_active_for_project(project_id, network_id)
        .await
        .map_err(network_error)?;
    Ok(())
}

pub(crate) fn port_response(value: PortRecord, security_groups: Vec<Uuid>) -> PortResponse {
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
        security_groups: security_groups
            .into_iter()
            .map(|id| id.to_string())
            .collect(),
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

fn unix_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn public_error(error: PublicAddressError) -> axum::response::Response {
    let (status, title) = match error {
        PublicAddressError::NotFound => (StatusCode::NOT_FOUND, "Not Found"),
        PublicAddressError::NotOwner
        | PublicAddressError::AssociationConflict
        | PublicAddressError::InUse
        | PublicAddressError::Exhausted => (StatusCode::CONFLICT, "Conflict"),
        PublicAddressError::InvalidPool
        | PublicAddressError::MissingEndpoint
        | PublicAddressError::MissingRealm => (StatusCode::BAD_REQUEST, "Bad Request"),
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

async fn dispatch_public_binding(
    state: &AppState,
    binding: &PublicAddressBinding,
    action: o3k_network::NetworkPlanAction,
) -> Result<(), axum::response::Response> {
    let Some(dispatcher) = state.network_dispatcher.as_ref() else {
        return Ok(());
    };
    let Some(controller) = state.network_controller.as_ref() else {
        return Ok(());
    };
    let Some(endpoint_id) = binding.endpoint_id else {
        return Ok(());
    };
    let network = network_service(state)?;
    let port = network
        .get_port_for_project(&binding.project_id, endpoint_id)
        .await
        .map_err(network_error)?;
    let Some(host) = port.binding_host.as_deref() else {
        return Ok(());
    };
    let agent = if let Some(registry) = state.agent_registry.as_ref()
        && let Some(agent) = registry.snapshot(host).await
    {
        o3k_network::NetworkAgentIdentity {
            agent_id: agent.agent_id,
            agent_epoch: agent.agent_epoch,
        }
    } else if let Some(agent) = state.network_agent.as_ref()
        && agent.agent_id == host
    {
        agent.clone()
    } else {
        return Err(keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "selected network agent is unavailable",
        ));
    };
    let subnet_id = port.subnet_id.ok_or_else(|| {
        keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "endpoint has no fixed subnet",
        )
    })?;
    let subnet = network
        .get_subnet_for_project(&binding.project_id, subnet_id)
        .await
        .map_err(network_error)?;
    let policies = network
        .list_policies_for_project(&binding.project_id, port.network_id)
        .await
        .map_err(network_error)?
        .into_iter()
        .filter(|policy| policy.endpoint_id == port.id)
        .collect();
    let deadline_unix_ms = unix_time_millis().saturating_add(30_000);
    let operation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!(
            "o3k:network:public:{}:{}:{:?}",
            binding.allocation_id, binding.generation, action
        )
        .as_bytes(),
    );
    let plan = o3k_network::compile_attachment_plan(o3k_network::AttachmentPlanInput {
        endpoint_id,
        realm_id: port.network_id,
        project_id: &binding.project_id,
        mac: &port.mac_address,
        fixed_ip: port.fixed_ip,
        subnet_cidr: &subnet.cidr,
        node_id: host,
        operation_id,
        deadline_unix_ms,
        public_address: Some(binding.public_address),
        external_realm_id: state.network_external_realm_id,
        policies,
    })
    .map_err(|error| keystone_error(StatusCode::BAD_REQUEST, "Bad Request", error.to_string()))?;
    let command_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:network:public-command:{operation_id}").as_bytes(),
    );
    let status = dispatcher
        .dispatch(o3k_network::NetworkPlanCommand {
            command_id,
            operation_id,
            idempotency_key: format!(
                "o3k:network:public:{}:{}:{:?}",
                binding.allocation_id, binding.generation, action
            ),
            action,
            target: o3k_network::NetworkAgentIdentity {
                agent_id: agent.agent_id,
                agent_epoch: agent.agent_epoch,
            },
            controller: controller.clone(),
            deadline_unix_ms,
            plan,
        })
        .await
        .map_err(|error| {
            keystone_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                error.to_string(),
            )
        })?;
    if status != o3k_network::NetworkPlanStatus::Succeeded {
        return Err(keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "public address realization requires observation",
        ));
    }
    Ok(())
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
    let Some(external_realm_id) = state.network_external_realm_id else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "floating IP external network is not configured",
        );
    };
    if body.floatingip.floating_network_id != Some(external_realm_id) {
        return public_error(PublicAddressError::InvalidPool);
    }
    let operation_id = headers
        .get("x-openstack-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let project_id = auth.effective_scope().id().as_str();
    let endpoint = if let Some(port_id) = body.floatingip.port_id {
        let service = match network_service(&state) {
            Ok(value) => value,
            Err(response) => return response,
        };
        match service.get_port_for_project(project_id, port_id).await {
            Ok(value) => Some(value),
            Err(_) => return public_error(PublicAddressError::MissingEndpoint),
        }
    } else {
        None
    };
    let mut binding = match allocator.allocate(project_id, &operation_id) {
        Ok(value) => value,
        Err(error) => return public_error(error),
    };
    if let Some(port) = endpoint {
        let port_id = port.id;
        binding = match allocator.associate(project_id, binding.allocation_id, port_id) {
            Ok(value) => value,
            Err(error) => return public_error(error),
        };
        if let Err(response) =
            dispatch_public_binding(&state, &binding, o3k_network::NetworkPlanAction::Apply).await
        {
            return response;
        }
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
            let binding = match allocator.associate(project_id, id, port_id) {
                Ok(value) => value,
                Err(error) => return public_error(error),
            };
            if let Err(response) =
                dispatch_public_binding(&state, &binding, o3k_network::NetworkPlanAction::Apply)
                    .await
            {
                return response;
            }
            Ok(binding)
        }
        None => {
            let binding = match allocator.get(project_id, id) {
                Ok(value) => value,
                Err(error) => return public_error(error),
            };
            if let Err(response) =
                dispatch_public_binding(&state, &binding, o3k_network::NetworkPlanAction::Remove)
                    .await
            {
                return response;
            }
            allocator.disassociate(project_id, id)
        }
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
    let project_id = auth.effective_scope().id().as_str();
    let binding = match allocator.get(project_id, id) {
        Ok(value) => value,
        Err(error) => return public_error(error),
    };
    if let Err(response) =
        dispatch_public_binding(&state, &binding, o3k_network::NetworkPlanAction::Remove).await
    {
        return response;
    }
    match allocator.release(project_id, id) {
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
        Ok(value) => match canonical_network_response(service, value).await {
            Ok(network) => (StatusCode::CREATED, Json(NetworkEnvelope { network })).into_response(),
            Err(error) => network_error(error),
        },
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
        Ok(values) => {
            let mut networks = Vec::with_capacity(values.len());
            for value in values {
                match canonical_network_response(service, value).await {
                    Ok(network) => networks.push(network),
                    Err(error) => return network_error(error),
                }
            }
            Json(NetworkList { networks }).into_response()
        }
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
        Ok(value) => match canonical_network_response(service, value).await {
            Ok(network) => Json(NetworkEnvelope { network }).into_response(),
            Err(error) => network_error(error),
        },
        Err(error) => network_error(error),
    }
}

pub(crate) async fn update_network(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    request: Result<Json<UpdateNetworkRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
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
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service
        .update_network(&auth, id, body.network.name, body.network.admin_state_up)
        .await
    {
        Ok(value) => match canonical_network_response(service, value).await {
            Ok(network) => Json(NetworkEnvelope { network }).into_response(),
            Err(error) => network_error(error),
        },
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
    let security_groups = body.port.security_groups.clone();
    match service
        .create_port(&auth, body.port.network_id, body.port.name)
        .await
    {
        Ok(value) => {
            if let Err(error) = service
                .replace_security_group_bindings_for_project(
                    auth.effective_scope().id().as_str(),
                    value.id,
                    security_groups,
                )
                .await
            {
                return network_error(error);
            }
            let groups = service
                .list_security_group_bindings_for_project(
                    auth.effective_scope().id().as_str(),
                    Some(value.id),
                )
                .await
                .map(|bindings| {
                    bindings
                        .into_iter()
                        .map(|binding| binding.security_group_id)
                        .collect()
                })
                .unwrap_or_default();
            (
                StatusCode::CREATED,
                Json(PortEnvelope {
                    port: port_response(value, groups),
                }),
            )
                .into_response()
        }
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
        Ok(values) => {
            let mut ports = Vec::with_capacity(values.len());
            for value in values {
                let groups = service
                    .list_security_group_bindings_for_project(
                        auth.effective_scope().id().as_str(),
                        Some(value.id),
                    )
                    .await
                    .map_err(network_error);
                let groups = match groups {
                    Ok(bindings) => bindings
                        .into_iter()
                        .map(|binding| binding.security_group_id)
                        .collect(),
                    Err(response) => return response,
                };
                ports.push(port_response(value, groups));
            }
            Json(PortList { ports }).into_response()
        }
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
        Ok(value) => {
            let groups = match service
                .list_security_group_bindings_for_project(
                    auth.effective_scope().id().as_str(),
                    Some(value.id),
                )
                .await
            {
                Ok(bindings) => bindings
                    .into_iter()
                    .map(|binding| binding.security_group_id)
                    .collect(),
                Err(error) => return network_error(error),
            };
            Json(PortEnvelope {
                port: port_response(value, groups),
            })
            .into_response()
        }
        Err(error) => network_error(error),
    }
}

pub(crate) async fn update_port(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    request: Result<Json<UpdatePortRequestBody>, JsonRejection>,
) -> axum::response::Response {
    let auth = match require_auth_context(&state, &headers) {
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
    let service = match network_service(&state) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let project = auth.effective_scope().id().as_str();
    let port = match service.get_port_for_project(project, id).await {
        Ok(value) => value,
        Err(error) => return network_error(error),
    };
    if let Err(error) = service
        .replace_security_group_bindings_for_project(project, id, body.port.security_groups)
        .await
    {
        return network_error(error);
    }
    if let Err(response) = dispatch_policy_network(&state, project, port.network_id, port.id).await
    {
        return response;
    }
    let groups = match service
        .list_security_group_bindings_for_project(project, Some(id))
        .await
    {
        Ok(values) => values
            .into_iter()
            .map(|value| value.security_group_id)
            .collect(),
        Err(error) => return network_error(error),
    };
    match service.get_port_for_project(project, id).await {
        Ok(value) => Json(PortEnvelope {
            port: port_response(value, groups),
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
