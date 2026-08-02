use std::{
    env,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use o3k_compute_agent::{
    AgentClient, AgentConfig, AgentError, CommandExecutionResult, CommandExecutor,
    ConsoleLogResult, TlsFiles,
};
use o3k_libvirt::{ErrorCategory, LibvirtAdapter, LibvirtConfig, stable_domain_name};
use o3k_provider_contract::compute_proto as proto;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct HealthState {
    agent: AgentClient,
    libvirt_ready: bool,
    libvirt_error: Option<String>,
}

struct LibvirtCommandExecutor {
    adapter: LibvirtAdapter,
    artifact_root: PathBuf,
    network: o3k_network::HostNetworkManager,
    dhcp: Arc<Mutex<DhcpRuntime>>,
}

struct DhcpRuntime {
    service: o3k_dhcp::DhcpService,
    supervisor: Option<o3k_dhcp::DnsmasqSupervisor>,
    binary: PathBuf,
    interface: String,
}

impl DhcpRuntime {
    fn open(
        root: impl Into<PathBuf>,
        binary: impl Into<PathBuf>,
        interface: String,
    ) -> Result<Self, o3k_dhcp::DhcpError> {
        Ok(Self {
            service: o3k_dhcp::DhcpService::open(root)?,
            supervisor: None,
            binary: binary.into(),
            interface,
        })
    }

    fn validate(&self, attachments: &[proto::NetworkAttachment]) -> Result<(), AgentError> {
        let Some(first) = attachments.first() else {
            return Err(AgentError::Protocol(
                "DHCP requires a network attachment".to_owned(),
            ));
        };
        if attachments.iter().any(|attachment| {
            attachment.subnet_cidr != first.subnet_cidr
                || attachment.gateway_ipv4 != first.gateway_ipv4
        }) {
            return Err(AgentError::Protocol(
                "multiple network subnets are not supported by the flat DHCP profile".to_owned(),
            ));
        }
        let gateway = first
            .gateway_ipv4
            .parse()
            .map_err(|_| AgentError::Protocol("DHCP gateway address is invalid".to_owned()))?;
        let expected = o3k_dhcp::DhcpConfig {
            subnet: first.subnet_cidr.clone(),
            gateway,
            dns: vec![gateway],
            interface: self.interface.clone(),
            lease_seconds: 3600,
        };
        if let Some(existing) = self.service.configuration() {
            if existing != &expected {
                return Err(AgentError::Protocol(
                    "the managed bridge already has a different DHCP subnet".to_owned(),
                ));
            }
        }
        for attachment in attachments {
            let address: Ipv4Addr = attachment
                .fixed_ipv4
                .parse()
                .map_err(|_| AgentError::Protocol("DHCP fixed address is invalid".to_owned()))?;
            if let Some(existing) = self.service.binding(&attachment.port_id) {
                if existing.mac != attachment.mac || existing.address != address {
                    return Err(AgentError::Protocol(
                        "DHCP port binding conflicts with durable state".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Applies only new bindings and returns those identities for precise rollback.
    fn apply(
        &mut self,
        attachments: &[proto::NetworkAttachment],
    ) -> Result<Vec<String>, AgentError> {
        self.validate(attachments)?;
        let first = attachments
            .first()
            .ok_or_else(|| AgentError::Protocol("DHCP requires a network attachment".to_owned()))?;
        let gateway = first
            .gateway_ipv4
            .parse()
            .map_err(|_| AgentError::Protocol("DHCP gateway address is invalid".to_owned()))?;
        if self.service.configuration().is_none() {
            self.service
                .configure(o3k_dhcp::DhcpConfig {
                    subnet: first.subnet_cidr.clone(),
                    gateway,
                    dns: vec![gateway],
                    interface: self.interface.clone(),
                    lease_seconds: 3600,
                })
                .map_err(|_| AgentError::Protocol("DHCP configuration is invalid".to_owned()))?;
        }
        let mut added = Vec::new();
        for attachment in attachments {
            if self.service.binding(&attachment.port_id).is_some() {
                continue;
            }
            let address = attachment
                .fixed_ipv4
                .parse()
                .map_err(|_| AgentError::Protocol("DHCP fixed address is invalid".to_owned()))?;
            if let Err(error) = self.service.upsert_binding(o3k_dhcp::Binding {
                port_id: attachment.port_id.clone(),
                mac: attachment.mac.clone(),
                address,
            }) {
                let _ = self.remove_ports(&added);
                return Err(AgentError::Protocol(format!(
                    "DHCP binding failed: {error}"
                )));
            }
            added.push(attachment.port_id.clone());
        }
        if let Some(supervisor) = self.supervisor.as_mut() {
            self.service
                .reload(supervisor)
                .map_err(|_| AgentError::Protocol("DHCP reload failed".to_owned()))?;
        } else {
            self.supervisor = Some(
                self.service
                    .start(&self.binary)
                    .map_err(|_| AgentError::Protocol("DHCP start failed".to_owned()))?,
            );
        }
        Ok(added)
    }

    fn remove_ports(&mut self, ports: &[String]) -> Result<(), AgentError> {
        for port in ports {
            self.service
                .remove_binding(port)
                .map_err(|_| AgentError::Protocol("DHCP binding cleanup failed".to_owned()))?;
        }
        self.service
            .write_config()
            .map_err(|_| AgentError::Protocol("DHCP configuration cleanup failed".to_owned()))?;
        if self.service.bindings().next().is_none() {
            if let Some(mut supervisor) = self.supervisor.take() {
                supervisor
                    .stop()
                    .map_err(|_| AgentError::Protocol("DHCP stop failed".to_owned()))?;
            }
        } else if let Some(supervisor) = self.supervisor.as_mut() {
            self.service
                .reload(supervisor)
                .map_err(|_| AgentError::Protocol("DHCP reload failed".to_owned()))?;
        }
        Ok(())
    }

    fn start_after_restart(
        &mut self,
        network: &o3k_network::HostNetworkManager,
    ) -> Result<(), AgentError> {
        if self.service.bindings().next().is_none() || self.supervisor.is_some() {
            return Ok(());
        }
        let config = self.service.configuration().cloned().ok_or_else(|| {
            AgentError::Protocol("DHCP bindings exist without configuration".to_owned())
        })?;
        let prefix_len = config
            .subnet
            .split_once('/')
            .and_then(|(_, prefix)| prefix.parse().ok())
            .ok_or_else(|| AgentError::Protocol("DHCP subnet prefix is invalid".to_owned()))?;
        network
            .ensure_gateway(o3k_network::GatewaySpec {
                address: config.gateway,
                prefix_len,
            })
            .map_err(|_| AgentError::Protocol("managed DHCP gateway is unavailable".to_owned()))?;
        self.supervisor = Some(
            self.service
                .start(&self.binary)
                .map_err(|_| AgentError::Protocol("DHCP restart failed".to_owned()))?,
        );
        Ok(())
    }
}

struct NetworkPreparation {
    created_taps: Vec<o3k_network::TapSpec>,
    added_dhcp_ports: Vec<String>,
}

fn rollback_network(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
) -> Result<(), AgentError> {
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

fn return_after_network_rollback(
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

fn cleanup_instance_network(
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

fn prepare_network(
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

/// Host-local evidence required to turn a create request into a libvirt
/// definition.  The path is supplied by the agent's committed artifact store;
/// it is never derived from an artifact id or digest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommittedArtifact {
    artifact_id: String,
    kind: proto::ArtifactKind,
    format: String,
    sha256: String,
    path: PathBuf,
}

/// A TAP name is usable only together with the network subsystem's ownership
/// evidence.  A port id and MAC address alone are not sufficient proof that a
/// host device may be attached to a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedTap {
    port_id: String,
    tap_name: String,
    mac_address: String,
    ownership_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateDomainIdentity {
    server_id: String,
    project_id: String,
    generation: u64,
    operation_id: String,
    managed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommittedCreateInputs {
    image: CommittedArtifact,
    config_drive: CommittedArtifact,
    owned_taps: Vec<OwnedTap>,
    identity: CreateDomainIdentity,
}

/// Resolve the host-local inputs for a create command before touching
/// libvirt.
///
/// The control-plane command deliberately carries artifact references, not
/// host paths.  The agent-side artifact store also requires the complete
/// authenticated `ArtifactOffer` (including its transfer identity and
/// expiry) to resolve a committed file.  Those fields are not part of
/// `CreateCommand.resolved`, so deriving a path from a digest or rebuilding an
/// offer here would weaken the transfer identity fence.  Network attachments
/// likewise contain only port/MAC/IP data; a libvirt interface requires a TAP
/// name that has been proven to be owned by the host network subsystem.
///
/// Keep this boundary explicit and fail closed until both authenticated
/// lookup metadata and a durable network-ownership lookup are present in the
/// command/executor contract.
fn resolve_create_domain_spec(
    command: &proto::Command,
    committed: Option<&CommittedCreateInputs>,
) -> Result<o3k_libvirt::DomainSpec, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol(
            "create command action is missing or has the wrong type".to_owned(),
        ));
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(AgentError::Protocol(
            "create command resolved inputs are missing".to_owned(),
        ));
    };
    if resolved.image_artifact_id.trim().is_empty()
        || resolved.image_sha256.trim().is_empty()
        || resolved.image_format.trim().is_empty()
        || resolved.config_drive_artifact_id.trim().is_empty()
        || resolved.config_drive_sha256.trim().is_empty()
    {
        return Err(AgentError::Protocol(
            "create command artifact references are incomplete".to_owned(),
        ));
    }

    let Some(committed) = committed else {
        return Err(AgentError::Protocol(
            "create is fail-closed: committed artifact bytes and owned TAP names are not available"
                .to_owned(),
        ));
    };

    if committed.image.artifact_id != resolved.image_artifact_id
        || committed.image.kind != proto::ArtifactKind::ImageBase
        || committed.image.sha256 != resolved.image_sha256
        || committed.image.format != resolved.image_format
        || committed.config_drive.artifact_id != resolved.config_drive_artifact_id
        || committed.config_drive.kind != proto::ArtifactKind::ConfigDriveIso
        || committed.config_drive.sha256 != resolved.config_drive_sha256
        || committed.config_drive.format != "iso"
    {
        return Err(AgentError::Protocol(
            "committed artifact evidence does not match create references".to_owned(),
        ));
    }
    if committed.identity.server_id != command.resource_id
        || committed.identity.project_id.trim().is_empty()
        || committed.identity.operation_id != command.operation_id
        || committed.identity.managed_by.trim().is_empty()
    {
        return Err(AgentError::Protocol(
            "create domain ownership identity is incomplete or mismatched".to_owned(),
        ));
    }
    if committed.image.path.as_os_str().is_empty()
        || !committed.image.path.is_absolute()
        || committed.config_drive.path.as_os_str().is_empty()
        || !committed.config_drive.path.is_absolute()
    {
        return Err(AgentError::Protocol(
            "committed artifact paths must be absolute host-local paths".to_owned(),
        ));
    }
    if committed.owned_taps.len() != resolved.network_attachments.len()
        || committed
            .owned_taps
            .iter()
            .any(|tap| tap.ownership_token.trim().is_empty())
    {
        return Err(AgentError::Protocol(
            "owned TAP evidence is incomplete or does not cover network attachments".to_owned(),
        ));
    }
    let network_interfaces = resolved
        .network_attachments
        .iter()
        .map(|attachment| {
            let tap = committed
                .owned_taps
                .iter()
                .find(|tap| tap.port_id == attachment.port_id);
            let Some(tap) = tap else {
                return Err(AgentError::Protocol(
                    "network attachment has no matching owned TAP".to_owned(),
                ));
            };
            if tap.mac_address != attachment.mac || tap.tap_name.trim().is_empty() {
                return Err(AgentError::Protocol(
                    "owned TAP evidence does not match network attachment".to_owned(),
                ));
            }
            Ok(o3k_libvirt::DomainNetworkInterface {
                tap_name: tap.tap_name.clone(),
                mac_address: tap.mac_address.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let spec = o3k_libvirt::DomainSpec {
        metadata: o3k_libvirt::DomainMetadata {
            server_id: committed.identity.server_id.clone(),
            project_id: committed.identity.project_id.clone(),
            generation: committed.identity.generation,
            operation_id: committed.identity.operation_id.clone(),
            managed_by: committed.identity.managed_by.clone(),
        },
        vcpus: resolved.vcpus,
        memory_mib: resolved.memory_mib,
        image_id: committed.image.path.to_string_lossy().into_owned(),
        config_drive_image: Some(o3k_libvirt::ConfigDriveImage {
            path: committed.config_drive.path.to_string_lossy().into_owned(),
            sha256: committed.config_drive.sha256.clone(),
        }),
        network_interfaces,
    };
    o3k_libvirt::build_domain_xml(&spec)
        .map(|_| spec)
        .map_err(|_| {
            AgentError::Protocol("resolved domain inputs failed libvirt validation".to_owned())
        })
}

fn resolve_committed_create_inputs(
    command: &proto::Command,
    artifact_root: &std::path::Path,
    network: &o3k_network::HostNetworkManager,
) -> Result<CommittedCreateInputs, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol("create action is missing".to_owned()));
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(AgentError::Protocol(
            "resolved create inputs are missing".to_owned(),
        ));
    };
    let store = o3k_compute_agent::ArtifactStore::open(artifact_root, &command.agent_id)
        .map_err(|_| AgentError::Protocol("agent artifact store is unavailable".to_owned()))?;
    let image_path = store
        .resolve_committed_artifact(&o3k_compute_agent::CommittedArtifactQuery {
            command_id: command.command_id.clone(),
            operation_id: command.operation_id.clone(),
            resource_id: command.resource_id.clone(),
            artifact_id: resolved.image_artifact_id.clone(),
            kind: proto::ArtifactKind::ImageBase,
            sha256: resolved.image_sha256.clone(),
            format: resolved.image_format.clone(),
        })
        .map_err(|_| AgentError::Protocol("committed image artifact is unavailable".to_owned()))?;
    let config_path = store
        .resolve_committed_artifact(&o3k_compute_agent::CommittedArtifactQuery {
            command_id: command.command_id.clone(),
            operation_id: command.operation_id.clone(),
            resource_id: command.resource_id.clone(),
            artifact_id: resolved.config_drive_artifact_id.clone(),
            kind: proto::ArtifactKind::ConfigDriveIso,
            sha256: resolved.config_drive_sha256.clone(),
            format: "iso".to_owned(),
        })
        .map_err(|_| {
            AgentError::Protocol("committed config-drive artifact is unavailable".to_owned())
        })?;
    let mut owned_taps = Vec::with_capacity(resolved.network_attachments.len());
    for attachment in &resolved.network_attachments {
        let tap_name = network
            .resolve_owned_tap(&o3k_network::TapSpec {
                instance_id: command.resource_id.clone(),
                port_id: attachment.port_id.clone(),
                mac: attachment.mac.clone(),
            })
            .map_err(|_| AgentError::Protocol("owned TAP is unavailable".to_owned()))?;
        owned_taps.push(OwnedTap {
            port_id: attachment.port_id.clone(),
            tap_name,
            mac_address: attachment.mac.clone(),
            ownership_token: "durable-network-manifest".to_owned(),
        });
    }
    Ok(CommittedCreateInputs {
        image: CommittedArtifact {
            artifact_id: resolved.image_artifact_id.clone(),
            kind: proto::ArtifactKind::ImageBase,
            format: resolved.image_format.clone(),
            sha256: resolved.image_sha256.clone(),
            path: image_path,
        },
        config_drive: CommittedArtifact {
            artifact_id: resolved.config_drive_artifact_id.clone(),
            kind: proto::ArtifactKind::ConfigDriveIso,
            format: "iso".to_owned(),
            sha256: resolved.config_drive_sha256.clone(),
            path: config_path,
        },
        owned_taps,
        identity: CreateDomainIdentity {
            server_id: command.resource_id.clone(),
            project_id: resolved.project_id.clone(),
            generation: 1,
            operation_id: command.operation_id.clone(),
            managed_by: "o3k-compute".to_owned(),
        },
    })
}

#[async_trait]
impl CommandExecutor for LibvirtCommandExecutor {
    async fn execute(
        &self,
        command: &proto::Command,
    ) -> Result<CommandExecutionResult, AgentError> {
        let name = stable_domain_name(&command.resource_id);
        let success = |message: &str, resource_state: proto::ResourceState| {
            Ok(CommandExecutionResult {
                state: proto::OperationState::Succeeded as i32,
                error_category: proto::ErrorCategory::Unspecified as i32,
                resource_state: resource_state as i32,
                redacted_message: message.to_owned(),
                provider_resource_id: name.clone(),
                console_log: None,
            })
        };
        match command.action.as_ref() {
            Some(proto::command::Action::Inspect(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(inspection) => inspection,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        return Ok(inspect_not_found_result(name));
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                success(
                    if inspection.active {
                        "domain is active"
                    } else {
                        "domain is inactive"
                    },
                    resource_state(&inspection),
                )
            }
            Some(proto::command::Action::Start(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(value) => value,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        return Err(agent_error(error));
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .start(name.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success("domain started", resource_state(&inspection))
            }
            Some(proto::command::Action::Stop(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .shutdown(name.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success("domain stopped", resource_state(&inspection))
            }
            Some(proto::command::Action::Reboot(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .reboot(name.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success("domain rebooted", resource_state(&inspection))
            }
            Some(proto::command::Action::Delete(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(value) => value,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        cleanup_instance_network(&self.network, &self.dhcp, &command.resource_id)?;
                        return success("domain already absent", proto::ResourceState::Deleted);
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                if inspection.active {
                    self.adapter
                        .force_stop(name.clone())
                        .await
                        .map_err(agent_error)?;
                }
                self.adapter
                    .undefine(name.clone())
                    .await
                    .map_err(agent_error)?;
                cleanup_instance_network(&self.network, &self.dhcp, &command.resource_id)?;
                success("domain deleted", proto::ResourceState::Deleted)
            }
            Some(proto::command::Action::Create(_)) => {
                let preparation = prepare_network(command, &self.network, &self.dhcp)?;
                match self.adapter.inspect(name.clone()).await {
                    Ok(existing) => {
                        if let Err(error) = verify_owned_domain(&existing, &command.resource_id) {
                            return Err(return_after_network_rollback(
                                &self.network,
                                &self.dhcp,
                                &preparation,
                                error,
                            ));
                        }
                        return success("domain already exists", resource_state(&existing));
                    }
                    Err(error) if error.category == ErrorCategory::NotFound => {}
                    Err(error) => {
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            agent_error(error),
                        ));
                    }
                }
                let committed = match resolve_committed_create_inputs(
                    command,
                    &self.artifact_root,
                    &self.network,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            error,
                        ));
                    }
                };
                let spec = match resolve_create_domain_spec(command, Some(&committed)) {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            error,
                        ));
                    }
                };
                let definition = match o3k_libvirt::build_domain_xml(&spec) {
                    Ok(value) => value,
                    Err(_) => {
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            AgentError::Protocol("domain XML is invalid".to_owned()),
                        ));
                    }
                };
                let definition_name = definition.name.clone();
                if let Err(error) = self
                    .adapter
                    .define(o3k_libvirt::DomainDefinition {
                        name: definition_name.clone(),
                        xml: definition.xml,
                    })
                    .await
                {
                    return Err(return_after_network_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        agent_error(error),
                    ));
                }
                if let Err(error) = self.adapter.start(definition_name.clone()).await {
                    let undefine_result = self.adapter.undefine(definition_name.clone()).await;
                    let error = match undefine_result {
                        Ok(()) => agent_error(error),
                        Err(cleanup_error) => AgentError::Protocol(format!(
                            "{}; domain rollback also failed: {}",
                            agent_error(error),
                            cleanup_error
                        )),
                    };
                    return Err(return_after_network_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        error,
                    ));
                }
                let inspection = match self.adapter.inspect(definition_name).await {
                    Ok(value) => value,
                    Err(error) => {
                        let error = match self.adapter.undefine(name.clone()).await {
                            Ok(()) => agent_error(error),
                            Err(cleanup_error) => AgentError::Protocol(format!(
                                "{}; domain rollback also failed: {}",
                                agent_error(error),
                                cleanup_error
                            )),
                        };
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            error,
                        ));
                    }
                };
                if let Err(error) = verify_owned_domain(&inspection, &command.resource_id) {
                    let error = match self.adapter.undefine(name.clone()).await {
                        Ok(()) => error,
                        Err(cleanup_error) => AgentError::Protocol(format!(
                            "{error}; domain rollback also failed: {cleanup_error}"
                        )),
                    };
                    return Err(return_after_network_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        error,
                    ));
                }
                success("domain created", resource_state(&inspection))
            }
            Some(proto::command::Action::ConsoleLog(request)) => {
                if request.offset > 0 {
                    return Err(AgentError::Protocol(
                        "libvirt console snapshots only support offset zero".to_owned(),
                    ));
                }
                let max_bytes = usize::try_from(request.max_bytes)
                    .map_err(|_| AgentError::Protocol("console bound is invalid".to_owned()))?
                    .min(o3k_console::MAX_CONSOLE_BYTES);
                if max_bytes == 0 {
                    return Err(AgentError::Protocol("console bound is invalid".to_owned()));
                }
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                let bytes = self
                    .adapter
                    .read_console(name.clone(), max_bytes, command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                Ok(CommandExecutionResult {
                    state: proto::OperationState::Succeeded as i32,
                    error_category: proto::ErrorCategory::Unspecified as i32,
                    resource_state: resource_state(&inspection) as i32,
                    redacted_message: "libvirt console output read".to_owned(),
                    provider_resource_id: name,
                    console_log: Some(ConsoleLogResult {
                        truncated: bytes.len() == max_bytes,
                        complete: bytes.len() < max_bytes,
                        offset: 0,
                        bytes,
                    }),
                })
            }
            None => Err(AgentError::Protocol("command action is missing".to_owned())),
        }
    }
}

fn inspect_not_found_result(provider_resource_id: String) -> CommandExecutionResult {
    CommandExecutionResult {
        state: proto::OperationState::Failed as i32,
        error_category: proto::ErrorCategory::NotFound as i32,
        resource_state: proto::ResourceState::Error as i32,
        redacted_message: "requested domain was not found".to_owned(),
        provider_resource_id,
        console_log: None,
    }
}

fn resource_state(inspection: &o3k_libvirt::DomainInspection) -> proto::ResourceState {
    match o3k_libvirt::project_domain_state(inspection.active, &inspection.state) {
        o3k_provider::InstanceState::Running => proto::ResourceState::Running,
        o3k_provider::InstanceState::Stopped => proto::ResourceState::Stopped,
        o3k_provider::InstanceState::Creating => proto::ResourceState::Creating,
        o3k_provider::InstanceState::Deleting => proto::ResourceState::Deleting,
        o3k_provider::InstanceState::Deleted => proto::ResourceState::Deleted,
        o3k_provider::InstanceState::Error => proto::ResourceState::Error,
    }
}

fn verify_owned_domain(
    inspection: &o3k_libvirt::DomainInspection,
    expected_server_id: &str,
) -> Result<(), AgentError> {
    match o3k_libvirt::discover_domain_xml(&inspection.name, &inspection.xml) {
        o3k_libvirt::DiscoveryResult::Owned { metadata, .. }
            if metadata.server_id == expected_server_id =>
        {
            Ok(())
        }
        _ => Err(AgentError::Protocol(
            "libvirt domain ownership verification failed".to_owned(),
        )),
    }
}

fn agent_error(_error: o3k_libvirt::LibvirtError) -> AgentError {
    AgentError::Protocol("libvirt command failed".to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = config_from_env()?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let libvirt = LibvirtAdapter::new(LibvirtConfig::default())?;
    let (libvirt_ready, libvirt_error) = match libvirt.capabilities().await {
        Ok(capabilities) => {
            let max_disk_gb = config.capabilities.max_disk_gb;
            config.capabilities = o3k_provider_contract::compute_proto::Capabilities {
                max_disk_gb,
                ..capabilities.to_protocol_capabilities()
            };
            (true, None)
        }
        Err(error) => {
            let message = error.to_string();
            tracing::warn!(error = %message, "local libvirt is unavailable");
            (false, Some(message))
        }
    };
    let agent = AgentClient::new(config.clone())?;
    let artifact_root = agent.identity_file().with_extension("artifacts");
    let network_root = agent
        .identity_file()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("network");
    let network = o3k_network::HostNetworkManager::with_ownership_root(
        o3k_network::HostNetworkConfig {
            bridge_name: env::var("O3K_COMPUTE_BRIDGE_NAME")
                .unwrap_or_else(|_| "o3k-br0".to_owned()),
            uplink: env::var("O3K_COMPUTE_UPLINK").ok(),
        },
        network_root,
    )?;
    let bridge_name = env::var("O3K_COMPUTE_BRIDGE_NAME").unwrap_or_else(|_| "o3k-br0".to_owned());
    let service_root = agent
        .identity_file()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let dhcp = Arc::new(Mutex::new(DhcpRuntime::open(
        service_root.join("dhcp"),
        env::var("O3K_COMPUTE_DHCP_BINARY").unwrap_or_else(|_| "dnsmasq".to_owned()),
        bridge_name,
    )?));
    dhcp.lock()
        .map_err(|_| "DHCP runtime lock is poisoned")?
        .start_after_restart(&network)
        .map_err(|error| format!("DHCP reconciliation failed: {error}"))?;
    let executor = Arc::new(LibvirtCommandExecutor {
        adapter: libvirt.clone(),
        artifact_root,
        network,
        dhcp,
    });
    info!(endpoint = %config.endpoint, host_label = %config.host_label, "o3k-compute starting");
    let health_addr = env::var("O3K_COMPUTE_HEALTH_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9100".to_owned())
        .parse::<SocketAddr>()?;
    let state = HealthState {
        agent: agent.clone(),
        libvirt_ready,
        libvirt_error,
    };
    let health_server = axum::serve(TcpListener::bind(health_addr).await?, health_router(state));
    tokio::select! {
        result = agent.run_with_executor(shutdown_signal(), executor) => { result?; }
        result = health_server.with_graceful_shutdown(shutdown_signal()) => { result?; }
    }
    info!("o3k-compute stopped");
    Ok(())
}

fn health_router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "{\"status\":\"alive\"}\n")
}

async fn readiness(State(state): State<HealthState>) -> impl IntoResponse {
    if state.agent.is_ready() && state.libvirt_ready {
        (StatusCode::OK, "{\"status\":\"ready\"}\n".to_owned())
    } else {
        let error = state
            .libvirt_error
            .as_deref()
            .unwrap_or("control plane is not connected");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{{\"status\":\"not_ready\",\"reason\":{}}}\n",
                serde_json::to_string(error).unwrap_or_else(|_| "\"unavailable\"".to_owned())
            ),
        )
    }
}

async fn metrics(State(state): State<HealthState>) -> impl IntoResponse {
    let ready = u8::from(state.agent.is_ready() && state.libvirt_ready);
    (StatusCode::OK, format!("o3k_compute_ready {ready}\n"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

fn config_from_env() -> Result<AgentConfig, Box<dyn std::error::Error>> {
    let data_dir = PathBuf::from(
        env::var("O3K_COMPUTE_DATA_DIR").unwrap_or_else(|_| "./compute-data".to_owned()),
    );
    let endpoint = env::var("O3K_COMPUTE_CONTROL_ENDPOINT")
        .unwrap_or_else(|_| "https://127.0.0.1:50051".to_owned());
    let server_name =
        env::var("O3K_COMPUTE_SERVER_NAME").unwrap_or_else(|_| "o3k-control-plane".to_owned());
    let host_label =
        env::var("O3K_COMPUTE_HOST_LABEL").unwrap_or_else(|_| "compute-host".to_owned());
    let software_version =
        env::var("O3K_COMPUTE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    let tls_dir = PathBuf::from(
        env::var("O3K_COMPUTE_TLS_DIR").unwrap_or_else(|_| "./compute-tls".to_owned()),
    );
    Ok(AgentConfig {
        endpoint,
        server_name,
        tls: TlsFiles {
            ca_certificate: tls_dir.join("ca.pem"),
            certificate: tls_dir.join("agent.pem"),
            private_key: tls_dir.join("agent-key.pem"),
        },
        identity_file: data_dir.join("agent-id"),
        host_label,
        software_version,
        heartbeat_interval: Duration::from_secs(5),
        max_reconnect_delay: Duration::from_secs(30),
        capabilities: o3k_provider_contract::compute_proto::Capabilities {
            architecture: env::consts::ARCH.to_owned(),
            agent_provider_name: "o3k-compute".to_owned(),
            agent_provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            max_disk_gb: env::var("O3K_COMPUTE_MAX_DISK_GB")
                .unwrap_or_else(|_| "0".to_owned())
                .parse()?,
            ..Default::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network_attachment(
        port_id: &str,
        fixed_ipv4: &str,
        subnet_cidr: &str,
        gateway_ipv4: &str,
    ) -> proto::NetworkAttachment {
        proto::NetworkAttachment {
            port_id: port_id.to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
            fixed_ipv4: fixed_ipv4.to_owned(),
            subnet_cidr: subnet_cidr.to_owned(),
            gateway_ipv4: gateway_ipv4.to_owned(),
        }
    }

    fn inspection(xml: &str) -> o3k_libvirt::DomainInspection {
        o3k_libvirt::DomainInspection {
            name: "o3k-domain".to_owned(),
            active: false,
            persistent: true,
            state: "shutoff".to_owned(),
            max_memory_kib: 128 * 1024,
            vcpus: 1,
            xml: xml.to_owned(),
        }
    }

    #[test]
    fn absent_domain_inspection_is_a_redacted_not_found_failure() {
        let result = inspect_not_found_result("o3k-domain".to_owned());
        assert_eq!(result.state, proto::OperationState::Failed as i32);
        assert_eq!(result.error_category, proto::ErrorCategory::NotFound as i32);
        assert_eq!(result.resource_state, proto::ResourceState::Error as i32);
        assert_eq!(result.redacted_message, "requested domain was not found");
        assert_eq!(result.provider_resource_id, "o3k-domain");
        assert!(result.console_log.is_none());
    }

    #[test]
    fn lifecycle_mutations_require_matching_owned_metadata() {
        let xml = "<domain><metadata><o3k:domain xmlns:o3k=\"urn:o3k:compute:domain\" server_id=\"server-1\" project_id=\"project\" generation=\"1\" operation_id=\"operation\" managed_by=\"o3k-compute\" /></metadata></domain>";
        assert!(verify_owned_domain(&inspection(xml), "server-1").is_ok());
        assert!(verify_owned_domain(&inspection(xml), "server-2").is_err());
        assert!(verify_owned_domain(&inspection("<domain />"), "server-1").is_err());
    }

    #[test]
    fn console_observation_requires_matching_owned_metadata() {
        let owned = "<domain><metadata><o3k:domain xmlns:o3k=\"urn:o3k:compute:domain\" server_id=\"server-console\" project_id=\"project\" generation=\"1\" operation_id=\"operation\" managed_by=\"o3k-compute\" /></metadata></domain>";
        assert!(verify_owned_domain(&inspection(owned), "server-console").is_ok());
        assert!(verify_owned_domain(&inspection(owned), "other-project-server").is_err());
        assert!(
            verify_owned_domain(
                &inspection("<domain><metadata /></domain>"),
                "server-console"
            )
            .is_err()
        );
    }

    #[test]
    fn dhcp_runtime_rejects_mixed_flat_networks_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!(
            "o3k-compute-dhcp-validation-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let runtime = DhcpRuntime::open(&root, "/does/not/exist", "o3k-br0".to_owned())?;
        let attachments = vec![
            network_attachment("port-1", "192.0.2.2", "192.0.2.0/29", "192.0.2.1"),
            network_attachment("port-2", "198.51.100.2", "198.51.100.0/29", "198.51.100.1"),
        ];
        assert!(runtime.validate(&attachments).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn create_fails_closed_when_transfer_and_tap_ownership_are_not_resolvable() {
        let command = proto::Command {
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                image_id: "image".to_owned(),
                flavor_id: "flavor".to_owned(),
                network_port_ids: vec!["port-1".to_owned()],
                resolved: Some(proto::ResolvedCreateInputs {
                    image_artifact_id: "image-artifact".to_owned(),
                    image_sha256: "a".repeat(64),
                    image_format: "qcow2".to_owned(),
                    vcpus: 1,
                    memory_mib: 512,
                    disk_gib: 1,
                    config_drive_artifact_id: "config-artifact".to_owned(),
                    config_drive_sha256: "b".repeat(64),
                    project_id: "project-1".to_owned(),
                    network_attachments: vec![proto::NetworkAttachment {
                        port_id: "port-1".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        fixed_ipv4: "192.0.2.10".to_owned(),
                        subnet_cidr: "192.0.2.0/24".to_owned(),
                        gateway_ipv4: "192.0.2.1".to_owned(),
                    }],
                }),
            })),
            ..Default::default()
        };

        let result = resolve_create_domain_spec(&command, None);
        assert!(result.is_err());
        if let Err(error) = result {
            assert!(error.to_string().contains("committed artifact bytes"));
            assert!(error.to_string().contains("owned TAP names"));
        }
    }

    #[test]
    fn create_rejects_missing_resolved_artifacts_before_any_host_lookup() {
        let command = proto::Command {
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                resolved: Some(proto::ResolvedCreateInputs {
                    image_artifact_id: String::new(),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };

        let result = resolve_create_domain_spec(&command, None);
        assert!(result.is_err());
        if let Err(error) = result {
            assert!(
                error
                    .to_string()
                    .contains("artifact references are incomplete")
            );
        }
    }

    #[test]
    fn typed_contract_rejects_artifact_identity_mismatch() {
        let command = proto::Command {
            command_id: "command-1".to_owned(),
            operation_id: "operation-1".to_owned(),
            resource_id: "server-1".to_owned(),
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                resolved: Some(proto::ResolvedCreateInputs {
                    image_artifact_id: "image-artifact".to_owned(),
                    image_sha256: "a".repeat(64),
                    image_format: "qcow2".to_owned(),
                    vcpus: 1,
                    memory_mib: 512,
                    config_drive_artifact_id: "config-artifact".to_owned(),
                    config_drive_sha256: "b".repeat(64),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        let committed = CommittedCreateInputs {
            image: CommittedArtifact {
                artifact_id: "different-image".to_owned(),
                kind: proto::ArtifactKind::ImageBase,
                format: "qcow2".to_owned(),
                sha256: "a".repeat(64),
                path: PathBuf::from("/var/lib/o3k/artifacts/image.qcow2"),
            },
            config_drive: CommittedArtifact {
                artifact_id: "config-artifact".to_owned(),
                kind: proto::ArtifactKind::ConfigDriveIso,
                format: "iso".to_owned(),
                sha256: "b".repeat(64),
                path: PathBuf::from("/var/lib/o3k/artifacts/config.iso"),
            },
            owned_taps: Vec::new(),
            identity: CreateDomainIdentity {
                server_id: "server-1".to_owned(),
                project_id: "project-1".to_owned(),
                generation: 1,
                operation_id: "operation-1".to_owned(),
                managed_by: "o3k-compute".to_owned(),
            },
        };

        let result = resolve_create_domain_spec(&command, Some(&committed));
        assert!(result.is_err());
        if let Err(error) = result {
            assert!(error.to_string().contains("does not match"));
        }
    }

    #[test]
    fn typed_contract_rejects_unowned_tap_even_with_matching_port_data() {
        let command = proto::Command {
            command_id: "command-1".to_owned(),
            operation_id: "operation-1".to_owned(),
            resource_id: "server-1".to_owned(),
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                resolved: Some(proto::ResolvedCreateInputs {
                    image_artifact_id: "image-artifact".to_owned(),
                    image_sha256: "a".repeat(64),
                    image_format: "qcow2".to_owned(),
                    vcpus: 1,
                    memory_mib: 512,
                    config_drive_artifact_id: "config-artifact".to_owned(),
                    config_drive_sha256: "b".repeat(64),
                    network_attachments: vec![proto::NetworkAttachment {
                        port_id: "port-1".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        fixed_ipv4: "192.0.2.10".to_owned(),
                        subnet_cidr: "192.0.2.0/24".to_owned(),
                        gateway_ipv4: "192.0.2.1".to_owned(),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        let committed = CommittedCreateInputs {
            image: CommittedArtifact {
                artifact_id: "image-artifact".to_owned(),
                kind: proto::ArtifactKind::ImageBase,
                format: "qcow2".to_owned(),
                sha256: "a".repeat(64),
                path: PathBuf::from("/var/lib/o3k/artifacts/image.qcow2"),
            },
            config_drive: CommittedArtifact {
                artifact_id: "config-artifact".to_owned(),
                kind: proto::ArtifactKind::ConfigDriveIso,
                format: "iso".to_owned(),
                sha256: "b".repeat(64),
                path: PathBuf::from("/var/lib/o3k/artifacts/config.iso"),
            },
            owned_taps: vec![OwnedTap {
                port_id: "port-1".to_owned(),
                tap_name: "o3ktap-port1".to_owned(),
                mac_address: "02:00:00:00:00:01".to_owned(),
                ownership_token: String::new(),
            }],
            identity: CreateDomainIdentity {
                server_id: "server-1".to_owned(),
                project_id: "project-1".to_owned(),
                generation: 1,
                operation_id: "operation-1".to_owned(),
                managed_by: "o3k-compute".to_owned(),
            },
        };

        let result = resolve_create_domain_spec(&command, Some(&committed));
        assert!(result.is_err());
        if let Err(error) = result {
            assert!(error.to_string().contains("owned TAP evidence"));
        }
    }
}
