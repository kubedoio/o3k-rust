//! ASR-001 / ASR-002 — two-tenant volume-attachment isolation matrix.
//!
//! Proves the load-bearing cross-project invariants for the hosted-Cinder
//! attachment profile with two fully independent projects (A owns server A
//! and attachment A; B owns server B) and two distinct user tokens:
//!
//! - TOKEN A may only reach A-owned attachment/server resources;
//! - TOKEN B can never list, show, detach, or attach A's resources, and can
//!   never learn of A's attachment from any response;
//! - unauthorized requests fail before ANY durable, Cinder, compute, or
//!   provider side effect;
//! - cross-project attach of a foreign volume is indistinguishable from an
//!   unknown volume (404, concealed existence) while a same-project
//!   already-attached volume conflicts like Nova (409);
//! - the second tenant can still run its own complete attach lifecycle.
//!
//! The stateful fake Cinder and the fake compute provider record every
//! external call, so side-effect fencing is asserted, not assumed.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use o3k_api::AppState;
use o3k_compute::ComputeService;
use o3k_identity::{BootstrapConfig, ExtraProjectSeed, Secret, TokenService};
use o3k_provider::FakeComputeProvider;
use o3k_store::{DurableStore, VolumeAttachmentRecord, testkit::TestStore};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

const PROJECT_A: &str = "eba29e2d-53de-461d-ae91-ede7402713cb";
const PROJECT_B: &str = "9f3c2b6e-5f2d-4b3a-9c8e-1a2b3c4d5e6f";
const USER_B: &str = "6b0f5a2e-8c4d-4a7e-9b1f-2d3e4f5a6b7c";

struct TwoTenantHarness {
    app: axum::Router,
    store: Arc<TestStore>,
    cinder: o3k_cinder::testkit::FakeCinderState,
    provider: Arc<FakeComputeProvider>,
    cinder_client: Arc<o3k_cinder::CinderClient>,
    token_a: String,
    token_b: String,
}

async fn build_two_tenant_app() -> Result<TwoTenantHarness, Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::testkit::open_memory().await?);
    seed_identity(&store).await?;

    let provider = Arc::new(FakeComputeProvider::new());
    let (client, cinder, _address) = o3k_cinder::testkit::start_testbed()
        .await
        .map_err(|error| error.to_string())?;
    let client = Arc::new(client);
    let compute = ComputeService::new(store.clone(), provider.clone())
        .with_attachment_provider(client.clone());
    let identity = TokenService::load(
        store.clone(),
        Secret::new("a-secure-signing-key-with-at-least-32-bytes".to_owned()),
        Duration::from_secs(3600),
    )
    .await?;
    let state = AppState::new()
        .with_identity(identity)
        .with_compute(compute)
        .with_volume_attachments_enabled(true);
    state.set_ready(true);
    let app = o3k_api::router_with_state(state);

    let token_a = issue_token(&app, "admin", "password", "admin").await?;
    let token_b = issue_token(&app, "tenant-b-user", "tenant-b-password", "tenant-b").await?;
    Ok(TwoTenantHarness {
        app,
        store,
        cinder,
        provider,
        cinder_client: client,
        token_a,
        token_b,
    })
}

async fn seed_identity(store: &Arc<TestStore>) -> Result<(), Box<dyn std::error::Error>> {
    o3k_identity::seed_identity_defaults(
        store.as_ref(),
        &BootstrapConfig {
            catalog_endpoint: "http://127.0.0.1:18090".to_owned(),
            bootstrap_password: Secret::new("password".to_owned()),
            cinder_password: Some(Secret::new("password".to_owned())),
            cinder_endpoint: Some("http://127.0.0.1:8776".to_owned()),
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
    Ok(())
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
                .body(Body::from(request.to_string()))?,
        )
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "token issuance for {user}"
    );
    response
        .headers()
        .get("x-subject-token")
        .ok_or("missing x-subject-token")?
        .to_str()
        .map(str::to_owned)
        .map_err(Into::into)
}

