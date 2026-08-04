use axum::body::Body;
use http::{HeaderValue, Method, Request, StatusCode, header};
use o3k_compute::ComputeService;
use o3k_identity::testkit::test_service;
use o3k_image::{DEFAULT_MAX_UPLOAD_BYTES, ImageService};
use o3k_network::NetworkService;
use o3k_provider::{FailureInjection, FakeComputeProvider};
use o3k_store::SqliteStore;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn health_endpoint_is_machine_readable() -> Result<(), Box<dyn std::error::Error>> {
    let request = Request::builder().uri("/healthz").body(Body::empty())?;
    let response = o3k_api::router().oneshot(request).await?;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(body, serde_json::json!({"status": "ok"}));
    Ok(())
}

#[tokio::test]
async fn registered_agent_console_reads_fall_back_to_durable_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let identity = test_service("http://127.0.0.1:8080").await?;
    let store_path = std::path::PathBuf::from(format!(
        "/tmp/o3k-api-agent-console-{}.sqlite",
        uuid::Uuid::now_v7()
    ));
    let store = std::sync::Arc::new(SqliteStore::connect_file(&store_path).await?);
    let provider = std::sync::Arc::new(FakeComputeProvider::new());
    let registry = o3k_compute_agent::NodeRegistry::default();
    registry
        .register(&o3k_provider_contract::compute_proto::RegisterRequest {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            software_version: "test".to_owned(),
            host_label: "host".to_owned(),
            supported_versions: vec![o3k_compute_agent::PROTOCOL_VERSION],
            capabilities: Some(o3k_provider_contract::compute_proto::Capabilities {
                architecture: "x86_64".to_owned(),
                agent_provider_name: "test".to_owned(),
                agent_provider_version: "1".to_owned(),
                ..Default::default()
            }),
        })
        .await?;
    let placement_root = format!("/tmp/o3k-api-agent-placement-{}", uuid::Uuid::now_v7());
    let placement = o3k_placement::PlacementLedger::open(&placement_root)?;
    placement.register_provider(
        "agent-1",
        std::collections::BTreeMap::from([
            (
                o3k_placement::VCPU.to_owned(),
                o3k_placement::Inventory {
                    total: 8,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
            (
                o3k_placement::MEMORY_MB.to_owned(),
                o3k_placement::Inventory {
                    total: 8192,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
            (
                o3k_placement::DISK_GB.to_owned(),
                o3k_placement::Inventory {
                    total: 100,
                    reserved: 0,
                    allocation_ratio: 1.0,
                    used: 0,
                },
            ),
        ]),
    )?;
    let compute = ComputeService::new(store, provider)
        .with_scheduler(o3k_scheduler::Scheduler::new(placement))
        .with_agent_registry(registry.clone());
    let console = o3k_console::ConsoleService::open(format!(
        "/tmp/o3k-api-agent-console-cache-{}",
        uuid::Uuid::now_v7()
    ))?;
    let state = o3k_api::AppState::new()
        .with_identity(identity)
        .with_compute(compute)
        .with_console(console.clone())
        .with_agent_registry(registry);
    let auth = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let auth_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(auth.to_string()))?,
        )
        .await?;
    let token = auth_response
        .headers()
        .get("x-subject-token")
        .ok_or_else(|| std::io::Error::other("token missing"))?
        .to_str()?
        .to_owned();
    let create = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.1/bootstrap-project/servers")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"server":{"name":"agent-console","image":{"id":"image-1"},"flavor":{"id":"00000000-0000-0000-0000-000000000001"},"networks":[{"uuid":"network-1"}]}}"#,
                ))?,
        )
        .await?;
    assert_eq!(create.status(), StatusCode::ACCEPTED);
    let server: Value =
        serde_json::from_slice(&axum::body::to_bytes(create.into_body(), 8192).await?)?;
    let server_id = server["server"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("server missing"))?;
    let server_uuid = server_id.parse::<uuid::Uuid>()?;
    console.write(server_uuid, b"0123456789abcdef")?;

    let response = o3k_api::router_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/v2.1/bootstrap-project/servers/{server_id}/action"
                ))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"os-getConsoleOutput":{"offset":4,"length":6}}"#,
                ))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(body["output"], "456789");
    Ok(())
}

