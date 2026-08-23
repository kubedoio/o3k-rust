use ed25519_dalek::SigningKey;
use o3k_database_example::{ChildLifecycleActions, DatabaseControllerHandler, InstanceSpec};
use o3k_kernel::{ActionId, Controller, ManifestRegistry, OwnershipScope, ScopeId};
use o3k_kernel::{ActionPolicy, PrincipalKind};
use o3k_kernel::{LimitKey, LimitValue};
use o3k_native_api::auth::{NativeTokenRequestV1, TokenIssuer};
use o3k_native_api::error::ProblemDetails;
use o3k_provider::FakeComputeProvider;
use o3k_service_sdk::composition::{ChildResourceRequest, ServiceCompositionClient};
use o3k_service_sdk::{
    DelegationClaims, GrpcControllerAdapter, SignedDelegation,
    composition::{CompositionServiceAdapter, GrpcCompositionClient},
};
use o3k_store::QuotaRepository;
use o3k_store::{
    CanonicalOperationRecord, DurableStore, IdempotencyReservationRequest, O3kStore,
    OperationRecord, OperationState, ResourceRecord, ResourceRelationshipRecord,
};
use std::{collections::HashMap, sync::Arc};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

struct ProcessTokenIssuer;

fn process_auth_context(project: &str) -> o3k_kernel::AuthContext {
    o3k_kernel::AuthContext::new(
        o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
            o3k_kernel::PrincipalId::new_unchecked(format!("user-{project}")),
            format!("user-{project}"),
            None,
        )),
        OwnershipScope::project(ScopeId::new_unchecked(project), None, None),
        vec!["member".into()],
        1,
        u64::MAX,
        "p12-6-http-audit",
        "p12-6-http-request",
        None,
    )
}

#[async_trait::async_trait]
impl TokenIssuer for ProcessTokenIssuer {
    async fn issue_native(
        &self,
        _request: &NativeTokenRequestV1,
    ) -> Result<(String, serde_json::Value), ProblemDetails> {
        Err(ProblemDetails::bad_request(
            "test issuer does not issue tokens",
        ))
    }

