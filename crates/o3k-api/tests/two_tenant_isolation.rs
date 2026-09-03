//! Integration tests for two-tenant isolation, cross-scope denial without
//! disclosure, and zero-side-effect enforcement on unauthorized/denied mutations.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use o3k_api::AppState;
use o3k_compute::ComputeService;
use o3k_domain::Ipv4Prefix;
use o3k_identity::{BootstrapConfig, ExtraProjectSeed, Secret, TokenService};
use o3k_image::ImageService;
use o3k_kernel::{AuditOutcome, MemoryAuditSink};
use o3k_network::{NetworkService, PublicAddressAllocator, PublicAddressPool};
use o3k_provider::FakeComputeProvider;
use o3k_store::{DurableStore, testkit::TestStore};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

const PROJECT_A: &str = "eba29e2d-53de-461d-ae91-ede7402713cb";
const PROJECT_B: &str = "9f3c2b6e-5f2d-4b3a-9c8e-1a2b3c4d5e6f";
const USER_B: &str = "6b0f5a2e-8c4d-4a7e-9b1f-2d3e4f5a6b7c";

struct TwoTenantHarness {
    app: axum::Router,
    store: Arc<TestStore>,
    provider: Arc<FakeComputeProvider>,
    audit_sink: Arc<MemoryAuditSink>,
    token_a: String,
    token_b: String,
    external_realm_id: uuid::Uuid,
    image_dir: std::path::PathBuf,
    net_dir: std::path::PathBuf,
    fip_dir: std::path::PathBuf,
}

async fn build_harness() -> Result<TwoTenantHarness, Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    o3k_identity::seed_identity_defaults(
        store.as_ref(),
        &BootstrapConfig {
            catalog_endpoint: "http://127.0.0.1:18090".to_owned(),
            bootstrap_password: Secret::new("password".to_owned()),
            cinder_password: None,
            cinder_endpoint: None,
            pbkdf2_iterations: 1_000,
            extra_projects: vec![ExtraProjectSeed {
                project_id: PROJECT_B.to_owned(),
                project_name: "tenant-b".to_owned(),
                user_id: USER_B.to_owned(),
                user_name: "tenant-b-user".to_owned(),
                password: Secret::new("tenant-b-password".to_owned()),
            }],
        },
    )
    .await?;

    let audit_sink = Arc::new(MemoryAuditSink::new());

    let provider = Arc::new(FakeComputeProvider::new());
    let compute =
        ComputeService::new(store.clone(), provider.clone()).with_audit_sink(audit_sink.clone());
    let identity = TokenService::load(
        store.clone(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )
    .await?;
    let image_dir = std::env::temp_dir().join(format!("o3k-img-test-{}", uuid::Uuid::now_v7()));
    let image = ImageService::open(&image_dir, 1024 * 1024, store.clone())
        .await?
        .with_audit_sink(audit_sink.clone());
    let net_dir = std::env::temp_dir().join(format!("o3k-net-test-{}", uuid::Uuid::now_v7()));
    let network = NetworkService::open(&net_dir, store.clone())
        .await?
        .with_audit_sink(audit_sink.clone());

    let fip_dir = std::env::temp_dir().join(format!("o3k-fip-test-{}", uuid::Uuid::now_v7()));
    let prefix = Ipv4Prefix::new("198.51.100.0".parse()?, 29).ok_or("invalid pool")?;
    let allocator = PublicAddressAllocator::open(
        fip_dir.join("public"),
        PublicAddressPool {
            prefix,
            first_usable: "198.51.100.2".parse()?,
            last_usable: "198.51.100.6".parse()?,
        },
    )?;
    let external_realm_id = uuid::Uuid::now_v7();

    let state = AppState::new()
        .with_identity(identity)
        .with_compute(compute)
        .with_image(image)
        .with_network(network)
        .with_public_allocator(allocator)
        .with_network_external_realm(external_realm_id);
    state.set_ready(true);
    let app = o3k_api::router_with_state(state);

    let token_a = issue_token(&app, "admin", "password", "admin").await?;
    let token_b = issue_token(&app, "tenant-b-user", "tenant-b-password", "tenant-b").await?;

    Ok(TwoTenantHarness {
        app,
        store,
        provider,
        audit_sink,
        token_a,
        token_b,
        external_realm_id,
        image_dir,
        net_dir,
        fip_dir,
    })
}

