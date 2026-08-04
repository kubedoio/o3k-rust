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

fn js(status: StatusCode, value: serde_json::Value) -> Response {
    (status, Json(value)).into_response()
}
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStatus {
    Creating,
    Reserved,
    Attached,
    Deleted,
}

impl AttachmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Reserved => "reserved",
            Self::Attached => "attached",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeAttachment {
    pub id: String,
    pub volume_id: String,
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
}

/// Per-operation fault injection. Each flag is consumed once when set.
#[derive(Debug, Clone, Default)]
pub struct FaultConfig {
    pub fail_create_attachment: bool,
    pub timeout_create_attachment: bool,
    pub fail_update_connector: bool,
    pub timeout_update_connector: bool,
    pub fail_complete_attachment: bool,
    pub fail_terminate_attachment: bool,
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
    next_volume_index: Arc<Mutex<u64>>,
    next_attachment_index: Arc<Mutex<u64>>,
}

impl FakeCinderState {
    pub fn new(keystone_endpoint: Option<String>) -> Self {
        Self {
            volumes: Arc::new(Mutex::new(HashMap::new())),
            attachments: Arc::new(Mutex::new(HashMap::new())),
            faults: Arc::new(Mutex::new(FaultConfig::default())),
            keystone_endpoint,
            next_volume_index: Arc::new(Mutex::new(1)),
            next_attachment_index: Arc::new(Mutex::new(1)),
        }
    }

    fn next_index(&self, counter: &Arc<Mutex<u64>>) -> u64 {
        let mut value = counter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = *value;
        *value = value.saturating_add(1);
        current
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
        fail_complete_attachment,
        fail_terminate_attachment,
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
    match driver_volume_type {
        "iscsi" => json!({
            "driver_volume_type": "iscsi",
            "data": {
                "target_portal": "10.0.0.10:3260",
                "target_iqn": "iqn.2026-01.example.com:volume-00000001",
                "target_lun": 1,
                "access_mode": "rw"
            }
        }),
        "local" => json!({
            "driver_volume_type": "local",
            "data": {
                "device_path": "/dev/mapper/o3k-vg-volume-00000001",
                "access_mode": "rw"
            }
        }),
        other => json!({
            "driver_volume_type": other,
            "data": {"access_mode": "rw"}
        }),
    }
}

/// Builds the fake Cinder v3 router.
pub fn router(state: FakeCinderState) -> Router {
    Router::new()
        .route("/v3/{project_id}/attachments", post(create_attachment))
        .route(
            "/v3/{project_id}/attachments/{attachment_id}",
            get(show_attachment),
        )
        .route(
            "/v3/{project_id}/attachments/{attachment_id}/update",
            post(update_attachment),
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
        return js(
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "volume not found"}}),
        );
    }
    let id = format!(
        "attachment-{:08}",
        state.next_index(&state.next_attachment_index)
    );
    let attachment = FakeAttachment {
        id: id.clone(),
        volume_id: volume_id.clone(),
        status: AttachmentStatus::Creating,
        connector: None,
        connection_info: None,
    };
    state
        .attachments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), attachment);
    js(
        StatusCode::ACCEPTED,
        json!({"attachment": {
            "id": id,
            "status": "creating",
            "volume_id": volume_id,
            "instance": null,
            "connection_info": null
        }}),
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
        json!({"attachment": {
            "id": attachment.id,
            "status": attachment.status.as_str(),
            "volume_id": attachment.volume_id,
            "connection_info": attachment.connection_info
        }}),
    )
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
    attachment.status = AttachmentStatus::Reserved;
    let connection_info = connection_info_for("iscsi");
    attachment.connection_info = Some(connection_info.clone());
    js(
        StatusCode::OK,
        json!({"attachment": {
            "id": attachment_id,
            "status": "reserved",
            "volume_id": attachment.volume_id,
            "connection_info": connection_info
        }}),
    )
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
        return js(
            StatusCode::OK,
            json!({"attachment": {"id": attachment_id, "status": "attached"}}),
        );
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
        let Some(attachment) = attachments.get_mut(&attachment_id) else {
            return js(
                StatusCode::NOT_FOUND,
                json!({"error": {"message": "attachment not found"}}),
            );
        };
        attachment.status = AttachmentStatus::Deleted;
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
    let id = format!("volume-{:08}", state.next_index(&state.next_volume_index));
    let volume = FakeVolume {
        id: id.clone(),
        name: name.clone(),
        size,
        status: "available".to_owned(),
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
    pub fn fail_complete_attachment(faults: &FaultConfig) -> bool {
        faults.fail_complete_attachment
    }
    pub fn fail_terminate_attachment(faults: &FaultConfig) -> bool {
        faults.fail_terminate_attachment
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