    async fn auth_context(&self, token: &str) -> Result<o3k_kernel::AuthContext, ProblemDetails> {
        token
            .strip_prefix("project-")
            .map(process_auth_context)
            .ok_or_else(ProblemDetails::unauthorized)
    }
}

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
    let store_path =
        std::env::temp_dir().join(format!("o3k-p12-6-store-{}.sqlite", uuid::Uuid::new_v4()));
    let store = Arc::new(O3kStore::connect_sqlite_file(&store_path).await?);
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
            network_service: network_service.clone(),
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
    let controller_handler =
        DatabaseControllerHandler::new(composition_client.clone(), lifecycle()?);
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
    let controller = Arc::new(
        GrpcControllerAdapter::connect(
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
        .with_delegation_signer("p12-6-test", SigningKey::from_bytes(&[9; 32])),
    );
    let api_application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: compute.clone(),
            network_service: network_service.clone(),
            store: store.clone(),
            server: Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: compute.clone(),
            }),
            network: Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            }),
            external_controllers: Arc::new(std::collections::BTreeMap::from([(
                "database-example".to_owned(),
                controller.clone(),
            )])),
        });
    manifests.register_controller("database-example", controller.session().clone())?;
    manifests.activate_controller("database-example")?;
    let mut api_authorizer = o3k_kernel::StaticAuthorizer::standard();
    for action_name in ["CreateInstance", "ReadInstance", "DeleteInstance"] {
        api_authorizer.register(ActionPolicy {
            action: ActionId::new("database", action_name)?,
            expected_resource_type: o3k_kernel::ResourceType::new("database", "instance")?,
            accepted_principals: vec![PrincipalKind::User, PrincipalKind::Service],
            require_ownership: true,
            required_roles: Vec::new(),
        });
    }
    let api_router = o3k_native_api::router(
        o3k_native_api::NativeApiState::new(
            Some(manifests.clone()),
            o3k_native_api::pagination::CursorConfig::default(),
            Some(Arc::new(ProcessTokenIssuer)),
            Some(Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: compute.clone(),
            })),
            None,
            Some(Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            })),
        )?
        .with_resource_application(api_application.clone())
        .with_authorizer(Arc::new(api_authorizer)),
    );
    let http_create = axum::http::Request::builder()
        .method("POST")
        .uri("/database/instances")
        .header("authorization", "Bearer project-project-http")
        .header("content-type", "application/json")
        .header("idempotency-key", "http-create-1")
        .body(axum::body::Body::from(serde_json::to_vec(
            &serde_json::json!({
                "api_version": "o3k.io/v1",
                "kind": "database:instance",
                "spec": {"engine":"test-engine","version":"1","storage_gb":1}
            }),
        )?))?;
    let http_response = tower::ServiceExt::oneshot(api_router.clone(), http_create).await?;
    if !http_response.status().is_success() {
        let status = http_response.status();
        let body = axum::body::to_bytes(http_response.into_body(), 1_048_576).await?;
        return Err(format!(
            "HTTP create failed: {status} {}",
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    let http_body = axum::body::to_bytes(http_response.into_body(), 1_048_576).await?;
    let http_json: serde_json::Value = serde_json::from_slice(&http_body)?;
    let http_id = http_json["resource_id"]
        .as_str()
        .ok_or_else(|| format!("HTTP create omitted resource id: {http_json}"))?
        .to_owned();
    let http_show = axum::http::Request::builder()
        .uri(format!("/database/instances/{http_id}"))
        .header("authorization", "Bearer project-project-http")
        .body(axum::body::Body::empty())?;
    let http_show_response = tower::ServiceExt::oneshot(api_router.clone(), http_show).await?;
    assert_eq!(http_show_response.status(), axum::http::StatusCode::OK);
    let http_delete = axum::http::Request::builder()
        .method("DELETE")
        .uri(format!("/database/instances/{http_id}"))
        .header("authorization", "Bearer project-project-http")
        .header("idempotency-key", "http-delete-1")
        .body(axum::body::Body::empty())?;
    let http_delete_response = tower::ServiceExt::oneshot(api_router, http_delete).await?;
    assert!(http_delete_response.status().is_success());

    let dispatcher =
        o3k_native_api::resource::ResourceDispatcher::from_manifest_registry(&manifests)
            .map_err(|error| format!("dispatcher: {error:?}"))?;
    let descriptor = dispatcher
        .resolve_resource_type(&o3k_kernel::ResourceType::new("database", "instance")?)
        .ok_or("database descriptor missing")?;
    let api_auth = o3k_kernel::AuthContext::new(
        o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
            o3k_kernel::PrincipalId::new("user-api")?,
            "user-api",
            None,
        )),
        OwnershipScope::project(ScopeId::new("project-api")?, None, None),
        Vec::new(),
        1,
        u64::MAX,
        "api-audit",
        uuid::Uuid::new_v4().to_string(),
        None,
    );
    let api_result = api_application
        .create(
            descriptor,
            &api_auth,
            o3k_native_api::resource::CreateRequest {
                api_version: Some("o3k.io/v1".into()),
                kind: Some("database:instance".into()),
                spec: serde_json::json!({"engine":"test-engine","version":"1","storage_gb":1}),
            },
            Some("api-create-1"),
        )
        .await
        .map_err(|error| format!("generic create: {error:?}"))?;
    let api_id = api_result
        .resource_id
        .clone()
        .ok_or("generic create omitted id")?;
    let replay = api_application
        .create(
            descriptor,
            &api_auth,
            o3k_native_api::resource::CreateRequest {
                api_version: Some("o3k.io/v1".into()),
                kind: Some("database:instance".into()),
                spec: serde_json::json!({"engine":"test-engine","version":"1","storage_gb":1}),
            },
            Some("api-create-1"),
        )
        .await
        .map_err(|error| format!("equivalent replay: {error:?}"))?;
    assert_eq!(replay.resource_id.as_deref(), Some(api_id.as_str()));
    let conflict = api_application
        .create(
            descriptor,
            &api_auth,
            o3k_native_api::resource::CreateRequest {
                api_version: Some("o3k.io/v1".into()),
                kind: Some("database:instance".into()),
                spec: serde_json::json!({"engine":"test-engine","version":"2","storage_gb":1}),
            },
            Some("api-create-1"),
        )
        .await;
    assert!(matches!(
        conflict,
        Err(o3k_native_api::resource::ResourceApplicationError::IdempotencyConflict)
    ));
    let api_show = api_application
        .show(descriptor, &api_auth, &api_id)
        .await
        .map_err(|error| format!("generic show: {error:?}"))?;
    assert!(api_show.get("status").is_some());
    let api_delete = api_application
        .delete(descriptor, &api_auth, &api_id, Some("api-delete-1"))
        .await
        .map_err(|error| format!("generic delete: {error:?}"))?;
    if !api_delete.complete {
        // A child mutation may be accepted without proving absence.  The
        // parent must remain recoverable rather than being reported deleted.
        let parent = store.get_resource(api_id.parse()?).await?;
        assert_ne!(parent.observed_state, "DELETED");
    }
    let quota_scope = OwnershipScope::project(ScopeId::new("project-quota")?, None, None);
    store
        .set_limit(
            &quota_scope,
            &LimitKey::compute_servers(),
            LimitValue::Maximum(0),
        )
        .await?;
    let quota_auth = o3k_kernel::AuthContext::new(
        o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
            o3k_kernel::PrincipalId::new("user-quota")?,
            "user-quota",
            None,
        )),
        quota_scope,
        Vec::new(),
        1,
        u64::MAX,
        "quota-audit",
        uuid::Uuid::new_v4().to_string(),
        None,
    );
    let quota_result = api_application
        .create(
            descriptor,
            &quota_auth,
            o3k_native_api::resource::CreateRequest {
                api_version: Some("o3k.io/v1".into()),
                kind: Some("database:instance".into()),
                spec: serde_json::json!({"engine":"test-engine","version":"1","storage_gb":1}),
            },
            Some("quota-create-1"),
        )
        .await
        .map_err(|error| format!("quota create: {error:?}"))?;
    assert!(!quota_result.complete);
    let quota_parent = quota_result
        .resource_id
        .as_deref()
        .ok_or("quota result omitted parent id")?
        .parse::<uuid::Uuid>()?;
    let quota_relationships = store.list_relationships(quota_parent).await?;
    assert!(quota_relationships.iter().all(|relationship| {
        relationship.expected_child_resource_type != "compute:server"
            || relationship.child_resource_id.is_none()
    }));
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

    // A correctly signed delegation for another owner scope must still fail
    // parent ownership validation before the generic composition service can
    // reserve a relationship or invoke a child application.
    let foreign_scope = OwnershipScope::project(ScopeId::new("project-foreign")?, None, None);
    let foreign_context = o3k_kernel::OperationContext {
        owner_scope: foreign_scope.clone(),
        audit_correlation: "p12-6-foreign-scope".into(),
        ..context.clone()
    };
    let foreign_delegation = controller.issue_parent_delegation(
        &foreign_context,
        "user-foreign",
        &resource_reference,
    )?;
    let foreign_credential = serde_json::to_vec(&SignedDelegation {
        claims: DelegationClaims {
            version: 1,
            credential_id: foreign_delegation.credential_id,
            issuer: "o3k-control-plane".into(),
            key_id: foreign_delegation.key_id.clone(),
            original_actor: foreign_delegation.original_actor.clone(),
            owner_scope: foreign_delegation.original_scope.to_string(),
            calling_service: foreign_delegation.calling_service.name().into(),
            recipient_service: foreign_delegation.recipient_service.name().into(),
            action: foreign_delegation.delegated_action.to_string(),
            resource_type: foreign_delegation.resource.resource_type.to_string(),
            resource_id: foreign_delegation.resource.resource_id.as_str().into(),
            request_id: foreign_delegation.request_id,
            operation_id: foreign_delegation.operation_id,
            session_id: foreign_delegation.session_id,
            session_generation: foreign_delegation.session_generation,
            issued_at_unix_ms: foreign_delegation.issued_at_unix_ms,
            expires_at_unix_ms: foreign_delegation.expires_at_unix_ms,
        },
        signature: foreign_delegation.signature.clone(),
    })?;
    let foreign_child = composition_client
        .create_child(ChildResourceRequest {
            parent: resource_reference.clone(),
            parent_operation_id: operation_id,
            child_operation_id: Some(uuid::Uuid::new_v4()),
            context: foreign_context,
            service_principal: "database-controller".into(),
            delegation: foreign_credential,
            child: None,
            action: ActionId::new("network", "CreateNetwork")?,
            resource_type: o3k_kernel::ResourceType::new("network", "network")?,
            owner_scope: foreign_scope,
            slot: "foreign-scope".into(),
            idempotency_key: "foreign-scope-child".into(),
            desired_spec: serde_json::json!({"name":"foreign"}),
        })
        .await;
    assert!(foreign_child.is_err());
    assert!(store.list_relationships(parent_id).await?.is_empty());

    let reconcile_request = o3k_kernel::ReconcileRequest {
        context,
        resource: o3k_kernel::ResourceSnapshot {
            reference: o3k_kernel::ResourceReference {
                resource_type,
                resource_id: o3k_kernel::ResourceId::new(parent_id.to_string())?,
                generation: 1,
            },
            desired_spec: serde_json::from_str(&resource.desired_state)?,
            known_status: None,
            owner_scope: scope.clone(),
        },
        delegation: Some(delegation),
    };
    // Two genuinely overlapping controller calls exercise both real gRPC
    // boundaries. Durable relationship uniqueness and canonical child
    // idempotency, rather than a process-local mutex, must converge them.
    let first_controller = controller.clone();
    let second_controller = controller.clone();
    let first_request = reconcile_request.clone();
    let second_request = reconcile_request;
    let (first_outcome, second_outcome) = tokio::join!(
        first_controller.reconcile(first_request),
        second_controller.reconcile(second_request),
    );
    assert!(
        matches!(
            first_outcome,
            o3k_kernel::ReconcileOutcome::Succeeded { .. }
        ),
        "unexpected first outcome: {first_outcome:?}"
    );
    assert!(
        matches!(
            second_outcome,
            o3k_kernel::ReconcileOutcome::Succeeded { .. }
        ),
        "unexpected second outcome: {second_outcome:?}"
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
    let restarted_handler =
        DatabaseControllerHandler::new(composition_client.clone(), lifecycle()?);
    let restarted_service = o3k_service_sdk::ServiceControllerServer::new(
        restarted_handler,
        "database-example",
        "database",
        "p12-6-test-manifest",
        1,
    )
    .with_service_principal("database-controller")
    .with_delegation_recipient("o3k-composition")
    .with_delegation_keys(HashMap::from([("p12-6-test".to_owned(), verification)]))
    .into_service();
    let restarted_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let restarted_address = restarted_listener.local_addr()?;
    let restarted_tls = tls_server()?;
    let restarted_task = tokio::spawn(async move {
        let mut builder = Server::builder().tls_config(restarted_tls)?;
        builder
            .add_service(restarted_service)
            .serve_with_incoming(TcpListenerStream::new(restarted_listener))
            .await
    });
    let restarted_controller = Arc::new(
        GrpcControllerAdapter::connect(
            &format!("https://{restarted_address}"),
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
        .with_delegation_signer("p12-6-test", SigningKey::from_bytes(&[9; 32])),
    );
    let restarted_session = restarted_controller.session().clone();
    let restarted_context = o3k_kernel::OperationContext {
        request_id,
        operation_id,
        action: ActionId::new("database", "CreateInstance")?,
        service_id: "database-example".into(),
        owner_scope: scope.clone(),
        session_id: restarted_session.session_id,
        session_generation: restarted_session.session_generation,
        deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
        replay_identity: "database-create-1".into(),
        audit_correlation: "p12-6-process-restart".into(),
    };
    let restarted_delegation = restarted_controller.issue_parent_delegation(
        &restarted_context,
        "user-a",
        &o3k_kernel::ResourceReference {
            resource_type: o3k_kernel::ResourceType::new("database", "instance")?,
            resource_id: o3k_kernel::ResourceId::new(parent_id.to_string())?,
            generation: 1,
        },
    )?;
    let restarted_outcome = restarted_controller
        .reconcile(o3k_kernel::ReconcileRequest {
            context: restarted_context,
            resource: o3k_kernel::ResourceSnapshot {
                reference: o3k_kernel::ResourceReference {
                    resource_type: o3k_kernel::ResourceType::new("database", "instance")?,
                    resource_id: o3k_kernel::ResourceId::new(parent_id.to_string())?,
                    generation: 1,
                },
                desired_spec: serde_json::from_str(&resource.desired_state)?,
                known_status: None,
                owner_scope: scope,
            },
            delegation: Some(restarted_delegation),
        })
        .await;
    assert!(matches!(
        restarted_outcome,
        o3k_kernel::ReconcileOutcome::Succeeded { .. }
    ));
    let after_restart = store.list_relationships(parent_id).await?;
    assert_eq!(after_restart.len(), 3);
    assert_eq!(
        after_restart
            .iter()
            .filter_map(|relationship| relationship.child_resource_id)
            .collect::<std::collections::BTreeSet<_>>(),
        relationships
            .iter()
            .filter_map(|relationship| relationship.child_resource_id)
            .collect::<std::collections::BTreeSet<_>>()
    );
    restarted_task.abort();
    composition_task.abort();
    let _ = std::fs::remove_file(store_path);
    Ok(())
}

#[tokio::test]
async fn unavailable_external_controller_and_composition_endpoints_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let composition = GrpcCompositionClient::connect("https://127.0.0.1:1", tls_client()?).await;
    assert!(composition.is_err());

    let controller = GrpcControllerAdapter::connect(
        "https://127.0.0.1:1",
        tls_client()?,
        "database-example",
        "database",
        o3k_kernel::ServicePrincipal::new(
            o3k_kernel::PrincipalId::new_unchecked("database-controller"),
            "database-controller",
            "database",
        ),
        "unavailable-test-manifest",
        1,
    )
    .await;
    assert!(controller.is_err());
    Ok(())
}