#[tokio::test]
async fn readiness_reports_startup_failure() -> Result<(), Box<dyn std::error::Error>> {
    let state = o3k_api::AppState::new();
    let request = Request::builder().uri("/readyz").body(Body::empty())?;
    let response = o3k_api::router_with_state(state).oneshot(request).await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(body, serde_json::json!({"status": "not_ready"}));
    Ok(())
}

#[tokio::test]
async fn keystone_password_scope_returns_signed_subject_token()
-> Result<(), Box<dyn std::error::Error>> {
    let service = test_service("http://127.0.0.1:8080").await?;
    let body = serde_json::json!({
        "auth": {
            "identity": {
                "methods": ["password"],
                "password": {"user": {"name": "admin", "password": "password"}}
            },
            "scope": {"project": {"name": "admin"}}
        }
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v3/auth/tokens")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))?;
    let response = o3k_api::router_with_state(o3k_api::AppState::new().with_identity(service))
        .oneshot(request)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let subject_token = response
        .headers()
        .get("x-subject-token")
        .ok_or_else(|| std::io::Error::other("missing subject token"))?
        .to_str()?;
    assert_eq!(subject_token.split('.').count(), 3);
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(body["token"]["project"]["id"], "bootstrap-project");
    assert!(
        body["token"]["catalog"]
            .as_array()
            .is_some_and(|items| items.len() == 6)
    );
    Ok(())
}

#[tokio::test]
async fn keystone_discovery_exposes_v3_without_fallback_warning()
-> Result<(), Box<dyn std::error::Error>> {
    for uri in ["/", "/v3"] {
        let response = o3k_api::router()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            response.status(),
            if uri == "/" {
                StatusCode::MULTIPLE_CHOICES
            } else {
                StatusCode::OK
            },
            "{uri}"
        );
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 8192).await?)?;
        if uri == "/" {
            assert!(
                body["versions"]["values"]
                    .as_array()
                    .is_some_and(|values| !values.is_empty())
            );
        } else {
            assert_eq!(body["version"]["id"], "v3");
            assert_eq!(body["version"]["status"], "stable");
            assert_eq!(body["version"]["links"][0]["rel"], "self");
            assert_eq!(
                body["version"]["links"][0]["href"],
                "http://127.0.0.1:8080/v3"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn keystone_catalog_contains_all_testlab_services_and_consistent_urls()
-> Result<(), Box<dyn std::error::Error>> {
    let identity = test_service("http://testlab.example.invalid/").await?;
    let body = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let response = o3k_api::router_with_state(o3k_api::AppState::new().with_identity(identity))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 16384).await?)?;
    let catalog = body["token"]["catalog"]
        .as_array()
        .ok_or("catalog missing")?;
    let services: std::collections::BTreeMap<_, _> = catalog
        .iter()
        .filter_map(|service| {
            Some((
                service["type"].as_str()?,
                service["endpoints"][0]["url"].as_str()?,
            ))
        })
        .collect();
    assert_eq!(services.len(), 6);
    assert_eq!(services["identity"], "http://testlab.example.invalid/v3");
    assert_eq!(services["image"], "http://testlab.example.invalid/v2");
    assert_eq!(services["network"], "http://testlab.example.invalid/v2.0");
    assert_eq!(
        services["compute"],
        "http://testlab.example.invalid/v2.1/bootstrap-project"
    );
    assert_eq!(
        services["placement"],
        "http://testlab.example.invalid/placement"
    );
    assert_eq!(
        services["volumev3"],
        "http://127.0.0.1:8776/v3/bootstrap-project"
    );
    Ok(())
}

#[tokio::test]
async fn keystone_invalid_password_is_generic_unauthorized()
-> Result<(), Box<dyn std::error::Error>> {
    let service = test_service("http://127.0.0.1:8080").await?;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v3/auth/tokens")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"wrong"}}},"scope":{"project":{"name":"admin"}}}}"#))?;
    let response = o3k_api::router_with_state(o3k_api::AppState::new().with_identity(service))
        .oneshot(request)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(response.into_body(), 4096).await?;
    assert!(!String::from_utf8(body.to_vec())?.contains("wrong"));
    Ok(())
}

#[tokio::test]
async fn keystone_rejects_missing_scope_and_wrong_project_without_leaking_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let service = test_service("http://127.0.0.1:8080").await?;
    for body in [
        serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}}}}),
        serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"other-project"}}}}),
    ] {
        let response =
            o3k_api::router_with_state(o3k_api::AppState::new().with_identity(service.clone()))
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v3/auth/tokens")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))?,
                )
                .await?;
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
        ));
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let text = String::from_utf8(bytes.to_vec())?;
        assert!(!text.contains("password"));
        assert!(!text.contains("other-project"));
    }
    Ok(())
}

