//! Shared helpers for the o3k-cinder integration tests. The Keystone-compatible
//! token endpoint is backed by the real O3K identity implementation and
//! exercises the same public surface used by Cinder's keystone middleware
//! (POST issues, GET validates with details, HEAD checks).

use axum::{
    Router,
    response::IntoResponse,
    routing::{get, post},
};
use o3k_cinder::testkit::{self, FakeCinderState};
use o3k_cinder::{CinderClient, CinderClientConfig};
use o3k_identity::{Secret, TokenService};
use serde_json::json;

pub fn subject_response(
    status: axum::http::StatusCode,
    subject: Option<String>,
    value: serde_json::Value,
) -> axum::response::Response {
    let mut response = (status, axum::Json(value)).into_response();
    if let Some(subject) = subject
        && let Ok(header) = axum::http::HeaderValue::from_str(&subject)
    {
        response.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-subject-token"),
            header,
        );
    }
    response.headers_mut().insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static("X-Auth-Token"),
    );
    response
}

/// Minimal Keystone-compatible token endpoint backed by the real O3K identity
/// implementation.
pub async fn keystone_router(
    service: TokenService,
) -> Result<(Router, std::net::SocketAddr), String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind keystone port: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local addr: {error}"))?;
    let app = Router::new()
        .route(
            "/v3/auth/tokens",
            post({
                let service = service.clone();
                move |body: axum::body::Bytes| {
                    let service = service.clone();
                    async move {
                        let request: o3k_identity::TokenRequest =
                            match serde_json::from_slice(&body) {
                                Ok(request) => request,
                                Err(_) => {
                                    return subject_response(
                                        axum::http::StatusCode::BAD_REQUEST,
                                        None,
                                        json!({"error": {"code": 400, "title": "Bad Request", "message": "invalid authentication request"}}),
                                    );
                                }
                            };
                        let result = service.issue(&request, std::time::SystemTime::now());
                        match result {
                            Ok((token, response)) => subject_response(
                                axum::http::StatusCode::CREATED,
                                Some(token),
                                match serde_json::to_value(response) {
                                    Ok(value) => value,
                                    Err(_) => return subject_response(
                                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                        None,
                                        json!({"error": {"code": 500, "title": "Internal Server Error", "message": "token could not be encoded"}}),
                                    ),
                                },
                            ),
                            Err(_) => subject_response(
                                axum::http::StatusCode::UNAUTHORIZED,
                                None,
                                json!({"error": {"code": 401, "title": "Unauthorized", "message": "The request has not been authenticated."}}),
                            ),
                        }
                    }
                }
            }),
        )
        .route(
            "/v3/auth/tokens",
            get({
                let service = service.clone();
                move |headers: axum::http::HeaderMap| {
                    let service = service.clone();
                    async move {
                        let token = headers
                            .get("x-subject-token")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default();
                        match service.verify_details(token, std::time::SystemTime::now()) {
                            Ok(response) => subject_response(
                                axum::http::StatusCode::OK,
                                Some(token.to_owned()),
                                match serde_json::to_value(response) {
                                    Ok(value) => value,
                                    Err(_) => return subject_response(
                                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                        None,
                                        json!({"error": {"code": 500, "title": "Internal Server Error", "message": "token could not be encoded"}}),
                                    ),
                                },
                            ),
                            Err(_) => subject_response(
                                axum::http::StatusCode::NOT_FOUND,
                                None,
                                json!({"error": {"code": 404, "title": "Not Found", "message": "Could not find token"}}),
                            ),
                        }
                    }
                }
            }),
        )
        .with_state(service);
    let service_task_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, service_task_app).await;
    });
    Ok((app, address))
}

/// Builds a `CinderClient` plus the stateful fake Cinder server, both backed
/// by a real O3K identity service on an ephemeral port.
pub async fn setup() -> Result<(CinderClient, FakeCinderState, String), String> {
    let identity = o3k_identity::testkit::test_service("http://127.0.0.1:8080")
        .await
        .map_err(|_| "identity service".to_owned())?;
    let (_app, keystone_address) = keystone_router(identity).await?;
    let keystone_endpoint = format!("http://{keystone_address}");
    let (fake, cinder_address) =
        testkit::start_fake_cinder(Some(keystone_endpoint.clone())).await?;
    let client = CinderClient::new(CinderClientConfig {
        keystone_endpoint,
        cinder_endpoint: format!("http://{cinder_address}"),
        username: "cinder".to_owned(),
        password: Secret::new("password".to_owned()),
        domain_name: "Default".to_owned(),
    });
    Ok((client, fake, format!("http://{cinder_address}")))
}

pub const PROJECT: &str = "eba29e2d-53de-461d-ae91-ede7402713cb";
