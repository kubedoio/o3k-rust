//! Nova-compatible volume attachment protocol adapter: attach/list/show/
//! delete handlers and wire models, limited to already-declared behavior.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use o3k_compute::ComputeError;
use o3k_domain::{
    AttachmentAccessMode, ServerId, StorageExecutionScope, VolumeAttachment, VolumeAttachmentId,
    VolumeAttachmentState, VolumeState,
};
use o3k_provider::BlockDeviceAttachment;
use o3k_storage::StorageAttachmentRequest;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    compute::{compute_error, project_auth_context, requested_compute_289},
    error::keystone_error,
};

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeAttachmentRequest {
    #[serde(rename = "volumeAttachment", alias = "volume_attachment")]
    volume_attachment: VolumeAttachmentRequestPayload,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeAttachmentRequestPayload {
    #[serde(rename = "volumeId", alias = "volume_id")]
    volume_id: String,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(
        rename = "delete_on_termination",
        alias = "deleteOnTermination",
        default
    )]
    delete_on_termination: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct VolumeAttachmentResponse {
    #[serde(rename = "volumeAttachment")]
    volume_attachment: VolumeAttachmentDetails,
}

#[derive(Debug, Serialize)]
pub(crate) struct VolumeAttachmentsResponse {
    #[serde(rename = "volumeAttachments")]
    volume_attachments: Vec<VolumeAttachmentDetails>,
}

#[derive(Debug, Serialize)]
pub(crate) struct VolumeAttachmentDetails {
    /// Legacy `id` field, emitted only at microversion 2.1 (and below).
    /// Upstream Nova removed it at 2.89 in favor of `attachment_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "attachment_id")]
    attachment_id: String,
    /// `attachmentId` (camel) is a legacy O3K alias emitted at 2.1 only; it is
    /// not part of the upstream 2.89 field set.
    #[serde(skip_serializing_if = "Option::is_none", rename = "attachmentId")]
    attachment_id_camel: Option<String>,
    #[serde(rename = "bdm_uuid")]
    bdm_uuid: String,
    #[serde(rename = "serverId")]
    server_id: String,
    #[serde(rename = "volumeId")]
    volume_id: String,
    device: String,
    tag: Option<String>,
    delete_on_termination: bool,
}

pub(crate) fn map_volume_attachment(
    record: o3k_store::VolumeAttachmentRecord,
    at_289: bool,
) -> VolumeAttachmentDetails {
    let attachment_id = record
        .cinder_attachment_id
        .clone()
        .unwrap_or_else(|| record.id.to_string());
    VolumeAttachmentDetails {
        id: if at_289 {
            None
        } else {
            Some(attachment_id.clone())
        },
        attachment_id: attachment_id.clone(),
        attachment_id_camel: if at_289 { None } else { Some(attachment_id) },
        bdm_uuid: record.id.to_string(),
        server_id: record.server_id.to_string(),
        volume_id: record.volume_id.to_string(),
        device: record.device,
        tag: record.tag,
        delete_on_termination: record.delete_on_termination,
    }
}

fn native_attachment_view(
    record: &o3k_store::VolumeAttachmentRecordV1,
    at_289: bool,
) -> VolumeAttachmentDetails {
    let id = record.attachment.id.to_string();
    VolumeAttachmentDetails {
        id: (!at_289).then(|| id.clone()),
        attachment_id: id.clone(),
        attachment_id_camel: (!at_289).then_some(id),
        bdm_uuid: record.attachment.id.to_string(),
        server_id: record.attachment.server_id.to_string(),
        volume_id: record.attachment.volume_id.to_string(),
        // The native execution provider deliberately keeps the host device
        // path out of canonical state.  Nova's bounded compatibility profile
        // nevertheless requires a stable device value on refresh; use the
        // provider-neutral default expected by the pinned provider.
        device: "/dev/vdb".to_owned(),
        tag: None,
        delete_on_termination: record.attachment.delete_on_termination,
    }
}

fn native_attachment_enabled(state: &AppState) -> bool {
    state.storage_store.is_some()
        && state.storage_provider.is_some()
        && state
            .compute
            .as_ref()
            .is_none_or(|compute| !compute.cinder_configured())
}

fn now_rfc3339() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}

