//! Bounded Cinder v3 projection over the canonical native Volume store.
//!
//! This module intentionally contains only the provider-discovered CRUD
//! surface.  The Cinder representation is a projection; native storage rows
//! remain the authority.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use o3k_domain::{StorageExecutionScope, Volume, VolumeId, VolumeState};
use o3k_storage::StorageVolumeRequest;
use o3k_store::VolumeRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppState, compute::project_auth_context, error::keystone_error};

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeRequest {
    volume: VolumeCreate,
}

#[derive(Debug, Deserialize)]
struct VolumeCreate {
    size: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    volume_type: Option<String>,
    #[serde(default)]
    availability_zone: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeUpdateRequest {
    volume: VolumeUpdate,
}

#[derive(Debug, Deserialize)]
struct VolumeUpdate {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct VolumeResponse {
    volume: VolumeView,
}

#[derive(Debug, Serialize)]
struct VolumeListResponse {
    volumes: Vec<VolumeView>,
}

#[derive(Debug, Serialize)]
struct VolumeView {
    id: String,
    status: String,
    size: u64,
    volume_type: String,
    name: String,
    description: String,
    availability_zone: Option<String>,
    metadata: serde_json::Value,
    attachments: Vec<serde_json::Value>,
    created_at: String,
    updated_at: String,
}

fn status(state: VolumeState) -> &'static str {
    match state {
        VolumeState::Requested | VolumeState::Creating => "creating",
        VolumeState::Available => "available",
        VolumeState::Attaching | VolumeState::InUse => "in-use",
        VolumeState::Detaching => "detaching",
        VolumeState::Deleting => "deleting",
        VolumeState::Deleted => "deleted",
        VolumeState::Unknown => "error",
        VolumeState::Error => "error",
    }
}

fn view(record: &VolumeRecord) -> VolumeView {
    let metadata = record
        .volume
        .metadata
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    VolumeView {
        id: record.volume.id.to_string(),
        status: status(record.volume.state).to_owned(),
        size: record.volume.size_bytes / (1024 * 1024 * 1024),
        volume_type: record.volume.volume_type.clone(),
        name: if record.volume.name.is_empty() {
            record.volume.id.to_string()
        } else {
            record.volume.name.clone()
        },
        description: record.volume.description.clone(),
        availability_zone: record.volume.availability_zone.clone(),
        metadata: serde_json::Value::Object(metadata),
        attachments: Vec::new(),
        created_at: record.created_at.clone(),
        updated_at: record.created_at.clone(),
    }
}

fn unavailable() -> Response {
    keystone_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable",
        "native volume service unavailable",
    )
    .into_response()
}

fn now_rfc3339() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}

fn volume_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id).ok()
}

fn volume_not_found() -> Response {
    keystone_error(StatusCode::NOT_FOUND, "Not Found", "volume not found").into_response()
}

async fn scoped_store(
    state: &AppState,
    headers: &HeaderMap,
    project_id: &str,
) -> Result<
    (
        o3k_kernel::AuthContext,
        std::sync::Arc<dyn o3k_store::StorageRepository>,
    ),
    Response,
> {
    let auth = project_auth_context(state, headers, project_id)?;
    state
        .storage_store
        .clone()
        .map(|store| (auth, store))
        .ok_or_else(unavailable)
}

pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> Response {
    let Ok((_, store)) = scoped_store(&state, &headers, &project_id).await else {
        return unavailable();
    };
    match store.list_volumes(&project_id).await {
        Ok(records) => Json(VolumeListResponse {
            volumes: records.iter().map(view).collect(),
        })
        .into_response(),
        Err(_) => unavailable(),
    }
}

pub(crate) async fn show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, id)): Path<(String, String)>,
) -> Response {
    let Ok((_, store)) = scoped_store(&state, &headers, &project_id).await else {
        return unavailable();
    };
    let Some(id) = volume_id(&id) else {
        return volume_not_found();
    };
    match store.get_volume(id).await {
        Ok(Some(record)) if record.volume.project_id == project_id => (
            StatusCode::OK,
            Json(VolumeResponse {
                volume: view(&record),
            }),
        )
            .into_response(),
        Ok(_) => {
            keystone_error(StatusCode::NOT_FOUND, "Not Found", "volume not found").into_response()
        }
        Err(_) => unavailable(),
    }
}

pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<VolumeRequest>,
) -> Response {
    let Ok((_, store)) = scoped_store(&state, &headers, &project_id).await else {
        return unavailable();
    };
    if request.volume.size == 0 {
        return keystone_error(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "size must be positive",
        )
        .into_response();
    }
    let size_bytes = match request.volume.size.checked_mul(1024 * 1024 * 1024) {
        Some(size_bytes) if size_bytes > 0 => size_bytes,
        _ => {
            return keystone_error(StatusCode::BAD_REQUEST, "Bad Request", "size is too large")
                .into_response();
        }
    };
    let id = VolumeId::from_uuid(Uuid::new_v4());
    let volume = Volume {
        id,
        project_id: project_id.clone(),
        name: request.volume.name.unwrap_or_else(|| id.to_string()),
        description: request.volume.description.unwrap_or_default(),
        metadata: match request.volume.metadata {
            Some(value) => match serde_json::from_value(value) {
                Ok(metadata) => metadata,
                Err(_) => {
                    return keystone_error(
                        StatusCode::BAD_REQUEST,
                        "Bad Request",
                        "metadata must be an object of strings",
                    )
                    .into_response();
                }
            },
            None => Default::default(),
        },
        availability_zone: request.volume.availability_zone,
        size_bytes,
        volume_type: request
            .volume
            .volume_type
            .unwrap_or_else(|| "lvmdriver-1".to_owned()),
        backend_id: "local".to_owned(),
        execution_scope: StorageExecutionScope::Host("local".to_owned()),
        state: VolumeState::Requested,
        generation: 1,
        operation_id: None,
        provider_reference: None,
    };
    let record = VolumeRecord {
        volume,
        created_at: now_rfc3339(),
    };
    match store.insert_volume(&record).await {
        Ok(()) => {
            let Some(provider) = state.storage_provider.as_ref() else {
                return unavailable();
            };
            let mut creating = record.clone();
            creating.volume.state = VolumeState::Creating;
            creating.volume.generation = record.volume.generation + 1;
            let creating = match store
                .update_volume(record.volume.generation, &creating)
                .await
            {
                Ok(creating) => creating,
                Err(_) => return unavailable(),
            };
            let request = StorageVolumeRequest {
                volume_id: creating.volume.id,
                project_id: creating.volume.project_id.clone(),
                size_bytes: creating.volume.size_bytes,
                generation: creating.volume.generation,
            };
            let record = {
                match provider.create_volume(&request).await {
                    Ok(observation) => {
                        let mut realized = creating.clone();
                        realized.volume.state = VolumeState::Available;
                        realized.volume.generation = creating.volume.generation + 1;
                        realized.volume.provider_reference =
                            Some(o3k_domain::StorageProviderReference {
                                provider: observation.provider_reference.provider,
                                resource_id: observation.provider_reference.resource_id,
                            });
                        match store
                            .update_volume(creating.volume.generation, &realized)
                            .await
                        {
                            Ok(updated) => updated,
                            Err(_) => {
                                return keystone_error(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "Service Unavailable",
                                    "native volume realization state could not be committed",
                                )
                                .into_response();
                            }
                        }
                    }
                    Err(error) => {
                        let mut failed = creating.clone();
                        failed.volume.state = if error.is_unknown_outcome() {
                            VolumeState::Unknown
                        } else {
                            VolumeState::Error
                        };
                        failed.volume.generation = failed.volume.generation.saturating_add(1);
                        let _ = store
                            .update_volume(creating.volume.generation, &failed)
                            .await;
                        return keystone_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Service Unavailable",
                            "native storage provider did not create the volume",
                        )
                        .into_response();
                    }
                }
            };
            (
                StatusCode::ACCEPTED,
                Json(VolumeResponse {
                    volume: view(&record),
                }),
            )
                .into_response()
        }
        Err(o3k_store::StoreError::ResourceAlreadyExists) => {
            keystone_error(StatusCode::CONFLICT, "Conflict", "volume already exists")
                .into_response()
        }
        Err(_) => unavailable(),
    }
}

