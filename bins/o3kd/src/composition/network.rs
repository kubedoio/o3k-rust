use async_trait::async_trait;
use o3k_compute::PortBindingProjector;
use o3k_network;
use o3k_network::{
    AttachmentPlanInput, NetworkAgentIdentity, NetworkControllerLease, NetworkDispatchError,
    NetworkError, NetworkPlanAction, NetworkPlanCommand, NetworkPlanDispatcher, NetworkPlanStatus,
    NetworkService, PortBindingState, compile_attachment_plan,
};
use o3k_network_protocol;
use o3k_provider::AgentNodeRegistry;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tracing;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct NetworkAgentDispatcher {
    pub(crate) endpoint: String,
    pub(crate) server_name: String,
    pub(crate) ca_certificate: PathBuf,
    pub(crate) client_certificate: PathBuf,
    pub(crate) client_key: PathBuf,
}

pub(crate) fn network_dispatcher_from_env()
-> Result<Option<Arc<dyn o3k_network::NetworkPlanDispatcher>>, Box<dyn std::error::Error>> {
    let names = [
        "O3K_NETWORK_AGENT_ENDPOINT",
        "O3K_NETWORK_AGENT_SERVER_NAME",
        "O3K_NETWORK_AGENT_CA",
        "O3K_NETWORK_AGENT_CLIENT_CERT",
        "O3K_NETWORK_AGENT_CLIENT_KEY",
    ];
    let values = names
        .iter()
        .map(|name| std::env::var(name).ok())
        .collect::<Vec<_>>();
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    if values.iter().any(Option::is_none) {
        return Err("all O3K_NETWORK_AGENT_* transport variables are required".into());
    }
    let [
        endpoint,
        server_name,
        ca_certificate,
        client_certificate,
        client_key,
    ] = values
        .try_into()
        .map_err(|_| "invalid network agent transport configuration")?;
    Ok(Some(Arc::new(NetworkAgentDispatcher {
        endpoint: endpoint.ok_or("missing network agent endpoint")?,
        server_name: server_name.ok_or("missing network agent server name")?,
        ca_certificate: PathBuf::from(ca_certificate.ok_or("missing network agent CA")?),
        client_certificate: PathBuf::from(
            client_certificate.ok_or("missing network agent client certificate")?,
        ),
        client_key: PathBuf::from(client_key.ok_or("missing network agent client key")?),
    })))
}

#[async_trait]
impl o3k_network::NetworkPlanDispatcher for NetworkAgentDispatcher {
    async fn dispatch(
        &self,
        command: o3k_network::NetworkPlanCommand,
    ) -> Result<o3k_network::NetworkPlanStatus, o3k_network::NetworkDispatchError> {
        let client = o3k_network_protocol::NetworkAgentClient::connect(
            &self.endpoint,
            &self.server_name,
            &self.ca_certificate,
            &self.client_certificate,
            &self.client_key,
        )
        .await
        .map_err(|error| o3k_network::NetworkDispatchError::Transport(error.to_string()))?;
        let command_id = command.command_id.to_string();
        let result = client
            .execute(
                o3k_network_protocol::proto::Register {
                    agent_id: command.target.agent_id.clone(),
                    agent_epoch: command.target.agent_epoch.clone(),
                },
                o3k_network_protocol::proto::NetworkCommand {
                    command_id: command_id.clone(),
                    operation_id: command.operation_id.to_string(),
                    idempotency_key: command.idempotency_key,
                    agent_id: command.target.agent_id,
                    agent_epoch: command.target.agent_epoch,
                    controller_id: command.controller.controller_id,
                    controller_epoch: command.controller.controller_epoch,
                    fencing_token: command.controller.fencing_token,
                    deadline_unix_ms: command.deadline_unix_ms,
                    plan_json: serde_json::to_string(&command.plan).map_err(|error| {
                        o3k_network::NetworkDispatchError::Rejected(error.to_string())
                    })?,
                    remove: matches!(command.action, o3k_network::NetworkPlanAction::Remove),
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    command_id = %command_id,
                    operation_id = %command.operation_id,
                    error = %error,
                    "network agent dispatch failed"
                );
                o3k_network::NetworkDispatchError::Transport(error.to_string())
            })?;
        tracing::debug!(
            command_id = %command_id,
            operation_id = %command.operation_id,
            status = %result.status,
            replayed = result.replayed,
            error_code = %result.error_code,
            "network agent dispatch completed"
        );
        match result.status.as_str() {
            "succeeded" | "replayed" | "recovered" => Ok(o3k_network::NetworkPlanStatus::Succeeded),
            "unknown" | "requires_observation" => Ok(o3k_network::NetworkPlanStatus::Unknown),
            other => Err(o3k_network::NetworkDispatchError::Rejected(
                if result.error_code.is_empty() {
                    other.to_owned()
                } else {
                    result.error_code
                },
            )),
        }
    }
}

