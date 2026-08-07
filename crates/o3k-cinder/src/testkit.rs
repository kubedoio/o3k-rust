//! Stateful fake Cinder v3 server used by contract, orchestration, and
//! compensation tests. It implements the frozen attachment and volume subset
//! with real state transitions and configurable fault injection.
//!
//! The fake is a test double, never a real Cinder deployment. Real-service
//! evidence must come from the protected profile.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;

fn js(status: StatusCode, value: serde_json::Value) -> Response {
    (status, Json(value)).into_response()
}

/// Whether a raw connection_info value carries a usable target (mirrors the
/// client's `ConnectionInfo::has_usable_target`). Used by the delete guard.
fn value_has_usable_target(value: &serde_json::Value) -> bool {
    if !value.is_object() {
        return false;
    }
    let data = value.get("data").unwrap_or(value);
    let driver = data
        .get("driver_volume_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    match driver {
        "iscsi" => data.get("target_iqn").is_some() && data.get("target_portal").is_some(),
        "local" => data.get("device_path").is_some(),
        _ => false,
    }
}
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStatus {
    Reserved,
    Attaching,
    Attached,
    Deleted,
}

impl AttachmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Attaching => "attaching",
            Self::Attached => "attached",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeAttachment {
    pub id: String,
    pub volume_id: String,
    pub instance_uuid: Option<String>,
    pub status: AttachmentStatus,
    pub connector: Option<Value>,
    pub connection_info: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct FakeVolume {
    pub id: String,
    pub name: Option<String>,
    pub size: u64,
    pub status: String,
    pub attach_status: String,
}

/// Per-operation fault injection. Each flag is consumed once when set.
#[derive(Debug, Clone, Default)]
pub struct FaultConfig {
    pub fail_create_attachment: bool,
    pub timeout_create_attachment: bool,
    pub fail_update_connector: bool,
    pub timeout_update_connector: bool,
    pub missing_connection_info_on_update: bool,
    pub null_connection_info_on_update: bool,
    pub malformed_connection_info_on_update: bool,
    pub fail_complete_attachment: bool,
    pub fail_terminate_attachment: bool,
    pub conflict_terminate_attachment: bool,
    pub fail_create_volume: bool,
    pub fail_show_volume: bool,
    pub fail_delete_volume: bool,
    pub reject_unknown_driver: bool,
}

#[derive(Clone)]
pub struct FakeCinderState {
    pub volumes: Arc<Mutex<HashMap<String, FakeVolume>>>,
    pub attachments: Arc<Mutex<HashMap<String, FakeAttachment>>>,
    pub faults: Arc<Mutex<FaultConfig>>,
    /// Optional Keystone endpoint used to validate incoming tokens through the
    /// public Identity API. When absent, any non-empty token is accepted.
    pub keystone_endpoint: Option<String>,
    pub last_openstack_api_version: Arc<Mutex<Option<String>>>,
    /// Records whether the most recent DELETE carried a valid service-role
    /// `X-Service-Token` (the Cinder 28 `attachment_deletion_allowed` guard).
    pub last_delete_service_token_validated: Arc<Mutex<Option<bool>>>,
}

impl FakeCinderState {
    pub fn new(keystone_endpoint: Option<String>) -> Self {
        Self {
            volumes: Arc::new(Mutex::new(HashMap::new())),
            attachments: Arc::new(Mutex::new(HashMap::new())),
            faults: Arc::new(Mutex::new(FaultConfig::default())),
            keystone_endpoint,
            last_openstack_api_version: Arc::new(Mutex::new(None)),
            last_delete_service_token_validated: Arc::new(Mutex::new(None)),
        }
    }

    pub fn last_openstack_api_version(&self) -> Option<String> {
        self.last_openstack_api_version
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn last_delete_service_token_validated(&self) -> Option<bool> {
        *self
            .last_delete_service_token_validated
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn volume_ids(&self) -> Vec<String> {
        self.volumes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|volume| volume.id.clone())
            .collect()
    }

    pub fn attachment_ids(&self) -> Vec<String> {
        self.attachments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|attachment| attachment.id.clone())
            .collect()
    }

    pub fn attachment_status(&self, id: &str) -> Option<String> {
        self.attachments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|attachment| attachment.status.as_str().to_owned())
    }

    fn take_fault(&self, key: fn(&FaultConfig) -> bool) -> bool {
        let mut faults = self
            .faults
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active = key(&faults);
        if active {
            set_fault_key(&mut faults, key, false);
        }
        active
    }
}

