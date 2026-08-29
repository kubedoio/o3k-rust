use super::dhcp::DhcpRuntime;
use super::runtime::agent_error;
use super::{AgentError, proto};
use async_trait::async_trait;
use o3k_libvirt::{ErrorCategory, LibvirtAdapter, stable_domain_name};
use std::sync::{Arc, Mutex};

pub(super) struct NetworkPreparation {
    pub(crate) created_taps: Vec<o3k_network::TapSpec>,
    pub(crate) added_dhcp_ports: Vec<String>,
    pub(crate) external_owner: bool,
}

pub(super) fn rollback_network(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
) -> Result<(), AgentError> {
    if preparation.external_owner {
        return Ok(());
    }
    let mut first_error = None;
    if let Ok(mut runtime) = dhcp.lock() {
        if let Err(error) = runtime.remove_ports(&preparation.added_dhcp_ports) {
            first_error = Some(error);
        }
    } else {
        first_error = Some(AgentError::Protocol(
            "DHCP runtime lock is poisoned".to_owned(),
        ));
    }
    for tap in preparation.created_taps.iter().rev() {
        if let Err(error) = network.delete_tap(tap) {
            first_error.get_or_insert_with(|| {
                AgentError::Protocol(format!("TAP rollback failed: {error}"))
            });
        }
    }
    if let Err(error) = network.cleanup_if_unused() {
        first_error.get_or_insert_with(|| {
            AgentError::Protocol(format!("network rollback failed: {error}"))
        });
    }
    first_error.map_or(Ok(()), Err)
}

pub(super) fn return_after_network_rollback(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
    error: AgentError,
) -> AgentError {
    match rollback_network(network, dhcp, preparation) {
        Ok(()) => error,
        Err(rollback_error) => AgentError::Protocol(format!(
            "{error}; network rollback also failed: {rollback_error}"
        )),
    }
}

pub(super) fn return_after_create_rollback(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
    image_materializer: &o3k_compute_agent::ImageMaterializer,
    artifact_root: &std::path::Path,
    instance_id: &str,
    error: AgentError,
) -> AgentError {
    let error = return_after_network_rollback(network, dhcp, preparation, error);
    match image_materializer.delete_instance(instance_id) {
        Ok(()) => match super::cleanup::cleanup_console_log(artifact_root, instance_id) {
            Ok(()) => error,
            Err(cleanup_error) => AgentError::Protocol(format!(
                "{error}; console rollback also failed: {cleanup_error}"
            )),
        },
        Err(cleanup_error) => AgentError::Protocol(format!(
            "{error}; image rollback also failed: {cleanup_error}"
        )),
    }
}

pub(super) fn cleanup_instance_network(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    instance_id: &str,
) -> Result<(), AgentError> {
    let port_ids = network
        .owned_port_ids_for_instance(instance_id)
        .map_err(|_| AgentError::Protocol("owned network lookup failed".to_owned()))?;
    {
        let mut runtime = dhcp
            .lock()
            .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
        runtime.remove_ports(&port_ids)?;
    }
    network
        .delete_taps_for_instance(instance_id)
        .map_err(|error| AgentError::Protocol(format!("owned TAP cleanup failed: {error}")))?;
    network
        .cleanup_if_unused()
        .map_err(|error| AgentError::Protocol(format!("owned bridge cleanup failed: {error}")))
}

#[async_trait]
pub(super) trait DomainPresence: Send + Sync {
    /// `Ok(true)`: the domain provably does not exist — its recorded network
    /// state may be reaped. `Ok(false)`: the domain exists. `Err`: presence
    /// is unknown — fail closed, the instance keeps its network state.
    async fn domain_is_absent(&self, name: &str) -> Result<bool, AgentError>;
}

#[async_trait]
impl DomainPresence for LibvirtAdapter {
    async fn domain_is_absent(&self, name: &str) -> Result<bool, AgentError> {
        match self.inspect(name.to_owned()).await {
            Err(error) if error.category == ErrorCategory::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(agent_error(error)),
        }
    }
}