pub(crate) fn public_allocator_from_env(
    data_dir: &std::path::Path,
) -> Result<Option<o3k_network::PublicAddressAllocator>, Box<dyn std::error::Error>> {
    let cidr = std::env::var("O3K_PUBLIC_POOL_CIDR").ok();
    let first = std::env::var("O3K_PUBLIC_POOL_FIRST").ok();
    let last = std::env::var("O3K_PUBLIC_POOL_LAST").ok();
    if cidr.is_none() && first.is_none() && last.is_none() {
        return Ok(None);
    }
    let cidr = cidr.ok_or("O3K_PUBLIC_POOL_CIDR is required")?;
    let first = first.ok_or("O3K_PUBLIC_POOL_FIRST is required")?.parse()?;
    let last = last.ok_or("O3K_PUBLIC_POOL_LAST is required")?.parse()?;
    let (network, prefix_len) = cidr
        .split_once('/')
        .ok_or("O3K_PUBLIC_POOL_CIDR must be IPv4/prefix-length")?;
    let prefix = o3k_domain::Ipv4Prefix::new(network.parse()?, prefix_len.parse()?)
        .ok_or("O3K_PUBLIC_POOL_CIDR is invalid")?;
    Ok(Some(o3k_network::PublicAddressAllocator::open(
        data_dir.join("public-addresses"),
        o3k_network::PublicAddressPool {
            prefix,
            first_usable: first,
            last_usable: last,
        },
    )?))
}

/// Projects terminal compute outcomes into the durable port binding state of
/// the network control plane. Wired only for the agent provider profile,
/// where the resolver records binding intent at create dispatch.
#[derive(Clone)]
pub(crate) struct NetworkBindingProjector {
    pub(crate) network: o3k_network::NetworkService,
    pub(crate) registry: Arc<dyn o3k_provider::AgentNodeRegistry>,
    pub(crate) network_dispatcher: Option<Arc<dyn o3k_network::NetworkPlanDispatcher>>,
    pub(crate) network_controller: o3k_network::NetworkControllerLease,
    pub(crate) network_external_realm_id: Option<Uuid>,
    pub(crate) network_agent: Option<o3k_network::NetworkAgentIdentity>,
}