fn set_fault_key(faults: &mut FaultConfig, key: fn(&FaultConfig) -> bool, value: bool) {
    macro_rules! set {
        ($($field:ident),*) => {
            $(if key(&FaultConfig { $field: true, ..Default::default() }) { faults.$field = value; })*
        };
    }
    set!(
        fail_create_attachment,
        timeout_create_attachment,
        fail_update_connector,
        timeout_update_connector,
        missing_connection_info_on_update,
        null_connection_info_on_update,
        malformed_connection_info_on_update,
        fail_complete_attachment,
        fail_terminate_attachment,
        conflict_terminate_attachment,
        fail_create_volume,
        fail_show_volume,
        fail_delete_volume,
        reject_unknown_driver
    );
}

impl FakeCinderState {
    pub fn set_fault(&self, key: fn(&FaultConfig) -> bool, value: bool) {
        let mut faults = self
            .faults
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_fault_key(&mut faults, key, value);
    }
}

fn connection_info_for(driver_volume_type: &str) -> Value {
    // Mirrors the real Cinder 28 LVM + tgt (tgtadm) driver shape. The manager
    // (`_connection_create`) flattens the driver's nested `data` to the top
    // level and appends `attachment_id` and `enforce_multipath`; the LVM
    // iSCSI target is always CHAP-authenticated.
    match driver_volume_type {
        "iscsi" => json!({
            "driver_volume_type": "iscsi",
            "target_discovered": false,
            "target_portal": "10.0.0.10:3260",
            "target_iqn": "iqn.2010-10.org.openstack:volume-00000001",
            "target_lun": 1,
            "volume_id": "00000000-0000-0000-0000-000000000000",
            "auth_method": "CHAP",
            "auth_username": "chap-user",
            "auth_password": "chap-password",
            "encrypted": false,
            "qos_specs": null,
            "access_mode": "rw",
            "attachment_id": "attachment-placeholder",
            "enforce_multipath": false
        }),
        "local" => json!({
            "driver_volume_type": "local",
            "device_path": "/dev/mapper/o3k-vg-volume-00000001",
            "access_mode": "rw",
            "attachment_id": "attachment-placeholder",
            "enforce_multipath": false
        }),
        other => json!({
            "driver_volume_type": other,
            "access_mode": "rw",
            "attachment_id": "attachment-placeholder",
            "enforce_multipath": false
        }),
    }
}

/// Builds the detail view of an attachment exactly as Cinder 28 does in
/// `cinder/api/v3/views/attachments.py`. The `status` field carries the
/// `attach_status` value. When the attachment carries no `connection_info`
/// value the key is omitted so tests can distinguish "missing" from an
/// explicit `null`.
fn detail_view(attachment: &FakeAttachment) -> Value {
    let mut result = json!({
        "id": attachment.id,
        "status": attachment.status.as_str(),
        "instance": attachment.instance_uuid,
        "volume_id": attachment.volume_id,
        "attached_at": "",
        "detached_at": "",
        "attach_mode": if attachment.status == AttachmentStatus::Reserved { "null" } else { "rw" },
    });
    if let Some(connection_info) = &attachment.connection_info {
        result["connection_info"] = connection_info.clone();
    }
    result
}

/// Builds the summary view used by list operations.
fn summary_view(attachment: &FakeAttachment) -> Value {
    json!({
        "id": attachment.id,
        "status": attachment.status.as_str(),
        "instance": attachment.instance_uuid,
        "volume_id": attachment.volume_id
    })
}

/// Builds the fake Cinder v3 router.
pub fn router(state: FakeCinderState) -> Router {
    Router::new()
        .route(
            "/v3/{project_id}/attachments",
            post(create_attachment).get(list_attachments),
        )
        .route(
            "/v3/{project_id}/attachments/{attachment_id}",
            get(show_attachment)
                .put(update_attachment)
                .delete(delete_attachment),
        )
        .route(
            "/v3/{project_id}/attachments/{attachment_id}/action",
            post(attachment_action),
        )
        .route(
            "/v3/{project_id}/volumes",
            get(list_volumes).post(create_volume),
        )
        .route(
            "/v3/{project_id}/volumes/{volume_id}",
            get(show_volume).delete(delete_volume),
        )
        .with_state(state)
}

