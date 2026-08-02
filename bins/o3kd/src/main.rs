use async_trait::async_trait;
use o3k_compute::{
    CreateArtifactResolver, ResolvedCreateArtifact, ResolvedCreateInputs, ResolvedCreateResolver,
};
use o3k_compute_agent::NodeSnapshot;
use o3k_provider::{
    ComputeProvider, ConfigDriveRequest, CreateInstanceRequest, ErrorCategory, OperationState,
    ProviderError,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Clone)]
struct DaemonCreateResolver {
    image: o3k_image::ImageService,
    network: o3k_network::NetworkService,
    config_drive: o3k_config_drive::ConfigDriveStore,
    iso_root: PathBuf,
}

impl DaemonCreateResolver {
    fn resolve_image(
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
            .resolve_artifact(&request.project_id, image_id)
            .map_err(|_| ProviderError::InvalidRequest)
    }

    fn resolve_network(
        &self,
        request: &CreateInstanceRequest,
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
                .get_port(&request.project_id, port_id)
                .map_err(|_| ProviderError::InvalidRequest)?;
            let subnet = self
                .network
                .get_subnet(&request.project_id, port.subnet_id)
                .map_err(|_| ProviderError::InvalidRequest)?;
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
        let output = self.iso_root.join(format!("{}.iso", request.o3k_server_id));
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
        _agent: &NodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError> {
        let image = self.resolve_image(request)?;
        let (network_attachments, network_data) = self.resolve_network(request)?;
        let (iso, _) = self.materialize_config_drive(request, network_data)?;
        let flavor_id = request
            .flavor_id
            .map(|id| id.to_string())
            .ok_or(ProviderError::InvalidRequest)?;
        let disk_gib = request.disk_gib.ok_or(ProviderError::InvalidRequest)?;
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
        _agent: &NodeSnapshot,
        inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError> {
        let image = self.resolve_image(request)?;
        if image.checksum != inputs.image_sha256 || image.format != inputs.image_format {
            return Err(ProviderError::Conflict);
        }
        let (_, network_data) = self.resolve_network(request)?;
        let (iso, iso_bytes) = self.materialize_config_drive(request, network_data)?;
        if iso.fingerprint_sha256 != inputs.config_drive_sha256 {
            return Err(ProviderError::Conflict);
        }
        Ok(vec![
            ResolvedCreateArtifact {
                artifact_id: inputs.image_artifact_id.clone(),
                kind: o3k_provider_contract::compute_proto::ArtifactKind::ImageBase,
                sha256: image.checksum,
                format: image.format,
                bytes: image.content,
            },
            ResolvedCreateArtifact {
                artifact_id: inputs.config_drive_artifact_id.clone(),
                kind: o3k_provider_contract::compute_proto::ArtifactKind::ConfigDriveIso,
                sha256: iso.fingerprint_sha256,
                format: "iso".to_owned(),
                bytes: iso_bytes,
            },
        ])
    }
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

    let database_path = config.data_dir.join("o3k.sqlite");
    let store = Arc::new(o3k_store::SqliteStore::connect_file(&database_path).await?);
    let image_service = o3k_image::ImageService::open(
        config.data_dir.join("images"),
        o3k_image::DEFAULT_MAX_UPLOAD_BYTES,
    )?;
    let network_service = o3k_network::NetworkService::open(config.data_dir.join("network"))?;
    let config_drive_root = config.data_dir.join("config-drive");
    let config_drive_store = o3k_config_drive::ConfigDriveStore::open(&config_drive_root)?;
    let config_drive_iso_root = config.data_dir.join("config-drive-iso");
    std::fs::create_dir_all(&config_drive_iso_root)?;
    let console_service = o3k_console::ConsoleService::open(config.data_dir.join("console"))?;
    let registry = o3k_compute_agent::NodeRegistry::default();
    let placement = o3k_placement::PlacementLedger::open(config.data_dir.join("placement"))
        .map_err(|error| format!("open Placement ledger: {error}"))?;
    let scheduler = o3k_scheduler::Scheduler::new(placement.clone());
    let agent_control_enabled = config.compute_server_certificate.is_some()
        && config.compute_server_private_key.is_some()
        && config.compute_client_ca.is_some();
    let mut compute_service = if config.provider == o3k_config::Provider::Agent {
        let resolver = Arc::new(DaemonCreateResolver {
            image: image_service.clone(),
            network: network_service.clone(),
            config_drive: config_drive_store.clone(),
            iso_root: config_drive_iso_root,
        });
        o3k_compute::ComputeService::new(
            store.clone(),
            Arc::new(
                o3k_compute::AgentComputeProvider::new_with_store(
                    registry.clone(),
                    resolver.clone(),
                    Some(store.clone()),
                )
                .with_artifact_resolver(resolver),
            ),
        )
    } else {
        match config.provider {
            o3k_config::Provider::Libvirt => {
                return Err(o3k_config::ConfigError::DirectLibvirtProviderUnavailable.into());
            }
            o3k_config::Provider::Fake => o3k_compute::ComputeService::new(
                store,
                Arc::new(o3k_provider::FakeComputeProvider::new()),
            ),
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
                o3k_compute::ComputeService::new(store, Arc::new(provider))
            }
            o3k_config::Provider::Agent => unreachable!("agent provider handled above"),
        }
    };
    if agent_control_enabled {
        compute_service = compute_service
            .with_scheduler(scheduler)
            .with_agent_registry(registry.clone());
    }
    let inventory_task = agent_control_enabled
        .then(|| o3k_compute::spawn_agent_inventory_publisher(registry.clone(), placement.clone()));
    let compute_ready = match tokio::time::timeout(
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
    };
    let event_task = compute_service.spawn_agent_event_consumer(registry.clone());
    let console_event_task =
        spawn_console_event_consumer(registry.clone(), console_service.clone());
    let identity = match (config.bootstrap_password(), config.token_signing_key()) {
        (Some(password), Some(signing_key)) => Some(
            o3k_identity::TokenService::new(
                "bootstrap-user".to_owned(),
                "admin".to_owned(),
                o3k_identity::Secret::new(password.expose().to_owned()),
                "bootstrap-project".to_owned(),
                "admin".to_owned(),
                o3k_identity::Secret::new(signing_key.expose().to_owned()),
                Duration::from_secs(3600),
            )?
            .with_catalog_endpoint(format!("http://{}", config.listen_addr)),
        ),
        _ => None,
    };
    let listener = TcpListener::bind(config.listen_addr).await?;
    info!(address = %config.listen_addr, data_dir = %config.data_dir.display(), provider = ?config.provider, "o3kd listening");
    let authorized_agents = config
        .compute_authorized_agents
        .as_deref()
        .map(o3k_compute_agent::parse_authorized_agents)
        .transpose()?
        .unwrap_or_default();

    let inspect_compute_service = compute_service.clone();
    let state = if let Some(identity) = identity {
        o3k_api::AppState::new()
            .with_identity(identity)
            .with_image(image_service)
            .with_network(network_service)
            .with_console(console_service.clone())
            .with_agent_registry(registry.clone())
            .with_compute(compute_service)
    } else {
        o3k_api::AppState::new()
            .with_image(image_service)
            .with_network(network_service)
            .with_console(console_service)
            .with_agent_registry(registry.clone())
            .with_compute(compute_service)
    };
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
    if let Some(mut task) = control_task
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = inspect_probe_task {
        task.abort();
        let _ = task.await;
    }
    event_task.abort();
    let _ = event_task.await;
    console_event_task.abort();
    let _ = console_event_task.await;
    if let Some(task) = inventory_task {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}

/// Runs an opt-in, read-only process-boundary probe for protected validation.
/// It is deliberately absent unless its output and either a fixed resource ID
/// or a lifecycle-produced resource-ID file are configured. It records only
/// command/observation state; it never creates or mutates a provider resource.
fn agent_inspect_probe_from_env(
    compute: o3k_compute::ComputeService,
) -> Option<tokio::task::JoinHandle<()>> {
    let resource_id = std::env::var("O3K_AGENT_INSPECT_PROBE_RESOURCE_ID").ok();
    let resource_file = std::env::var("O3K_AGENT_INSPECT_PROBE_RESOURCE_FILE").ok();
    let output = std::env::var("O3K_AGENT_INSPECT_PROBE_OUTPUT").ok()?;
    let project_id = std::env::var("O3K_AGENT_INSPECT_PROBE_PROJECT_ID")
        .unwrap_or_else(|_| "bootstrap-project".to_owned());
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
    let output = PathBuf::from(output);
    if !output.is_absolute() || output.is_symlink() {
        tracing::warn!("agent inspect probe output path is invalid");
        return None;
    }
    let resource_file = resource_file.map(PathBuf::from);
    if resource_file.as_ref().is_some_and(|path| {
        !path.is_absolute() || path.to_string_lossy().contains("..") || path.is_symlink()
    }) {
        tracing::warn!("agent inspect probe resource file is invalid");
        return None;
    }
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let resource_id = match (fixed_resource_id, resource_file) {
            (Some(value), _) => value.trim().to_owned(),
            (None, Some(path)) => std::fs::read_to_string(path)
                .ok()
                .map(|value| value.trim().to_owned())
                .unwrap_or_default(),
            (None, None) => String::new(),
        };
        let Ok(resource_id) = Uuid::parse_str(&resource_id) else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        match compute
            .inspect_server(project_id, resource_id, "o3k-agent-inspect-probe")
            .await
        {
            Ok(operation)
                if matches!(
                    operation.state,
                    OperationState::Succeeded
                        | OperationState::Failed
                        | OperationState::UnknownOutcome
                ) =>
            {
                let expected = operation.state == OperationState::Failed
                    && operation.error_category == Some(ErrorCategory::NotFound);
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
                        "error_category": "not_found",
                        "operation_state": "failed",
                        "observation_state": "failed_not_found",
                        "observation_operation_state": "failed",
                        "transitions": ["accepted", "operation_failed", "observation_failed"],
                        "transport": "mutual_tls"
                    },
                    "redacted": true,
                    "scope": "o3kd-compute-service-to-scheduler-to-agent-to-libvirt",
                    "status": "passed"
                }));
            }
            Ok(_)
            | Err(o3k_compute::ComputeError::NotFound | o3k_compute::ComputeError::Conflict) => {
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
    registry: o3k_compute_agent::NodeRegistry,
    console: o3k_console::ConsoleService,
) -> tokio::task::JoinHandle<()> {
    let mut events = registry.subscribe_events();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(o3k_compute_agent::AgentEvent::Observation(observation))
                    if !observation.console_log_bytes.is_empty() =>
                {
                    let Ok(instance_id) = observation.resource_id.parse::<uuid::Uuid>() else {
                        tracing::warn!(resource_id = %observation.resource_id, "agent console observation has invalid resource id");
                        continue;
                    };
                    if let Err(error) = console.write_chunk(
                        instance_id,
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