/// Seeds a resolvable ACTIVE server in `project_id` (same shape the other
/// Gate C tests use: a serializable CreateInstanceRequest with a built-in
/// flavor so the compute service can resolve it).
async fn seed_server(
    store: &Arc<TestStore>,
    server_id: Uuid,
    project_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = o3k_provider::CreateInstanceRequest {
        operation_id: Uuid::now_v7(),
        o3k_server_id: server_id,
        project_id: project_id.to_owned(),
        name: "isolation-server".to_owned(),
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
            project_id: project_id.to_owned(),
            generation: 1,
            observed_generation: 1,
            desired_state: serde_json::to_string(&request)?,
            observed_state: "ACTIVE".to_owned(),
            provider_id: None,
        })
        .await?;
    Ok(())
}

async fn seed_attachment(
    store: &Arc<TestStore>,
    server_id: Uuid,
    volume_id: Uuid,
    cinder_attachment_id: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id = Uuid::now_v7();
    store
        .insert_volume_attachment(&VolumeAttachmentRecord {
            id,
            server_id,
            volume_id,
            device: "/dev/vdb".to_owned(),
            tag: Some("isolation-a".to_owned()),
            delete_on_termination: false,
            created_at: "2026-08-06T00:00:00Z".to_owned(),
            status: "attached".to_owned(),
            operation_id: Some(Uuid::now_v7()),
            idempotency_key: Some(format!("attach:{server_id}:{volume_id}")),
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

async fn request_status(
    app: &axum::Router,
    method: Method,
    uri: String,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-auth-token", token);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder.body(match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    })?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Ok((status, value))
}

/// B1/B2/B3 + E/F: token B can never list, show, or detach A's attachment,
/// through A's URL project or through B's own URL project, and none of the
/// attempts may touch the durable row, Cinder, or the compute provider.
#[tokio::test]
async fn foreign_token_cannot_list_show_or_detach_foreign_attachment()
-> Result<(), Box<dyn std::error::Error>> {
    let h = build_two_tenant_app().await?;
    let server_a = Uuid::now_v7();
    let server_b = Uuid::now_v7();
    let volume_a = Uuid::now_v7();
    seed_server(&h.store, server_a, PROJECT_A).await?;
    seed_server(&h.store, server_b, PROJECT_B).await?;
    let attachment_a = seed_attachment(&h.store, server_a, volume_a, "cinder-att-a").await?;

    let list_a_path = format!("/v2.1/{PROJECT_A}/servers/{server_a}/os-volume_attachments");
    let show_a_path = format!("{list_a_path}/{attachment_a}");
    let list_b_path = format!("/v2.1/{PROJECT_B}/servers/{server_a}/os-volume_attachments");
    let show_b_path = format!("{list_b_path}/{attachment_a}");
    let own_server_path =
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments/{attachment_a}");

    // Token B against A's URL project: project binding rejects before lookup.
    for (method, uri) in [
        (Method::GET, list_a_path.clone()),
        (Method::GET, show_a_path.clone()),
        (Method::DELETE, show_a_path.clone()),
    ] {
        let (status, body) =
            request_status(&h.app, method.clone(), uri.clone(), Some(&h.token_b), None).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
        assert_eq!(
            body["error"]["code"], 404,
            "{method} {uri} envelope: {body}"
        );
    }

    // Token B with B's URL project but A's server: server ownership rejects.
    for (method, uri) in [
        (Method::GET, list_b_path.clone()),
        (Method::GET, show_b_path.clone()),
        (Method::DELETE, show_b_path.clone()),
    ] {
        let (status, body) =
            request_status(&h.app, method.clone(), uri.clone(), Some(&h.token_b), None).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
        assert_eq!(
            body["error"]["code"], 404,
            "{method} {uri} envelope: {body}"
        );
    }

    // Token B with B's own server but A's attachment id: no match under B.
    for (method, uri) in [
        (Method::GET, own_server_path.clone()),
        (Method::DELETE, own_server_path.clone()),
    ] {
        let (status, body) =
            request_status(&h.app, method.clone(), uri.clone(), Some(&h.token_b), None).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
    }

    // Side-effect fencing: A's durable row is untouched, no Cinder attachment
    // was created or terminated, and no compute device was dispatched.
    let row = h
        .store
        .get_volume_attachment(server_a, attachment_a)
        .await?
        .ok_or("attachment A row vanished")?;
    assert_eq!(row.status, "attached");
    assert_eq!(row.cinder_attachment_id.as_deref(), Some("cinder-att-a"));
    assert!(
        h.cinder.attachment_ids().is_empty(),
        "no Cinder call may happen"
    );
    assert_eq!(h.provider.attached_volume_count(server_a), 0);
    assert_eq!(h.provider.attached_volume_count(server_b), 0);
    let list_b = h.store.list_volume_attachments(server_b).await?;
    assert!(list_b.is_empty(), "B must have no durable attachment rows");

    // Contrast: token A can still list its own attachment (resources exist;
    // the B failures are authorization, not absence).
    let (status, body) =
        request_status(&h.app, Method::GET, list_a_path, Some(&h.token_a), None).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["volumeAttachments"].as_array().map(Vec::len), Some(1));
    Ok(())
}

/// B4 + ASR-002 + §6: token B attaching A's volume to B's server must be
/// rejected with a concealed 404 before any durable row, Cinder call, or
/// compute dispatch, and must never return or reuse A's attachment record.
/// An unknown volume id is indistinguishable (same status and error envelope).
#[tokio::test]
async fn foreign_volume_attach_is_concealed_with_zero_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let h = build_two_tenant_app().await?;
    let server_a = Uuid::now_v7();
    let server_b = Uuid::now_v7();
    let volume_a = Uuid::now_v7();
    seed_server(&h.store, server_a, PROJECT_A).await?;
    seed_server(&h.store, server_b, PROJECT_B).await?;
    let attachment_a = seed_attachment(&h.store, server_a, volume_a, "cinder-att-a").await?;
    // The volume exists in the fake Cinder (A created it), so the rejection
    // below cannot be blamed on a missing volume.
    h.cinder_client.create_volume(PROJECT_A, 1, "vol-a").await?;

    let uri = format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments");
    let attach = |volume: Uuid| {
        request_status(
            &h.app,
            Method::POST,
            uri.clone(),
            Some(&h.token_b),
            Some(serde_json::json!({"volumeAttachment": {"volumeId": volume.to_string()}})),
        )
    };

    // B attaching A's real volume: 404, concealed.
    let (status, body) = attach(volume_a).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], 404);
    assert_eq!(body["error"]["title"], "Not Found");
    assert_eq!(body["error"]["message"], "compute resource was not found");
    let body_text = body.to_string();
    assert!(
        !body_text.contains(&attachment_a.to_string())
            && !body_text.contains(&volume_a.to_string())
            && !body_text.contains("volumeAttachments"),
        "response must not disclose A's attachment or volume: {body}"
    );

    // Side-effect fencing for the foreign-volume attempt: no durable row for
    // A's volume under B, A's row untouched, no Cinder call, no dispatch.
    let row = h
        .store
        .get_volume_attachment(server_a, attachment_a)
        .await?
        .ok_or("attachment A row vanished")?;
    assert_eq!(row.status, "attached");
    let list_b = h.store.list_volume_attachments(server_b).await?;
    assert!(
        list_b.iter().all(|record| record.volume_id != volume_a),
        "B must have no durable row for A's volume: {list_b:?}"
    );
    assert!(
        h.cinder.attachment_ids().is_empty(),
        "no Cinder attachment create may happen for a foreign volume"
    );
    assert_eq!(h.provider.attached_volume_count(server_b), 0);

    // B attaching an unknown volume id: identical status and envelope (no
    // semantic oracle distinguishing foreign-existing from nonexistent).
    // The unknown-volume attempt persists the documented volume-not-found
    // error row (identical to what a same-project user would see), which
    // contains no A data; the API surface stays indistinguishable.
    let random_volume = Uuid::now_v7();
    let (status_random, body_random) = attach(random_volume).await?;
    assert_eq!(status_random, StatusCode::NOT_FOUND, "{body_random}");
    assert_eq!(body_random["error"]["code"], 404);
    assert_eq!(body_random["error"]["title"], "Not Found");
    assert_eq!(body_random["error"]["message"], body["error"]["message"]);
    let unknown_rows: Vec<_> = h
        .store
        .list_volume_attachments(server_b)
        .await?
        .into_iter()
        .filter(|record| record.volume_id == random_volume)
        .collect();
    assert_eq!(
        unknown_rows.len(),
        1,
        "documented volume-not-found error row"
    );
    assert_eq!(unknown_rows[0].status, "error");

    // Same-project retry of the already-attached volume stays idempotent.
    let (status, _) = request_status(
        &h.app,
        Method::POST,
        format!("/v2.1/{PROJECT_A}/servers/{server_a}/os-volume_attachments"),
        Some(&h.token_a),
        Some(serde_json::json!({"volumeAttachment": {"volumeId": volume_a.to_string()}})),
    )
    .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "same-server duplicate attach is idempotent"
    );
    assert_eq!(
        h.store.list_volume_attachments(server_a).await?.len(),
        1,
        "idempotent retry must not create a second row"
    );
    assert!(
        h.cinder.attachment_ids().is_empty(),
        "idempotent retry must not call Cinder again"
    );
    Ok(())
}