async fn authorize(state: &FakeCinderState, headers: &HeaderMap) -> bool {
    if let Some(ver) = headers
        .get("openstack-api-version")
        .and_then(|value| value.to_str().ok())
    {
        *state
            .last_openstack_api_version
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(ver.to_owned());
    }
    let token = headers
        .get("x-auth-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if token.is_empty() {
        return false;
    }
    let Some(keystone_endpoint) = &state.keystone_endpoint else {
        return true;
    };
    validate_through_keystone(keystone_endpoint, token).await
}

async fn validate_through_keystone(keystone_endpoint: &str, token: &str) -> bool {
    let url = format!("{}/v3/auth/tokens", keystone_endpoint.trim_end_matches('/'));
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyper_util::client::legacy::connect::HttpConnector::new());
    let Ok(request) = hyper::Request::builder()
        .method("GET")
        .uri(url)
        .header("x-subject-token", token)
        .header("x-auth-token", token)
        .body(Body::empty())
    else {
        return false;
    };
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.request(request),
        )
        .await,
        Ok(Ok(response)) if response.status() == StatusCode::OK
    )
}

/// Validates the service token and returns the role names it carries.
async fn keystone_roles(keystone_endpoint: &str, token: &str) -> Vec<String> {
    let url = format!("{}/v3/auth/tokens", keystone_endpoint.trim_end_matches('/'));
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyper_util::client::legacy::connect::HttpConnector::new());
    let Ok(request) = hyper::Request::builder()
        .method("GET")
        .uri(url)
        .header("x-subject-token", token)
        .header("x-auth-token", token)
        .body(Body::empty())
    else {
        return Vec::new();
    };
    let Ok(Ok(response)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), client.request(request)).await
    else {
        return Vec::new();
    };
    if response.status() != StatusCode::OK {
        return Vec::new();
    }
    let Ok(bytes) = response.into_body().collect().await.map(|b| b.to_bytes()) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    value["token"]["roles"]
        .as_array()
        .map(|roles| {
            roles
                .iter()
                .filter_map(|role| role["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Mirrors Cinder 28 `is_service_request`: a request is a service request when
/// an `X-Service-Token` with a role in `service_token_roles` (default
/// `service`) is present and valid. The fake validates the service token
/// through the public Identity API and records the outcome on the shared
/// state.
async fn service_token_validated(state: &FakeCinderState, headers: &HeaderMap) -> bool {
    let service_token = headers
        .get("x-service-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let Some(keystone_endpoint) = &state.keystone_endpoint else {
        let valid = !service_token.is_empty();
        *state
            .last_delete_service_token_validated
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(valid);
        return valid;
    };
    let roles = keystone_roles(keystone_endpoint, service_token).await;
    let valid = !service_token.is_empty() && roles.iter().any(|role| role == "service");
    *state
        .last_delete_service_token_validated
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(valid);
    valid
}

async fn create_attachment(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    if state.take_fault(|faults| faults.fail_create_attachment) {
        return js(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": {"message": "fake: attachment service unavailable"}}),
        );
    }
    if state.take_fault(|faults| faults.timeout_create_attachment) {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    let value: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let volume_id = value["attachment"]["volume_id"]
        .as_str()
        .or_else(|| value["attachment"]["volume_uuid"].as_str())
        .unwrap_or_default()
        .to_owned();
    if volume_id.is_empty() {
        return js(
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "volume_id is required"}}),
        );
    }
    if !state
        .volumes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&volume_id)
    {
        // Real Cinder's controller does objects.Volume.get_by_id, raising
        // VolumeNotFound -> 404 for a missing volume.
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "volume not found"}}),
        );
    }
    let id = format!("attachment-{:08}", uuid::Uuid::now_v7().to_string());
    let instance_uuid = value["attachment"]["instance_uuid"]
        .as_str()
        .map(str::to_owned);
    let attachment = FakeAttachment {
        id: id.clone(),
        volume_id: volume_id.clone(),
        instance_uuid,
        status: AttachmentStatus::Reserved,
        connector: None,
        connection_info: Some(json!({})),
    };
    state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), attachment.clone());
    // Cinder 28 create returns the detail view with HTTP 200 (`@wsgi.response
    // (HTTPStatus.OK)`) and `status` carrying the reserved attach_status.
    js(
        StatusCode::OK,
        json!({"attachment": detail_view(&attachment)}),
    )
}