/// Startup reconciliation for crash residue (issue #87 S3 rerun #5): a
/// create prepares the host network (bridge, TAPs, DHCP bindings) before the
/// domain is defined, so an agent death in that window leaves O3K-owned
/// artifacts behind while the control-plane delete converges through local
/// completion and never dispatches an agent delete. This reaps the recorded
/// network state of every manifest instance whose domain provably does not
/// exist; the durable ownership manifest is the only authority that binds a
/// host interface to an instance.
///
/// An observation failure skips the instance (fail closed: a live or
/// uninspectable domain must never lose its TAP). Reap errors are returned
/// for logging only and are never fatal, so the residue is retried on the
/// next restart. `cleanup_if_unused` keeps the shared bridge in place while
/// any other recorded instance still uses it, and every deletion is bounded
/// by the manifest and the kernel ownership checks.
pub(super) async fn reap_stale_instance_networks(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    presence: &dyn DomainPresence,
) -> Result<(), AgentError> {
    let instance_ids = network
        .owned_instance_ids()
        .map_err(|error| AgentError::Protocol(format!("owned instance lookup failed: {error}")))?;
    let mut first_error = None;
    for instance_id in instance_ids {
        match presence
            .domain_is_absent(&stable_domain_name(&instance_id))
            .await
        {
            Ok(true) => {
                tracing::info!(
                    instance_id = %instance_id,
                    "reaping network residue of absent instance"
                );
                if let Err(error) = cleanup_instance_network(network, dhcp, &instance_id) {
                    first_error.get_or_insert(error);
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    instance_id = %instance_id,
                    "skipping network residue reap: domain presence is unknown"
                );
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(super) fn prepare_network(
    command: &proto::Command,
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
) -> Result<NetworkPreparation, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol("create action is missing".to_owned()));
    };
    let resolved = create
        .resolved
        .as_ref()
        .ok_or_else(|| AgentError::Protocol("resolved create inputs are missing".to_owned()))?;
    let runtime = dhcp
        .lock()
        .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
    runtime.validate(&resolved.network_attachments)?;
    let first = resolved
        .network_attachments
        .first()
        .ok_or_else(|| AgentError::Protocol("network attachment is missing".to_owned()))?;
    let gateway = first
        .gateway_ipv4
        .parse()
        .map_err(|_| AgentError::Protocol("network gateway address is invalid".to_owned()))?;
    let prefix_len = first
        .subnet_cidr
        .split_once('/')
        .and_then(|(_, prefix)| prefix.parse().ok())
        .ok_or_else(|| AgentError::Protocol("network subnet prefix is invalid".to_owned()))?;
    network
        .ensure_gateway(o3k_network::GatewaySpec {
            address: gateway,
            prefix_len,
        })
        .map_err(|error| AgentError::Protocol(format!("gateway preparation failed: {error}")))?;
    drop(runtime);
    let mut preparation = NetworkPreparation {
        created_taps: Vec::new(),
        added_dhcp_ports: Vec::new(),
        external_owner: false,
    };
    for attachment in &resolved.network_attachments {
        let spec = o3k_network::TapSpec {
            instance_id: command.resource_id.clone(),
            port_id: attachment.port_id.clone(),
            mac: attachment.mac.clone(),
        };
        match network.ensure_tap(&spec) {
            Ok((_, true)) => preparation.created_taps.push(spec.clone()),
            Ok((_, false)) => {}
            Err(error) => {
                return Err(return_after_network_rollback(
                    network,
                    dhcp,
                    &preparation,
                    AgentError::Protocol(format!("TAP preparation failed: {error}")),
                ));
            }
        }
    }
    let mut runtime = dhcp
        .lock()
        .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
    match runtime.apply(&resolved.network_attachments) {
        Ok(added) => preparation.added_dhcp_ports = added,
        Err(error) => {
            drop(runtime);
            return Err(return_after_network_rollback(
                network,
                dhcp,
                &preparation,
                error,
            ));
        }
    }
    Ok(preparation)
}