#[tokio::test]
async fn p12_6_relationship_recovery_reopens_store_and_serializes_process_race()
-> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "o3k-p12-6-recovery-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let parent = uuid::Uuid::new_v4();
    let parent_operation = uuid::Uuid::new_v4();
    let child_operation = uuid::Uuid::new_v4();
    let record = ResourceRelationshipRecord {
        parent_resource_id: parent,
        parent_resource_type: "database:instance".into(),
        slot: "network-primary".into(),
        expected_child_resource_type: "network:network".into(),
        child_resource_id: None,
        ownership: "exclusive".into(),
        parent_operation_id: parent_operation,
        child_operation_id: Some(child_operation),
        owner_scope: "project:project-recovery".into(),
        state: "reserved".into(),
        fingerprint: "parent-slot-fingerprint".into(),
    };
    {
        let first = O3kStore::connect_sqlite_file(&path).await?;
        first
            .insert_resource(&ResourceRecord {
                id: parent,
                kind: "database:instance".into(),
                project_id: "project-recovery".into(),
                generation: 1,
                observed_generation: 0,
                desired_state: "{}".into(),
                observed_state: "PROVISIONING".into(),
                provider_id: None,
            })
            .await?;
        first.reserve_relationship(&record).await?;
        assert_eq!(first.list_relationships(parent).await?.len(), 1);
    }

    // Runtime A is gone: only the SQLite file remains. Runtime B must see the
    // unresolved intent and preserve its child operation identity instead of
    // treating the slot as empty.
    let reopened = O3kStore::connect_sqlite_file(&path).await?;
    let restored = reopened.list_relationships(parent).await?;
    assert_eq!(restored, vec![record.clone()]);
    drop(reopened);

    // Two independent store/application owners race the same reservation.
    // The database uniqueness constraint, not a process-local mutex, chooses
    // one durable slot and both equivalent attempts observe the same record.
    let left = O3kStore::connect_sqlite_file(&path).await?;
    let right = O3kStore::connect_sqlite_file(&path).await?;
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let left_barrier = barrier.clone();
    let right_barrier = barrier.clone();
    let left_record = record.clone();
    let right_record = record.clone();
    let left_task = tokio::spawn(async move {
        left_barrier.wait().await;
        left.reserve_relationship(&left_record).await
    });
    let right_task = tokio::spawn(async move {
        right_barrier.wait().await;
        right.reserve_relationship(&right_record).await
    });
    barrier.wait().await;
    let (left_result, right_result) = tokio::join!(left_task, right_task);
    assert_eq!(left_result??, record);
    assert_eq!(right_result??, record);

    let final_store = O3kStore::connect_sqlite_file(&path).await?;
    let final_relationships = final_store.list_relationships(parent).await?;
    assert_eq!(final_relationships.len(), 1);
    assert_eq!(
        final_relationships[0].child_operation_id,
        Some(child_operation)
    );
    drop(final_store);
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// This is deliberately scoped as two runtime lifetimes.  The first helper
/// returns only durable identifiers; every application/store/registry/server
/// value is dropped before the second helper opens the SQLite file.
#[tokio::test]
async fn p12_6_reconstructs_two_independent_control_plane_runtimes()
-> Result<(), Box<dyn std::error::Error>> {
    let path =
        std::env::temp_dir().join(format!("o3k-p12-6-runtime-{}.sqlite", uuid::Uuid::new_v4()));
    let parent = uuid::Uuid::new_v4();
    let parent_operation = uuid::Uuid::new_v4();
    let child_operation = uuid::Uuid::new_v4();
    let relationship = ResourceRelationshipRecord {
        parent_resource_id: parent,
        parent_resource_type: "database:instance".into(),
        slot: "network-primary".into(),
        expected_child_resource_type: "network:network".into(),
        child_resource_id: Some(uuid::Uuid::new_v4()),
        ownership: "exclusive".into(),
        parent_operation_id: parent_operation,
        child_operation_id: Some(child_operation),
        owner_scope: "project:runtime-recovery".into(),
        state: "bound".into(),
        fingerprint: "runtime-recovery-network".into(),
    };

    // Runtime A owns no state in this scope after the block exits.  The file
    // and static manifest fixture are the only inputs intentionally carried
    // to runtime B.
    let durable = {
        let store = Arc::new(O3kStore::connect_sqlite_file(&path).await?);
        let parent_type = o3k_kernel::ResourceType::new("database", "instance")?;
        let parent_action = ActionId::new("database", "CreateInstance")?;
        let parent_scope =
            OwnershipScope::project(ScopeId::new("project-runtime-recovery")?, None, None);
        let canonical =
            CanonicalOperationRecord::from_kernel_operation(&o3k_kernel::Operation::new(
                parent_operation,
                "database-example",
                parent_action.clone(),
                "user-runtime-recovery",
                parent_scope.clone(),
                parent_type.clone(),
                Some(o3k_kernel::ResourceId::new(parent.to_string())?),
                Some("runtime-recovery-request".into()),
            ))?;
        let operation = OperationRecord {
            id: parent_operation,
            resource_id: parent,
            kind: "lifecycle:create".into(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let identity = IdempotencyReservationRequest::from_semantics(
            "project-runtime-recovery",
            parent_action.to_string(),
            "runtime-recovery-create",
            &parent_type.to_string(),
            Some(&parent.to_string()),
            &serde_json::json!({"engine":"test-engine","version":"1","storage_gb":1}),
            parent_operation,
        )?;
        store
            .create_or_replay_canonical_resource_operation(
                &ResourceRecord {
                    id: parent,
                    kind: "database:instance".into(),
                    project_id: "project-runtime-recovery".into(),
                    generation: 1,
                    observed_generation: 0,
                    desired_state:
                        serde_json::json!({"engine":"test-engine","version":"1","storage_gb":1})
                            .to_string(),
                    observed_state: "PROVISIONING".into(),
                    provider_id: None,
                },
                &operation,
                &canonical,
                &identity,
                None,
            )
            .await?;
        store.reserve_relationship(&relationship).await?;
        let child_id = relationship
            .child_resource_id
            .ok_or("fixture child id missing")?;
        store
            .bind_relationship(parent, "network-primary", child_id, child_operation)
            .await?;
        for (slot, kind, child_operation_id) in [
            ("volume-data", "volume:volume", uuid::Uuid::new_v4()),
            ("compute-primary", "compute:server", uuid::Uuid::new_v4()),
        ] {
            let child_id = uuid::Uuid::new_v4();
            store
                .reserve_relationship(&ResourceRelationshipRecord {
                    parent_resource_id: parent,
                    parent_resource_type: "database:instance".into(),
                    slot: slot.into(),
                    expected_child_resource_type: kind.into(),
                    child_resource_id: Some(child_id),
                    ownership: "exclusive".into(),
                    parent_operation_id: parent_operation,
                    child_operation_id: Some(child_operation_id),
                    owner_scope: "project:runtime-recovery".into(),
                    state: "reserved".into(),
                    fingerprint: format!("runtime-recovery-{slot}"),
                })
                .await?;
            store
                .bind_relationship(parent, slot, child_id, child_operation_id)
                .await?;
        }
        let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/o3k-database-example/service-manifest.json");
        let mut registry = ManifestRegistry::new();
        registry.seed_core()?;
        registry.register_json_file(manifest_path)?;
        let compute = Arc::new(o3k_compute::ComputeService::new(
            store.clone(),
            Arc::new(FakeComputeProvider::new()),
        ));
        let network = Arc::new(
            o3k_network::NetworkService::open(
                std::env::temp_dir().join(format!("o3k-p12-6-runtime-a-{}", uuid::Uuid::new_v4())),
                store.clone(),
            )
            .await?,
        );
        let application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
            Arc::new(o3kd::native_adapters::GenericResourceApplication {
                compute: compute.clone(),
                network_service: network.clone(),
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
        let dispatcher =
            o3k_native_api::resource::ResourceDispatcher::from_manifest_registry(&registry)
                .map_err(|error| format!("runtime A dispatcher: {error:?}"))?;
        let _composition_handler = o3kd::native_adapters::CompositionResourceHandler {
            application,
            store: store.clone(),
            manifests: Arc::new(registry.clone()),
            delegation_keys: HashMap::new(),
            dispatcher,
        };
        let records = store.list_relationships(parent).await?;
        assert_eq!(records.len(), 3);
        (parent, parent_operation, records)
    };

    // Runtime A's store/application/registry/handler are all dropped above.
    // Runtime B uses new instances and reloads the external manifest through
    // ManifestRegistry rather than copying the prior registry.
    let store_b = Arc::new(O3kStore::connect_sqlite_file(&path).await?);
    let records_b = store_b.list_relationships(durable.0).await?;
    assert_eq!(records_b, durable.2);
    assert_eq!(records_b[0].parent_operation_id, durable.1);
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/o3k-database-example/service-manifest.json");
    let mut registry_b = ManifestRegistry::new();
    registry_b.seed_core()?;
    registry_b.register_json_file(manifest_path)?;
    let session_a = o3k_kernel::ControllerSession {
        service_id: "database-example".into(),
        namespace: "database".into(),
        service_principal: o3k_kernel::ServicePrincipal::new(
            o3k_kernel::PrincipalId::new_unchecked("database-controller"),
            "database-controller",
            "database",
        ),
        session_id: uuid::Uuid::new_v4(),
        session_generation: 1,
        protocol_version: o3k_kernel::ProtocolVersion { major: 1, minor: 0 },
        manifest_digest: "p12-6-test-manifest".into(),
        manifest_generation: 1,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let session_b = o3k_kernel::ControllerSession {
        session_id: uuid::Uuid::new_v4(),
        session_generation: 2,
        ..session_a.clone()
    };
    registry_b.register_controller("database-example", session_a.clone())?;
    registry_b.register_controller("database-example", session_b.clone())?;
    registry_b.activate_controller("database-example")?;
    assert_ne!(session_a.session_id, session_b.session_id);
    assert!(
        registry_b
            .register_controller("database-example", session_a.clone())
            .is_err()
    );
    let compute_b = Arc::new(o3k_compute::ComputeService::new(
        store_b.clone(),
        Arc::new(FakeComputeProvider::new()),
    ));
    let network_b = Arc::new(
        o3k_network::NetworkService::open(
            std::env::temp_dir().join(format!("o3k-p12-6-runtime-b-{}", uuid::Uuid::new_v4())),
            store_b.clone(),
        )
        .await?,
    );
    let application_b: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: compute_b.clone(),
            network_service: network_b,
            store: store_b.clone(),
            server: Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: compute_b.clone(),
            }),
            network: Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: store_b.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            }),
            external_controllers: Arc::new(Default::default()),
        });
    let dispatcher_b =
        o3k_native_api::resource::ResourceDispatcher::from_manifest_registry(&registry_b)
            .map_err(|error| format!("runtime B dispatcher: {error:?}"))?;
    assert!(
        dispatcher_b
            .resolve_resource_type(&o3k_kernel::ResourceType::new("database", "instance")?)
            .is_some()
    );
    let verification = SigningKey::from_bytes(&[9; 32]).verifying_key();
    let composition_service_b = CompositionServiceAdapter::new(
        Arc::new(o3kd::native_adapters::CompositionResourceHandler {
            application: application_b.clone(),
            store: store_b.clone(),
            manifests: Arc::new(registry_b.clone()),
            delegation_keys: HashMap::from([(String::from("p12-6-test"), verification)]),
            dispatcher: dispatcher_b,
        }),
        "database-example",
        "database-controller",
    )
    .with_delegation_keys(
        "o3k-composition",
        HashMap::from([(String::from("p12-6-test"), verification)]),
    )
    .into_server();
    let composition_listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let composition_address_b = composition_listener_b.local_addr()?;
    let composition_tls_b = tls_server()?;
    let composition_task_b = tokio::spawn(async move {
        let mut builder = Server::builder().tls_config(composition_tls_b)?;
        builder
            .add_service(composition_service_b)
            .serve_with_incoming(TcpListenerStream::new(composition_listener_b))
            .await
    });
    let composition_client_b = Arc::new(
        GrpcCompositionClient::connect(&format!("https://{composition_address_b}"), tls_client()?)
            .await?,
    );
    let controller_service_b = o3k_service_sdk::ServiceControllerServer::new(
        DatabaseControllerHandler::new(composition_client_b, lifecycle()?),
        "database-example",
        "database",
        "p12-6-test-manifest",
        1,
    )
    .with_service_principal("database-controller")
    .with_delegation_recipient("o3k-composition")
    .with_delegation_keys(HashMap::from([("p12-6-test".to_owned(), verification)]))
    .into_service();
    let controller_listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let controller_address_b = controller_listener_b.local_addr()?;
    let controller_tls_b = tls_server()?;
    let controller_task_b = tokio::spawn(async move {
        let mut builder = Server::builder().tls_config(controller_tls_b)?;
        builder
            .add_service(controller_service_b)
            .serve_with_incoming(TcpListenerStream::new(controller_listener_b))
            .await
    });
    let controller_b = GrpcControllerAdapter::connect(
        &format!("https://{controller_address_b}"),
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
    assert_eq!(controller_b.session().service_id, "database-example");
    assert_eq!(controller_b.session().session_generation, 1);
    let parent_b = store_b.get_resource(durable.0).await?;
    assert_eq!(parent_b.id, durable.0);
    assert_eq!(parent_b.generation, 1);
    drop(controller_b);
    controller_task_b.abort();
    composition_task_b.abort();
    drop(application_b);
    drop(store_b);
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[tokio::test]
async fn p12_6_independent_application_instances_converge_durable_slots()
-> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "o3k-p12-6-app-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let parent = uuid::Uuid::new_v4();
    let record = ResourceRelationshipRecord {
        parent_resource_id: parent,
        parent_resource_type: "database:instance".into(),
        slot: "network-primary".into(),
        expected_child_resource_type: "network:network".into(),
        child_resource_id: None,
        ownership: "exclusive".into(),
        parent_operation_id: uuid::Uuid::new_v4(),
        child_operation_id: Some(uuid::Uuid::new_v4()),
        owner_scope: "project-independent-race".into(),
        state: "reserved".into(),
        fingerprint: "independent-application-race".into(),
    };
    let records = vec![
        record.clone(),
        ResourceRelationshipRecord {
            slot: "volume-data".into(),
            expected_child_resource_type: "volume:volume".into(),
            child_operation_id: Some(uuid::Uuid::new_v4()),
            fingerprint: "independent-application-volume".into(),
            ..record.clone()
        },
        ResourceRelationshipRecord {
            slot: "compute-primary".into(),
            expected_child_resource_type: "compute:server".into(),
            child_operation_id: Some(uuid::Uuid::new_v4()),
            fingerprint: "independent-application-compute".into(),
            ..record.clone()
        },
    ];
    let left_store = Arc::new(O3kStore::connect_sqlite_file(&path).await?);
    left_store
        .insert_resource(&ResourceRecord {
            id: parent,
            kind: "database:instance".into(),
            project_id: "project-independent-race".into(),
            generation: 1,
            observed_generation: 0,
            desired_state: "{}".into(),
            observed_state: "PROVISIONING".into(),
            provider_id: None,
        })
        .await?;
    let right_store = Arc::new(O3kStore::connect_sqlite_file(&path).await?);
    let left_compute = Arc::new(o3k_compute::ComputeService::new(
        left_store.clone(),
        Arc::new(FakeComputeProvider::new()),
    ));
    let right_compute = Arc::new(o3k_compute::ComputeService::new(
        right_store.clone(),
        Arc::new(FakeComputeProvider::new()),
    ));
    let left_network = Arc::new(
        o3k_network::NetworkService::open(
            std::env::temp_dir().join(format!("o3k-p12-6-app-left-{}", uuid::Uuid::new_v4())),
            left_store.clone(),
        )
        .await?,
    );
    let right_network = Arc::new(
        o3k_network::NetworkService::open(
            std::env::temp_dir().join(format!("o3k-p12-6-app-right-{}", uuid::Uuid::new_v4())),
            right_store.clone(),
        )
        .await?,
    );
    let left_application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: left_compute.clone(),
            network_service: left_network,
            store: left_store.clone(),
            server: Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: left_compute,
            }),
            network: Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: left_store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            }),
            external_controllers: Arc::new(Default::default()),
        });
    let right_application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: right_compute.clone(),
            network_service: right_network,
            store: right_store.clone(),
            server: Arc::new(o3kd::native_adapters::ServerReaderAdapter {
                service: right_compute,
            }),
            network: Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
                store: right_store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
            }),
            external_controllers: Arc::new(Default::default()),
        });
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let left_barrier = barrier.clone();
    let right_barrier = barrier.clone();
    let left_records = records.clone();
    let right_records = records.clone();
    let left_task = tokio::spawn(async move {
        let _application = left_application;
        left_barrier.wait().await;
        for candidate in &left_records {
            left_store.reserve_relationship(candidate).await?;
        }
        Ok::<(), o3k_store::StoreError>(())
    });
    let right_task = tokio::spawn(async move {
        let _application = right_application;
        right_barrier.wait().await;
        for candidate in &right_records {
            right_store.reserve_relationship(candidate).await?;
        }
        Ok::<(), o3k_store::StoreError>(())
    });
    barrier.wait().await;
    let (left, right) = tokio::join!(left_task, right_task);
    left??;
    right??;
    let final_store = O3kStore::connect_sqlite_file(&path).await?;
    let final_relationships = final_store.list_relationships(parent).await?;
    assert_eq!(final_relationships.len(), 3);
    assert_eq!(
        final_relationships
            .iter()
            .map(|relationship| relationship.slot.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ["compute-primary", "network-primary", "volume-data"]
            .into_iter()
            .collect()
    );
    drop(final_store);
    let _ = std::fs::remove_file(path);
    Ok(())
}