async fn show_attachment(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, attachment_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    let attachment = state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&attachment_id)
        .cloned();
    let Some(attachment) = attachment else {
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "attachment not found"}}),
        );
    };
    js(
        StatusCode::OK,
        json!({"attachment": detail_view(&attachment)}),
    )
}

async fn list_attachments(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    let attachments: Vec<Value> = state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .map(summary_view)
        .collect();
    js(StatusCode::OK, json!({"attachments": attachments}))
}

async fn update_attachment(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, attachment_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    if state.take_fault(|faults| faults.fail_update_connector) {
        return js(
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "fake: connector update rejected"}}),
        );
    }
    if state.take_fault(|faults| faults.timeout_update_connector) {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    let value: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let connector = value["attachment"]["connector"].clone();
    let mut attachments = state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(attachment) = attachments.get_mut(&attachment_id) else {
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "attachment not found"}}),
        );
    };
    attachment.connector = Some(connector);
    attachment.status = AttachmentStatus::Attaching;
    let connection_info = if state.take_fault(|faults| faults.missing_connection_info_on_update) {
        // The key is absent from the response entirely.
        None
    } else if state.take_fault(|faults| faults.null_connection_info_on_update) {
        // Present but explicitly null.
        Some(Value::Null)
    } else if state.take_fault(|faults| faults.malformed_connection_info_on_update) {
        // Present but not a JSON object.
        Some(Value::String("not-an-object".to_owned()))
    } else {
        let mut connection_info = connection_info_for("iscsi");
        if let Some(map) = connection_info.as_object_mut() {
            map.insert(
                "attachment_id".to_owned(),
                Value::String(attachment_id.clone()),
            );
        }
        Some(connection_info)
    };
    attachment.connection_info = connection_info.clone();
    let response_attachment = attachment.clone();
    drop(attachments);
    // The PUT response serializes the STALE API object: Cinder 28 returns the
    // controller's pre-update attachment_ref, so `status` is still the
    // reserved attach_status and `attach_mode` is still "null" (the mode arg
    // is 3.54+). The connection_info IS populated. A subsequent GET show
    // returns the fresh view (attaching / rw).
    let mut stale = json!({
        "id": response_attachment.id,
        "status": "reserved",
        "instance": response_attachment.instance_uuid,
        "volume_id": response_attachment.volume_id,
        "attached_at": "",
        "detached_at": "",
        "attach_mode": "null",
    });
    if let Some(connection_info) = &response_attachment.connection_info {
        stale["connection_info"] = connection_info.clone();
    }
    js(StatusCode::OK, json!({"attachment": stale}))
}

async fn delete_attachment(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, attachment_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    // Mirror Cinder 28 `attachment_deletion_allowed` (bug #2004555): a DELETE
    // of a LIVE attachment (instance set AND real connection information) is
    // rejected with 409 `ConflictNovaUsingAttachment` unless the request
    // carries a service-role X-Service-Token. A reserved or not-connected
    // attachment may be deleted by a plain user. O3K always sends the service
    // token, so the O3K client path never hits the 409.
    let is_service = service_token_validated(&state, &headers).await;
    if !is_service {
        let live = {
            let attachments = state
                .attachments
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            attachments.get(&attachment_id).is_some_and(|attachment| {
                attachment.instance_uuid.is_some()
                    && attachment
                        .connection_info
                        .as_ref()
                        .is_some_and(value_has_usable_target)
            })
        };
        if live {
            return js(
                StatusCode::CONFLICT,
                json!({"conflictNovaUsingAttachment": {
                    "message": "Detected user call to delete in-use attachment. Call must come from the nova service and nova must be configured to send the service token. Bug #2004555"
                }}),
            );
        }
    }
    if state.take_fault(|faults| faults.conflict_terminate_attachment) {
        return js(
            StatusCode::CONFLICT,
            json!({"conflictNovaUsingAttachment": {
                "message": "fake: terminate rejected with conflict"
            }}),
        );
    }
    if state.take_fault(|faults| faults.fail_terminate_attachment) {
        return js(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": {"message": "fake: terminate failed"}}),
        );
    }
    let mut attachments = state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(attachment) = attachments.remove(&attachment_id) else {
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "attachment not found"}}),
        );
    };
    // Cinder 28 delete returns HTTP 200 with a summary list of any remaining
    // shared attachments.
    let volume_id = attachment.volume_id;
    let remaining: Vec<Value> = attachments
        .values()
        .filter(|attachment| attachment.volume_id == volume_id)
        .map(summary_view)
        .collect();
    drop(attachments);
    js(StatusCode::OK, json!({"attachments": remaining}))
}

