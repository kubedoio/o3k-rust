use axum::body::Body;
use http::{Method, Request, StatusCode, header};
use o3k_identity::{Secret, TokenService};
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
            .is_some_and(|items| items.len() == 1)
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
