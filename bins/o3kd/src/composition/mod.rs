pub mod compute;
pub mod external_controllers;
pub mod network;
pub mod runtime;
pub mod storage;

use o3k_kernel::Controller;
use o3k_provider::ComputeProvider;
use o3k_store::ComputeRepository;
use std::{sync::Arc, time::Duration};
use tracing::info;
use uuid::Uuid;

use self::compute::{
    DaemonCreateResolver, agent_inspect_probe_from_env, parse_extra_project_seeds,
};
use self::external_controllers::external_controllers_from_config;
use self::network::{
    NetworkBindingProjector, network_dispatcher_from_env, public_allocator_from_env,
};
use self::runtime::{control_shutdown_signal, spawn_console_event_consumer};
use self::storage::{
    LocalComputeAttachmentExecutor, LocalStorageFence, NativeStorageAttachmentWorkflow,
    storage_intent_epoch,
};

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

pub struct Composition {
    pub state: o3k_api::AppState,
    controller_id: o3k_store::ControllerId,
    controller_epoch: o3k_store::ControllerEpoch,
    coordination_store: Arc<dyn o3k_store::CoordinationRepository>,
    session_heartbeat_task: tokio::task::JoinHandle<()>,
    event_task: tokio::task::JoinHandle<()>,
    console_event_task: tokio::task::JoinHandle<()>,
    attachment_reconciler: tokio::task::JoinHandle<()>,
    create_convergence_reconciler: tokio::task::JoinHandle<()>,
    lifecycle_convergence_reconciler: tokio::task::JoinHandle<()>,
    inventory_task: Option<tokio::task::JoinHandle<()>>,
    composition_task: Option<tokio::task::JoinHandle<()>>,
    native_storage_recovery_task: Option<tokio::task::JoinHandle<()>>,
    control_task: Option<tokio::task::JoinHandle<()>>,
    inspect_probe_task: Option<tokio::task::JoinHandle<()>>,
}

impl Composition {
    pub async fn shutdown(self) {
        if let Some(task) = self.composition_task {
            task.abort();
            let _ = task.await;
        }
        if let Some(mut task) = self.control_task
            && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        if let Some(mut task) = self.inspect_probe_task
            && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.event_task.abort();
        let _ = self.event_task.await;
        self.console_event_task.abort();
        let _ = self.console_event_task.await;
        self.attachment_reconciler.abort();
        let _ = self.attachment_reconciler.await;
        self.create_convergence_reconciler.abort();
        let _ = self.create_convergence_reconciler.await;
        self.lifecycle_convergence_reconciler.abort();
        if let Some(task) = self.native_storage_recovery_task {
            task.abort();
            let _ = task.await;
        }
        let _ = self.lifecycle_convergence_reconciler.await;
        if let Some(task) = self.inventory_task {
            task.abort();
            let _ = task.await;
        }
        self.session_heartbeat_task.abort();
        let _ = self.session_heartbeat_task.await;
        let _ = self
            .coordination_store
            .drain_controller_session(&self.controller_id, &self.controller_epoch)
            .await;
        info!(
            controller_id = %self.controller_id,
            controller_epoch = %self.controller_epoch,
            "controller session drained"
        );
    }
}

