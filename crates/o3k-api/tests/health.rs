use axum::body::Body;
use http::{Method, Request, StatusCode, header};
use o3k_identity::{Secret, TokenService};
use o3k_image::{DEFAULT_MAX_UPLOAD_BYTES, ImageService};
use o3k_network::NetworkService;
use serde_json::Value;
use std::time::Duration;
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
    let service = TokenService::new(
        "bootstrap-user".to_owned(),
        "admin".to_owned(),
        Secret::new("password".to_owned()),
        "bootstrap-project".to_owned(),
        "admin".to_owned(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )?;
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
            .is_some_and(|items| items.len() == 3)
    );
    Ok(())
}

#[tokio::test]
async fn keystone_invalid_password_is_generic_unauthorized()
-> Result<(), Box<dyn std::error::Error>> {
    let service = TokenService::new(
        "bootstrap-user".to_owned(),
        "admin".to_owned(),
        Secret::new("password".to_owned()),
        "bootstrap-project".to_owned(),
        "admin".to_owned(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )?;
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
async fn glance_image_lifecycle_is_project_scoped_and_immutable_after_upload()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(format!("/tmp/o3k-api-images-{}", std::process::id()));
    let identity = TokenService::new(
        "bootstrap-user".to_owned(),
        "admin".to_owned(),
        Secret::new("password".to_owned()),
        "bootstrap-project".to_owned(),
        "admin".to_owned(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )?;
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
    let identity = TokenService::new(
        "bootstrap-user".to_owned(),
        "admin".to_owned(),
        Secret::new("password".to_owned()),
        "bootstrap-project".to_owned(),
        "admin".to_owned(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )?;
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
    assert_eq!(port["port"]["fixed_ips"][0]["ip_address"], "192.0.2.2");
    std::fs::remove_dir_all(root)?;
    Ok(())
}
