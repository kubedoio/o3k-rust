use std::time::Duration;

use axum::{
    Router,
    body::Body,
    response::IntoResponse,
    routing::{get, post},
};
use o3k_cinder::testkit::{self, faults};
use o3k_cinder::{AttachTarget, CinderClient, CinderClientConfig, CinderError, ComputeConnector};
use o3k_identity::Secret;
use serde_json::json;

fn subject_response(
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
/// implementation. Exercises the same public surface used by Cinder's
/// keystone middleware: POST issues, GET validates with details, HEAD checks.
async fn keystone_router(
    service: o3k_identity::TokenService,
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
                move |body: axum::body::Bytes| async move {
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
            }),
        )
        .route(
            "/v3/auth/tokens",
            get({
                let service = service.clone();
                move |headers: axum::http::HeaderMap| async move {
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
            }),
        )
        .with_state(service);
    let service_task_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, service_task_app).await;
    });
    Ok((app, address))
}

async fn setup() -> Result<(CinderClient, testkit::FakeCinderState, String), String> {
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

fn connector() -> ComputeConnector {
    ComputeConnector {
        host: "compute-1".to_owned(),
        ip: "10.0.0.5".to_owned(),
        platform: "x86_64".to_owned(),
        os_type: "linux".to_owned(),
        multipath: false,
        initiator: Some("iqn.1993-08.org.debian:01:o3k-compute-1".to_owned()),
    }
}

#[tokio::test]
async fn attachment_lifecycle_validates_token_through_keystone()
-> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _cinder_endpoint) = setup().await?;
    let project = "bootstrap-project";

    let volume = client.create_volume(project, 1, "vol-1").await?;
    assert_eq!(volume.status, "available");

    let attachment = client.create_attachment(project, &volume.id).await?;
    assert_eq!(attachment.status, "creating");

    let shown = client.show_attachment(project, &attachment.id).await?;
    assert_eq!(shown.id, attachment.id);

    let updated = client
        .update_attachment_connector(project, &attachment.id, &connector())
        .await?;
    let connection_info = updated.connection_info.ok_or("connection_info missing")?;
    assert_eq!(connection_info.driver_volume_type(), Some("iscsi"));
    let target = connection_info
        .attach_target()
        .ok_or("attach target missing")?;
    assert_eq!(
        target.target_iqn.as_deref(),
        Some("iqn.2026-01.example.com:volume-00000001")
    );
    assert_eq!(target.target_portal.as_deref(), Some("10.0.0.10:3260"));
    assert_eq!(target.target_lun, Some(1));
    assert_eq!(target.auth_method, None);

    client.complete_attachment(project, &attachment.id).await?;
    assert_eq!(
        fake.attachment_status(&attachment.id).as_deref(),
        Some("attached")
    );

    client.terminate_attachment(project, &attachment.id).await?;
    assert!(!fake.attachment_ids().contains(&attachment.id));

    client.delete_volume(project, &volume.id).await?;
    assert!(fake.volume_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn connection_info_is_redacted_and_digested() -> Result<(), Box<dyn std::error::Error>> {
    let (client, _fake, _cinder_endpoint) = setup().await?;
    let project = "bootstrap-project";
    let volume = client.create_volume(project, 1, "vol-secret").await?;
    let attachment = client.create_attachment(project, &volume.id).await?;
    let updated = client
        .update_attachment_connector(project, &attachment.id, &connector())
        .await?;
    let connection_info = updated.connection_info.ok_or("connection_info missing")?;
    let debug = format!("{connection_info:?}");
    assert!(!debug.contains("iqn.2026-01.example.com"));
    assert!(debug.contains("sha256="));

    let digest = connection_info.digest();
    assert_eq!(digest.len(), 43);
    assert_eq!(digest, connection_info.digest());

    let target: AttachTarget = connection_info
        .attach_target()
        .ok_or("attach target missing")?;
    let debug = format!("{target:?}");
    assert!(debug.contains("iqn"));
    assert!(debug.contains("<redacted>"));

    client.delete_volume(project, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn invalid_service_credentials_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let identity = o3k_identity::testkit::test_service("http://127.0.0.1:8080").await?;
    let (_app, keystone_address) = keystone_router(identity).await?;
    let (fake, cinder_address) =
        testkit::start_fake_cinder(Some(format!("http://{keystone_address}"))).await?;
    let client = CinderClient::new(CinderClientConfig {
        keystone_endpoint: format!("http://{keystone_address}"),
        cinder_endpoint: format!("http://{cinder_address}"),
        username: "cinder".to_owned(),
        password: Secret::new("wrong-password".to_owned()),
        domain_name: "Default".to_owned(),
    });
    let error = match client.create_volume("bootstrap-project", 1, "v").await {
        Err(error) => error,
        Ok(_) => return Err("expected token acquisition failure".into()),
    };
    assert!(matches!(error, CinderError::Auth(_)));
    let _ = fake;
    Ok(())
}

#[tokio::test]
async fn service_unavailable_and_not_found_mapping() -> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _cinder_endpoint) = setup().await?;
    let project = "bootstrap-project";
    let volume = client.create_volume(project, 1, "v").await?;

    fake.set_fault(faults::fail_create_attachment, true);
    let error = match client.create_attachment(project, &volume.id).await {
        Err(error) => error,
        Ok(_) => return Err("expected availability failure".into()),
    };
    assert!(matches!(error, CinderError::ServiceUnavailable));

    let error = match client.show_attachment(project, "attachment-missing").await {
        Err(error) => error,
        Ok(_) => return Err("expected not-found".into()),
    };
    assert!(matches!(error, CinderError::NotFound(_)));

    fake.set_fault(faults::fail_complete_attachment, true);
    let attachment = client.create_attachment(project, &volume.id).await?;
    client
        .update_attachment_connector(project, &attachment.id, &connector())
        .await?;
    let error = match client.complete_attachment(project, &attachment.id).await {
        Err(error) => error,
        Ok(_) => return Err("expected completion failure".into()),
    };
    assert!(matches!(error, CinderError::UnknownOutcome(_)));

    client.terminate_attachment(project, &attachment.id).await?;
    client.delete_volume(project, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn timeout_is_unknown_outcome() -> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _cinder_endpoint) = setup().await?;
    let client = client.with_timeout(Duration::from_secs(1));
    let project = "bootstrap-project";
    let volume = client.create_volume(project, 1, "v").await?;
    fake.set_fault(faults::timeout_create_attachment, true);
    let error = match client.create_attachment(project, &volume.id).await {
        Err(error) => error,
        Ok(_) => return Err("expected timeout".into()),
    };
    assert!(matches!(error, CinderError::UnknownOutcome(_)));
    client.delete_volume(project, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn volume_lifecycle_is_typed() -> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _cinder_endpoint) = setup().await?;
    let project = "bootstrap-project";
    let volume = client.create_volume(project, 2, "typed-vol").await?;
    let volumes = client.list_volumes(project).await?;
    assert_eq!(volumes.len(), 1);
    let shown = client.show_volume(project, &volume.id).await?;
    assert_eq!(shown.name.as_deref(), Some("typed-vol"));
    assert_eq!(shown.size, 2);
    client.delete_volume(project, &volume.id).await?;
    assert!(fake.volume_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn fake_rejects_unvalidated_token() -> Result<(), Box<dyn std::error::Error>> {
    let identity = o3k_identity::testkit::test_service("http://127.0.0.1:8080").await?;
    let (_app, keystone_address) = keystone_router(identity).await?;
    let (fake, cinder_address) =
        testkit::start_fake_cinder(Some(format!("http://{keystone_address}"))).await?;
    let _ = fake;
    let response = axum::http::Request::builder()
        .method("GET")
        .uri(format!(
            "http://{cinder_address}/v3/bootstrap-project/volumes"
        ))
        .header("x-auth-token", "bogus-token")
        .body(Body::empty())?;
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyper_util::client::legacy::connect::HttpConnector::new());
    let response = client.request(response).await?;
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    Ok(())
}