async fn attachment_action(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, attachment_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    let value: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let complete = value.get("os-complete").is_some();
    let terminate = value.get("os-terminate").is_some();
    if complete {
        if state.take_fault(|faults| faults.fail_complete_attachment) {
            return js(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": {"message": "fake: complete failed"}}),
            );
        }
        let mut attachments = state
            .attachments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(attachment) = attachments.get_mut(&attachment_id) else {
            return js(
                StatusCode::NOT_FOUND,
                json!({"error": {"message": "attachment not found"}}),
            );
        };
        attachment.status = AttachmentStatus::Attached;
        // Cinder 28 `os-complete` returns 204 No Content with no body.
        return StatusCode::NO_CONTENT.into_response();
    }
    if terminate {
        if state.take_fault(|faults| faults.fail_terminate_attachment) {
            return js(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": {"message": "fake: terminate failed"}}),
            );
        }
        let mut attachments = state
            .attachments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(attachment) = attachments.remove(&attachment_id) else {
            return js(
                StatusCode::NOT_FOUND,
                json!({"error": {"message": "attachment not found"}}),
            );
        };
        let _ = attachment;
        return js(
            StatusCode::OK,
            json!({"attachment": {"id": attachment_id, "status": "deleted"}}),
        );
    }
    js(
        StatusCode::BAD_REQUEST,
        json!({"error": {"message": "unsupported action"}}),
    )
}

async fn list_volumes(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    let volumes: Vec<Value> = state
        .volumes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .map(|volume| {
            json!({"id": volume.id, "status": volume.status, "size": volume.size, "name": volume.name})
        })
        .collect();
    js(StatusCode::OK, json!({"volumes": volumes}))
}

async fn create_volume(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    if state.take_fault(|faults| faults.fail_create_volume) {
        return js(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": {"message": "fake: volume create unavailable"}}),
        );
    }
    let value: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let size = value["volume"]["size"].as_u64().unwrap_or(0);
    let name = value["volume"]["name"].as_str().map(str::to_owned);
    let id = uuid::Uuid::now_v7().to_string();
    let volume = FakeVolume {
        id: id.clone(),
        name: name.clone(),
        size,
        status: "available".to_owned(),
        attach_status: "detached".to_owned(),
    };
    state
        .volumes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), volume);
    js(
        StatusCode::ACCEPTED,
        json!({"volume": {"id": id, "status": "available", "size": size, "name": name}}),
    )
}

async fn show_volume(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, volume_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    if state.take_fault(|faults| faults.fail_show_volume) {
        return js(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": {"message": "fake: volume show unavailable"}}),
        );
    }
    let volume = state
        .volumes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&volume_id)
        .cloned();
    let Some(volume) = volume else {
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "volume not found"}}),
        );
    };
    js(
        StatusCode::OK,
        json!({"volume": {"id": volume.id, "status": volume.status, "size": volume.size, "name": volume.name}}),
    )
}

async fn delete_volume(
    State(state): State<FakeCinderState>,
    headers: HeaderMap,
    Path((_project_id, volume_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if !authorize(&state, &headers).await {
        return js(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}));
    }
    if state.take_fault(|faults| faults.fail_delete_volume) {
        return js(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": {"message": "fake: volume delete failed"}}),
        );
    }
    let mut volumes = state
        .volumes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if volumes.remove(&volume_id).is_none() {
        return js(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "volume not found"}}),
        );
    }
    js(StatusCode::ACCEPTED, json!({}))
}

/// Starts a fake Cinder server on an ephemeral port and returns the state and
/// bound address. The returned state stays connected to the running server.
pub async fn start_fake_cinder(
    keystone_endpoint: Option<String>,
) -> Result<(FakeCinderState, SocketAddr), String> {
    let state = FakeCinderState::new(keystone_endpoint);
    let app = router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind fake cinder port: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local addr: {error}"))?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((state, address))
}

