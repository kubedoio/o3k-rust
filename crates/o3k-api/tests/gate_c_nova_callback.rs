//! Gate C — Nova callback gate.
//!
//! Proves the operation-scoped Nova microversion 2.89 volume-attachment
//! GET profile that Cinder 28's `attachment_deletion_allowed` (bug #2004555)
//! requires (`novaclient volumes.get_server_volume`):
//!
//! - `GET /v2.1/{project}/servers/{server}/os-volume_attachments` at 2.89
//!   emits the exact upstream field set (`attachment_id`, `bdm_uuid`,
//!   `serverId`, `volumeId`, `device`, `tag`, `delete_on_termination`) and
//!   omits the legacy `id`;
//! - the show variant resolves the volume id (what Cinder calls) as well as the
//!   O3K and Cinder attachment ids;
//! - the 2.1 shape is unchanged (`id` present);
//! - every other 2.89 request (non-GET attachment routes, unrelated routes) is
//!   rejected with 406;
//! - the version discovery document stays `version`/`min_version` = 2.1.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use o3k_api::AppState;
use o3k_compute::ComputeService;
use o3k_provider::FakeComputeProvider;
use o3k_store::{DurableStore, VolumeAttachmentRecord, testkit::TestStore};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

fn build_app(store: Arc<TestStore>) -> axum::Router {
    let provider = Arc::new(FakeComputeProvider::new());
    let compute = ComputeService::new(store, provider);
    let state = AppState::new().with_compute(compute);
    state.set_ready(true);
    o3k_api::router_with_state(state)
}

async fn seed_server_and_attachment(
    store: &TestStore,
    server_id: Uuid,
    volume_id: Uuid,
    cinder_attachment_id: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    // The compute service requires a resolvable server: desired_state must be a
    // serializable CreateInstanceRequest whose flavor_id resolves to a built-in
    // flavor (Uuid::from_u128(1) == test.small).
    let request = o3k_provider::CreateInstanceRequest {
        operation_id: Uuid::now_v7(),
        o3k_server_id: server_id,
        project_id: "project-gate-c".to_owned(),
        name: "gate-c-server".to_owned(),
        vcpus: 1,
        memory_mib: 512,
        flavor_id: Uuid::from_u128(1).to_string(),
        disk_gib: 10,
        image_id: Some(Uuid::now_v7().to_string()),
        key_name: None,
        keypair_id: None,
        network_ids: Vec::new(),
        placement_provider_id: Some("compute-1".to_owned()),
        placement_allocation_id: Some(Uuid::now_v7().to_string()),
        config_drive: None,
        idempotency_key: format!("create:{server_id}"),
    };
    store
        .insert_resource(&o3k_store::ResourceRecord {
            id: server_id,
            kind: "compute_instance".to_owned(),
            project_id: "project-gate-c".to_owned(),
            generation: 1,
            observed_generation: 1,
            desired_state: serde_json::to_string(&request)?,
            observed_state: "ACTIVE".to_owned(),
            provider_id: None,
        })
        .await?;
    let id = Uuid::now_v7();
    store
        .insert_volume_attachment(&VolumeAttachmentRecord {
            id,
            server_id,
            volume_id,
            device: "/dev/vdb".to_owned(),
            tag: Some("gate-c".to_owned()),
            delete_on_termination: false,
            created_at: "2026-08-06T00:00:00Z".to_owned(),
            status: "attached".to_owned(),
            operation_id: Some(Uuid::now_v7()),
            idempotency_key: Some("attach:1".to_owned()),
            cinder_attachment_id: Some(cinder_attachment_id.to_owned()),
            connector_host: Some("compute-1".to_owned()),
            connector_ip: Some("10.0.0.5".to_owned()),
            connector_initiator: Some("iqn.1993-08.org.debian:01:o3k".to_owned()),
            driver_volume_type: Some("iscsi".to_owned()),
            target_iqn: Some("iqn.2010-10.org.openstack:volume-1".to_owned()),
            target_portal: Some("10.0.0.10:3260".to_owned()),
            target_lun: Some(1),
            connection_info_digest: Some("digest".to_owned()),
            error: None,
        })
        .await?;
    Ok(id)
}

fn assert_289_shape(value: &Value, server_id: &str, volume_id: &str) {
    let attachment = &value["volumeAttachment"];
    // The exact upstream 2.89 field set; nothing more, nothing less.
    let keys: Vec<&str> = attachment
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let mut expected: Vec<&str> = vec![
        "attachment_id",
        "bdm_uuid",
        "serverId",
        "volumeId",
        "device",
        "tag",
        "delete_on_termination",
    ];
    expected.sort_unstable();
    let mut actual = keys.clone();
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "2.89 must emit exactly the upstream field set: {attachment}"
    );
    assert_eq!(
        attachment.get("attachment_id").and_then(Value::as_str),
        Some("cinder-att-0001"),
        "2.89 must include attachment_id"
    );
    assert_eq!(
        attachment
            .get("bdm_uuid")
            .and_then(Value::as_str)
            .map(str::len),
        Some(36),
        "bdm_uuid must be a UUID"
    );
    assert_eq!(
        attachment.get("serverId").and_then(Value::as_str),
        Some(server_id),
        "serverId must be present"
    );
    assert_eq!(
        attachment.get("volumeId").and_then(Value::as_str),
        Some(volume_id),
        "volumeId must be present"
    );
    assert_eq!(
        attachment.get("device").and_then(Value::as_str),
        Some("/dev/vdb")
    );
    assert_eq!(
        attachment.get("tag").and_then(Value::as_str),
        Some("gate-c")
    );
    assert_eq!(
        attachment
            .get("delete_on_termination")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert!(
        attachment.get("id").is_none(),
        "2.89 must omit the legacy id field: {attachment}"
    );
    assert!(
        attachment.get("attachmentId").is_none(),
        "2.89 must omit the legacy camel attachmentId field: {attachment}"
    );
}

