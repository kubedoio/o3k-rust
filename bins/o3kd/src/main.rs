use o3k_provider::ComputeProvider;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

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
    let console_service = o3k_console::ConsoleService::open(config.data_dir.join("console"))?;
    let registry = o3k_compute_agent::NodeRegistry::default();
    let placement = o3k_placement::PlacementLedger::open(config.data_dir.join("placement"))
        .map_err(|error| format!("open Placement ledger: {error}"))?;
    let scheduler = o3k_scheduler::Scheduler::new(placement.clone());
    let agent_control_enabled = config.compute_server_certificate.is_some()
        && config.compute_server_private_key.is_some()
        && config.compute_client_ca.is_some();
    let mut compute_service = if config.provider == o3k_config::Provider::Agent {
        o3k_compute::ComputeService::new(
            store.clone(),
            Arc::new(o3k_compute::AgentComputeProvider::new_with_store(
                registry.clone(),
                Arc::new(o3k_compute::UnconfiguredResolvedCreateResolver),
                Some(store.clone()),
            )),
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
    let shutdown_state = state.clone();
    axum::serve(listener, o3k_api::router_with_state(state))
        .with_graceful_shutdown(shutdown_signal(shutdown_state))
        .await?;
    if let Some(mut task) = control_task {
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
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
