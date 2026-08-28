use async_trait::async_trait;
use o3kd::native_adapters;

use o3k_domain::ServerId;
use o3k_kernel::Controller;
use o3k_provider::{
    AgentNodeSnapshot, ArtifactKind, BlockDeviceAttachment, ComputeProvider, ConfigDriveRequest,
    CreateArtifactResolver, CreateInstanceRequest, OperationState, ProviderError,
    ResolvedCreateArtifact, ResolvedCreateInputs, ResolvedCreateResolver,
};
use o3k_store::{ComputeRepository, DurableStore, StorageRepository};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Debug, serde::Deserialize)]
struct ExternalControllerConfigFile {
    controllers: Vec<ExternalControllerConfig>,
}

#[derive(Debug, serde::Deserialize)]
struct ExternalControllerConfig {
    service_id: String,
    namespace: String,
    endpoint: String,
    server_name: String,
    ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    principal_id: String,
    principal_name: String,
    manifest_digest: String,
    manifest_generation: u64,
    #[serde(default)]
    delegation_key_id: Option<String>,
    #[serde(default)]
    delegation_signing_key_file: Option<PathBuf>,
}

struct LocalStorageFence;

#[async_trait]
impl o3k_reconciler::storage_workflow::StorageControllerFence for LocalStorageFence {
    async fn assert_current(
        &self,
        controller_epoch: u64,
    ) -> Result<(), o3k_reconciler::storage_workflow::StorageWorkflowError> {
        if controller_epoch == 0 {
            Err(o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence)
        } else {
            Ok(())
        }
    }
}

struct LocalComputeAttachmentExecutor {
    compute: Arc<o3k_compute::ComputeService>,
}

#[async_trait]
impl o3k_reconciler::storage_workflow::ComputeAttachmentExecutor
    for LocalComputeAttachmentExecutor
{
    async fn attach(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
        prepared: &o3k_storage::PreparedAttachment,
    ) -> Result<(), o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        let device = BlockDeviceAttachment {
            volume_id: attachment.volume_id.to_string(),
            attachment_id: attachment.id.to_string(),
            driver_volume_type: "local".to_owned(),
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            local_path: Some(prepared.device_path().to_owned()),
            device_path: None,
            multipath: false,
            initiator: None,
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };
        self.compute
            .provider()
            .attach_block_device(attachment.server_id, &device)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }

    async fn inspect(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
    ) -> Result<bool, o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        self.compute
            .provider()
            .observe_block_device(attachment.server_id, &attachment.volume_id.to_string())
            .await
            .map(|observation| observation.is_some_and(|value| value.attached))
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }

    async fn detach(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
    ) -> Result<(), o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        let device = BlockDeviceAttachment {
            volume_id: attachment.volume_id.to_string(),
            attachment_id: attachment.id.to_string(),
            driver_volume_type: "local".to_owned(),
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            local_path: None,
            device_path: None,
            multipath: false,
            initiator: None,
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };
        self.compute
            .provider()
            .detach_block_device(attachment.server_id, &device)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }
}

struct NativeStorageAttachmentWorkflow {
    store: Arc<o3k_store::O3kStore>,
    workflow: o3k_reconciler::storage_workflow::StorageAttachmentWorkflow<
        o3k_store::O3kStore,
        o3k_storage::LvmStorageProvider,
        LocalComputeAttachmentExecutor,
        LocalStorageFence,
    >,
}

