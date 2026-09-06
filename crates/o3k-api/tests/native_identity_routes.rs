use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn federated_scope_route_reaches_native_identity_handler()
-> Result<(), Box<dyn std::error::Error>> {
    let request = Request::builder()
        .method("POST")
        .uri("/o3k/v1/identity/scopes")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"federated":{"access_token":"opaque"}}"#))?;
    let state = o3k_api::AppState::new().with_native_api(Default::default());
    let response = o3k_api::router_with_state(state).oneshot(request).await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(body["title"], "Not Available");
    assert_eq!(body["detail"], "IAM is not configured");
    Ok(())
}