fn assert_21_shape(value: &Value) {
    let attachment = &value["volumeAttachment"];
    assert!(
        attachment.get("id").is_some(),
        "2.1 must include the legacy id field: {attachment}"
    );
    assert!(
        attachment.get("attachment_id").is_some(),
        "2.1 shape unchanged includes attachment_id"
    );
}

async fn get_with_microversion(
    app: &axum::Router,
    uri: String,
    version: &str,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("OpenStack-API-Version", format!("compute {version}"))
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Ok((status, value))
}

#[tokio::test]
async fn list_at_289_emits_exact_fields_without_legacy_id() -> Result<(), Box<dyn std::error::Error>>
{
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let server_id = Uuid::now_v7();
    let volume_id = Uuid::now_v7();
    seed_server_and_attachment(&store, server_id, volume_id, "cinder-att-0001").await?;
    let app = build_app(store);

    let (status, value) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{value}");
    let list = value["volumeAttachments"]
        .as_array()
        .ok_or("attachments array missing")?;
    assert_eq!(list.len(), 1);
    assert_289_shape(
        &json_wrap(list[0].clone()),
        &server_id.to_string(),
        &volume_id.to_string(),
    );
    Ok(())
}

fn json_wrap(value: Value) -> Value {
    serde_json::json!({"volumeAttachment": value})
}

#[tokio::test]
async fn show_at_289_resolves_volume_id_like_cinder_does() -> Result<(), Box<dyn std::error::Error>>
{
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let server_id = Uuid::now_v7();
    let volume_id = Uuid::now_v7();
    seed_server_and_attachment(&store, server_id, volume_id, "cinder-att-0001").await?;
    let app = build_app(store);

    // Cinder calls get_server_volume(server_id, volume_id) at 2.89.
    let (status, value) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments/{volume_id}"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_289_shape(&value, &server_id.to_string(), &volume_id.to_string());

    // The Cinder attachment id also resolves.
    let (status, value) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments/cinder-att-0001"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_289_shape(&value, &server_id.to_string(), &volume_id.to_string());

    // Unknown id at 2.89 is 404.
    let (status, _) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments/missing"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn list_and_show_at_21_keep_the_legacy_id() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let server_id = Uuid::now_v7();
    let volume_id = Uuid::now_v7();
    seed_server_and_attachment(&store, server_id, volume_id, "cinder-att-0001").await?;
    let app = build_app(store);

    let (status, value) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments"),
        "2.1",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let list = value["volumeAttachments"]
        .as_array()
        .ok_or("attachments array missing")?;
    assert_21_shape(&json_wrap(list[0].clone()));

    let (status, value) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments/{volume_id}"),
        "2.1",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_21_shape(&value);
    Ok(())
}

#[tokio::test]
async fn non_get_289_attachment_requests_are_rejected_with_406()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let server_id = Uuid::now_v7();
    let volume_id = Uuid::now_v7();
    seed_server_and_attachment(&store, server_id, volume_id, "cinder-att-0001").await?;
    let app = build_app(store);

    // POST attach at 2.89 must be 406 (the 2.89 profile is GET-only).
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments"
        ))
        .header("OpenStack-API-Version", "compute 2.89")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);

    // DELETE at 2.89 must be 406.
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(format!(
            "/v2.1/project-gate-c/servers/{server_id}/os-volume_attachments/cinder-att-0001"
        ))
        .header("OpenStack-API-Version", "compute 2.89")
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    Ok(())
}

#[tokio::test]
async fn unrelated_289_requests_are_rejected_with_406() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let app = build_app(store);
    let server_id = Uuid::now_v7();

    let (status, _) = get_with_microversion(
        &app,
        format!("/v2.1/project-gate-c/servers/{server_id}"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    Ok(())
}

#[tokio::test]
async fn discovery_document_stays_at_21() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let app = build_app(store);
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v2.1")
        .body(Body::empty())?;
    let resp = app.clone().oneshot(req).await?;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["version"]["version"], "2.1");
    assert_eq!(value["version"]["min_version"], "2.1");
    assert_ne!(value["version"]["version"], "2.89");
    Ok(())
}

#[tokio::test]
async fn project_isolation_is_preserved_at_289() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    let server_id = Uuid::now_v7();
    let volume_id = Uuid::now_v7();
    seed_server_and_attachment(&store, server_id, volume_id, "cinder-att-0001").await?;
    let app = build_app(store);

    let (status, _) = get_with_microversion(
        &app,
        format!("/v2.1/project-other/servers/{server_id}/os-volume_attachments"),
        "2.89",
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