pub(crate) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, id)): Path<(String, String)>,
    Json(request): Json<VolumeUpdateRequest>,
) -> Response {
    let Ok((_, store)) = scoped_store(&state, &headers, &project_id).await else {
        return unavailable();
    };
    let Some(id) = volume_id(&id) else {
        return volume_not_found();
    };
    let Ok(Some(mut record)) = store.get_volume(id).await else {
        return keystone_error(StatusCode::NOT_FOUND, "Not Found", "volume not found")
            .into_response();
    };
    if record.volume.project_id != project_id {
        return keystone_error(StatusCode::NOT_FOUND, "Not Found", "volume not found")
            .into_response();
    }
    let VolumeUpdateRequest {
        volume:
            VolumeUpdate {
                name,
                description,
                metadata,
            },
    } = request;
    if let Some(name) = name {
        record.volume.name = name;
    }
    if let Some(description) = description {
        record.volume.description = description;
    }
    if let Some(metadata) = metadata {
        let Ok(metadata) = serde_json::from_value(metadata) else {
            return keystone_error(
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "metadata must be an object",
            )
            .into_response();
        };
        record.volume.metadata = metadata;
    }
    record.volume.generation = record.volume.generation.saturating_add(1);
    match store
        .update_volume(record.volume.generation - 1, &record)
        .await
    {
        Ok(updated) => (
            StatusCode::OK,
            Json(VolumeResponse {
                volume: view(&updated),
            }),
        )
            .into_response(),
        Err(o3k_store::StoreError::StaleGeneration) => keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "volume generation changed",
        )
        .into_response(),
        Err(_) => unavailable(),
    }
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, id)): Path<(String, String)>,
) -> Response {
    let Ok((_, store)) = scoped_store(&state, &headers, &project_id).await else {
        return unavailable();
    };
    let Some(id) = volume_id(&id) else {
        return volume_not_found();
    };
    let Ok(Some(record)) = store.get_volume(id).await else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if record.volume.project_id != project_id {
        return keystone_error(StatusCode::NOT_FOUND, "Not Found", "volume not found")
            .into_response();
    }
    let attachments = match store.list_volume_attachments_v1(&project_id).await {
        Ok(attachments) => attachments,
        Err(_) => return unavailable(),
    };
    if attachments.into_iter().any(|a| {
        a.attachment.volume_id.as_uuid() == id
            && !matches!(
                a.attachment.state,
                o3k_domain::VolumeAttachmentState::Deleted
            )
    }) {
        return keystone_error(
            StatusCode::CONFLICT,
            "Conflict",
            "volume has an active attachment",
        )
        .into_response();
    }
    if state.storage_provider.is_none() {
        // Do not remove canonical state while a provider is unavailable: the
        // durable row is the recovery inventory for an owned provider volume.
        return unavailable();
    }
    if let Some(provider) = state.storage_provider.as_ref() {
        let mut deleting = record.clone();
        deleting.volume.state = VolumeState::Deleting;
        deleting.volume.generation = record.volume.generation.saturating_add(1);
        if store
            .update_volume(record.volume.generation, &deleting)
            .await
            .is_err()
        {
            return unavailable();
        }
        let request = StorageVolumeRequest {
            volume_id: id.into(),
            project_id: project_id.clone(),
            size_bytes: record.volume.size_bytes,
            generation: record.volume.generation,
        };
        match provider.delete_volume(&request).await {
            Ok(()) | Err(o3k_storage::StorageProviderError::NotFound) => {
                match provider.inspect_volume(&request).await {
                    Err(o3k_storage::StorageProviderError::NotFound) => {}
                    Ok(_) | Err(_) => return unavailable(),
                }
            }
            Err(_) => {
                return (
                    StatusCode::ACCEPTED,
                    Json(VolumeResponse {
                        volume: view(&deleting),
                    }),
                )
                    .into_response();
            }
        }
    }
    match store.delete_volume(&project_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => unavailable(),
    }
}