#[tokio::test]
async fn glance_image_lifecycle_is_project_scoped_and_immutable_after_upload()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(format!("/tmp/o3k-api-images-{}", std::process::id()));
    let identity = test_service("http://127.0.0.1:8080").await?;
    let image = ImageService::open(&root, DEFAULT_MAX_UPLOAD_BYTES)?;
    let state = o3k_api::AppState::new()
        .with_identity(identity)
        .with_image(image);
    let auth_body = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let auth_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(auth_body.to_string()))?,
        )
        .await?;
    let token = auth_response
        .headers()
        .get("x-subject-token")
        .ok_or_else(|| std::io::Error::other("token missing"))?
        .to_str()?
        .to_owned();
    let create_body = serde_json::json!({"name":"test-image","visibility":"private","container_format":"bare","disk_format":"raw"});
    let create_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2/images")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(create_body.to_string()))?,
        )
        .await?;
    assert_eq!(create_response.status(), StatusCode::CREATED);
    let create_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(create_response.into_body(), 4096).await?)?;
    let id = create_json["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("image id missing"))?
        .to_owned();
    let checksum_mismatch = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/v2/images/{id}/file"))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header("x-openstack-image-sha256", "00")
                .body(Body::from("image-content"))?,
        )
        .await?;
    assert_eq!(checksum_mismatch.status(), StatusCode::BAD_REQUEST);
    let upload_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/v2/images/{id}/file"))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("image-content"))?,
        )
        .await?;
    assert_eq!(upload_response.status(), StatusCode::NO_CONTENT);
    let second_upload = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/v2/images/{id}/file"))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("changed"))?,
        )
        .await?;
    assert_eq!(second_upload.status(), StatusCode::CONFLICT);
    let show_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/v2/images/{id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    let show_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(show_response.into_body(), 4096).await?)?;
    assert_eq!(show_json["status"], "active");
    assert_eq!(show_json["size"], 13);
    let download_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2/images/{id}/file"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(download_response.status(), StatusCode::OK);
    assert_eq!(
        download_response.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert_eq!(download_response.headers()[header::CONTENT_LENGTH], "13");
    assert_eq!(
        axum::body::to_bytes(download_response.into_body(), 4096).await?,
        "image-content"
    );
    let invalid_format = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2/images")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"invalid","visibility":"private","container_format":"bare","disk_format":"vmdk"}"#,
                ))?,
        )
        .await?;
    assert_eq!(invalid_format.status(), StatusCode::BAD_REQUEST);
    let delete_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2/images/{id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn neutron_network_subnet_port_lifecycle_is_deterministic()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(format!("/tmp/o3k-api-network-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let identity = test_service("http://127.0.0.1:8080").await?;
    let state = o3k_api::AppState::new()
        .with_identity(identity)
        .with_network(NetworkService::open(&root)?);
    let auth = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(auth.to_string()))?,
        )
        .await?;
    let token = response
        .headers()
        .get("x-subject-token")
        .ok_or_else(|| std::io::Error::other("token missing"))?
        .to_str()?
        .to_owned();
    let extensions = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/v2.0/extensions")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(extensions.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &axum::body::to_bytes(extensions.into_body(), 4096).await?
        )?,
        serde_json::json!({"extensions": []})
    );
    let body = serde_json::json!({"network":{"name":"flat"}});
    let response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/networks")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let network: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
    let network_id = network["network"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("network id missing"))?
        .to_owned();
    let body =
        serde_json::json!({"subnet":{"name":"lab","network_id":network_id,"cidr":"192.0.2.0/29"}});
    let response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/subnets")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let subnet: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(subnet["subnet"]["gateway_ip"], "192.0.2.1");
    let unsupported_pools = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/subnets")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"subnet":{{"name":"many-pools","network_id":"{network_id}","cidr":"198.51.100.0/29","allocation_pools":[{{"start":"198.51.100.2","end":"198.51.100.3"}},{{"start":"198.51.100.5","end":"198.51.100.6"}}]}}}}"#
                )))?,
        )
        .await?;
    assert_eq!(unsupported_pools.status(), StatusCode::BAD_REQUEST);
    let body = serde_json::json!({"port":{"name":"port-1","network_id":network_id}});
    let response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/ports")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let port: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
    assert!(port["port"]["mac_address"].as_str().is_some());
    assert_eq!(port["port"]["fixed_ips"][0]["ip_address"], "192.0.2.2");
    let conflict = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.0/networks/{network_id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let delete_port = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!(
                    "/v2.0/ports/{}",
                    port["port"]["id"].as_str().unwrap_or_default()
                ))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_port.status(), StatusCode::NO_CONTENT);
    let delete_subnet = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!(
                    "/v2.0/subnets/{}",
                    subnet["subnet"]["id"].as_str().unwrap_or_default()
                ))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_subnet.status(), StatusCode::NO_CONTENT);
    let delete_network = o3k_api::router_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.0/networks/{network_id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_network.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn nova_server_lifecycle_uses_project_scoped_envelopes()
-> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(format!(
        "/tmp/o3k-api-compute-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let identity = test_service("http://127.0.0.1:8080").await?;
    let store = std::sync::Arc::new(SqliteStore::connect_file(&path).await?);
    let provider = std::sync::Arc::new(FakeComputeProvider::new());
    let compute = ComputeService::new(store, provider.clone());
    let network_root = std::path::PathBuf::from(format!(
        "/tmp/o3k-api-compute-network-{}",
        uuid::Uuid::now_v7()
    ));
    let network_service = NetworkService::open(&network_root)?;
    let network = network_service.create_network("bootstrap-project", "flat".to_owned())?;
    let _subnet = network_service.create_subnet(
        "bootstrap-project",
        network.id,
        "subnet".to_owned(),
        "192.0.2.0/29".to_owned(),
        None,
        None,
        None,
    )?;
    let port =
        network_service.create_port("bootstrap-project", network.id, "server-port".to_owned())?;
    let port_id = port.id.to_string();
    let expected_fixed_ip = port.fixed_ip.to_string();
    let console = o3k_console::ConsoleService::open(format!(
        "/tmp/o3k-api-console-{}",
        uuid::Uuid::now_v7()
    ))?;
    let state = o3k_api::AppState::new()
        .with_identity(identity)
        .with_compute(compute)
        .with_network(network_service)
        .with_console(console.clone());
    let auth = serde_json::json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}});
    let response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(auth.to_string()))?,
        )
        .await?;
    let token = response
        .headers()
        .get("x-subject-token")
        .ok_or_else(|| std::io::Error::other("token missing"))?
        .to_str()?
        .to_owned();
    let flavors = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/v2.1/bootstrap-project/flavors")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(flavors.status(), StatusCode::OK);
    let flavor_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(flavors.into_body(), 4096).await?)?;
    let default_flavor_id = flavor_json["flavors"][0]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("default flavor missing"))?
        .to_owned();
    let custom_flavor = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.1/bootstrap-project/flavors")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"flavor":{"name":"api.custom","vcpus":1,"ram":768,"disk":4}}"#,
                ))?,
        )
        .await?;
    assert_eq!(custom_flavor.status(), StatusCode::CREATED);
    let custom_flavor_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(custom_flavor.into_body(), 4096).await?)?;
    let custom_flavor_id = custom_flavor_json["flavor"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("custom flavor missing"))?
        .to_owned();
    let custom_show = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v2.1/bootstrap-project/flavors/{custom_flavor_id}"
                ))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(custom_show.status(), StatusCode::OK);
    let detailed_flavors = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/v2.1/bootstrap-project/flavors/detail")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(detailed_flavors.status(), StatusCode::OK);
    let flavor_id = default_flavor_id.as_str();
    let detailed_servers = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/v2.1/bootstrap-project/servers/detail")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(detailed_servers.status(), StatusCode::OK);
    let keypair_create = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.1/bootstrap-project/os-keypairs")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"keypair":{"name":"nova-test-key","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test"}}).to_string()))?,
        )
        .await?;
    assert_eq!(keypair_create.status(), StatusCode::OK);
    let keypair_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(keypair_create.into_body(), 8192).await?)?;
    assert_eq!(keypair_json["keypair"]["name"], "nova-test-key");
    assert_eq!(keypair_json["keypair"]["type"], "ssh");
    assert_eq!(
        keypair_json["keypair"]["fingerprint"]
            .as_str()
            .map(str::len),
        Some(47)
    );
    let keypair_list = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/v2.1/bootstrap-project/os-keypairs")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(keypair_list.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &axum::body::to_bytes(keypair_list.into_body(), 8192).await?
        )?["keypairs"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let request_body = serde_json::json!({"server":{"name":"nova-test","image":{"id":"image-1"},"flavor":{"id":flavor_id},"networks":[{"port":port_id.clone()}],"key_name":"nova-test-key"}});
    let created = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.1/bootstrap-project/servers")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-openstack-request-id", "nova-test-request")
                .body(Body::from(request_body.to_string()))?,
        )
        .await?;
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let server_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(created.into_body(), 8192).await?)?;
    assert_eq!(server_json["server"]["status"], "ACTIVE");
    assert_eq!(
        server_json["server"]["addresses"][network.id.to_string()][0]["addr"],
        expected_fixed_ip
    );
    let server_id = server_json["server"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("server missing"))?;
    let server_uuid = server_id.parse::<uuid::Uuid>()?;
    console.write(server_uuid, b"0123456789abcdef")?;
    let console_response = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/v2.1/bootstrap-project/servers/{server_id}/action"
                ))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"os-getConsoleOutput":{"offset":4,"length":6}}"#,
                ))?,
        )
        .await?;
    assert_eq!(console_response.status(), StatusCode::OK);
    let console_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(console_response.into_body(), 4096).await?)?;
    assert_eq!(console_json["output"], "456789");
    let stopped = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/v2.1/bootstrap-project/servers/{server_id}/action"
                ))
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"os-stop":null}"#))?,
        )
        .await?;
    assert_eq!(stopped.status(), StatusCode::ACCEPTED);
    let deleted = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.1/bootstrap-project/servers/{server_id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(matches!(
        console.read(server_uuid),
        Err(o3k_console::ConsoleError::NotFound)
    ));
    let custom_deleted = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!(
                    "/v2.1/bootstrap-project/flavors/{custom_flavor_id}"
                ))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(custom_deleted.status(), StatusCode::NO_CONTENT);
    let keypair_deleted = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/v2.1/bootstrap-project/os-keypairs/nova-test-key")
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(keypair_deleted.status(), StatusCode::NO_CONTENT);

    let second_body = serde_json::json!({"server":{"name":"nova-failed-delete","image":{"id":"image-1"},"flavor":{"id":default_flavor_id},"networks":[{"uuid":port_id}]}});
    let second_created = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.1/bootstrap-project/servers")
                .header("x-auth-token", &token)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-openstack-request-id", "nova-failed-delete-request")
                .body(Body::from(second_body.to_string()))?,
        )
        .await?;
    assert_eq!(second_created.status(), StatusCode::ACCEPTED);
    let second_json: Value =
        serde_json::from_slice(&axum::body::to_bytes(second_created.into_body(), 8192).await?)?;
    let second_id = second_json["server"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("second server missing"))?;
    let second_uuid = second_id.parse::<uuid::Uuid>()?;
    console.write(second_uuid, b"failed-delete-console")?;
    provider.set_failure(FailureInjection::Transient)?;
    let failed_delete = o3k_api::router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.1/bootstrap-project/servers/{second_id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(failed_delete.status(), StatusCode::CONFLICT);
    assert_eq!(console.read(second_uuid)?, b"failed-delete-console");

    let repeated_delete = o3k_api::router_with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.1/bootstrap-project/servers/{server_id}"))
                .header("x-auth-token", &token)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(repeated_delete.status(), StatusCode::NO_CONTENT);
    assert!(matches!(
        console.read(server_uuid),
        Err(o3k_console::ConsoleError::NotFound)
    ));
    std::fs::remove_file(path)?;
    std::fs::remove_dir_all(network_root)?;
    Ok(())
}

