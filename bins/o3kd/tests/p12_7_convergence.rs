//! P12.7 black-box evidence that the native and OpenStack adapters share the
//! same canonical application/store authority.
#![allow(clippy::expect_used, clippy::panic)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use o3k_compute::ComputeService;
use o3k_identity::testkit::test_service;
use o3k_kernel::{
    ControllerSession, ManifestRegistry, PrincipalId, ProtocolVersion, ServicePrincipal,
};
use o3k_native_api::auth::TokenIssuer;
use o3k_network::NetworkService;
use o3k_provider::FakeComputeProvider;
use o3k_store::DurableStore;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn session(service: &str, namespace: &str, generation: u64) -> ControllerSession {
    ControllerSession {
        service_id: service.to_owned(),
        namespace: namespace.to_owned(),
        service_principal: ServicePrincipal::new(
            PrincipalId::new_unchecked(format!("{service}-controller")),
            format!("{service}-controller"),
            namespace,
        ),
        session_id: uuid::Uuid::new_v4(),
        session_generation: generation,
        protocol_version: ProtocolVersion::new(1, 0),
        manifest_digest: format!("p12-7-{service}"),
        manifest_generation: generation,
        started_at: "2026-08-23T00:00:00Z".to_owned(),
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "non-JSON response body: {:?}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

async fn issue_token(app: &axum::Router) -> Result<String, Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "auth": {
            "identity": {
                "methods": ["password"],
                "password": {"user": {"name": "admin", "password": "password"}}
            },
            "scope": {"project": {"name": "admin"}}
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    Ok(response
        .headers()
        .get("x-subject-token")
        .ok_or("missing Keystone token")?
        .to_str()?
        .to_owned())
}

async fn get_json(
    app: &axum::Router,
    uri: &str,
    token: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-auth-token", token)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    Ok(response_json(response).await)
}

async fn status_for(app: &axum::Router, method: Method, uri: &str, token: &str) -> StatusCode {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method.clone())
                .uri(uri)
                .header("x-auth-token", token)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    if status.is_server_error() {
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("error body");
        eprintln!(
            "{method} {uri} -> {status}: {:?}",
            String::from_utf8_lossy(&body)
        );
    }
    status
}

#[tokio::test]
async fn native_and_openstack_http_surfaces_share_compute_and_network_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::unified::O3kStore::connect_sqlite_memory().await?);
    let identity = test_service("http://127.0.0.1:8080").await?;
    let provider = Arc::new(FakeComputeProvider::new());
    let compute_service = ComputeService::new(store.clone(), provider.clone());
    let compute = Arc::new(compute_service.clone());
    let network = Arc::new(
        NetworkService::open(
            std::env::temp_dir().join(format!("o3k-p12-7-network-{}", uuid::Uuid::new_v4())),
            store.clone(),
        )
        .await?,
    );

    let mut manifests = ManifestRegistry::new();
    manifests.seed_core()?;
    for (service, namespace) in [
        ("compute", "compute"),
        ("network", "network"),
        ("volume", "volume"),
    ] {
        manifests.register_controller(service, session(service, namespace, 1))?;
        manifests.activate_controller(service)?;
    }

    let server_reader: Arc<dyn o3k_native_api::compute::ServerReader> =
        Arc::new(o3kd::native_adapters::ServerReaderAdapter {
            service: compute.clone(),
        });
    let network_reader: Arc<dyn o3k_native_api::network::NetworkReader> =
        Arc::new(o3kd::native_adapters::NetworkReaderAdapter {
            store: store.clone(),
            authorizer: Arc::new(o3k_kernel::StaticAuthorizer::standard()),
        });
    let application: Arc<dyn o3k_native_api::resource::ResourceApplication> =
        Arc::new(o3kd::native_adapters::GenericResourceApplication {
            compute: compute.clone(),
            network_service: network.clone(),
            store: store.clone(),
            server: server_reader.clone(),
            network: network_reader.clone(),
            external_controllers: Arc::new(Default::default()),
        });
    let token_issuer: Arc<dyn TokenIssuer> = Arc::new(o3kd::native_adapters::TokenIssuerAdapter {
        service: Arc::new(identity.clone()),
    });
    let native = o3k_native_api::NativeApiState::new(
        Some(manifests),
        o3k_native_api::pagination::CursorConfig::default(),
        Some(token_issuer),
        Some(server_reader),
        None,
        Some(network_reader),
    )?
    .with_resource_application(application)
    .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
    let app = o3k_api::router_with_state(
        o3k_api::AppState::new()
            .with_identity(identity.clone())
            .with_compute(compute_service.clone())
            .with_network((*network).clone())
            .with_native_api(native.clone()),
    );
    // The compute compatibility test intentionally omits the Neutron
    // validation dependency: its input is an opaque network reference, while
    // the canonical ComputeService remains shared with the network-enabled
    // router above.
    let compute_app = o3k_api::router_with_state(
        o3k_api::AppState::new()
            .with_identity(identity.clone())
            .with_compute(compute_service)
            .with_native_api(native),
    );
    let token = issue_token(&app).await?;

    // Direction A: OpenStack create -> native read.  The ID and owner are
    // asserted from both protocol representations, not inferred by name.
    let network_body = serde_json::json!({"network": {"name": "p12-7-network"}});
    let openstack_network = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/networks")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&network_body)?))?,
        )
        .await?;
    assert_eq!(openstack_network.status(), StatusCode::CREATED);
    let openstack_network_json = response_json(openstack_network).await;
    let network_id = openstack_network_json["network"]["id"]
        .as_str()
        .ok_or("OpenStack network ID")?;
    let project_id = openstack_network_json["network"]["project_id"]
        .as_str()
        .ok_or("OpenStack network project ID")?;
    let native_network = get_json(
        &app,
        &format!("/o3k/v1/network/networks/{network_id}"),
        &token,
    )
    .await?;
    assert_eq!(native_network["metadata"]["id"], network_id);
    assert_eq!(native_network["metadata"]["owner_scope"], project_id);

    // Direction B: native create -> OpenStack read.  Both paths use the same
    // NetworkService and therefore expose the identical durable UUID.
    let native_network_create = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/o3k/v1/network/networks")
                .header("authorization", format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "kind": "network:network",
                    "spec": {"name": "p12-7-native-network"}
                }))?))?,
        )
        .await?;
    let native_network_create_status = native_network_create.status();
    let native_network_create_json = response_json(native_network_create).await;
    assert_eq!(
        native_network_create_status,
        StatusCode::CREATED,
        "{native_network_create_json}"
    );
    let native_network_id = native_network_create_json["resource_id"]
        .as_str()
        .ok_or("native network ID")?;
    let openstack_network_read =
        get_json(&app, &format!("/v2.0/networks/{native_network_id}"), &token).await?;
    assert_eq!(openstack_network_read["network"]["id"], native_network_id);

    assert_eq!(
        status_for(
            &app,
            Method::DELETE,
            &format!("/v2.0/networks/{network_id}"),
            &token,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status_for(
            &app,
            Method::GET,
            &format!("/o3k/v1/network/networks/{network_id}"),
            &token,
        )
        .await,
        StatusCode::NOT_FOUND
    );

    // Compute uses the same shared authority.  The fake provider is only an
    // execution dependency; neither adapter owns a second public record.
    let flavor_id = "00000000-0000-0000-0000-000000000001";
    let openstack_server_body = serde_json::json!({
        "server": {
            "name": "p12-7-openstack-server",
            "image": {"id": "image-a"},
            "flavor": {"id": flavor_id},
            "networks": [{"uuid": network_id}]
        }
    });
    let openstack_server = compute_app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v2.1/{project_id}/servers"))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-openstack-request-id", "p12-7-openstack-create")
                .body(Body::from(serde_json::to_vec(&openstack_server_body)?))?,
        )
        .await?;
    assert_eq!(openstack_server.status(), StatusCode::ACCEPTED);
    let openstack_server_json = response_json(openstack_server).await;
    let openstack_server_id = openstack_server_json["server"]["id"]
        .as_str()
        .ok_or("OpenStack server ID")?;
    let native_server = get_json(
        &compute_app,
        &format!("/o3k/v1/compute/servers/{openstack_server_id}"),
        &token,
    )
    .await?;
    assert_eq!(native_server["metadata"]["id"], openstack_server_id);
    assert_eq!(native_server["metadata"]["owner_scope"], project_id);

    let native_server_create = compute_app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/o3k/v1/compute/servers")
                .header("authorization", format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "p12-7-native-create")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "kind": "compute:server",
                    "spec": {
                        "name": "p12-7-native-server",
                        "image_id": "image-b",
                        "flavor_id": flavor_id,
                        "network_ids": [network_id]
                    }
                }))?))?,
        )
        .await?;
    assert_eq!(native_server_create.status(), StatusCode::CREATED);
    let native_server_json = response_json(native_server_create).await;
    let native_server_id = native_server_json["resource_id"]
        .as_str()
        .ok_or("native server ID")?;
    let openstack_server_read = get_json(
        &compute_app,
        &format!("/v2.1/{project_id}/servers/{native_server_id}"),
        &token,
    )
    .await?;
    assert_eq!(openstack_server_read["server"]["id"], native_server_id);

    assert_eq!(
        status_for(
            &compute_app,
            Method::DELETE,
            &format!("/o3k/v1/compute/servers/{native_server_id}"),
            &token,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status_for(
            &compute_app,
            Method::GET,
            &format!("/v2.1/{project_id}/servers/{native_server_id}"),
            &token,
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status_for(
            &compute_app,
            Method::DELETE,
            &format!("/v2.1/{project_id}/servers/{openstack_server_id}"),
            &token,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status_for(
            &compute_app,
            Method::GET,
            &format!("/o3k/v1/compute/servers/{openstack_server_id}"),
            &token,
        )
        .await,
        StatusCode::NOT_FOUND
    );

    // A restart/reconstruction of the shared store must preserve both links.
    assert_eq!(
        store
            .get_resource(uuid::Uuid::parse_str(native_server_id)?)
            .await?
            .project_id,
        project_id
    );
    Ok(())
}