#[async_trait]
impl o3k_api::NativeAttachmentWorkflow for NativeStorageAttachmentWorkflow {
    async fn attach(&self, attachment_id: Uuid) -> Result<(), String> {
        let record = self
            .store
            .get_volume_attachment_v1(attachment_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native attachment disappeared".to_owned())?;
        let attachment = record.attachment;
        let intent = native_storage_intent(&attachment, "attach");
        self.workflow
            .attach(intent)
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok(())
    }

    async fn detach(&self, attachment_id: Uuid) -> Result<(), String> {
        let record = self
            .store
            .get_volume_attachment_v1(attachment_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native attachment disappeared".to_owned())?;
        let attachment = record.attachment;
        self.workflow
            .detach(native_storage_intent(&attachment, "detach"))
            .await
            .map_err(|error| format!("{error:?}"))?;
        // The workflow has observed the provider-side detach and leaves the
        // durable record Detached.  Finalize the canonical child here before
        // releasing the volume, so a subsequent volume delete cannot mistake
        // a completed detach for an active attachment.
        if let Some(mut current) = self
            .store
            .get_volume_attachment_v1(attachment_id)
            .await
            .map_err(|error| error.to_string())?
        {
            current.attachment.state = o3k_domain::VolumeAttachmentState::Deleted;
            current.attachment.generation += 1;
            self.store
                .update_volume_attachment_v1(current.attachment.generation - 1, &current)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn recover(&self) -> Result<(), String> {
        for command in self
            .store
            .list_recoverable_agent_commands()
            .await
            .map_err(|error| error.to_string())?
        {
            let Ok(envelope) =
                serde_json::from_slice::<o3k_domain::StorageCommandEnvelope>(&command.payload)
            else {
                continue;
            };
            if self
                .store
                .get_volume_attachment_v1(command.resource_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
            {
                continue;
            }
            self.workflow
                .reconcile(&command.command_id, envelope.controller_epoch)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn native_storage_intent(
    attachment: &o3k_domain::VolumeAttachment,
    operation: &str,
) -> o3k_reconciler::storage_workflow::StorageAttachmentIntent {
    o3k_reconciler::storage_workflow::StorageAttachmentIntent {
        attachment_id: attachment.id,
        volume_id: attachment.volume_id,
        server_id: attachment.server_id,
        project_id: attachment.project_id.clone(),
        access_mode: attachment.access_mode,
        delete_on_termination: attachment.delete_on_termination,
        controller_epoch: 1,
        target_agent_id: "local".to_owned(),
        target_agent_epoch: 1,
        idempotency_key: format!("native-{operation}:{}", attachment.id),
        trace_id: format!("native-{operation}:{}", attachment.id),
        deadline: "2099-01-01T00:00:00.000".to_owned(),
    }
}

async fn external_controllers_from_config() -> Result<
    std::collections::BTreeMap<String, std::sync::Arc<o3k_service_sdk::GrpcControllerAdapter>>,
    Box<dyn std::error::Error>,
> {
    let Some(path) = std::env::var_os("O3K_EXTERNAL_CONTROLLER_CONFIG") else {
        return Ok(std::collections::BTreeMap::new());
    };
    let config: ExternalControllerConfigFile = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut controllers = std::collections::BTreeMap::new();
    for entry in config.controllers {
        let tls = o3k_service_sdk::tls::client(
            &entry.ca,
            &entry.client_certificate,
            &entry.client_key,
            &entry.server_name,
        )?;
        let principal = o3k_kernel::ServicePrincipal::new(
            o3k_kernel::PrincipalId::new(entry.principal_id)?,
            entry.principal_name,
            entry.namespace.clone(),
        );
        let controller = o3k_service_sdk::GrpcControllerAdapter::connect(
            &entry.endpoint,
            tls,
            entry.service_id.clone(),
            entry.namespace,
            principal,
            entry.manifest_digest,
            entry.manifest_generation,
        )
        .await?;
        let controller = match (entry.delegation_key_id, entry.delegation_signing_key_file) {
            (Some(key_id), Some(path)) => controller.with_delegation_signer(
                key_id,
                ed25519_dalek::SigningKey::from_bytes(
                    &fs::read(path)?
                        .try_into()
                        .map_err(|_| "delegation signing key must be 32 bytes")?,
                ),
            ),
            (None, None) => controller,
            _ => return Err("delegation key id and key file must be configured together".into()),
        };
        controllers.insert(entry.service_id, std::sync::Arc::new(controller));
    }
    Ok(controllers)
}

#[derive(Clone)]
struct DaemonCreateResolver {
    image: o3k_image::ImageService,
    network: o3k_network::NetworkService,
    config_drive: o3k_config_drive::ConfigDriveStore,
    network_dispatcher: Option<Arc<dyn o3k_network::NetworkPlanDispatcher>>,
    network_controller: o3k_network::NetworkControllerLease,
    network_external_realm_id: Option<Uuid>,
    network_agent: Option<o3k_network::NetworkAgentIdentity>,
}

/// Composition-root adapter for the bounded node-local network transport.
/// The network application remains transport-independent; this adapter only
/// maps its typed command envelope to the versioned mTLS wire protocol.
#[derive(Clone)]
struct NetworkAgentDispatcher {
    endpoint: String,
    server_name: String,
    ca_certificate: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
}

fn network_dispatcher_from_env()
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

fn placement_consumer_ids(resources: &[o3k_store::ResourceRecord]) -> Vec<String> {
    let mut ids = resources
        .iter()
        .filter(|resource| resource.observed_state != "DELETED")
        .map(|resource| resource.id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn unix_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn public_allocator_from_env(
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
struct NetworkBindingProjector {
    network: o3k_network::NetworkService,
    registry: Arc<dyn o3k_provider::AgentNodeRegistry>,
    network_dispatcher: Option<Arc<dyn o3k_network::NetworkPlanDispatcher>>,
    network_controller: o3k_network::NetworkControllerLease,
    network_external_realm_id: Option<Uuid>,
    network_agent: Option<o3k_network::NetworkAgentIdentity>,
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
            let deadline_unix_ms = unix_time_millis().saturating_add(30_000);
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
        let deadline_unix_ms = unix_time_millis().saturating_add(30_000);
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

impl DaemonCreateResolver {
    fn config_drive_iso_path(
        generated_directory: &std::path::Path,
        server_id: Uuid,
    ) -> Result<PathBuf, ProviderError> {
        let output_root = generated_directory
            .parent()
            .ok_or(ProviderError::InvalidRequest)?;
        Ok(output_root.join(format!("{server_id}.iso")))
    }

    async fn resolve_image(
        &self,
        request: &CreateInstanceRequest,
    ) -> Result<o3k_image::ImageArtifact, ProviderError> {
        let image_id = request
            .image_id
            .as_deref()
            .ok_or(ProviderError::InvalidRequest)?
            .parse::<Uuid>()
            .map_err(|_| ProviderError::InvalidRequest)?;
        self.image
            .resolve_artifact_for_project(&request.project_id, image_id)
            .await
            .map_err(|_| ProviderError::InvalidRequest)
    }

    async fn resolve_network(
        &self,
        request: &CreateInstanceRequest,
        agent_id: &str,
        agent_epoch: &str,
    ) -> Result<
        (
            Vec<o3k_compute_agent::NetworkAttachmentSpec>,
            BTreeMap<String, String>,
        ),
        ProviderError,
    > {
        let mut attachments = Vec::with_capacity(request.network_ids.len());
        let mut network_data = BTreeMap::new();
        for network_id in &request.network_ids {
            let port_id = network_id
                .parse::<Uuid>()
                .map_err(|_| ProviderError::InvalidRequest)?;
            let port = self
                .network
                .get_port_for_project(&request.project_id, port_id)
                .await
                .map_err(|_| ProviderError::InvalidRequest)?;
            let subnet = self
                .network
                .get_subnet_for_project(
                    &request.project_id,
                    port.subnet_id.ok_or(ProviderError::InvalidRequest)?,
                )
                .await
                .map_err(|_| ProviderError::InvalidRequest)?;
            // Record the selected-host intent only after the full attachment
            // resolved; a port whose subnet cannot be resolved is never
            // dispatched and must not carry a binding intent.
            let network_agent_id = self
                .network_agent
                .as_ref()
                .map_or(agent_id, |agent| agent.agent_id.as_str());
            self.network
                .record_binding_intent(&request.project_id, port_id, network_agent_id)
                .await
                .map_err(|error| match error {
                    o3k_network::NetworkError::Conflict => ProviderError::Conflict,
                    _ => ProviderError::InvalidRequest,
                })?;
            if let Some(dispatcher) = &self.network_dispatcher {
                let deadline_unix_ms = unix_time_millis().saturating_add(30_000);
                let plan = o3k_network::compile_attachment_plan(o3k_network::AttachmentPlanInput {
                    endpoint_id: port.id,
                    realm_id: port.subnet_id.ok_or(ProviderError::InvalidRequest)?,
                    project_id: &request.project_id,
                    mac: &port.mac_address,
                    fixed_ip: port.fixed_ip,
                    subnet_cidr: &subnet.cidr,
                    // The network plan is owned by the network execution
                    // agent, not by the selected compute host.  Keeping the
                    // plan node bound to the network agent lets the executor
                    // reject cross-agent replay without conflating compute
                    // placement with network mutation authority.
                    node_id: network_agent_id,
                    operation_id: request.operation_id,
                    deadline_unix_ms,
                    public_address: None,
                    external_realm_id: self.network_external_realm_id,
                    policies: self
                        .network
                        .list_policies_for_project(&request.project_id, port.network_id)
                        .await
                        .map_err(|_| ProviderError::InvalidRequest)?
                        .into_iter()
                        .filter(|policy| policy.endpoint_id == port.id)
                        .collect(),
                })
                .map_err(|_| ProviderError::InvalidRequest)?;
                let command_id = Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!(
                        "o3k:network:apply:{}:{}:{}",
                        request.operation_id, port.id, plan.fingerprint_sha256
                    )
                    .as_bytes(),
                );
                let status = dispatcher
                    .dispatch(o3k_network::NetworkPlanCommand {
                        command_id,
                        operation_id: request.operation_id,
                        idempotency_key: format!("{}:network:{}", request.idempotency_key, port.id),
                        action: o3k_network::NetworkPlanAction::Apply,
                        target: self.network_agent.clone().unwrap_or_else(|| {
                            o3k_network::NetworkAgentIdentity {
                                agent_id: agent_id.to_owned(),
                                agent_epoch: agent_epoch.to_owned(),
                            }
                        }),
                        controller: self.network_controller.clone(),
                        deadline_unix_ms,
                        plan,
                    })
                    .await
                    .map_err(|error| match error {
                        o3k_network::NetworkDispatchError::Unavailable
                        | o3k_network::NetworkDispatchError::Transport(_) => {
                            ProviderError::UnknownOutcome {
                                operation_id: request.operation_id,
                            }
                        }
                        o3k_network::NetworkDispatchError::Rejected(_) => {
                            ProviderError::InvalidRequest
                        }
                    })?;
                if status == o3k_network::NetworkPlanStatus::Unknown {
                    return Err(ProviderError::UnknownOutcome {
                        operation_id: request.operation_id,
                    });
                }
            }
            let port_id = port.id.to_string();
            let fixed_ip = port.fixed_ip.to_string();
            attachments.push(o3k_compute_agent::NetworkAttachmentSpec {
                port_id: port_id.clone(),
                mac: port.mac_address.clone(),
                fixed_ipv4: fixed_ip.clone(),
                subnet_cidr: subnet.cidr,
                gateway_ipv4: subnet.gateway_ip.to_string(),
            });
            network_data.insert(format!("{port_id}.mac"), port.mac_address);
            network_data.insert(format!("{port_id}.ipv4"), fixed_ip);
        }
        Ok((attachments, network_data))
    }

    fn config_drive_input(
        request: &CreateInstanceRequest,
        config: &ConfigDriveRequest,
        network_data: BTreeMap<String, String>,
    ) -> o3k_config_drive::ConfigDriveInput {
        let mut metadata = BTreeMap::new();
        metadata.insert("project_id".to_owned(), request.project_id.clone());
        metadata.insert("server_id".to_owned(), request.o3k_server_id.to_string());
        o3k_config_drive::ConfigDriveInput {
            instance_id: request.o3k_server_id.to_string(),
            hostname: request.name.clone(),
            ssh_public_key: config.ssh_public_key.clone(),
            user_data: config.user_data.clone(),
            metadata,
            network_data,
            vendor_data: config.vendor_data.clone(),
        }
    }

    fn materialize_config_drive(
        &self,
        request: &CreateInstanceRequest,
        network_data: BTreeMap<String, String>,
    ) -> Result<(o3k_config_drive::ConfigDriveIsoResult, Vec<u8>), ProviderError> {
        let config = request
            .config_drive
            .as_ref()
            .ok_or(ProviderError::InvalidRequest)?;
        let input = Self::config_drive_input(request, config, network_data);
        let generated = self
            .config_drive
            .generate(&input)
            .map_err(|_| ProviderError::InvalidRequest)?;
        // ConfigDriveStore authenticates the ISO against its managed root and
        // expects the published ISO beside the instance directory. Derive the
        // output location from the generated directory so the resolver cannot
        // accidentally place it in an unrelated root.
        let output = Self::config_drive_iso_path(&generated.directory, request.o3k_server_id)?;
        let iso = self
            .config_drive
            .materialize_iso(&generated.directory, output)
            .map_err(|_| ProviderError::InvalidRequest)?;
        let bytes = self
            .config_drive
            .read_verified_iso(&iso)
            .map_err(|_| ProviderError::InvalidRequest)?;
        Ok((iso, bytes))
    }
}

#[async_trait]
impl ResolvedCreateResolver for DaemonCreateResolver {
    async fn resolve(
        &self,
        request: &CreateInstanceRequest,
        agent: &AgentNodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError> {
        let image = self.resolve_image(request).await?;
        let (network_attachments, network_data) = self
            .resolve_network(request, &agent.agent_id, &agent.agent_epoch)
            .await?;
        let (iso, _) = self.materialize_config_drive(request, network_data)?;
        let flavor_id = (!request.flavor_id.trim().is_empty())
            .then(|| request.flavor_id.clone())
            .ok_or(ProviderError::InvalidRequest)?;
        let disk_gib = (request.disk_gib > 0)
            .then_some(request.disk_gib)
            .ok_or(ProviderError::InvalidRequest)?;
        let config_artifact_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "o3k:config-drive:{}:{}",
                request.o3k_server_id, iso.fingerprint_sha256
            )
            .as_bytes(),
        )
        .to_string();
        Ok(ResolvedCreateInputs {
            flavor_id,
            image_artifact_id: image.id.to_string(),
            image_sha256: image.checksum,
            image_format: image.format,
            disk_gib,
            config_drive_artifact_id: config_artifact_id,
            config_drive_sha256: iso.fingerprint_sha256,
            network_attachments,
        })
    }
}

#[async_trait]
impl CreateArtifactResolver for DaemonCreateResolver {
    async fn resolve_artifacts(
        &self,
        request: &CreateInstanceRequest,
        agent: &AgentNodeSnapshot,
        inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError> {
        let image = self.resolve_image(request).await?;
        if image.checksum != inputs.image_sha256 || image.format != inputs.image_format {
            return Err(ProviderError::Conflict);
        }
        let (_, network_data) = self
            .resolve_network(request, &agent.agent_id, &agent.agent_epoch)
            .await?;
        let (iso, iso_bytes) = self.materialize_config_drive(request, network_data)?;
        if iso.fingerprint_sha256 != inputs.config_drive_sha256 {
            return Err(ProviderError::Conflict);
        }
        Ok(vec![
            ResolvedCreateArtifact {
                artifact_id: inputs.image_artifact_id.clone(),
                kind: ArtifactKind::ImageBase,
                sha256: image.checksum,
                format: image.format,
                bytes: image.content,
            },
            ResolvedCreateArtifact {
                artifact_id: inputs.config_drive_artifact_id.clone(),
                kind: ArtifactKind::ConfigDriveIso,
                sha256: iso.fingerprint_sha256,
                format: "iso".to_owned(),
                bytes: iso_bytes,
            },
        ])
    }
}

/// Parses the protected two-tenant isolation seeding environment. Every
/// variable is required together: a partial set is a misconfiguration and
/// fails closed. Disabled by default; only the hosted-service testbed runner
/// sets these to prove cross-tenant isolation.
fn parse_extra_project_seeds()
-> Result<Vec<o3k_identity::ExtraProjectSeed>, Box<dyn std::error::Error>> {
    const PREFIX: &str = "O3K_EXTRA_TENANT_";
    let vars = [
        "PROJECT_ID",
        "PROJECT_NAME",
        "USER_ID",
        "USER_NAME",
        "PASSWORD",
    ];
    let values: Vec<Option<String>> = vars
        .iter()
        .map(|suffix| std::env::var(format!("{PREFIX}{suffix}")).ok())
        .collect();
    if values.iter().all(Option::is_none) {
        return Ok(Vec::new());
    }
    let require = |suffix: &str, index: usize| -> Result<String, Box<dyn std::error::Error>> {
        values[index].clone().ok_or_else(|| {
            format!("{PREFIX}{suffix} is required when any {PREFIX}* variable is set").into()
        })
    };
    let project_id = require("PROJECT_ID", 0)?;
    let project_name = require("PROJECT_NAME", 1)?;
    let user_id = require("USER_ID", 2)?;
    let user_name = require("USER_NAME", 3)?;
    let password = require("PASSWORD", 4)?;
    Uuid::parse_str(&project_id).map_err(|error| -> Box<dyn std::error::Error> {
        format!("{PREFIX}PROJECT_ID: {error}").into()
    })?;
    Uuid::parse_str(&user_id).map_err(|error| -> Box<dyn std::error::Error> {
        format!("{PREFIX}USER_ID: {error}").into()
    })?;
    Ok(vec![o3k_identity::ExtraProjectSeed {
        project_id,
        project_name,
        user_id,
        user_name,
        password: o3k_identity::Secret::new(password),
    }])
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = o3k_config::Config::from_sources(std::env::args(), std::env::vars())?;
    let subscriber =
        tracing_subscriber::fmt().with_env_filter(EnvFilter::try_new(&config.log_filter)?);
    match config.log_format {
        o3k_config::LogFormat::Json => subscriber.json().init(),
        o3k_config::LogFormat::Pretty => subscriber.pretty().init(),
    }

    let store = match config.database_backend {
        o3k_config::DatabaseBackend::Sqlite => {
            let database_path = config.data_dir.join("o3k.sqlite");
            Arc::new(o3k_store::O3kStore::connect_sqlite_file(&database_path).await?)
        }
        o3k_config::DatabaseBackend::Postgres => {
            let url = config
                .database_url()
                .map(|s| s.expose())
                .ok_or("missing O3K_DATABASE_URL for PostgreSQL backend")?;
            Arc::new(o3k_store::O3kStore::connect_postgres(url).await?)
        }
    };
    let native_api_store = store.clone();

    let controller_id = o3k_store::ControllerId::new(
        std::env::var("O3K_CONTROLLER_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string()),
    );
    let controller_epoch = std::env::var("O3K_CONTROLLER_EPOCH")
        .map(o3k_store::ControllerEpoch::new)
        .unwrap_or_else(|_| o3k_store::ControllerEpoch::random());
    let session = o3k_store::ControllerSession {
        controller_id: controller_id.clone(),
        controller_epoch: controller_epoch.clone(),
        started_at: String::new(),
        heartbeat_at: String::new(),
        lease_until: String::new(),
        software_version: env!("CARGO_PKG_VERSION").to_owned(),
        source_commit: std::env::var("O3K_SOURCE_COMMIT").unwrap_or_else(|_| "HEAD".to_owned()),
        state: o3k_store::ControllerState::Active,
    };

    let coordination_store: Arc<dyn o3k_store::CoordinationRepository> = store.clone();
    coordination_store
        .register_controller_session(&session, Duration::from_secs(15))
        .await?;

    info!(
        controller_id = %controller_id,
        controller_epoch = %controller_epoch,
        "controller session registered"
    );

    let heartbeat_store = coordination_store.clone();
    let heartbeat_ctrl_id = controller_id.clone();
    let heartbeat_ctrl_epoch = controller_epoch.clone();
    let session_heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = heartbeat_store
                .heartbeat_controller_session(
                    &heartbeat_ctrl_id,
                    &heartbeat_ctrl_epoch,
                    Duration::from_secs(15),
                )
                .await
            {
                tracing::warn!(%error, "controller session heartbeat failed");
            }
        }
    });

    let identity_store = store.clone();
    let image_repository: Arc<dyn o3k_store::ImageRepository> = store.clone();
    let image_service = o3k_image::ImageService::open(
        config.data_dir.join("images"),
        o3k_image::DEFAULT_MAX_UPLOAD_BYTES,
        image_repository,
    )
    .await?;
    let network_repository: Arc<dyn o3k_store::NetworkRepository> = store.clone();
    let network_service =
        o3k_network::NetworkService::open(config.data_dir.join("network"), network_repository)
            .await?;
    let config_drive_root = config.data_dir.join("config-drive");
    let config_drive_store = o3k_config_drive::ConfigDriveStore::open(&config_drive_root)?;
    let console_service = o3k_console::ConsoleService::open(config.data_dir.join("console"))?;
    // The registry's durable store is load-bearing for the console-log path:
    // o3k-api persists dispatched console commands through
    // `registry.persist_pending_command`, which requires this store to be
    // wired before the registry is shared.
    let registry = o3k_compute_agent::NodeRegistry::default()
        .with_store(store.clone())
        .with_coordination(
            coordination_store.clone(),
            controller_id.clone(),
            controller_epoch.clone(),
        );
    // The console-log consumer keeps its own durable liveness handle: the
    // `store` arc itself is moved into the compute service below.
    let console_store: Arc<dyn o3k_store::DurableStore> = store.clone();
    let placement_repository: Arc<dyn o3k_store::PlacementRepository> = store.clone();
    let placement = o3k_placement::PlacementLedger::open(
        config.data_dir.join("placement"),
        placement_repository,
    )
    .await
    .map_err(|error| format!("open Placement ledger: {error}"))?;
    let durable_compute_resources = store.list_resources_by_kind("compute_instance").await?;
    let consumer_ids = placement_consumer_ids(&durable_compute_resources);
    let reconciliation = placement
        .reconcile_consumers(&consumer_ids)
        .await
        .map_err(|error| format!("reconcile Placement consumers: {error}"))?;
    if !reconciliation.orphaned_allocations.is_empty()
        || !reconciliation.abandoned_intents.is_empty()
    {
        info!(
            orphaned_allocations = reconciliation.orphaned_allocations.len(),
            abandoned_intents = reconciliation.abandoned_intents.len(),
            "reconciled Placement state against durable compute resources"
        );
    }
    let scheduler = o3k_scheduler::Scheduler::new(placement.clone());
    let network_dispatcher = network_dispatcher_from_env()?;
    let public_allocator = public_allocator_from_env(&config.data_dir)?;
    let network_controller = o3k_network::NetworkControllerLease {
        controller_id: controller_id.to_string(),
        controller_epoch: controller_epoch.to_string(),
        fencing_token: std::env::var("O3K_NETWORK_FENCING_TOKEN")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
    };
    let network_external_realm_id = std::env::var("O3K_NETWORK_EXTERNAL_REALM_ID")
        .ok()
        .map(|value| Uuid::parse_str(&value))
        .transpose()?;
    let network_agent_identity = match (
        std::env::var("O3K_NETWORK_AGENT_ID").ok(),
        std::env::var("O3K_NETWORK_AGENT_EPOCH").ok(),
    ) {
        (Some(agent_id), Some(agent_epoch)) => Some(o3k_network::NetworkAgentIdentity {
            agent_id,
            agent_epoch,
        }),
        (None, None) => None,
        _ => {
            return Err(
                "O3K_NETWORK_AGENT_ID and O3K_NETWORK_AGENT_EPOCH must be set together".into(),
            );
        }
    };
    let agent_control_enabled = config.compute_server_certificate.is_some()
        && config.compute_server_private_key.is_some()
        && config.compute_client_ca.is_some();
    let binding_projector = Arc::new(NetworkBindingProjector {
        network: network_service.clone(),
        registry: Arc::new(registry.clone()),
        network_dispatcher: network_dispatcher.clone(),
        network_controller: network_controller.clone(),
        network_external_realm_id,
        network_agent: network_agent_identity.clone(),
    });
    let mut compute_service = if config.provider == o3k_config::Provider::Agent {
        let resolver = Arc::new(DaemonCreateResolver {
            image: image_service.clone(),
            network: network_service.clone(),
            config_drive: config_drive_store.clone(),
            network_dispatcher: network_dispatcher.clone(),
            network_controller: network_controller.clone(),
            network_external_realm_id,
            network_agent: network_agent_identity.clone(),
        });
        o3k_compute::ComputeService::new(
            store.clone(),
            Arc::new(
                o3k_compute_agent::AgentComputeProvider::new_with_store(
                    registry.clone(),
                    resolver.clone(),
                    Some(store.clone()),
                )
                .with_artifact_resolver(resolver),
            ),
        )
        .with_binding_projector(binding_projector.clone())
        .with_config_drive_cleaner(config_drive_store.clone())
    } else {
        match config.provider {
            o3k_config::Provider::Libvirt => {
                return Err(o3k_config::ConfigError::DirectLibvirtProviderUnavailable.into());
            }
            o3k_config::Provider::Fake => o3k_compute::ComputeService::new(
                store.clone(),
                Arc::new(o3k_provider::FakeComputeProvider::new()),
            )
            .with_binding_projector(binding_projector.clone()),
            o3k_config::Provider::CellHv => {
                let provider = o3k_cellhv::CellHvProvider::connect(&o3k_cellhv::CellHvConfig {
                    endpoint: config
                        .cellhv_endpoint
                        .clone()
                        .ok_or("missing CellHV endpoint")?,
                    expected_version: config
                        .cellhv_expected_version
                        .clone()
                        .ok_or("missing CellHV expected version")?,
                    ca_certificate: config.cellhv_ca_certificate.clone(),
                    client_certificate: config.cellhv_client_certificate.clone(),
                    client_key: config.cellhv_client_key.clone(),
                })
                .await?;
                o3k_compute::ComputeService::new(store.clone(), Arc::new(provider))
                    .with_binding_projector(binding_projector.clone())
            }
            o3k_config::Provider::Agent => unreachable!("agent provider handled above"),
        }
    };
    compute_service = compute_service.with_coordination(
        coordination_store.clone(),
        controller_id.clone(),
        controller_epoch.clone(),
    );
    if agent_control_enabled {
        compute_service = compute_service
            .with_scheduler(scheduler)
            .with_agent_registry(Arc::new(registry.clone()));
    }
    if let (Some(cinder_password), Ok(cinder_endpoint)) = (
        config.cinder_password(),
        std::env::var("O3K_CINDER_ENDPOINT"),
    ) {
        let catalog_endpoint = format!("http://{}", config.listen_addr);
        let cinder_client = Arc::new(o3k_cinder::CinderClient::new(
            o3k_cinder::CinderClientConfig {
                keystone_endpoint: catalog_endpoint,
                cinder_endpoint,
                username: "cinder".to_owned(),
                password: o3k_identity::Secret::new(cinder_password.expose().to_owned()),
                domain_name: "Default".to_owned(),
            },
        ));
        compute_service = compute_service.with_attachment_provider(cinder_client);
        info!("external Cinder attachment client enabled");
    }
    let inventory_task = agent_control_enabled.then(|| {
        o3k_compute::spawn_agent_inventory_publisher(
            Arc::new(registry.clone()),
            placement.clone(),
            registry.registration_notify(),
        )
    });
    let compute_ready = if config.provider == o3k_config::Provider::Agent && agent_control_enabled {
        // The authenticated agent is deliberately started after o3kd's health
        // endpoint.  A capability probe before registration would permanently
        // publish `not_ready`, deadlocking the agent bootstrap.  The compute
        // process owns the agent-registration/libvirt readiness gate; o3kd's
        // readyz here means that its authenticated control endpoint can accept
        // that registration.  If the control task later stops, the task below
        // clears readiness again.
        info!("agent control plane is ready for authenticated registration");
        true
    } else {
        match tokio::time::timeout(
            Duration::from_secs(5),
            compute_service.provider().capabilities(),
        )
        .await
        {
            Ok(Ok(capabilities)) => {
                info!(provider = %capabilities.provider_name, "compute provider is ready");
                true
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "compute provider is not ready");
                false
            }
            Err(_) => {
                tracing::warn!("compute provider readiness probe timed out");
                false
            }
        }
    };
    let event_task = compute_service.spawn_agent_event_consumer(Arc::new(registry.clone()));
    let console_event_task = spawn_console_event_consumer(
        registry.subscribe_events(),
        console_service.clone(),
        console_store.clone(),
    );
    let attachment_reconciler = compute_service.spawn_attachment_reconciler(5);
    let create_convergence_reconciler = compute_service.spawn_create_convergence_reconciler(5);
    let lifecycle_convergence_reconciler =
        compute_service.spawn_lifecycle_convergence_reconciler(5);
    let extra_projects = parse_extra_project_seeds()?;
    let identity = match (config.bootstrap_password(), config.token_signing_key()) {
        (Some(password), Some(signing_key)) => {
            let catalog_endpoint = format!("http://{}", config.listen_addr);
            o3k_identity::seed_identity_defaults(
                identity_store.as_ref(),
                &o3k_identity::BootstrapConfig {
                    catalog_endpoint: catalog_endpoint.clone(),
                    bootstrap_password: o3k_identity::Secret::new(password.expose().to_owned()),
                    cinder_password: config
                        .cinder_password()
                        .map(|secret| o3k_identity::Secret::new(secret.expose().to_owned())),
                    cinder_endpoint: std::env::var("O3K_CINDER_ENDPOINT").ok(),
                    pbkdf2_iterations: 0,
                    extra_projects,
                },
            )
            .await?;
            Some(
                o3k_identity::TokenService::load(
                    identity_store.clone(),
                    o3k_identity::Secret::new(signing_key.expose().to_owned()),
                    Duration::from_secs(3600),
                )
                .await?
                .with_catalog_endpoint(catalog_endpoint),
            )
        }
        _ => {
            tracing::warn!(
                "identity is not configured: token authentication is disabled until O3K_BOOTSTRAP_PASSWORD and O3K_TOKEN_SIGNING_KEY are set (see scripts/generate-passwords.sh)"
            );
            None
        }
    };

    let listener = TcpListener::bind(config.listen_addr).await?;
    info!(address = %config.listen_addr, data_dir = %config.data_dir.display(), provider = ?config.provider, "o3kd listening");
    let authorized_agents = config
        .compute_authorized_agents
        .as_deref()
        .map(o3k_compute_agent::parse_authorized_agents)
        .transpose()?
        .unwrap_or_default();

    let mut native_manifest_registry = o3k_kernel::ManifestRegistry::new();
    native_manifest_registry
        .seed_core()
        .map_err(|e| format!("native manifest seed_core failed: {e}"))?;
    if let Ok(manifest_directory) = std::env::var("O3K_MANIFEST_DIR") {
        let path = std::path::Path::new(&manifest_directory);
        native_manifest_registry
            .register_json_directory(path)
            .map_err(|e| format!("external manifest directory failed: {e}"))?;
        info!(directory = %path.display(), "external service manifests loaded");
    }

    // Wire native API service adapters.
    let server_reader: Option<std::sync::Arc<dyn o3k_native_api::compute::ServerReader>> =
        Some(std::sync::Arc::new(native_adapters::ServerReaderAdapter {
            service: std::sync::Arc::new(compute_service.clone()),
        })
            as std::sync::Arc<dyn o3k_native_api::compute::ServerReader>);
    let volume_reader: Option<std::sync::Arc<dyn o3k_native_api::volume::VolumeReader>> =
        Some(std::sync::Arc::new(native_adapters::VolumeReaderAdapter {
            store: native_api_store.clone(),
            authorizer: std::sync::Arc::new(o3k_kernel::StaticAuthorizer::standard()),
        })
            as std::sync::Arc<dyn o3k_native_api::volume::VolumeReader>);
    let network_reader: Option<std::sync::Arc<dyn o3k_native_api::network::NetworkReader>> =
        Some(std::sync::Arc::new(native_adapters::NetworkReaderAdapter {
            store: native_api_store.clone(),
            authorizer: std::sync::Arc::new(o3k_kernel::StaticAuthorizer::standard()),
        })
            as std::sync::Arc<dyn o3k_native_api::network::NetworkReader>);
    let operation_reader: std::sync::Arc<dyn o3k_native_api::operation::OperationReader> =
        std::sync::Arc::new(native_adapters::OperationReaderAdapter {
            store: native_api_store.clone(),
        });
    let token_issuer: Option<std::sync::Arc<dyn o3k_native_api::auth::TokenIssuer>> =
        identity.as_ref().map(|id_service| {
            std::sync::Arc::new(native_adapters::TokenIssuerAdapter {
                service: std::sync::Arc::new(id_service.clone()),
            }) as std::sync::Arc<dyn o3k_native_api::auth::TokenIssuer>
        });
    let external_controllers = external_controllers_from_config().await?;
    for (service_id, controller) in &external_controllers {
        let manifest = native_manifest_registry
            .get(service_id)
            .ok_or_else(|| format!("external controller has no manifest: {service_id}"))?;
        let capabilities = controller.capabilities().await;
        let declared_types = manifest
            .resource_types
            .iter()
            .map(|resource| resource.resource_type.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let required_actions = manifest
            .resource_types
            .iter()
            .flat_map(|resource| resource.operations.values().map(ToString::to_string))
            .collect::<std::collections::BTreeSet<_>>();
        if !capabilities
            .resource_types
            .iter()
            .all(|resource| declared_types.contains(resource))
            || !capabilities
                .actions
                .iter()
                .all(|action| manifest.actions.iter().any(|declared| declared == action))
            || !required_actions.iter().all(|action| {
                capabilities
                    .actions
                    .iter()
                    .any(|advertised| advertised == action)
            })
        {
            return Err(
                format!("external controller capabilities exceed manifest: {service_id}").into(),
            );
        }
        native_manifest_registry.register_controller(service_id, controller.session().clone())?;
        let health = controller.health().await;
        native_manifest_registry.update_controller_health(service_id, health)?;
    }
    let native_lvm_provider = match (
        std::env::var("O3K_LVM_VOLUME_GROUP").ok(),
        std::env::var("O3K_LVM_THIN_POOL").ok(),
        std::env::var("O3K_LVM_PROVIDER_NAMESPACE").ok(),
    ) {
        (Some(volume_group), Some(thin_pool), Some(provider_namespace)) => Some(Arc::new(
            o3k_storage::LvmStorageProvider::new(o3k_storage::LvmConfig {
                volume_group,
                thin_pool,
                provider_namespace,
            })?,
        )),
        _ => None,
    };
    let native_storage_provider: Option<Arc<dyn o3k_storage::StorageProvider>> =
        native_lvm_provider.clone().map(|provider| provider as _);
    let generic_application: std::sync::Arc<dyn o3k_native_api::resource::ResourceApplication> =
        std::sync::Arc::new(native_adapters::GenericResourceApplication {
            compute: std::sync::Arc::new(compute_service.clone()),
            network_service: std::sync::Arc::new(network_service.clone()),
            store: native_api_store.clone(),
            storage_provider: native_storage_provider.clone(),
            server: server_reader
                .clone()
                .ok_or("generic native application requires compute reader")?,
            network: network_reader
                .clone()
                .ok_or("generic native application requires network reader")?,
            external_controllers: std::sync::Arc::new(external_controllers),
        });

    let composition_task = if let Ok(listen_addr) = std::env::var("O3K_COMPOSITION_LISTEN_ADDR") {
        let address: std::net::SocketAddr = listen_addr
            .parse()
            .map_err(|_| "invalid O3K_COMPOSITION_LISTEN_ADDR")?;
        let ca = std::env::var("O3K_COMPOSITION_CLIENT_CA")
            .map_err(|_| "O3K_COMPOSITION_CLIENT_CA is required")?;
        let certificate = std::env::var("O3K_COMPOSITION_SERVER_CERT")
            .map_err(|_| "O3K_COMPOSITION_SERVER_CERT is required")?;
        let key = std::env::var("O3K_COMPOSITION_SERVER_KEY")
            .map_err(|_| "O3K_COMPOSITION_SERVER_KEY is required")?;
        let service_id = std::env::var("O3K_COMPOSITION_SERVICE_ID")
            .map_err(|_| "O3K_COMPOSITION_SERVICE_ID is required")?;
        let service_principal = std::env::var("O3K_COMPOSITION_SERVICE_PRINCIPAL")
            .map_err(|_| "O3K_COMPOSITION_SERVICE_PRINCIPAL is required")?;
        let key_id = std::env::var("O3K_COMPOSITION_DELEGATION_KEY_ID")
            .map_err(|_| "O3K_COMPOSITION_DELEGATION_KEY_ID is required")?;
        let key_path = std::env::var("O3K_COMPOSITION_DELEGATION_KEY")
            .map_err(|_| "O3K_COMPOSITION_DELEGATION_KEY is required")?;
        let key_bytes = std::fs::read(key_path)?;
        let key_bytes: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| "delegation verification key must be 32 bytes")?;
        let verification_key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| "invalid delegation verification key")?;
        let tls = o3k_service_sdk::tls::server(&ca, &certificate, &key)
            .map_err(|error| format!("composition TLS configuration failed: {error}"))?;
        let handler = std::sync::Arc::new(native_adapters::CompositionResourceHandler {
            application: generic_application.clone(),
            store: native_api_store.clone(),
            manifests: std::sync::Arc::new(native_manifest_registry.clone()),
            delegation_keys: std::collections::HashMap::from([(key_id.clone(), verification_key)]),
            dispatcher: o3k_native_api::resource::ResourceDispatcher::from_manifest_registry(
                &native_manifest_registry,
            )
            .map_err(|_| "failed to build composition resource descriptors")?,
        });
        let service = o3k_service_sdk::composition::CompositionServiceAdapter::new(
            handler,
            service_id,
            service_principal,
        )
        .with_delegation_keys(
            "o3k-composition",
            std::collections::HashMap::from([(key_id, verification_key)]),
        );
        info!(address = %address, "generic composition service enabled");
        Some(tokio::spawn(async move {
            let mut builder = match tonic::transport::Server::builder().tls_config(tls) {
                Ok(builder) => builder,
                Err(error) => {
                    tracing::error!(%error, "composition server configuration failed");
                    return;
                }
            };
            if let Err(error) = builder
                .add_service(service.into_server())
                .serve(address)
                .await
            {
                tracing::error!(%error, "composition service stopped");
            }
        }))
    } else {
        None
    };

    let inspect_compute_service = compute_service.clone();
    let native_attachment_workflow: Option<Arc<dyn o3k_api::NativeAttachmentWorkflow>> =
        native_lvm_provider.as_ref().map(|provider| {
            let workflow = o3k_reconciler::storage_workflow::StorageAttachmentWorkflow::new(
                store.clone(),
                provider.clone(),
                Arc::new(LocalComputeAttachmentExecutor {
                    compute: Arc::new(compute_service.clone()),
                }),
                Arc::new(LocalStorageFence),
            );
            Arc::new(NativeStorageAttachmentWorkflow {
                store: store.clone(),
                workflow,
            }) as Arc<dyn o3k_api::NativeAttachmentWorkflow>
        });
    // Native storage is always wired in this composition root; the adapter
    // selects the canonical native path when external Cinder is absent.
    let volume_attachments_enabled = true;
    let mut state = if let Some(identity) = identity {
        o3k_api::AppState::new()
            .with_identity(identity)
            .with_image(image_service)
            .with_network(network_service)
            .with_console(console_service.clone())
            .with_agent_registry(registry.clone())
            .with_volume_attachments_enabled(volume_attachments_enabled)
            .with_compute(compute_service)
    } else {
        o3k_api::AppState::new()
            .with_image(image_service)
            .with_network(network_service)
            .with_console(console_service)
            .with_agent_registry(registry.clone())
            .with_volume_attachments_enabled(volume_attachments_enabled)
            .with_compute(compute_service)
    };
    // Native pagination is reachable only when IAM is configured.  In the
    // IAM-disabled health/operational profile, keep the API unavailable and
    // avoid requiring production secrets solely to start healthz.
    let cursor_config = if token_issuer.is_some() {
        o3k_native_api::pagination::CursorConfig::from_env()
            .map_err(|error| format!("native cursor configuration failed: {error}"))?
    } else {
        o3k_native_api::pagination::CursorConfig::default()
    };
    state = state.with_native_api(
        o3k_native_api::NativeApiState::new(
            Some(native_manifest_registry),
            cursor_config,
            token_issuer,
            server_reader,
            volume_reader,
            network_reader,
        )?
        .with_operation_reader(operation_reader)
        .with_resource_application(generic_application)
        .with_authorizer(std::sync::Arc::new(o3k_kernel::StaticAuthorizer::standard())),
    );
    state = state.with_storage_store(store.clone());
    if let Some(provider) = native_storage_provider {
        state = state.with_storage_provider(provider);
    }
    o3k_api::recover_native_volumes(&state).await;
    if let Some(workflow) = native_attachment_workflow {
        state = state.with_native_attachment_workflow(workflow.clone());
        if let Err(error) = workflow.recover().await {
            tracing::warn!(%error, "native storage attachment recovery is incomplete");
        }
    }
    if let Some(allocator) = public_allocator {
        state = state.with_public_allocator(allocator);
    }
    if let Some(realm_id) = network_external_realm_id {
        state = state.with_network_external_realm(realm_id);
    }
    if let Some(dispatcher) = network_dispatcher {
        state = state.with_network_dispatcher(dispatcher, network_controller);
    }
    if let Some(agent) = network_agent_identity {
        state = state.with_network_agent_identity(agent);
    }
    // Recover canonical gateway and gateway-attachment deletion reservations
    // after the execution boundary is available.  This is intentionally
    // startup work, not a replay of an HTTP request.
    o3k_api::recover_l3_gateway_operations(&state).await;
    state.set_ready(compute_ready);
    let control_task = match (
        config.compute_server_certificate.clone(),
        config.compute_server_private_key.clone(),
        config.compute_client_ca.clone(),
    ) {
        (Some(server_certificate), Some(server_private_key), Some(client_ca_certificate)) => {
            let server = o3k_compute_agent::ControlPlaneServer {
                registry: registry.clone(),
                address: config.compute_control_addr,
                tls: o3k_compute_agent::ControlPlaneTls {
                    server_certificate,
                    server_private_key,
                    client_ca_certificate,
                },
                authorized_agents,
            };
            let readiness = state.clone();
            info!(address = %config.compute_control_addr, "compute-agent control plane enabled");
            Some(tokio::spawn(async move {
                let result = server.serve(control_shutdown_signal()).await;
                if let Err(error) = &result {
                    readiness.set_ready(false);
                    tracing::error!(%error, "compute-agent control plane stopped before shutdown");
                }
                result
            }))
        }
        _ => {
            info!(
                "compute-agent control plane disabled; configure all compute TLS paths to enable it"
            );
            None
        }
    };
    let inspect_probe_task = agent_inspect_probe_from_env(inspect_compute_service);
    let shutdown_state = state.clone();
    axum::serve(listener, o3k_api::router_with_state(state))
        .with_graceful_shutdown(shutdown_signal(shutdown_state))
        .await?;
    if let Some(task) = composition_task {
        task.abort();
        let _ = task.await;
    }
    if let Some(mut task) = control_task
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    if let Some(mut task) = inspect_probe_task
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    event_task.abort();
    let _ = event_task.await;
    console_event_task.abort();
    let _ = console_event_task.await;
    attachment_reconciler.abort();
    let _ = attachment_reconciler.await;
    create_convergence_reconciler.abort();
    let _ = create_convergence_reconciler.await;
    lifecycle_convergence_reconciler.abort();
    let _ = lifecycle_convergence_reconciler.await;
    if let Some(task) = inventory_task {
        task.abort();
        let _ = task.await;
    }
    session_heartbeat_task.abort();
    let _ = session_heartbeat_task.await;
    let _ = coordination_store
        .drain_controller_session(&controller_id, &controller_epoch)
        .await;
    info!(
        controller_id = %controller_id,
        controller_epoch = %controller_epoch,
        "controller session drained"
    );
    Ok(())
}

/// Runs an opt-in, read-only process-boundary probe for protected validation.
/// It is deliberately absent unless its output and either a fixed resource ID
/// or a lifecycle-produced resource-ID file are configured. It records only
/// command/observation state; it never creates or mutates a provider resource.
fn validate_inspect_probe_paths(output: Option<&str>, resource_file: Option<&str>) -> bool {
    let Some(output) = output else {
        return false;
    };
    let output_path = std::path::Path::new(output);
    if !output_path.is_absolute() || output_path.is_symlink() {
        return false;
    }
    if let Some(resource_file) = resource_file {
        let path = std::path::Path::new(resource_file);
        if !path.is_absolute() || path.to_string_lossy().contains("..") || path.is_symlink() {
            return false;
        }
    }
    true
}

fn agent_inspect_probe_from_env(
    compute: o3k_compute::ComputeService,
) -> Option<tokio::task::JoinHandle<()>> {
    let resource_id = std::env::var("O3K_AGENT_INSPECT_PROBE_RESOURCE_ID").ok();
    let resource_file = std::env::var("O3K_AGENT_INSPECT_PROBE_RESOURCE_FILE").ok();
    let output = std::env::var("O3K_AGENT_INSPECT_PROBE_OUTPUT").ok()?;
    let project_id = std::env::var("O3K_AGENT_INSPECT_PROBE_PROJECT_ID")
        .unwrap_or_else(|_| "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned());
    if resource_id
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && resource_file
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        tracing::warn!("agent inspect probe configuration is incomplete");
        return None;
    }
    if !validate_inspect_probe_paths(Some(&output), resource_file.as_deref()) {
        tracing::warn!("agent inspect probe path configuration is invalid");
        return None;
    }
    let output = PathBuf::from(output);
    let resource_file = resource_file.map(PathBuf::from);
    Some(tokio::spawn(async move {
        let result = run_agent_inspect_probe(
            &compute,
            &project_id,
            resource_id.as_deref(),
            resource_file.as_deref(),
        )
        .await;
        let document = match result {
            Ok(evidence) => evidence,
            Err(reason) => serde_json::json!({
                "artifact_type": "compute-agent-process-mtls",
                "redacted": true,
                "status": "failed",
                "reason": reason,
            }),
        };
        if let Err(error) = std::fs::write(&output, format!("{document}\n")) {
            tracing::warn!(error = %error, "agent inspect probe evidence could not be written");
        }
    }))
}

async fn run_agent_inspect_probe(
    compute: &o3k_compute::ComputeService,
    project_id: &str,
    fixed_resource_id: Option<&str>,
    resource_file: Option<&std::path::Path>,
) -> Result<serde_json::Value, String> {
    // The probe starts when o3kd starts, but the lifecycle server is created
    // later. Use a long deadline so the probe can wait for the resource file
    // to appear and then for the inspect operation to reach a terminal state.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let mut resource_id: Option<Uuid> = None;
    while tokio::time::Instant::now() < deadline {
        let candidate = match (fixed_resource_id, resource_file) {
            (Some(value), _) => value.trim().to_owned(),
            (None, Some(path)) => std::fs::read_to_string(path)
                .ok()
                .map(|value| value.trim().to_owned())
                .unwrap_or_default(),
            (None, None) => String::new(),
        };
        if let Ok(id) = Uuid::parse_str(&candidate) {
            resource_id = Some(id);
        }
        let Some(id) = resource_id else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        // Dispatch inspect (or re-check durable state if already terminal).
        let inspect_result = compute
            .inspect_server(
                project_id,
                ServerId::from_uuid(id),
                "o3k-agent-inspect-probe",
            )
            .await;
        match inspect_result {
            Ok(operation)
                if matches!(
                    operation.state,
                    OperationState::Succeeded
                        | OperationState::Failed
                        | OperationState::UnknownOutcome
                ) =>
            {
                let expected = operation.state == OperationState::Succeeded;
                if !expected {
                    return Err(format!(
                        "agent inspect probe state mismatch: state={:?} error_category={:?}",
                        operation.state, operation.error_category
                    ));
                }
                return Ok(serde_json::json!({
                    "artifact_type": "compute-agent-process-mtls",
                    "evidence": {
                        "command": "inspect",
                        "command_state": "accepted",
                        "operation_state": "succeeded",
                        "observation_state": "running",
                        "observation_operation_state": "succeeded",
                        "resource_source": "real-lifecycle-server",
                        "transitions": ["accepted", "operation_succeeded", "observation_succeeded"],
                        "transport": "mutual_tls"
                    },
                    "redacted": true,
                    "scope": "o3kd-compute-service-to-scheduler-to-agent-to-libvirt",
                    "status": "passed"
                }));
            }
            Ok(_) => {
                // Inspect dispatched (Accepted/Running); wait for observation.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(o3k_compute::ComputeError::NotFound | o3k_compute::ComputeError::Conflict) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(format!("agent inspect probe failed: {error}")),
        }
    }
    Err(
        "agent inspect probe timed out waiting for a durable server record and observation"
            .to_owned(),
    )
}

fn spawn_console_event_consumer(
    mut events: tokio::sync::broadcast::Receiver<o3k_provider::AgentEvent>,
    console: o3k_console::ConsoleService,
    store: Arc<dyn o3k_store::DurableStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(o3k_provider::AgentEvent::Observation(observation))
                    if !observation.console_log_bytes.is_empty() =>
                {
                    // Liveness guard (issue #89, defect 4): after a crash the
                    // agent re-delivers committed journal observations. For a
                    // server whose delete already completed, the delete path's
                    // `console.cleanup` removed the console log, so writing
                    // the replayed bytes would resurrect owned host state that
                    // must stay absent. The delete projection keeps a DELETED
                    // tombstone (the row is never removed), so a durable read
                    // decides: only a present, decodable, non-Deleted resource
                    // may receive console bytes. Anything else is stale replay
                    // evidence and is skipped.
                    let resource_is_live = match store.get_resource(observation.resource_id).await {
                        Ok(resource) => {
                            match o3k_store::server_state_from_storage(&resource.observed_state) {
                                Ok(o3k_domain::ServerState::Deleted) => {
                                    tracing::debug!(
                                        resource_id = %observation.resource_id,
                                        "skipping console observation for deleted resource"
                                    );
                                    false
                                }
                                Ok(_) => true,
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        resource_id = %observation.resource_id,
                                        "skipping console observation for resource with corrupt state"
                                    );
                                    false
                                }
                            }
                        }
                        Err(o3k_store::StoreError::ResourceNotFound) => {
                            tracing::debug!(
                                resource_id = %observation.resource_id,
                                "skipping console observation for absent resource"
                            );
                            false
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                resource_id = %observation.resource_id,
                                "skipping console observation: resource liveness could not be verified"
                            );
                            false
                        }
                    };
                    if !resource_is_live {
                        continue;
                    }
                    if let Err(error) = console.write_chunk(
                        observation.resource_id,
                        observation.console_log_offset,
                        &observation.console_log_bytes,
                    ) {
                        tracing::warn!(%error, resource_id = %observation.resource_id, "agent console observation was rejected");
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    tracing::warn!(count, "console observation consumer lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn control_shutdown_signal() {
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

async fn shutdown_signal(state: o3k_api::AppState) {
    let ctrl_c = async { tokio::signal::ctrl_c().await };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
                Ok(())
            }
            Err(error) => Err(error),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<Option<()>>();

    tokio::select! {
        result = ctrl_c => match result {
            Ok(()) => info!("received Ctrl+C, shutting down"),
            Err(error) => tracing::error!(%error, "Ctrl+C handler failed; shutting down"),
        },
        result = terminate => match result {
            Ok(()) => info!("received SIGTERM, shutting down"),
            Err(error) => tracing::error!(%error, "SIGTERM handler failed; shutting down"),
        },
    }
    state.set_ready(false);
}

#[cfg(test)]
mod tests {
    use super::{DaemonCreateResolver, NetworkBindingProjector, placement_consumer_ids};
    use o3k_compute::PortBindingProjector;
    use std::net::Ipv4Addr;
    use std::path::Path;
    use std::sync::Arc;
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct RecordingNetworkDispatcher {
        commands: Arc<std::sync::Mutex<Vec<o3k_network::NetworkPlanCommand>>>,
    }

    #[async_trait::async_trait]
    impl o3k_network::NetworkPlanDispatcher for RecordingNetworkDispatcher {
        async fn dispatch(
            &self,
            command: o3k_network::NetworkPlanCommand,
        ) -> Result<o3k_network::NetworkPlanStatus, o3k_network::NetworkDispatchError> {
            self.commands
                .lock()
                .map_err(|_| o3k_network::NetworkDispatchError::Unavailable)?
                .push(command);
            Ok(o3k_network::NetworkPlanStatus::Succeeded)
        }
    }

    #[test]
    fn config_drive_iso_is_published_beside_owned_instance_directory() -> Result<(), String> {
        let server_id = Uuid::now_v7();
        let directory = Path::new("/var/lib/o3k-testlab/config-drive").join(server_id.to_string());
        let output = DaemonCreateResolver::config_drive_iso_path(&directory, server_id)
            .map_err(|error| error.to_string())?;
        let parent = directory
            .parent()
            .ok_or_else(|| "instance directory should have a parent".to_owned())?;
        assert_eq!(output, parent.join(format!("{server_id}.iso")));
        Ok(())
    }

    #[test]
    fn placement_startup_consumer_set_is_live_sorted_and_deduplicated() {
        let live = Uuid::now_v7();
        let deleted = Uuid::now_v7();
        let resources = vec![
            o3k_store::ResourceRecord {
                id: deleted,
                kind: "compute_instance".to_owned(),
                project_id: "p".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: String::new(),
                observed_state: "DELETED".to_owned(),
                provider_id: None,
            },
            o3k_store::ResourceRecord {
                id: live,
                kind: "compute_instance".to_owned(),
                project_id: "p".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: String::new(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            },
        ];
        assert_eq!(placement_consumer_ids(&resources), vec![live.to_string()]);
    }

    #[test]
    fn agent_inspect_probe_rejects_invalid_relative_traversal_or_symlinked_paths() {
        assert!(!super::validate_inspect_probe_paths(
            Some("relative/path.json"),
            None
        ));
        assert!(!super::validate_inspect_probe_paths(
            Some("/tmp/valid-output.json"),
            Some("/tmp/../etc/passwd")
        ));
        assert!(super::validate_inspect_probe_paths(
            Some("/tmp/valid-output.json"),
            Some("/tmp/valid-resource-file")
        ));
    }

    #[tokio::test]
    async fn console_observation_rejects_stale_replay_for_deleted_or_absent_resource()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("o3kd-console-guard-{}", Uuid::now_v7()));
        let sqlite_path = root.with_extension("sqlite");
        std::fs::create_dir_all(&root)?;
        let store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_handle: Arc<dyn o3k_store::DurableStore> = store.clone();
        let console = o3k_console::ConsoleService::open(root.join("console"))?;

        let live_id = Uuid::now_v7();
        let deleted_id = Uuid::now_v7();
        let absent_id = Uuid::now_v7();
        let record = |id: Uuid, observed_state: &str| o3k_store::ResourceRecord {
            id,
            kind: "compute_instance".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 1,
            desired_state: "{}".to_owned(),
            observed_state: observed_state.to_owned(),
            provider_id: None,
        };
        store_handle
            .insert_resource(&record(live_id, "ACTIVE"))
            .await?;
        // The delete projection keeps a DELETED tombstone (issue #89, defect
        // 4: a crash + journal replay must not resurrect the console log).
        store_handle
            .insert_resource(&record(deleted_id, "DELETED"))
            .await?;

        let (sender, receiver) = tokio::sync::broadcast::channel(16);
        let task = super::spawn_console_event_consumer(receiver, console.clone(), store_handle);
        let observation = |resource_id: Uuid, bytes: &[u8]| {
            o3k_provider::AgentEvent::Observation(Box::new(o3k_provider::AgentObservation {
                agent_id: "agent-1".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                resource_id,
                provider_resource_id: None,
                state: o3k_provider::InstanceState::Running,
                operation_id: Uuid::now_v7(),
                operation_state: o3k_provider::AgentOperationState::Succeeded,
                observation_sequence: 1,
                observed_at_unix_ms: 0,
                redacted_message: None,
                console_log_bytes: bytes.to_vec(),
                console_log_offset: 0,
                console_log_complete: true,
                console_log_truncated: false,
                block_device: None,
            }))
        };
        sender.send(observation(deleted_id, b"stale delete replay"))?;
        sender.send(observation(absent_id, b"stale absent replay"))?;
        sender.send(observation(live_id, b"live boot"))?;
        drop(sender);
        task.await?;

        assert!(
            matches!(
                console.read(deleted_id),
                Err(o3k_console::ConsoleError::NotFound)
            ),
            "deleted resource console replay must not write the console log"
        );
        assert!(
            matches!(
                console.read(absent_id),
                Err(o3k_console::ConsoleError::NotFound)
            ),
            "absent resource console replay must not write the console log"
        );
        assert_eq!(
            console.read(live_id)?,
            b"live boot",
            "live resource console observation must still be written"
        );

        drop(console);
        std::fs::remove_dir_all(&root)?;
        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn binding_intent_is_recorded_only_after_attachment_resolution_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("o3kd-resolver-{}", Uuid::now_v7()));
        let sqlite_path = root.with_extension("sqlite");
        std::fs::create_dir_all(&root)?;
        let store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let image = o3k_image::ImageService::open(
            root.join("images"),
            o3k_image::DEFAULT_MAX_UPLOAD_BYTES,
            store.clone(),
        )
        .await?;
        let config_drive = o3k_config_drive::ConfigDriveStore::open(root.join("config-drive"))?;
        let network_repository: Arc<dyn o3k_store::NetworkRepository> = store.clone();
        let network =
            o3k_network::NetworkService::open(root.join("network"), network_repository).await?;
        let resolver = DaemonCreateResolver {
            image,
            network: network.clone(),
            config_drive,
            network_dispatcher: None,
            network_controller: o3k_network::NetworkControllerLease {
                controller_id: "test-controller".to_owned(),
                controller_epoch: "test-epoch".to_owned(),
                fencing_token: 1,
            },
            network_agent: None,
            network_external_realm_id: None,
        };
        let net = network
            .create_network_for_project("project-a", "flat".to_owned())
            .await?;
        let _subnet = network
            .create_subnet_for_project(
                "project-a",
                net.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = network
            .create_port_for_project("project-a", net.id, "one".to_owned())
            .await?;
        let request = o3k_provider::CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 1,
            image_id: None,
            key_name: None,
            keypair_id: None,
            network_ids: vec![port.id.to_string()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "test".to_owned(),
        };
        let (attachments, _) = resolver
            .resolve_network(&request, "compute-1", "epoch-1")
            .await?;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].port_id, port.id.to_string());
        let bound = network.get_port_for_project("project-a", port.id).await?;
        assert_eq!(bound.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(bound.binding_state.as_deref(), Some("binding"));

        let unresolved_port = o3k_store::PortRecord {
            id: Uuid::now_v7(),
            network_id: net.id,
            subnet_id: None,
            project_id: "project-a".to_owned(),
            name: "legacy-unresolvable".to_owned(),
            mac_address: "02:00:00:00:00:77".to_owned(),
            fixed_ip: Ipv4Addr::new(192, 0, 2, 7),
            status: "ACTIVE".to_owned(),
            binding_host: None,
            binding_state: None,
        };
        store.insert_port(&unresolved_port).await?;
        let unresolved = o3k_provider::CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 1,
            image_id: None,
            key_name: None,
            keypair_id: None,
            network_ids: vec![unresolved_port.id.to_string()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "test".to_owned(),
        };
        let failed = resolver
            .resolve_network(&unresolved, "compute-1", "epoch-1")
            .await;
        assert!(failed.is_err());
        let after = store
            .get_port("project-a", &unresolved_port.id)
            .await?
            .ok_or("legacy projection disappeared")?;
        assert_eq!(after.binding_host, None);
        assert_eq!(after.binding_state, None);
        drop(resolver);
        drop(network);
        std::fs::remove_dir_all(&root)?;
        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn configured_network_agent_owns_binding_target_separately_from_compute_host()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("o3kd-network-target-{}", Uuid::now_v7()));
        let sqlite_path = root.with_extension("sqlite");
        std::fs::create_dir_all(&root)?;
        let store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let image = o3k_image::ImageService::open(
            root.join("images"),
            o3k_image::DEFAULT_MAX_UPLOAD_BYTES,
            store.clone(),
        )
        .await?;
        let config_drive = o3k_config_drive::ConfigDriveStore::open(root.join("config-drive"))?;
        let network_repository: Arc<dyn o3k_store::NetworkRepository> = store.clone();
        let network =
            o3k_network::NetworkService::open(root.join("network"), network_repository).await?;
        let resolver = DaemonCreateResolver {
            image,
            network: network.clone(),
            config_drive,
            network_dispatcher: None,
            network_controller: o3k_network::NetworkControllerLease {
                controller_id: "test-controller".to_owned(),
                controller_epoch: "test-epoch".to_owned(),
                fencing_token: 1,
            },
            network_agent: Some(o3k_network::NetworkAgentIdentity {
                agent_id: "network-agent-1".to_owned(),
                agent_epoch: "network-epoch-1".to_owned(),
            }),
            network_external_realm_id: None,
        };
        let net = network
            .create_network_for_project("project-a", "flat".to_owned())
            .await?;
        network
            .create_subnet_for_project(
                "project-a",
                net.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = network
            .create_port_for_project("project-a", net.id, "one".to_owned())
            .await?;
        let request = o3k_provider::CreateInstanceRequest {
            operation_id: Uuid::now_v7(),
            o3k_server_id: Uuid::now_v7(),
            project_id: "project-a".to_owned(),
            name: "server".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            flavor_id: String::new(),
            disk_gib: 1,
            image_id: None,
            key_name: None,
            keypair_id: None,
            network_ids: vec![port.id.to_string()],
            placement_provider_id: None,
            placement_allocation_id: None,
            config_drive: None,
            idempotency_key: "test-network-agent-target".to_owned(),
        };
        let (attachments, _) = resolver
            .resolve_network(&request, "compute-agent-1", "compute-epoch-1")
            .await?;
        assert_eq!(attachments[0].port_id, port.id.to_string());
        let bound = network.get_port_for_project("project-a", port.id).await?;
        assert_eq!(bound.binding_host.as_deref(), Some("network-agent-1"));
        assert_eq!(bound.binding_state.as_deref(), Some("binding"));
        drop(resolver);
        drop(network);
        std::fs::remove_dir_all(&root)?;
        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn terminal_fake_provider_outcome_dispatches_unbound_network_once()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let root = std::env::temp_dir().join(format!("o3kd-terminal-binding-{}", Uuid::now_v7()));
        let sqlite_path = root.with_extension("sqlite");
        std::fs::create_dir_all(&root)?;
        let store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let network_repository: Arc<dyn o3k_store::NetworkRepository> = store.clone();
        let network =
            o3k_network::NetworkService::open(root.join("network"), network_repository).await?;
        let net = network
            .create_network_for_project("project-a", "terminal".to_owned())
            .await?;
        network
            .create_subnet_for_project(
                "project-a",
                net.id,
                "subnet".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = network
            .create_port_for_project("project-a", net.id, "endpoint".to_owned())
            .await?;
        let dispatcher = RecordingNetworkDispatcher::default();
        let commands = dispatcher.commands.clone();
        let projector = NetworkBindingProjector {
            network: network.clone(),
            registry: Arc::new(o3k_compute_agent::NodeRegistry::default()),
            network_dispatcher: Some(Arc::new(dispatcher)),
            network_controller: o3k_network::NetworkControllerLease {
                controller_id: "controller".to_owned(),
                controller_epoch: "epoch".to_owned(),
                fencing_token: 1,
            },
            network_external_realm_id: None,
            network_agent: Some(o3k_network::NetworkAgentIdentity {
                agent_id: "network-agent".to_owned(),
                agent_epoch: "agent-epoch".to_owned(),
            }),
        };
        projector
            .project_create_outcome("project-a", &port.id.to_string(), true)
            .await?;
        projector
            .project_create_outcome("project-a", &port.id.to_string(), true)
            .await?;
        let bound = network.get_port_for_project("project-a", port.id).await?;
        assert_eq!(bound.binding_host.as_deref(), Some("network-agent"));
        assert_eq!(bound.binding_state.as_deref(), Some("bound"));
        assert_eq!(commands.lock().map_err(|_| "commands poisoned")?.len(), 1);
        std::fs::remove_dir_all(&root)?;
        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn network_binding_projector_reflects_outcomes_on_recorded_intent()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let root = std::env::temp_dir().join(format!("o3kd-projector-{}", Uuid::now_v7()));
        let sqlite_path = root.with_extension("sqlite");
        std::fs::create_dir_all(&root)?;
        let store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let network_repository: Arc<dyn o3k_store::NetworkRepository> = store.clone();
        let network =
            o3k_network::NetworkService::open(root.join("network"), network_repository).await?;
        let projector = NetworkBindingProjector {
            network: network.clone(),
            registry: Arc::new(o3k_compute_agent::NodeRegistry::default()),
            network_dispatcher: None,
            network_controller: o3k_network::NetworkControllerLease {
                controller_id: "test-controller".to_owned(),
                controller_epoch: "test-epoch".to_owned(),
                fencing_token: 1,
            },
            network_agent: None,
            network_external_realm_id: None,
        };
        let net = network
            .create_network_for_project("project-a", "flat".to_owned())
            .await?;
        network
            .create_subnet_for_project(
                "project-a",
                net.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = network
            .create_port_for_project("project-a", net.id, "one".to_owned())
            .await?;
        // Projection without a recorded intent is rejected (logged upstream).
        assert!(
            projector
                .project_create_outcome("project-a", &port.id.to_string(), true)
                .await
                .is_err()
        );
        network
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        projector
            .project_create_outcome("project-a", &port.id.to_string(), true)
            .await?;
        let bound = network.get_port_for_project("project-a", port.id).await?;
        assert_eq!(bound.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(bound.binding_state.as_deref(), Some("bound"));
        projector
            .unbind_port("project-a", &port.id.to_string())
            .await?;
        let unbound = network.get_port_for_project("project-a", port.id).await?;
        assert_eq!(unbound.binding_host, None);
        assert_eq!(unbound.binding_state, None);
        drop(projector);
        drop(network);
        std::fs::remove_dir_all(&root)?;
        let _ = std::fs::remove_file(&sqlite_path);
        let _ = std::fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }
}