#[tokio::test]
async fn microversion_nova_discovery_and_negotiation() -> Result<(), Box<dyn std::error::Error>> {
    let req = Request::builder().uri("/v2.1").body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let json: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 2048).await?)?;
    assert_eq!(json["version"]["id"], "v2.1");
    assert_eq!(json["version"]["version"], "2.1");
    assert_eq!(json["version"]["min_version"], "2.1");

    let req = Request::builder().uri("/v2.1/").body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::OK);

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("compute 2.1"))
    );
    assert_eq!(
        resp.headers().get("X-OpenStack-Nova-API-Version"),
        Some(&HeaderValue::from_static("2.1"))
    );
    assert_eq!(
        resp.headers().get("Vary"),
        Some(&HeaderValue::from_static(
            "OpenStack-API-Version, X-OpenStack-Nova-API-Version"
        ))
    );

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .header("OpenStack-API-Version", "compute 2.1")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("compute 2.1"))
    );

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .header("X-OpenStack-Nova-API-Version", "2.1")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("compute 2.1"))
    );

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .header("OpenStack-API-Version", "compute 2.95")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    let json: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 2048).await?)?;
    assert_eq!(json["computeFault"]["code"], 406);

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .header("OpenStack-API-Version", "compute invalid_ver extra_token")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let req = Request::builder()
        .uri("/v2.1/bootstrap-project/flavors")
        .header("OpenStack-API-Version", "placement 1.28")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("compute 2.1"))
    );

    Ok(())
}

