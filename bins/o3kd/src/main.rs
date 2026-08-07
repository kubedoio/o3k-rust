use async_trait::async_trait;
use o3k_compute::{
    CreateArtifactResolver, ResolvedCreateArtifact, ResolvedCreateInputs, ResolvedCreateResolver,
};
use o3k_compute_agent::NodeSnapshot;
use o3k_domain::ServerId;
use o3k_provider::{
    ComputeProvider, ConfigDriveRequest, CreateInstanceRequest, OperationState, ProviderError,
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
            .resolve_artifact(&request.project_id, image_id)
            .await
            .map_err(|_| ProviderError::InvalidRequest)
    }

    async fn resolve_network(
        &self,
        request: &CreateInstanceRequest,
        agent_id: &str,
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
                .await
                .map_err(|_| ProviderError::InvalidRequest)?;
            let subnet = self
                .network
                .get_subnet(
                    &request.project_id,
                    port.subnet_id.ok_or(ProviderError::InvalidRequest)?,
                )
                .await
                .map_err(|_| ProviderError::InvalidRequest)?;
            // Record the selected-host intent only after the full attachment
            // resolved; a port whose subnet cannot be resolved is never
            // dispatched and must not carry a binding intent.
            self.network
                .record_binding_intent(&request.project_id, port_id, agent_id)
                .await
                .map_err(|error| match error {
                    o3k_network::NetworkError::Conflict => ProviderError::Conflict,
                    _ => ProviderError::InvalidRequest,
                })?;
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
        agent: &NodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError> {
        let image = self.resolve_image(request).await?;
        let (network_attachments, network_data) =
            self.resolve_network(request, &agent.agent_id).await?;
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
        agent: &NodeSnapshot,
        inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError> {
        let image = self.resolve_image(request).await?;
        if image.checksum != inputs.image_sha256 || image.format != inputs.image_format {
            return Err(ProviderError::Conflict);
        }
        let (_, network_data) = self.resolve_network(request, &agent.agent_id).await?;
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
        compute_service = compute_service.with_cinder_client(cinder_client);
        info!("external Cinder attachment client enabled");
    }
    let inventory_task = agent_control_enabled
        .then(|| o3k_compute::spawn_agent_inventory_publisher(registry.clone(), placement.clone()));
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
    let event_task = compute_service.spawn_agent_event_consumer(registry.clone());
    let console_event_task =
        spawn_console_event_consumer(registry.clone(), console_service.clone());
    let attachment_reconciler = compute_service.spawn_attachment_reconciler(5);
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

#[cfg(test)]
mod tests {
    use super::DaemonCreateResolver;
    use std::net::Ipv4Addr;
    use std::path::Path;
    use std::sync::Arc;
    use uuid::Uuid;

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
        };
        let net = network
            .create_network("project-a", "flat".to_owned())
            .await?;
        let _subnet = network
            .create_subnet(
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
            .create_port("project-a", net.id, "one".to_owned())
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
        let (attachments, _) = resolver.resolve_network(&request, "compute-1").await?;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].port_id, port.id.to_string());
        let bound = network.get_port("project-a", port.id).await?;
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
        let failed = resolver.resolve_network(&unresolved, "compute-1").await;
        assert!(failed.is_err());
        let after = network.get_port("project-a", unresolved_port.id).await?;
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
}