pub async fn build_composition(
    config: o3k_config::Config,
) -> Result<Composition, Box<dyn std::error::Error>> {
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

    // Build compute service based on configured provider.
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
    let server_reader: Option<std::sync::Arc<dyn o3k_native_api::compute::ServerReader>> = Some(
        std::sync::Arc::new(crate::native_adapters::ServerReaderAdapter {
            service: std::sync::Arc::new(compute_service.clone()),
        }) as std::sync::Arc<dyn o3k_native_api::compute::ServerReader>,
    );
    let volume_reader: Option<std::sync::Arc<dyn o3k_native_api::volume::VolumeReader>> = Some(
        std::sync::Arc::new(crate::native_adapters::VolumeReaderAdapter {
            store: native_api_store.clone(),
            authorizer: std::sync::Arc::new(o3k_kernel::StaticAuthorizer::standard()),
        }) as std::sync::Arc<dyn o3k_native_api::volume::VolumeReader>,
    );
    let network_reader: Option<std::sync::Arc<dyn o3k_native_api::network::NetworkReader>> = Some(
        std::sync::Arc::new(crate::native_adapters::NetworkReaderAdapter {
            store: native_api_store.clone(),
            authorizer: std::sync::Arc::new(o3k_kernel::StaticAuthorizer::standard()),
        }) as std::sync::Arc<dyn o3k_native_api::network::NetworkReader>,
    );
    let operation_reader: std::sync::Arc<dyn o3k_native_api::operation::OperationReader> =
        std::sync::Arc::new(crate::native_adapters::OperationReaderAdapter {
            store: native_api_store.clone(),
        });
    let token_issuer: Option<std::sync::Arc<dyn o3k_native_api::auth::TokenIssuer>> =
        identity.as_ref().map(|id_service| {
            std::sync::Arc::new(crate::native_adapters::TokenIssuerAdapter {
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
        std::sync::Arc::new(crate::native_adapters::GenericResourceApplication {
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
        let handler = std::sync::Arc::new(crate::native_adapters::CompositionResourceHandler {
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
    let storage_intent_epoch = storage_intent_epoch(&controller_epoch);
    let native_attachment_workflow: Option<Arc<dyn o3k_api::NativeAttachmentWorkflow>> =
        native_lvm_provider.as_ref().map(|provider| {
            let workflow = o3k_reconciler::storage_workflow::StorageAttachmentWorkflow::new(
                store.clone(),
                provider.clone(),
                Arc::new(LocalComputeAttachmentExecutor {
                    compute: Arc::new(compute_service.clone()),
                }),
                Arc::new(LocalStorageFence {
                    coordination: coordination_store.clone(),
                    controller_id: controller_id.clone(),
                    controller_epoch: controller_epoch.clone(),
                    intent_epoch: storage_intent_epoch,
                    execution_lock_path: config.data_dir.join("storage.execution.lock"),
                    attempt: Arc::new(tokio::sync::Mutex::new(None)),
                }),
            );
            Arc::new(NativeStorageAttachmentWorkflow {
                store: store.clone(),
                controller_epoch: storage_intent_epoch,
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
    let native_storage_recovery_task = if let Some(workflow) = native_attachment_workflow.clone() {
        state = state.with_native_attachment_workflow(workflow.clone());
        if let Err(error) = workflow.recover().await {
            tracing::warn!(%error, "native storage attachment recovery is incomplete");
        }
        // Startup can race the previous controller's lease expiry.  Keep the
        // existing recovery boundary live so a Busy takeover is retried
        // automatically without requiring the original client request.
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = workflow.recover().await {
                    tracing::debug!(%error, "native storage recovery pass deferred");
                }
            }
        }))
    } else {
        None
    };
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
                let _ = result;
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

    Ok(Composition {
        state,
        controller_id,
        controller_epoch,
        coordination_store,
        session_heartbeat_task,
        event_task,
        console_event_task,
        attachment_reconciler,
        create_convergence_reconciler,
        lifecycle_convergence_reconciler,
        inventory_task,
        composition_task,
        native_storage_recovery_task,
        control_task,
        inspect_probe_task,
    })
}

/// Runs an opt-in, read-only process-boundary probe for protected validation.
/// It is deliberately absent unless its output and either a fixed resource ID
/// or a lifecycle-produced resource-ID file are configured. It records only
pub async fn shutdown_signal(state: o3k_api::AppState) {
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
    use crate::composition::compute::validate_inspect_probe_paths;
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
        assert!(!validate_inspect_probe_paths(
            Some("relative/path.json"),
            None
        ));
        assert!(!validate_inspect_probe_paths(
            Some("/tmp/valid-output.json"),
            Some("/tmp/../etc/passwd")
        ));
        assert!(validate_inspect_probe_paths(
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
