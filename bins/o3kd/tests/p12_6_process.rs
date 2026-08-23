use ed25519_dalek::SigningKey;
use o3k_database_example::{ChildLifecycleActions, DatabaseControllerHandler, InstanceSpec};
use o3k_kernel::{ActionId, Controller, ManifestRegistry, OwnershipScope, ScopeId};
use o3k_provider::FakeComputeProvider;
use o3k_service_sdk::{
    GrpcControllerAdapter,
    composition::{CompositionServiceAdapter, GrpcCompositionClient},
};
use o3k_store::{
    CanonicalOperationRecord, DurableStore, IdempotencyReservationRequest, O3kStore,
    OperationRecord, OperationState, ResourceRecord,
};
use std::{collections::HashMap, sync::Arc};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

fn fixture(name: &str) -> String {
    format!(
        "{}/../../crates/o3k-compute-agent/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn tls_server() -> Result<tonic::transport::ServerTlsConfig, Box<dyn std::error::Error>> {
    Ok(o3k_service_sdk::tls::server(
        fixture("ca.pem"),
        fixture("server-chain.pem"),
        fixture("server-key.pem"),
    )?)
}

fn tls_client() -> Result<tonic::transport::ClientTlsConfig, Box<dyn std::error::Error>> {
    Ok(o3k_service_sdk::tls::client(
        fixture("ca.pem"),
        fixture("agent-chain.pem"),
        fixture("agent-key.pem"),
        "o3k-control-plane",
    )?)
}

fn lifecycle() -> Result<ChildLifecycleActions, Box<dyn std::error::Error>> {
    Ok(ChildLifecycleActions {
        network_create: ActionId::new("network", "CreateNetwork")?,
        network_observe: ActionId::new("network", "ReadNetwork")?,
        network_delete: ActionId::new("network", "DeleteNetwork")?,
        volume_create: ActionId::new("volume", "CreateVolume")?,
        volume_observe: ActionId::new("volume", "ReadVolume")?,
        volume_delete: ActionId::new("volume", "DeleteVolume")?,
        compute_create: ActionId::new("compute", "CreateServer")?,
        compute_observe: ActionId::new("compute", "ReadServer")?,
        compute_delete: ActionId::new("compute", "DeleteServer")?,
    })
}

#[tokio::test]
async fn database_controller_and_composition_cross_real_mtls_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(O3kStore::connect_sqlite_memory().await?);
    let compute = Arc::new(o3k_compute::ComputeService::new(
        store.clone(),
        Arc::new(FakeComputeProvider::new()),
    ));
    let network_service = Arc::new(
        o3k_network::NetworkService::open(
            std::env::temp_dir().join(format!("o3k-p12-6-{}", uuid::Uuid::new_v4())),
            store.clone(),
        )
        .await?,
    );
    let application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: compute.clone(),
            network_service,
            store: store.clone(),
            server: Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: compute.clone(),
            }),
            network: Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            }),
            external_controllers: Arc::new(Default::default()),
        });
    let mut manifests = ManifestRegistry::new();
    manifests.seed_core()?;
    manifests.register(o3k_database_example::manifest())?;
    let verification = SigningKey::from_bytes(&[9; 32]).verifying_key();
    let handler = o3kd::native_adapters::CompositionResourceHandler {
        application,
        store: store.clone(),
        manifests: Arc::new(manifests.clone()),
        delegation_keys: HashMap::from([(String::from("p12-6-test"), verification)]),
        dispatcher: o3k_native_api::resource::ResourceDispatcher::from_manifest_registry(
            &manifests,
        )
        .map_err(|error| format!("dispatcher: {error:?}"))?,
    };
    let composition_service = CompositionServiceAdapter::new(
        Arc::new(handler),
        "database-example",
        "database-controller",
    )
    .with_delegation_keys(
        "o3k-composition",
        HashMap::from([(String::from("p12-6-test"), verification)]),
    )
    .into_server();
    let composition_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let composition_address = composition_listener.local_addr()?;
    let composition_tls = tls_server()?;
    let composition_task = tokio::spawn(async move {
        let mut builder = Server::builder().tls_config(composition_tls)?;
        builder
            .add_service(composition_service)
            .serve_with_incoming(TcpListenerStream::new(composition_listener))
            .await
    });
    let composition_client = Arc::new(
        GrpcCompositionClient::connect(&format!("https://{composition_address}"), tls_client()?)
            .await?,
    );
    let controller_handler = DatabaseControllerHandler::new(composition_client, lifecycle()?);
    let controller_service = o3k_service_sdk::ServiceControllerServer::new(
        controller_handler,
        "database-example",
        "database",
        "p12-6-test-manifest",
        1,
    )
    .with_service_principal("database-controller")
    .with_delegation_recipient("o3k-composition")
    .with_delegation_keys(HashMap::from([(String::from("p12-6-test"), verification)]))
    .into_service();
    let controller_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let controller_address = controller_listener.local_addr()?;
    let controller_tls = tls_server()?;
    let controller_task = tokio::spawn(async move {
        let mut builder = Server::builder().tls_config(controller_tls)?;
        builder
            .add_service(controller_service)
            .serve_with_incoming(TcpListenerStream::new(controller_listener))
            .await
    });
    let controller = GrpcControllerAdapter::connect(
        &format!("https://{controller_address}"),
        tls_client()?,
        "database-example",
        "database",
        o3k_kernel::ServicePrincipal::new(
            o3k_kernel::PrincipalId::new_unchecked("database-controller"),
            "database-controller",
            "database",
        ),
        "p12-6-test-manifest",
        1,
    )
    .await?
    .with_delegation_signer("p12-6-test", SigningKey::from_bytes(&[9; 32]));
    let scope = OwnershipScope::project(ScopeId::new("project-a")?, None, None);
    let parent_id = uuid::Uuid::new_v4();
    let operation_id = uuid::Uuid::new_v4();
    let request_id = uuid::Uuid::new_v4();
    let resource_type = o3k_kernel::ResourceType::new("database", "instance")?;
    let resource = ResourceRecord {
        id: parent_id,
        kind: resource_type.to_string(),
        project_id: "project-a".into(),
        generation: 1,
        observed_generation: 0,
        desired_state: serde_json::to_string(&InstanceSpec {
            engine: "test-engine".into(),
            version: "1".into(),
            storage_gb: 1,
        })?,
        observed_state: "PROVISIONING".into(),
        provider_id: None,
    };
    let action = ActionId::new("database", "CreateInstance")?;
    let operation = OperationRecord {
        id: operation_id,
        resource_id: parent_id,
        kind: "lifecycle:create".into(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let canonical = CanonicalOperationRecord::from_kernel_operation(&o3k_kernel::Operation::new(
        operation_id,
        "database-example",
        action.clone(),
        "user-a",
        scope.clone(),
        resource_type.clone(),
        Some(o3k_kernel::ResourceId::new(parent_id.to_string())?),
        Some(request_id.to_string()),
    ))?;
    let identity = IdempotencyReservationRequest::from_semantics(
        "project-a",
        action.to_string(),
        "database-create-1",
        &resource_type.to_string(),
        Some(&parent_id.to_string()),
        &serde_json::from_str::<serde_json::Value>(&resource.desired_state)?,
        operation_id,
    )?;
    store
        .create_or_replay_canonical_resource_operation(
            &resource, &operation, &canonical, &identity, None,
        )
        .await?;
    let session = controller.session().clone();
    let context = o3k_kernel::OperationContext {
        request_id,
        operation_id,
        action,
        service_id: "database-example".into(),
        owner_scope: scope.clone(),
        session_id: session.session_id,
        session_generation: session.session_generation,
        deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
        replay_identity: "database-create-1".into(),
        audit_correlation: "p12-6-process".into(),
    };
    let resource_reference = o3k_kernel::ResourceReference {
        resource_type: o3k_kernel::ResourceType::new("database", "instance")?,
        resource_id: o3k_kernel::ResourceId::new(parent_id.to_string())?,
        generation: 1,
    };
    let delegation = controller.issue_parent_delegation(&context, "user-a", &resource_reference)?;
    let outcome = controller
        .reconcile(o3k_kernel::ReconcileRequest {
            context,
            resource: o3k_kernel::ResourceSnapshot {
                reference: o3k_kernel::ResourceReference {
                    resource_type,
                    resource_id: o3k_kernel::ResourceId::new(parent_id.to_string())?,
                    generation: 1,
                },
                desired_spec: serde_json::from_str(&resource.desired_state)?,
                known_status: None,
                owner_scope: scope,
            },
            delegation: Some(delegation),
        })
        .await;
    assert!(
        matches!(outcome, o3k_kernel::ReconcileOutcome::Succeeded { .. }),
        "unexpected outcome: {outcome:?}"
    );
    let relationships = store.list_relationships(parent_id).await?;
    assert_eq!(relationships.len(), 3);
    assert!(relationships.iter().all(|relationship| {
        relationship.state == "bound"
            && relationship.parent_resource_id == parent_id
            && relationship.owner_scope == "project-a"
            && relationship.child_resource_id.is_some()
    }));
    controller_task.abort();
    composition_task.abort();
    Ok(())
}