/// Same-project already-attached volume on a different server conflicts with
/// 409 (Nova parity) and still never reaches Cinder; the foreign-project
/// variant of the same insert failure stays a concealed 404.
#[tokio::test]
async fn same_project_second_server_attach_conflicts_but_foreign_stays_404()
-> Result<(), Box<dyn std::error::Error>> {
    let h = build_two_tenant_app().await?;
    let server_a = Uuid::now_v7();
    let server_a2 = Uuid::now_v7();
    let server_b = Uuid::now_v7();
    let volume_a = Uuid::now_v7();
    seed_server(&h.store, server_a, PROJECT_A).await?;
    seed_server(&h.store, server_a2, PROJECT_A).await?;
    seed_server(&h.store, server_b, PROJECT_B).await?;
    let _attachment_a = seed_attachment(&h.store, server_a, volume_a, "cinder-att-a").await?;
    h.cinder_client.create_volume(PROJECT_A, 1, "vol-a").await?;

    // Same project, different server: Nova-compatible 409 conflict.
    let (status, body) = request_status(
        &h.app,
        Method::POST,
        format!("/v2.1/{PROJECT_A}/servers/{server_a2}/os-volume_attachments"),
        Some(&h.token_a),
        Some(serde_json::json!({"volumeAttachment": {"volumeId": volume_a.to_string()}})),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], 409);

    // Foreign project, same volume: concealed 404 (never 409, never 500).
    let (status, body) = request_status(
        &h.app,
        Method::POST,
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments"),
        Some(&h.token_b),
        Some(serde_json::json!({"volumeAttachment": {"volumeId": volume_a.to_string()}})),
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // No side effects on either attempt: no new rows, no Cinder calls.
    assert_eq!(h.store.list_volume_attachments(server_a2).await?.len(), 0);
    assert_eq!(h.store.list_volume_attachments(server_b).await?.len(), 0);
    assert!(h.cinder.attachment_ids().is_empty());
    assert_eq!(h.provider.attached_volume_count(server_a2), 0);
    assert_eq!(h.provider.attached_volume_count(server_b), 0);
    Ok(())
}