/// Reconciles native volumes left in a transitional state by a process
/// restart.  Canonical volume rows are the recovery inventory; provider
/// inspection is performed before any repeat mutation and provider state is
/// never used to create a canonical volume.
pub async fn recover_native_volumes(state: &AppState) {
    let (Some(store), Some(provider)) = (
        state.storage_store.as_ref(),
        state.storage_provider.as_ref(),
    ) else {
        return;
    };
    let records = match store.list_all_volumes().await {
        Ok(records) => records,
        Err(error) => {
            tracing::error!(%error, "failed to enumerate native volumes during startup recovery");
            return;
        }
    };
    for record in records {
        let request = StorageVolumeRequest {
            volume_id: record.volume.id,
            project_id: record.volume.project_id.clone(),
            size_bytes: record.volume.size_bytes,
            generation: record.volume.generation,
        };
        match record.volume.state {
            VolumeState::Requested | VolumeState::Creating | VolumeState::Unknown => match provider
                .inspect_volume(&request)
                .await
            {
                Ok(observation) => {
                    let mut available = record.clone();
                    available.volume.state = VolumeState::Available;
                    available.volume.generation = match record.volume.generation.checked_add(1) {
                        Some(generation) => generation,
                        None => {
                            tracing::error!(volume_id = %record.volume.id, "native volume generation overflow during recovery");
                            continue;
                        }
                    };
                    available.volume.provider_reference =
                        Some(o3k_domain::StorageProviderReference {
                            provider: observation.provider_reference.provider,
                            resource_id: observation.provider_reference.resource_id,
                        });
                    if let Err(error) = store
                        .update_volume(record.volume.generation, &available)
                        .await
                    {
                        tracing::warn!(%error, volume_id = %record.volume.id, "native volume recovery state update was fenced");
                    }
                }
                Err(o3k_storage::StorageProviderError::NotFound)
                    if matches!(
                        record.volume.state,
                        VolumeState::Requested | VolumeState::Creating | VolumeState::Unknown
                    ) =>
                {
                    match provider.create_volume(&request).await {
                        Ok(observation) => {
                            let mut available = record.clone();
                            available.volume.state = VolumeState::Available;
                            available.volume.generation =
                                match record.volume.generation.checked_add(1) {
                                    Some(generation) => generation,
                                    None => continue,
                                };
                            available.volume.provider_reference =
                                Some(o3k_domain::StorageProviderReference {
                                    provider: observation.provider_reference.provider,
                                    resource_id: observation.provider_reference.resource_id,
                                });
                            let _ = store
                                .update_volume(record.volume.generation, &available)
                                .await;
                        }
                        Err(error) if error.is_unknown_outcome() => {
                            if record.volume.state != VolumeState::Requested {
                                let mut unknown = record.clone();
                                unknown.volume.state = VolumeState::Unknown;
                                unknown.volume.generation =
                                    unknown.volume.generation.saturating_add(1);
                                let _ = store
                                    .update_volume(record.volume.generation, &unknown)
                                    .await;
                            }
                        }
                        Err(_) => {
                            if record.volume.state != VolumeState::Requested {
                                let mut failed = record.clone();
                                failed.volume.state = VolumeState::Error;
                                failed.volume.generation =
                                    failed.volume.generation.saturating_add(1);
                                let _ =
                                    store.update_volume(record.volume.generation, &failed).await;
                            }
                        }
                    }
                }
                Err(error) if error.is_unknown_outcome() => {
                    if record.volume.state != VolumeState::Requested {
                        let mut unknown = record.clone();
                        unknown.volume.state = VolumeState::Unknown;
                        unknown.volume.generation = unknown.volume.generation.saturating_add(1);
                        let _ = store
                            .update_volume(record.volume.generation, &unknown)
                            .await;
                    }
                }
                Err(_) => {}
            },
            VolumeState::Deleting => match provider.inspect_volume(&request).await {
                Err(o3k_storage::StorageProviderError::NotFound) => {
                    let _ = store
                        .delete_volume(&record.volume.project_id, record.volume.id.as_uuid())
                        .await;
                }
                Ok(_) => match provider.delete_volume(&request).await {
                    Ok(()) | Err(o3k_storage::StorageProviderError::NotFound) => {
                        if matches!(
                            provider.inspect_volume(&request).await,
                            Err(o3k_storage::StorageProviderError::NotFound)
                        ) {
                            let _ = store
                                .delete_volume(
                                    &record.volume.project_id,
                                    record.volume.id.as_uuid(),
                                )
                                .await;
                        }
                    }
                    Err(_) => {}
                },
                Err(_) => {}
            },
            _ => {}
        }
    }
}