async fn create_native_attachment(
    state: &AppState,
    auth: &o3k_kernel::AuthContext,
    server_id: Uuid,
    volume_id: Uuid,
    device: Option<String>,
    delete_on_termination: bool,
) -> Result<o3k_store::VolumeAttachmentRecordV1, StatusCode> {
    let store = state
        .storage_store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let compute = state
        .compute
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    compute
        .show_server_for_auth(auth, ServerId::from_uuid(server_id))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let volume = store
        .get_volume(volume_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if volume.volume.project_id != auth.effective_scope().id().as_str()
        || volume.volume.state != VolumeState::Available
    {
        return Err(StatusCode::CONFLICT);
    }
    let record = o3k_store::VolumeAttachmentRecordV1 {
        attachment: VolumeAttachment {
            id: VolumeAttachmentId::from_uuid(Uuid::new_v4()),
            project_id: auth.effective_scope().id().as_str().to_owned(),
            volume_id: volume.volume.id,
            server_id,
            execution_scope: StorageExecutionScope::Host("local".to_owned()),
            access_mode: AttachmentAccessMode::ReadWrite,
            delete_on_termination,
            state: VolumeAttachmentState::Reserved,
            generation: 1,
            operation_id: None,
        },
        created_at: now_rfc3339(),
    };
    store
        .insert_volume_attachment_v1(&record)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    // The durable V1 attachment is the canonical storage record.  The
    // generic resource row is only the existing operation-journal projection;
    // the workflow uses it as its foreign-key anchor before crossing either
    // provider boundary.
    if store
        .insert_resource(&o3k_store::ResourceRecord {
            id: record.attachment.id.as_uuid(),
            kind: "native_volume_attachment".to_owned(),
            project_id: record.attachment.project_id.clone(),
            generation: record.attachment.generation as i64,
            observed_generation: record.attachment.generation as i64,
            desired_state: "attached".to_owned(),
            observed_state: "reserved".to_owned(),
            provider_id: None,
        })
        .await
        .is_err()
    {
        // The canonical attachment was inserted before the operation-journal
        // projection.  Without the projection no provider boundary has been
        // crossed, so compensate rather than leaving an orphan attachment.
        let _ = store
            .delete_volume_attachment_v1(
                &record.attachment.project_id,
                record.attachment.id.as_uuid(),
            )
            .await;
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if let Some(workflow) = state.native_attachment_workflow.as_ref() {
        if let Err(error) = workflow.attach(record.attachment.id.as_uuid()).await {
            tracing::warn!(attachment_id = %record.attachment.id, error = ?error, "native attachment workflow failed");
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        return store
            .get_volume_attachment_v1(record.attachment.id.as_uuid())
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .ok_or(StatusCode::SERVICE_UNAVAILABLE);
    }
    if let Some(provider) = state.storage_provider.as_ref() {
        let request = StorageAttachmentRequest {
            attachment_id: record.attachment.id,
            volume_id: record.attachment.volume_id,
            project_id: record.attachment.project_id.clone(),
            volume_generation: volume.volume.generation,
            host_id: "local".to_owned(),
            access_mode: record.attachment.access_mode,
        };
        let prepared = match provider.prepare_attachment(&request).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let mut failed = record.clone();
                failed.attachment.state = if error.is_unknown_outcome() {
                    VolumeAttachmentState::Unknown
                } else {
                    VolumeAttachmentState::Error
                };
                failed.attachment.generation += 1;
                let _ = store
                    .update_volume_attachment_v1(record.attachment.generation, &failed)
                    .await;
                return Err(if error.is_unknown_outcome() {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::CONFLICT
                });
            }
        };
        let block_device = BlockDeviceAttachment {
            volume_id: record.attachment.volume_id.to_string(),
            attachment_id: record.attachment.id.to_string(),
            driver_volume_type: "local".to_owned(),
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            local_path: Some(prepared.device_path().to_owned()),
            device_path: device.clone(),
            multipath: false,
            initiator: None,
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };
        if let Err(error) = compute
            .provider()
            .attach_block_device(server_id, &block_device)
            .await
        {
            let _ = provider.terminate_attachment(&request).await;
            let mut failed = record.clone();
            failed.attachment.state = if error.is_unknown_outcome() {
                VolumeAttachmentState::Unknown
            } else {
                VolumeAttachmentState::Error
            };
            failed.attachment.generation += 1;
            let _ = store
                .update_volume_attachment_v1(record.attachment.generation, &failed)
                .await;
            return Err(if error.is_unknown_outcome() {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::CONFLICT
            });
        }
    }
    let mut attached = record.clone();
    attached.attachment.state = VolumeAttachmentState::Attached;
    attached.attachment.generation += 1;
    let attached = store
        .update_volume_attachment_v1(record.attachment.generation, &attached)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut in_use = volume.clone();
    in_use.volume.state = VolumeState::InUse;
    in_use.volume.generation += 1;
    store
        .update_volume(volume.volume.generation, &in_use)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(attached)
}

pub(crate) async fn attach_volume(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, server_id)): Path<(String, String)>,
    Json(request): Json<VolumeAttachmentRequest>,
) -> impl IntoResponse {
    let auth = match project_auth_context(&state, &headers, &project_id) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Ok(volume_uuid) = Uuid::parse_str(&request.volume_attachment.volume_id) else {
        return compute_error(ComputeError::InvalidRequest).into_response();
    };
    if native_attachment_enabled(&state) {
        return match create_native_attachment(
            &state,
            &auth,
            server_uuid,
            volume_uuid,
            request.volume_attachment.device,
            request.volume_attachment.delete_on_termination,
        )
        .await
        {
            Ok(record) => (
                StatusCode::OK,
                Json(VolumeAttachmentResponse {
                    volume_attachment: native_attachment_view(&record, false),
                }),
            )
                .into_response(),
            Err(status) => status.into_response(),
        };
    }
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .attach_volume_for_auth(
            &auth,
            ServerId::from_uuid(server_uuid),
            volume_uuid,
            request.volume_attachment.device,
            request.volume_attachment.tag,
            request.volume_attachment.delete_on_termination,
        )
        .await
    {
        Ok(record) => (
            StatusCode::OK,
            Json(VolumeAttachmentResponse {
                volume_attachment: map_volume_attachment(record, false),
            }),
        )
            .into_response(),
        Err(error) => compute_error(error).into_response(),
    }
}

