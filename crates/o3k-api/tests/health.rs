use axum::body::Body;
use http::{Request, StatusCode};
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