#[async_trait]
impl o3k_compute::PortBindingProjector for NetworkBindingProjector {
    async fn project_create_outcome(
        &self,
        project_id: &str,
        port_id: &str,
        succeeded: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port_id = port_id.parse::<Uuid>().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid port id {port_id:?}: {error}"),
            )
        })?;
        let state = if succeeded {
            o3k_network::PortBindingState::Bound
        } else {
            o3k_network::PortBindingState::Error
        };
        if succeeded {
            self.dispatch_unbound_port(project_id, port_id).await?;
        }
        self.network
            .project_create_outcome(project_id, port_id, state)
            .await
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(())
    }

    async fn unbind_port(
        &self,
        project_id: &str,
        port_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port_id = port_id.parse::<Uuid>().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid port id {port_id:?}: {error}"),
            )
        })?;
        let port = self
            .network
            .get_port_for_project(project_id, port_id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if let (Some(dispatcher), Some(host)) = (
            self.network_dispatcher.as_ref(),
            port.binding_host.as_deref(),
        ) {
            let agent = if let Some(configured) = self.network_agent.as_ref() {
                if configured.agent_id != host {
                    return Err(
                        std::io::Error::other("bound network agent identity changed").into(),
                    );
                }
                configured.clone()
            } else {
                let snapshot =
                    self.registry.snapshot(host).await.ok_or_else(|| {
                        std::io::Error::other("network agent snapshot unavailable")
                    })?;
                o3k_network::NetworkAgentIdentity {
                    agent_id: snapshot.agent_id,
                    agent_epoch: snapshot.agent_epoch,
                }
            };
            let subnet_id = port
                .subnet_id
                .ok_or_else(|| std::io::Error::other("bound port has no subnet"))?;
            let subnet = self
                .network
                .get_subnet_for_project(project_id, subnet_id)
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let policies = self
                .network
                .list_policies_for_project(project_id, port.network_id)
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .into_iter()
                .filter(|policy| policy.endpoint_id == port.id)
                .collect();
            let deadline_unix_ms = super::unix_time_millis().saturating_add(30_000);
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:network:remove:{project_id}:{port_id}").as_bytes(),
            );
            let plan = o3k_network::compile_attachment_plan(o3k_network::AttachmentPlanInput {
                endpoint_id: port.id,
                realm_id: port.network_id,
                project_id,
                mac: &port.mac_address,
                fixed_ip: port.fixed_ip,
                subnet_cidr: &subnet.cidr,
                node_id: host,
                operation_id,
                deadline_unix_ms,
                public_address: None,
                external_realm_id: self.network_external_realm_id,
                policies,
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
            let command_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:network:remove-command:{operation_id}").as_bytes(),
            );
            let status = dispatcher
                .dispatch(o3k_network::NetworkPlanCommand {
                    command_id,
                    operation_id,
                    idempotency_key: format!("o3k:network:remove:{project_id}:{port_id}"),
                    action: o3k_network::NetworkPlanAction::Remove,
                    target: agent,
                    controller: self.network_controller.clone(),
                    deadline_unix_ms,
                    plan,
                })
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if status != o3k_network::NetworkPlanStatus::Succeeded {
                return Err(std::io::Error::other(
                    "network removal requires observation before unbinding",
                )
                .into());
            }
        }
        self.network
            .unbind_port(project_id, port_id)
            .await
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(())
    }
}

impl NetworkBindingProjector {
    /// The agent-provider resolver dispatches before compute mutation.  Other
    /// providers (notably the portable fake/TestLab provider) complete the
    /// server operation without that resolver, so the terminal binding
    /// projection is the safe point at which to admit their network plan.
    /// This is deliberately limited to an explicitly configured network
    /// agent; without one, the historical binding projection remains a
    /// control-plane-only observation.
    async fn dispatch_unbound_port(
        &self,
        project_id: &str,
        port_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(dispatcher) = self.network_dispatcher.as_ref() else {
            return Ok(());
        };
        let Some(agent) = self.network_agent.as_ref() else {
            return Ok(());
        };
        let port = self
            .network
            .get_port_for_project(project_id, port_id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if port.binding_host.is_some() {
            return Ok(());
        }
        let subnet_id = port
            .subnet_id
            .ok_or_else(|| std::io::Error::other("network port has no subnet"))?;
        let subnet = self
            .network
            .get_subnet_for_project(project_id, subnet_id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        self.network
            .record_binding_intent(project_id, port_id, &agent.agent_id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let policies = self
            .network
            .list_policies_for_project(project_id, port.network_id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .into_iter()
            .filter(|policy| policy.endpoint_id == port.id)
            .collect();
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:network:terminal-binding:{project_id}:{port_id}").as_bytes(),
        );
        let deadline_unix_ms = super::unix_time_millis().saturating_add(30_000);
        let plan = o3k_network::compile_attachment_plan(o3k_network::AttachmentPlanInput {
            endpoint_id: port.id,
            realm_id: port.network_id,
            project_id,
            mac: &port.mac_address,
            fixed_ip: port.fixed_ip,
            subnet_cidr: &subnet.cidr,
            node_id: &agent.agent_id,
            operation_id,
            deadline_unix_ms,
            public_address: None,
            external_realm_id: self.network_external_realm_id,
            policies,
        })
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        let command_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:network:terminal-binding-command:{operation_id}").as_bytes(),
        );
        let status = dispatcher
            .dispatch(o3k_network::NetworkPlanCommand {
                command_id,
                operation_id,
                idempotency_key: format!("o3k:network:terminal-binding:{project_id}:{port_id}"),
                action: o3k_network::NetworkPlanAction::Apply,
                target: agent.clone(),
                controller: self.network_controller.clone(),
                deadline_unix_ms,
                plan,
            })
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if status != o3k_network::NetworkPlanStatus::Succeeded {
            return Err(std::io::Error::other(
                "network binding requires observed provider success",
            )
            .into());
        }
        Ok(())
    }
}