pub(crate) async fn list_volume_attachments(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, server_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let auth = match project_auth_context(&state, &headers, &project_id) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    if native_attachment_enabled(&state) {
        let Some(store) = state.storage_store.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let records = match store.list_volume_attachments_v1(&project_id).await {
            Ok(records) => records
                .into_iter()
                .filter(|record| {
                    record.attachment.server_id == server_uuid
                        && record.attachment.state == VolumeAttachmentState::Attached
                })
                .map(|record| native_attachment_view(&record, requested_compute_289(&headers)))
                .collect(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        return (
            StatusCode::OK,
            Json(VolumeAttachmentsResponse {
                volume_attachments: records,
            }),
        )
            .into_response();
    }
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .list_volume_attachments_for_auth(&auth, ServerId::from_uuid(server_uuid))
        .await
    {
        Ok(records) => {
            let at_289 = requested_compute_289(&headers);
            (
                StatusCode::OK,
                Json(VolumeAttachmentsResponse {
                    volume_attachments: records
                        .into_iter()
                        .filter(|r| r.status == "attached")
                        .map(|record| map_volume_attachment(record, at_289))
                        .collect(),
                }),
            )
                .into_response()
        }
        Err(error) => compute_error(error).into_response(),
    }
}

pub(crate) async fn show_volume_attachment(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, server_id, attachment_id)): Path<(String, String, String)>,
) -> impl IntoResponse {
    let auth = match project_auth_context(&state, &headers, &project_id) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    if native_attachment_enabled(&state) {
        let Some(store) = state.storage_store.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let Ok(attachment_uuid) = Uuid::parse_str(&attachment_id) else {
            return compute_error(ComputeError::NotFound).into_response();
        };
        if let Ok(Some(record)) = store.get_volume_attachment_v1(attachment_uuid).await
            && record.attachment.project_id == project_id
            && record.attachment.server_id == server_uuid
            && record.attachment.state == VolumeAttachmentState::Attached
        {
            return (
                StatusCode::OK,
                Json(VolumeAttachmentResponse {
                    volume_attachment: native_attachment_view(
                        &record,
                        requested_compute_289(&headers),
                    ),
                }),
            )
                .into_response();
        }
        return compute_error(ComputeError::NotFound).into_response();
    }
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };
    let at_289 = requested_compute_289(&headers);

    if let Ok(records) = compute
        .list_volume_attachments_for_auth(&auth, ServerId::from_uuid(server_uuid))
        .await
    {
        for record in records {
            if record.status == "attached"
                && (record.id.to_string() == attachment_id
                    || record.volume_id.to_string() == attachment_id
                    || record.cinder_attachment_id.as_deref() == Some(&attachment_id))
            {
                return (
                    StatusCode::OK,
                    Json(VolumeAttachmentResponse {
                        volume_attachment: map_volume_attachment(record, at_289),
                    }),
                )
                    .into_response();
            }
        }
    }

    compute_error(ComputeError::NotFound).into_response()
}