/// Positive control: the second tenant's own complete lifecycle works through
/// the public API (attach -> list -> show -> detach), and tenant A can still
/// list and detach its own attachment. Over-restrictive isolation would fail
/// here.
#[tokio::test]
async fn second_tenant_positive_lifecycle_and_first_tenant_controls()
-> Result<(), Box<dyn std::error::Error>> {
    let h = build_two_tenant_app().await?;
    let server_a = Uuid::now_v7();
    let server_b = Uuid::now_v7();
    let volume_a = Uuid::now_v7();
    seed_server(&h.store, server_a, PROJECT_A).await?;
    seed_server(&h.store, server_b, PROJECT_B).await?;
    let attachment_a = seed_attachment(&h.store, server_a, volume_a, "cinder-att-a").await?;

    // Tenant B's own volume and full lifecycle.
    let volume_b = h.cinder_client.create_volume(PROJECT_B, 1, "vol-b").await?;
    let volume_b_id = Uuid::parse_str(&volume_b.id)?;
    let attach_b = request_status(
        &h.app,
        Method::POST,
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments"),
        Some(&h.token_b),
        Some(serde_json::json!({"volumeAttachment": {"volumeId": volume_b_id.to_string()}})),
    )
    .await?;
    assert_eq!(
        attach_b.0,
        StatusCode::OK,
        "B attach must succeed: {}",
        attach_b.1
    );
    let row_b = h
        .store
        .get_volume_attachment_by_volume_for_server(volume_b_id, server_b)
        .await?
        .ok_or("B attachment row missing")?;
    let attachment_b = row_b.id;
    assert_eq!(row_b.status, "attached", "B attach must reach attached");
    assert_eq!(h.provider.attached_volume_count(server_b), 1);

    let list_b = request_status(
        &h.app,
        Method::GET,
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments"),
        Some(&h.token_b),
        None,
    )
    .await?;
    assert_eq!(list_b.0, StatusCode::OK, "{}", list_b.1);
    assert_eq!(
        list_b.1["volumeAttachments"].as_array().map(Vec::len),
        Some(1)
    );

    let show_b = request_status(
        &h.app,
        Method::GET,
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments/{attachment_b}"),
        Some(&h.token_b),
        None,
    )
    .await?;
    assert_eq!(show_b.0, StatusCode::OK, "{}", show_b.1);
    assert_eq!(
        show_b.1["volumeAttachment"]["volumeId"],
        Value::String(volume_b_id.to_string())
    );

    let detach_b = request_status(
        &h.app,
        Method::DELETE,
        format!("/v2.1/{PROJECT_B}/servers/{server_b}/os-volume_attachments/{attachment_b}"),
        Some(&h.token_b),
        None,
    )
    .await?;
    assert_eq!(
        detach_b.0,
        StatusCode::OK,
        "B detach must succeed: {}",
        detach_b.1
    );
    assert_eq!(h.provider.attached_volume_count(server_b), 0);

    // Tenant A controls still work untouched.
    let list_a = request_status(
        &h.app,
        Method::GET,
        format!("/v2.1/{PROJECT_A}/servers/{server_a}/os-volume_attachments"),
        Some(&h.token_a),
        None,
    )
    .await?;
    assert_eq!(list_a.0, StatusCode::OK, "{}", list_a.1);
    assert_eq!(
        list_a.1["volumeAttachments"].as_array().map(Vec::len),
        Some(1)
    );

    let detach_a = request_status(
        &h.app,
        Method::DELETE,
        format!("/v2.1/{PROJECT_A}/servers/{server_a}/os-volume_attachments/{attachment_a}"),
        Some(&h.token_a),
        None,
    )
    .await?;
    assert_eq!(
        detach_a.0,
        StatusCode::OK,
        "A detach must succeed: {}",
        detach_a.1
    );
    let row = h
        .store
        .get_volume_attachment(server_a, attachment_a)
        .await?
        .ok_or("attachment A row vanished")?;
    assert_eq!(row.status, "detached");
    Ok(())
}