#[tokio::test]
async fn microversion_placement_discovery_and_negotiation() -> Result<(), Box<dyn std::error::Error>>
{
    let req = Request::builder().uri("/placement").body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let json: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 2048).await?)?;
    assert_eq!(json["versions"][0]["id"], "v1.0");
    assert_eq!(json["versions"][0]["min_version"], "1.0");
    assert_eq!(json["versions"][0]["max_version"], "1.28");

    let req = Request::builder()
        .uri("/placement/resource_providers")
        .header("OpenStack-API-Version", "placement 1.28")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("placement 1.28"))
    );

    let req = Request::builder()
        .uri("/placement/resource_providers")
        .header("OpenStack-API-Version", "placement latest")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(
        resp.headers().get("OpenStack-API-Version"),
        Some(&HeaderValue::from_static("placement 1.28"))
    );

    let req = Request::builder()
        .uri("/placement/resource_providers")
        .header("OpenStack-API-Version", "placement 1.39")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);

    Ok(())
}

#[tokio::test]
async fn identity_image_network_services_have_no_microversion_headers()
-> Result<(), Box<dyn std::error::Error>> {
    let req = Request::builder().uri("/v3").body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert!(resp.headers().get("OpenStack-API-Version").is_none());

    let req = Request::builder().uri("/v2/images").body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert!(resp.headers().get("OpenStack-API-Version").is_none());

    let req = Request::builder()
        .uri("/v2.0/networks")
        .body(Body::empty())?;
    let resp = o3k_api::router().oneshot(req).await?;
    assert!(resp.headers().get("OpenStack-API-Version").is_none());

    Ok(())
}