pub(crate) async fn delete_volume_attachment(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, server_id, attachment_id)): Path<(String, String, String)>,
) -> impl IntoResponse {
    let auth = match project_auth_context(&state, &headers, &project_id) {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    if native_attachment_enabled(&state) {
        let Some(store) = state.storage_store.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let Ok(attachment_uuid) = Uuid::parse_str(&attachment_id) else {
            return compute_error(ComputeError::NotFound).into_response();
        };
        let Ok(Some(record)) = store.get_volume_attachment_v1(attachment_uuid).await else {
            return compute_error(ComputeError::NotFound).into_response();
        };
        if record.attachment.project_id != project_id || record.attachment.server_id != server_uuid
        {
            return compute_error(ComputeError::NotFound).into_response();
        }
        if let Some(workflow) = state.native_attachment_workflow.as_ref() {
            if workflow
                .detach(record.attachment.id.as_uuid())
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Ok(Some(mut volume)) = store
                .get_volume(record.attachment.volume_id.as_uuid())
                .await
            {
                volume.volume.state = VolumeState::Available;
                volume.volume.generation += 1;
                if store
                    .update_volume(volume.volume.generation - 1, &volume)
                    .await
                    .is_err()
                {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            }
            return StatusCode::NO_CONTENT.into_response();
        }
        let mut deleting = record.clone();
        deleting.attachment.state = VolumeAttachmentState::Detaching;
        deleting.attachment.generation += 1;
        if store
            .update_volume_attachment_v1(record.attachment.generation, &deleting)
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if let Some(provider) = state.storage_provider.as_ref() {
            let Some(compute) = state.compute.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let Ok(Some(volume)) = store
                .get_volume(record.attachment.volume_id.as_uuid())
                .await
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let request = StorageAttachmentRequest {
                attachment_id: record.attachment.id,
                volume_id: record.attachment.volume_id,
                project_id: record.attachment.project_id.clone(),
                volume_generation: volume.volume.generation,
                host_id: "local".to_owned(),
                access_mode: record.attachment.access_mode,
            };
            let block_device = BlockDeviceAttachment {
                volume_id: record.attachment.volume_id.to_string(),
                attachment_id: record.attachment.id.to_string(),
                driver_volume_type: "local".to_owned(),
                target_iqn: None,
                target_portal: None,
                target_lun: None,
                local_path: None,
                device_path: None,
                multipath: false,
                initiator: None,
                auth_method: None,
                auth_username: None,
                auth_password: None,
            };
            if compute
                .provider()
                .detach_block_device(server_uuid, &block_device)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Err(error) = provider.terminate_attachment(&request).await {
                return if error.is_unknown_outcome() {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    StatusCode::CONFLICT.into_response()
                };
            }
        }
        let volume_id = record.attachment.volume_id.as_uuid();
        if let Ok(Some(mut volume)) = store.get_volume(volume_id).await {
            volume.volume.state = VolumeState::Available;
            volume.volume.generation += 1;
            if store
                .update_volume(volume.volume.generation - 1, &volume)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
        deleting.attachment.state = VolumeAttachmentState::Deleted;
        deleting.attachment.generation += 1;
        return match store
            .update_volume_attachment_v1(deleting.attachment.generation - 1, &deleting)
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    let target_uuid = if let Ok(uuid) = Uuid::parse_str(&attachment_id) {
        if compute
            .get_volume_attachment_for_auth(&auth, ServerId::from_uuid(server_uuid), uuid)
            .await
            .is_ok()
        {
            Some(uuid)
        } else {
            None
        }
    } else {
        None
    };

    let target_uuid = match target_uuid {
        Some(uuid) => uuid,
        None => {
            if let Ok(records) = compute
                .list_volume_attachments_for_auth(&auth, ServerId::from_uuid(server_uuid))
                .await
            {
                let found = records.into_iter().find(|r| {
                    r.volume_id.to_string() == attachment_id
                        || r.cinder_attachment_id.as_deref() == Some(&attachment_id)
                });
                match found {
                    Some(r) => r.id,
                    None => return compute_error(ComputeError::NotFound).into_response(),
                }
            } else {
                return compute_error(ComputeError::NotFound).into_response();
            }
        }
    };

    match compute
        .detach_volume_for_auth(&auth, ServerId::from_uuid(server_uuid), target_uuid)
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => compute_error(error).into_response(),
    }
}