async fn issue_token(
    app: &axum::Router,
    user: &str,
    password: &str,
    project: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let request = serde_json::json!({
        "auth": {
            "identity": {"methods": ["password"], "password": {"user": {"name": user, "password": password}}},
            "scope": {"project": {"name": project}}
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v3/auth/tokens")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request)?))?,
        )
        .await?;
    let token = response
        .headers()
        .get("x-subject-token")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing x-subject-token header")?
        .to_owned();
    Ok(token)
}

#[tokio::test]
async fn two_tenant_path_and_resource_isolation() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness().await?;

    // 1. Tenant A creates an image
    let img_req = serde_json::json!({
        "name": "image-a",
        "disk_format": "qcow2",
        "container_format": "bare"
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2/images")
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&img_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let img_json: Value = serde_json::from_slice(&body_bytes)?;
    let img_id = img_json["id"].as_str().ok_or("missing image id")?;

    // 2. Tenant B listing images does NOT see Tenant A's private image
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v2/images")
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let list_json: Value = serde_json::from_slice(&body_bytes)?;
    let images = list_json["images"].as_array().ok_or("missing images")?;
    assert!(images.is_empty());

    // 3. Tenant B attempting to get Tenant A's image gets 404 (no existence disclosure)
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2/images/{img_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 4. Tenant B attempting to delete Tenant A's image gets 404
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2/images/{img_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 5. Tenant A creates a network
    let net_req = serde_json::json!({
        "network": {
            "name": "net-a"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/networks")
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&net_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let net_json: Value = serde_json::from_slice(&body_bytes)?;
    let net_id = net_json["network"]["id"]
        .as_str()
        .ok_or("missing network id")?;

    // 6. Tenant B attempting to get Tenant A's network gets 404
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2.0/networks/{net_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A foreign project cannot mutate the network by UUID.
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/v2.0/networks/{net_id}"))
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"network":{"name":"stolen"}}"#))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.0/networks/{net_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 7. Tenant A Token with Tenant B path -> 404 Not Found (OpenStack concealed scope denial)
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2.1/{PROJECT_B}/flavors"))
                .header("x-auth-token", &harness.token_a)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 8. Tenant A imports keypair
    let kp_req = serde_json::json!({
        "keypair": {
            "name": "key-a",
            "public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBJuQvak7YBzsbN71EyvJnDK8pODWM1Ox/3wO3tT8Adj o3k-test"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v2.1/{PROJECT_A}/os-keypairs"))
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&kp_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);

    // 9. Tenant B attempting to show Tenant A's keypair gets 404
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2.1/{PROJECT_B}/os-keypairs/key-a"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A foreign project cannot delete the keypair by its name.
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.1/{PROJECT_B}/os-keypairs/key-a"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 10. Zero side effects on unauthorized server action
    let fake_srv_id = uuid::Uuid::now_v7();
    let action_req = serde_json::json!({
        "os-start": null
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v2.1/{PROJECT_B}/servers/{fake_srv_id}/action"))
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&action_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(harness.provider.instance_count(), 0);
    assert!(harness.store.get_resource(fake_srv_id).await.is_err());

    // 11. Assert audit trail captures outcomes correctly without secrets
    let events = harness.audit_sink.events();
    assert!(!events.is_empty(), "audit sink must record events");

    let succeeded_count = events
        .iter()
        .filter(|e| e.outcome == AuditOutcome::Succeeded)
        .count();

    assert!(
        succeeded_count >= 3,
        "expected at least 3 successful mutations (image create, network create, keypair create), got {succeeded_count}"
    );

    // Verify no secrets/tokens leaked in audit events
    for event in &events {
        let serialized = serde_json::to_string(event)?;
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("tenant-b-password"));
    }

    // 12. Clean up temp dirs
    let _ = std::fs::remove_dir_all(&harness.image_dir);
    let _ = std::fs::remove_dir_all(&harness.net_dir);
    let _ = std::fs::remove_dir_all(&harness.fip_dir);

    Ok(())
}

#[tokio::test]
async fn two_tenant_floating_ip_cross_scope_denial_is_concealed()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness().await?;

    // Tenant A allocates a floating IP.
    let fip_req = serde_json::json!({
        "floatingip": {
            "floating_network_id": harness.external_realm_id
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/floatingips")
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&fip_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let fip_json: Value = serde_json::from_slice(&body_bytes)?;
    let fip_id = fip_json["floatingip"]["id"]
        .as_str()
        .ok_or("missing floating ip id")?
        .to_owned();

    // Tenant B attempting to get Tenant A's floating IP gets 404, and the body
    // must not disclose the foreign allocation id or owning project.
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v2.0/floatingips/{fip_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    assert!(!body.contains(&fip_id));
    assert!(!body.contains(PROJECT_A));
    assert!(!body.contains("admin"));

    // Tenant B attempting to delete Tenant A's floating IP gets 404 with the
    // same non-disclosure guarantee.
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v2.0/floatingips/{fip_id}"))
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    assert!(!body.contains(&fip_id));
    assert!(!body.contains(PROJECT_A));
    assert!(!body.contains("admin"));

    let _ = std::fs::remove_dir_all(&harness.image_dir);
    let _ = std::fs::remove_dir_all(&harness.net_dir);
    let _ = std::fs::remove_dir_all(&harness.fip_dir);

    Ok(())
}

#[tokio::test]
async fn two_tenant_security_group_binding_cross_scope_denial_is_concealed()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness().await?;

    // Tenant A creates a security group.
    let sg_req = serde_json::json!({
        "security_group": {
            "name": "sg-a"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/security-groups")
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&sg_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let sg_json: Value = serde_json::from_slice(&body_bytes)?;
    let sg_id = sg_json["security_group"]["id"]
        .as_str()
        .ok_or("missing security group id")?
        .to_owned();

    // Tenant B creates a network and a port.
    let net_req = serde_json::json!({
        "network": {
            "name": "net-b"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/networks")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&net_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let net_json: Value = serde_json::from_slice(&body_bytes)?;
    let net_id = net_json["network"]["id"]
        .as_str()
        .ok_or("missing network id")?;

    let subnet_req = serde_json::json!({
        "subnet": {
            "network_id": net_id,
            "name": "subnet-b",
            "cidr": "10.0.0.0/29"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/subnets")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&subnet_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let port_req = serde_json::json!({
        "port": {
            "network_id": net_id,
            "name": "port-b"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/ports")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&port_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let port_json: Value = serde_json::from_slice(&body_bytes)?;
    let port_id = port_json["port"]["id"].as_str().ok_or("missing port id")?;

    // Tenant B attempting to bind Tenant A's security group to B's port gets
    // 404 (not 400 or 409), and the body must not disclose the foreign group.
    let bind_req = serde_json::json!({
        "port": {
            "security_groups": [sg_id]
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/v2.0/ports/{port_id}"))
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bind_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    assert!(!body.contains(&sg_id));
    assert!(!body.contains(PROJECT_A));
    assert!(!body.contains("admin"));

    let _ = std::fs::remove_dir_all(&harness.image_dir);
    let _ = std::fs::remove_dir_all(&harness.net_dir);
    let _ = std::fs::remove_dir_all(&harness.fip_dir);

    Ok(())
}

#[tokio::test]
async fn foreign_security_group_port_create_is_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness().await?;

    // Tenant A creates a security group.
    let sg_req = serde_json::json!({
        "security_group": {
            "name": "sg-a"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/security-groups")
                .header("x-auth-token", &harness.token_a)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&sg_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let sg_json: Value = serde_json::from_slice(&body_bytes)?;
    let sg_id = sg_json["security_group"]["id"]
        .as_str()
        .ok_or("missing security group id")?
        .to_owned();

    // Tenant B creates a network and subnet.
    let net_req = serde_json::json!({
        "network": {
            "name": "net-b"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/networks")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&net_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let net_json: Value = serde_json::from_slice(&body_bytes)?;
    let net_id = net_json["network"]["id"]
        .as_str()
        .ok_or("missing network id")?
        .to_owned();

    let subnet_req = serde_json::json!({
        "subnet": {
            "network_id": net_id,
            "name": "subnet-b",
            "cidr": "10.0.0.0/29"
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/subnets")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&subnet_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Tenant B attempts to CREATE a port on B's own network while binding
    // Tenant A's security group in the same request. The request must be
    // rejected non-disclosingly AND must not leave a partially created port
    // behind (fail-closed: no provider/store side effect from a denied
    // cross-scope relationship).
    let port_req = serde_json::json!({
        "port": {
            "network_id": net_id,
            "name": "port-b-with-foreign-sg",
            "security_groups": [sg_id]
        }
    });
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v2.0/ports")
                .header("x-auth-token", &harness.token_b)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&port_req)?))?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    assert!(!body.contains(&sg_id));
    assert!(!body.contains(PROJECT_A));
    assert!(!body.contains("admin"));

    // No port may have been created for tenant B.
    let resp = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v2.0/ports")
                .header("x-auth-token", &harness.token_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let ports_json: Value = serde_json::from_slice(&body_bytes)?;
    let ports = ports_json["ports"]
        .as_array()
        .ok_or("missing ports array")?;
    assert!(
        ports.is_empty(),
        "denied cross-scope port create left a partial port behind"
    );

    let _ = std::fs::remove_dir_all(&harness.image_dir);
    let _ = std::fs::remove_dir_all(&harness.net_dir);
    let _ = std::fs::remove_dir_all(&harness.fip_dir);

    Ok(())
}