pub mod faults {
    use super::FaultConfig;
    pub fn fail_create_attachment(faults: &FaultConfig) -> bool {
        faults.fail_create_attachment
    }
    pub fn timeout_create_attachment(faults: &FaultConfig) -> bool {
        faults.timeout_create_attachment
    }
    pub fn fail_update_connector(faults: &FaultConfig) -> bool {
        faults.fail_update_connector
    }
    pub fn timeout_update_connector(faults: &FaultConfig) -> bool {
        faults.timeout_update_connector
    }
    pub fn missing_connection_info_on_update(faults: &FaultConfig) -> bool {
        faults.missing_connection_info_on_update
    }
    pub fn null_connection_info_on_update(faults: &FaultConfig) -> bool {
        faults.null_connection_info_on_update
    }
    pub fn malformed_connection_info_on_update(faults: &FaultConfig) -> bool {
        faults.malformed_connection_info_on_update
    }
    pub fn fail_complete_attachment(faults: &FaultConfig) -> bool {
        faults.fail_complete_attachment
    }
    pub fn fail_terminate_attachment(faults: &FaultConfig) -> bool {
        faults.fail_terminate_attachment
    }
    pub fn conflict_terminate_attachment(faults: &FaultConfig) -> bool {
        faults.conflict_terminate_attachment
    }
    pub fn fail_create_volume(faults: &FaultConfig) -> bool {
        faults.fail_create_volume
    }
    pub fn fail_show_volume(faults: &FaultConfig) -> bool {
        faults.fail_show_volume
    }
    pub fn fail_delete_volume(faults: &FaultConfig) -> bool {
        faults.fail_delete_volume
    }
    pub fn reject_unknown_driver(faults: &FaultConfig) -> bool {
        faults.reject_unknown_driver
    }
}

/// Starts a keystone-compatible token endpoint backed by the real O3K
/// identity implementation together with a fake Cinder server that validates
/// caller tokens through that endpoint. Returns the client and fake state.
pub async fn start_testbed()
-> Result<(crate::CinderClient, FakeCinderState, std::net::SocketAddr), String> {
    let identity = o3k_identity::testkit::test_service("http://127.0.0.1:8080")
        .await
        .map_err(|_| "identity service".to_owned())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind keystone: {error}"))?;
    let keystone = listener
        .local_addr()
        .map_err(|error| format!("local addr: {error}"))?;

    fn subject_response(
        status: axum::http::StatusCode,
        subject: Option<String>,
        value: serde_json::Value,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let mut response = (status, axum::Json(value)).into_response();
        if let Some(subject) = subject
            && let Ok(header) = axum::http::HeaderValue::from_str(&subject)
        {
            response.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-subject-token"),
                header,
            );
        }
        response
    }

    let app = axum::Router::new()
        .route(
            "/v3/auth/tokens",
            axum::routing::post({
                let identity = identity.clone();
                move |body: axum::body::Bytes| {
                    let identity = identity.clone();
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
                        match identity.issue(&request, std::time::SystemTime::now()) {
                            Ok((token, response)) => subject_response(
                                axum::http::StatusCode::CREATED,
                                Some(token),
                                serde_json::to_value(response).unwrap_or_default(),
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
            axum::routing::get({
                let identity = identity.clone();
                move |headers: axum::http::HeaderMap| {
                    let identity = identity.clone();
                    async move {
                        let token = headers
                            .get("x-subject-token")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default();
                        match identity.verify_details(token, std::time::SystemTime::now()) {
                            Ok(response) => subject_response(
                                axum::http::StatusCode::OK,
                                Some(token.to_owned()),
                                serde_json::to_value(response).unwrap_or_default(),
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
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let keystone_endpoint = format!("http://{keystone}");
    let (fake, cinder_addr) = start_fake_cinder(Some(keystone_endpoint.clone())).await?;
    let client = crate::CinderClient::new(crate::CinderClientConfig {
        keystone_endpoint,
        cinder_endpoint: format!("http://{cinder_addr}"),
        username: "cinder".to_owned(),
        password: o3k_identity::Secret::new("password".to_owned()),
        domain_name: "Default".to_owned(),
    });
    Ok((client, fake, cinder_addr))
}