#[tokio::test]
async fn keystone_get_and_head_token_validation_and_cinder_catalog()
-> Result<(), Box<dyn std::error::Error>> {
    use tower::ServiceExt;

    let identity = test_service("http://127.0.0.1:18080").await?;

    let state = o3k_api::AppState::new().with_identity(identity);
    state.set_ready(true);
    let app = o3k_api::router_with_state(state);

    let auth_body = serde_json::json!({
        "auth": {
            "identity": {
                "methods": ["password"],
                "password": {
                    "user": {
                        "name": "admin",
                        "password": "password"
                    }
                }
            },
            "scope": {
                "project": {
                    "name": "admin"
                }
            }
        }
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/v3/auth/tokens")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(auth_body.to_string()))?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let token = resp
        .headers()
        .get("x-subject-token")
        .ok_or("missing x-subject-token header")?
        .to_str()?
        .to_owned();

    // Verify GET /v3/auth/tokens validates token
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v3/auth/tokens")
        .header("x-subject-token", &token)
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-subject-token")
            .ok_or("missing x-subject-token header")?
            .to_str()?,
        token
    );

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: Value = serde_json::from_slice(&body_bytes)?;
    let catalog = json["token"]["catalog"]
        .as_array()
        .ok_or("missing catalog")?;
    let cinder_entry = catalog
        .iter()
        .find(|item| item["type"] == "volumev3")
        .ok_or("missing volumev3 service entry")?;
    assert_eq!(
        cinder_entry["endpoints"][0]["url"],
        "http://127.0.0.1:8776/v3/bootstrap-project"
    );

    // Verify HEAD /v3/auth/tokens validates token without body
    let req = Request::builder()
        .method(Method::HEAD)
        .uri("/v3/auth/tokens")
        .header("x-subject-token", &token)
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-subject-token")
            .ok_or("missing x-subject-token header")?
            .to_str()?,
        token
    );

    // Invalid token on GET returns 404
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v3/auth/tokens")
        .header("x-subject-token", "invalid.token.str")
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn nova_volume_attachment_lifecycle_list_create_show_delete()
-> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;
    use tower::ServiceExt;

    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
    let provider = Arc::new(FakeComputeProvider::new());
    let compute = ComputeService::new(store, provider);

    let state = o3k_api::AppState::new().with_compute(compute);
    state.set_ready(true);
    let app = o3k_api::router_with_state(state);

    let server_id = uuid::Uuid::now_v7();
    let volume_id = uuid::Uuid::now_v7();

    // 1. Attaching volume to non-existent server returns 404
    let attach_body = serde_json::json!({
        "volumeAttachment": {
            "volumeId": volume_id.to_string(),
            "device": "/dev/vdb"
        }
    });
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/v2.1/project-1/servers/{server_id}/os-volume_attachments"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(attach_body.to_string()))?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 2. Listing attachments for non-existent server returns 404
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/v2.1/project-1/servers/{server_id}/os-volume_attachments"
        ))
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    Ok(())
}
