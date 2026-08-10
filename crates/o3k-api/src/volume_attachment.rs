//! Nova-compatible volume attachment protocol adapter: attach/list/show/
//! delete handlers and wire models, limited to already-declared behavior.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use o3k_compute::ComputeError;
use o3k_domain::ServerId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    compute::{compute_error, project_token, requested_compute_289},
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
    device: Option<String>,
    tag: Option<String>,
    #[serde(default, alias = "delete_on_termination")]
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

pub(crate) async fn attach_volume(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((project_id, server_id)): Path<(String, String)>,
    Json(request): Json<VolumeAttachmentRequest>,
) -> impl IntoResponse {
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Ok(volume_uuid) = Uuid::parse_str(&request.volume_attachment.volume_id) else {
        return compute_error(ComputeError::InvalidRequest).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .attach_volume(
            &project_id,
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
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
    let Some(compute) = state.compute else {
        return keystone_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "compute service unavailable",
        )
        .into_response();
    };

    match compute
        .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
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
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
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
        .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
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
    if let Err(response) = project_token(&state, &headers, &project_id) {
        return response;
    }
    let Ok(server_uuid) = Uuid::parse_str(&server_id) else {
        return compute_error(ComputeError::NotFound).into_response();
    };
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
            .get_volume_attachment(&project_id, ServerId::from_uuid(server_uuid), uuid)
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
                .list_volume_attachments(&project_id, ServerId::from_uuid(server_uuid))
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
        .detach_volume(&project_id, ServerId::from_uuid(server_uuid), target_uuid)
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => compute_error(error).into_response(),
    }
}
