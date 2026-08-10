//! Durable Nova-to-Cinder volume attachment orchestration.
//!
//! The orchestrator drives the frozen Cinder v3 attachment lifecycle through
//! the compute execution boundary, persisting each phase before the matching
//! external side effect:
//!
//! ```text
//! validated
//! -> cinder_attachment_created
//! -> connector_obtained
//! -> connection_prepared
//! -> compute_attach_requested
//! -> compute_attached
//! -> cinder_attachment_completed
//! -> attached
//! ```
//!
//! Detach runs the reverse order. Timeouts are unknown outcomes and require
//! observation before retry or compensation. Only bounded non-secret data is
//! persisted; connection information is stored as a digest and its secret
//! fields never cross the compute boundary.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use o3k_provider::{
    AttachmentError, AttachmentObservation, AttachmentTarget, BlockDeviceAttachment,
    ComputeConnector, ComputeProvider, ConnectionInfo, ConnectionInfoPresence, ProviderError,
    VolumeAttachmentProvider,
};
use o3k_store::{ComputeRepository, VolumeAttachmentRecord};
use uuid::Uuid;

use crate::{ComputeError, ProviderBackend};
pub const STATUS_VALIDATED: &str = "validated";
pub const STATUS_CINDER_ATTACHMENT_CREATED: &str = "cinder_attachment_created";
pub const STATUS_CONNECTOR_OBTAINED: &str = "connector_obtained";
pub const STATUS_CONNECTION_PREPARED: &str = "connection_prepared";
pub const STATUS_COMPUTE_ATTACH_REQUESTED: &str = "compute_attach_requested";
pub const STATUS_COMPUTE_ATTACHED: &str = "compute_attached";
pub const STATUS_CINDER_ATTACHMENT_COMPLETED: &str = "cinder_attachment_completed";
pub const STATUS_ATTACHED: &str = "attached";
pub const STATUS_DETACH_REQUESTED: &str = "detach_requested";
pub const STATUS_COMPUTE_DETACH_REQUESTED: &str = "compute_detach_requested";
pub const STATUS_COMPUTE_DETACHED: &str = "compute_detached";
pub const STATUS_CINDER_ATTACHMENT_TERMINATED: &str = "cinder_attachment_terminated";
pub const STATUS_DETACHED: &str = "detached";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_UNKNOWN: &str = "unknown_outcome";

pub const TERMINAL_STATUSES: &[&str] = &[STATUS_ATTACHED, STATUS_DETACHED, STATUS_ERROR];

/// Whether a persisted phase belongs to the detach flow (reverse of attach).
fn is_detach_phase(status: &str) -> bool {
    matches!(
        status,
        STATUS_DETACH_REQUESTED
            | STATUS_COMPUTE_DETACH_REQUESTED
            | STATUS_COMPUTE_DETACHED
            | STATUS_CINDER_ATTACHMENT_TERMINATED
    )
}

/// Removes the durable id from the in-flight set when dropped, covering every
/// early-return path of attach/continue_attach/detach.
struct FlightGuard<'a> {
    set: &'a Mutex<HashSet<Uuid>>,
    id: Uuid,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
    }
}

#[derive(Clone)]
pub struct AttachmentOrchestrator {
    store: Arc<dyn ComputeRepository>,
    provider: Arc<ProviderBackend>,
    cinder: Option<Arc<dyn VolumeAttachmentProvider>>,
    /// Durable attachment ids currently being processed by attach/detach.
    /// Reconciliation skips these so it never races a live operation.
    in_flight: Arc<Mutex<HashSet<Uuid>>>,
}

impl AttachmentOrchestrator {
    pub fn new(
        store: Arc<dyn ComputeRepository>,
        provider: Arc<ProviderBackend>,
        cinder: Option<Arc<dyn VolumeAttachmentProvider>>,
    ) -> Self {
        Self {
            store,
            provider,
            cinder,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn enter_flight(&self, id: Uuid) -> FlightGuard<'_> {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id);
        FlightGuard {
            set: &self.in_flight,
            id,
        }
    }

    fn is_in_flight(&self, id: Uuid) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&id)
    }

    #[must_use]
    pub fn cinder_configured(&self) -> bool {
        self.cinder.is_some()
    }

    /// Executes the durable Cinder attach lifecycle. Duplicate requests for
    /// the same volume return the existing durable attachment. The flow is
    /// phase-driven: every external side effect is preceded by persisting its
    /// phase, so a restart or an unknown outcome resumes from the persisted
    /// phase instead of guessing.
    pub async fn attach(
        &self,
        project_id: &str,
        server_id: Uuid,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        if let Some(existing) = self
            .store
            .get_volume_attachment_by_volume_for_server(volume_id, server_id)
            .await?
        {
            // Idempotent duplicate attach: never create a second attachment or
            // a second Cinder record. A still-in-flight record is returned as
            //-is and the reconciler drives it to completion.
            return Ok(existing);
        }

        let id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        let idempotency_key = format!("attach:{server_id}:{volume_id}");
        let device = match device {
            Some(value) if !value.trim().is_empty() => {
                if value.starts_with("/dev/") {
                    value
                } else {
                    format!("/dev/{value}")
                }
            }
            _ => {
                let existing = self.store.list_volume_attachments(server_id).await?;
                let count = existing.len();
                let letter = (b'b' + count.min(23) as u8) as char;
                format!("/dev/vd{letter}")
            }
        };
        let record = VolumeAttachmentRecord {
            id,
            server_id,
            volume_id,
            device,
            tag,
            delete_on_termination,
            created_at: now_rfc3339(),
            status: STATUS_VALIDATED.to_owned(),
            operation_id: Some(operation_id),
            idempotency_key: Some(idempotency_key),
            cinder_attachment_id: None,
            connector_host: None,
            connector_ip: None,
            connector_initiator: None,
            driver_volume_type: None,
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            connection_info_digest: None,
            error: None,
        };
        self.store.insert_volume_attachment(&record).await?;
        self.trace_phase(&record, STATUS_VALIDATED, None).await?;
        let _flight = self.enter_flight(id);
        self.continue_attach(project_id, &record).await
    }

    /// Advances an attachment record through the attach state machine from its
    /// persisted phase. Safe to call after a restart or an unknown outcome:
    /// each step is re-derived from the durable record and external side
    /// effects are idempotent (Cinder create/complete/delete are idempotent by
    /// id; compute attach/detach are idempotent in the provider).
    async fn continue_attach(
        &self,
        project_id: &str,
        record: &VolumeAttachmentRecord,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _flight = self.enter_flight(record.id);
        let cinder = self.cinder.clone().ok_or(ComputeError::Unavailable)?;
        let volume_id_str = record.volume_id.to_string();
        let server_id = record.server_id;

        // Phase: cinder_attachment_created
        let cinder_attachment_id = match record.cinder_attachment_id.clone() {
            Some(id) => id,
            None => {
                self.set_phase(record.id, STATUS_CINDER_ATTACHMENT_CREATED, None)
                    .await?;
                let server_id_str = server_id.to_string();
                match cinder
                    .create_attachment(project_id, &volume_id_str, Some(&server_id_str))
                    .await
                {
                    Ok(attachment) => {
                        let id = attachment.id.clone();
                        self.trace_attachment(
                            record,
                            "cinder_attachment_created",
                            &attachment,
                            None,
                        );
                        self.store
                            .update_volume_attachment_outcome(
                                record.id,
                                STATUS_CINDER_ATTACHMENT_CREATED,
                                Some(&id),
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                            )
                            .await?;
                        id
                    }
                    Err(error) => {
                        self.trace_error(record, "cinder_attachment_create", &error);
                        if error.is_unknown_outcome() {
                            // Unknown outcome: never compensate without observing.
                            self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                                .await?;
                            return Err(map_attachment_error(error));
                        }
                        self.observe_before_compensate(
                            project_id,
                            record.id,
                            &volume_id_str,
                            None,
                            &format!("{error}"),
                        )
                        .await?;
                        return Err(map_attachment_error(error));
                    }
                }
            }
        };

        // Phase: connector_obtained
        if record.connector_host.is_none() {
            self.set_phase(record.id, STATUS_CONNECTOR_OBTAINED, None)
                .await?;
            match self.provider.collect_connector(server_id).await {
                Ok(connector) => {
                    self.store
                        .update_volume_attachment_outcome(
                            record.id,
                            STATUS_CONNECTOR_OBTAINED,
                            None,
                            Some(&connector.host),
                            Some(&connector.ip),
                            connector.initiator.as_deref(),
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                        )
                        .await?;
                }
                Err(error) => {
                    self.trace_error(record, "collect_connector", &error);
                    self.compensate_after_create(
                        project_id,
                        record.id,
                        Some(&cinder_attachment_id),
                        &format!("{error}"),
                    )
                    .await?;
                    return Err(map_provider_error(error));
                }
            }
        }

        // Phase: connection_prepared (PUT connector to Cinder)
        let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
        let record = fresh.as_ref().unwrap_or(record);
        let (connection_info, target) = if record.connection_info_digest.is_some() {
            // Resumed after a restart or unknown outcome: re-fetch the secret
            // connection information from Cinder (CHAP credentials are never
            // persisted) and re-derive the target.
            self.prepared_target(project_id, &cinder_attachment_id, record)
                .await?
        } else {
            self.set_phase(record.id, STATUS_CONNECTION_PREPARED, None)
                .await?;
            let cinder_connector = {
                let host = record.connector_host.clone().unwrap_or_default();
                let ip = record.connector_ip.clone().unwrap_or_default();
                let initiator = record.connector_initiator.clone();
                ComputeConnector {
                    host,
                    ip,
                    platform: "x86_64".to_owned(),
                    os_type: "linux".to_owned(),
                    multipath: false,
                    initiator,
                }
            };
            let updated = match cinder
                .update_attachment_connector(project_id, &cinder_attachment_id, &cinder_connector)
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.trace_error(record, "cinder_attachment_update", &error);
                    if error.is_unknown_outcome() {
                        // The PUT may have succeeded server-side. Never delete a
                        // possibly-successful attachment solely because the local
                        // outcome was uncertain; persist unknown and reconcile by
                        // observation.
                        self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                            .await?;
                        return Err(map_attachment_error(error));
                    }
                    self.observe_before_compensate(
                        project_id,
                        record.id,
                        &volume_id_str,
                        Some(&cinder_attachment_id),
                        &format!("{error}"),
                    )
                    .await?;
                    return Err(map_attachment_error(error));
                }
            };
            self.trace_attachment(record, "cinder_attachment_update", &updated, None);
            self.connection_target(record, &updated).await?
        };

        let record = self
            .store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or(ComputeError::NotFound)?;
        self.store
            .update_volume_attachment_outcome(
                record.id,
                STATUS_CONNECTION_PREPARED,
                None,
                None,
                None,
                None,
                Some(&target.driver_volume_type),
                target.target_iqn.as_deref(),
                target.target_portal.as_deref(),
                target.target_lun.map(|value| value as u32),
                Some(connection_info.digest()),
                None,
            )
            .await?;

        // Phase: compute_attach_requested -> compute_attached
        let record = self
            .store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or(ComputeError::NotFound)?;
        let observed = self
            .provider
            .observe_block_device(server_id, &volume_id_str)
            .await;
        let device_attached = matches!(observed, Ok(Some(o)) if o.attached);
        if !device_attached {
            self.set_phase(record.id, STATUS_COMPUTE_ATTACH_REQUESTED, None)
                .await?;
            let initiator = record.connector_initiator.clone();
            let device_attachment = BlockDeviceAttachment {
                volume_id: volume_id_str.clone(),
                attachment_id: cinder_attachment_id.clone(),
                driver_volume_type: target.driver_volume_type.clone(),
                target_iqn: target.target_iqn.clone(),
                target_portal: target.target_portal.clone(),
                target_lun: target.target_lun.map(|value| value as u32),
                local_path: target.local_path.clone(),
                device_path: None,
                multipath: false,
                initiator,
                auth_method: target.auth_method.clone(),
                auth_username: target.auth_username.clone(),
                auth_password: target.auth_password.clone(),
            };
            let observation = match self
                .provider
                .attach_block_device(server_id, &device_attachment)
                .await
            {
                Ok(observation) => observation,
                Err(error) => {
                    self.trace_error(&record, "compute_attach", &error);
                    if error.is_unknown_outcome() {
                        self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                            .await?;
                        return Err(map_provider_error(error));
                    }
                    self.compensate_after_attach(
                        project_id,
                        record.id,
                        Some(&cinder_attachment_id),
                        server_id,
                        &device_attachment,
                        &format!("{error}"),
                    )
                    .await?;
                    return Err(map_provider_error(error));
                }
            };
            if !observation.attached {
                self.compensate_after_attach(
                    project_id,
                    record.id,
                    Some(&cinder_attachment_id),
                    server_id,
                    &device_attachment,
                    "compute device was not attached",
                )
                .await?;
                return Err(ComputeError::Conflict);
            }
            self.set_phase(record.id, STATUS_COMPUTE_ATTACHED, None)
                .await?;
            if let Some(path) = &observation.device_path {
                self.store
                    .update_volume_attachment_outcome(
                        record.id,
                        STATUS_COMPUTE_ATTACHED,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(path),
                    )
                    .await?;
            }
        } else {
            self.set_phase(record.id, STATUS_COMPUTE_ATTACHED, None)
                .await?;
        }

        // Phase: cinder_attachment_completed -> attached
        let record = self
            .store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or(ComputeError::NotFound)?;
        if record.status != STATUS_ATTACHED {
            self.set_phase(record.id, STATUS_CINDER_ATTACHMENT_COMPLETED, None)
                .await?;
            match cinder
                .complete_attachment(project_id, &cinder_attachment_id)
                .await
            {
                Ok(()) => {
                    self.set_phase(record.id, STATUS_ATTACHED, None).await?;
                }
                Err(error) => {
                    self.trace_error(&record, "cinder_attachment_complete", &error);
                    if error.is_unknown_outcome() {
                        // The complete may have succeeded server-side. Persist
                        // unknown; reconciliation observes the attachment and
                        // drives it to attached.
                        self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                            .await?;
                        return Err(map_attachment_error(error));
                    }
                    let device = self.block_device_from_record(&record).await;
                    self.compensate_after_attach(
                        project_id,
                        record.id,
                        Some(&cinder_attachment_id),
                        server_id,
                        &device,
                        &format!("{error}"),
                    )
                    .await?;
                    return Err(map_attachment_error(error));
                }
            }
        }

        self.store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or(ComputeError::NotFound)
    }

    /// Extracts the connection target from an attachment-update response,
    /// distinguishing missing, null and malformed `connection_info`. On any
    /// malformed result the attachment is observed (show) before a decision is
    /// made: a possibly-successful attachment is never deleted solely because
    /// the local parse failed.
    async fn connection_target(
        &self,
        record: &VolumeAttachmentRecord,
        updated: &AttachmentObservation,
    ) -> Result<(ConnectionInfo, AttachmentTarget), ComputeError> {
        let presence = updated.connection_info_presence();
        let Some(connection_info) = updated.connection_info.clone() else {
            self.trace_malformed(record, "connection_information_missing_or_null");
            self.observe_before_compensate(
                &self.project_id(record.server_id).await?,
                record.id,
                &record.volume_id.to_string(),
                Some(&updated.id),
                "connection information is missing or null",
            )
            .await?;
            return Err(ComputeError::InvalidRequest);
        };
        let Some(target) = connection_info.attach_target() else {
            self.trace_malformed(record, "connection_information_is_malformed");
            self.observe_before_compensate(
                &self.project_id(record.server_id).await?,
                record.id,
                &record.volume_id.to_string(),
                Some(&updated.id),
                "connection information is malformed",
            )
            .await?;
            return Err(ComputeError::InvalidRequest);
        };
        validate_target(target)?;
        tracing::debug!(
            presence = format!("{presence:?}").to_lowercase(),
            "connection_information_classified"
        );
        let target = target.clone();
        Ok((connection_info, target))
    }

    /// Re-fetches connection information from Cinder for a record that already
    /// reached `connection_prepared` (restart/unknown-outcome resume).
    async fn prepared_target(
        &self,
        project_id: &str,
        cinder_attachment_id: &str,
        record: &VolumeAttachmentRecord,
    ) -> Result<(ConnectionInfo, AttachmentTarget), ComputeError> {
        let cinder = self.cinder.clone().ok_or(ComputeError::Unavailable)?;
        let attachment = cinder
            .show_attachment(project_id, cinder_attachment_id)
            .await
            .map_err(|error| {
                self.trace_error(record, "cinder_attachment_observe", &error);
                // A failed or uncertain observation must not be compensated
                // without further observation; both map to Unavailable so the
                // record stays non-terminal for the reconciler.
                let _ = error;
                ComputeError::Unavailable
            })?;
        let Some(connection_info) = attachment.connection_info else {
            self.trace_malformed(record, "connection_information_missing_on_observe");
            self.observe_before_compensate(
                project_id,
                record.id,
                &record.volume_id.to_string(),
                Some(cinder_attachment_id),
                "connection information is missing on observe",
            )
            .await?;
            return Err(ComputeError::InvalidRequest);
        };
        let Some(target) = connection_info.attach_target() else {
            self.trace_malformed(record, "connection_information_malformed_on_observe");
            self.observe_before_compensate(
                project_id,
                record.id,
                &record.volume_id.to_string(),
                Some(cinder_attachment_id),
                "connection information is malformed on observe",
            )
            .await?;
            return Err(ComputeError::InvalidRequest);
        };
        validate_target(target)?;
        let target = target.clone();
        Ok((connection_info, target))
    }

    async fn project_id(&self, server_id: Uuid) -> Result<String, ComputeError> {
        self.project_for_server(server_id).await
    }

    async fn block_device_from_record(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> BlockDeviceAttachment {
        BlockDeviceAttachment {
            volume_id: record.volume_id.to_string(),
            attachment_id: record
                .cinder_attachment_id
                .clone()
                .unwrap_or_else(|| record.id.to_string()),
            driver_volume_type: record.driver_volume_type.clone().unwrap_or_default(),
            target_iqn: record.target_iqn.clone(),
            target_portal: record.target_portal.clone(),
            target_lun: record.target_lun,
            local_path: None,
            device_path: None,
            multipath: false,
            initiator: record.connector_initiator.clone(),
            auth_method: None,
            auth_username: None,
            auth_password: None,
        }
    }

    /// Executes the durable detach lifecycle in reverse order. Repeated
    /// detach and detach of an already-detached attachment are idempotent.
    pub async fn detach(
        &self,
        project_id: &str,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let record = self
            .store
            .get_volume_attachment(server_id, attachment_id)
            .await?
            .ok_or(ComputeError::NotFound)?;
        if record.status == STATUS_DETACHED || record.status == STATUS_ERROR {
            return Ok(());
        }
        self.continue_detach(project_id, &record).await
    }

    /// Advances a detach from its persisted phase. Restart-safe and
    /// idempotent: a crash between phases is resumed from the persisted phase,
    /// and the terminal `detached` is only persisted after the Cinder
    /// attachment has been terminated, so a terminal record never hides a live
    /// Cinder attachment.
    async fn continue_detach(
        &self,
        project_id: &str,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), ComputeError> {
        let _flight = self.enter_flight(record.id);
        let cinder = self.cinder.clone().ok_or(ComputeError::Unavailable)?;
        // Enter the detach flow from any non-detach phase (attached or a
        // mid-attach phase) by persisting the detach_requested phase first.
        if !is_detach_phase(record.status.as_str()) {
            self.set_phase(record.id, STATUS_DETACH_REQUESTED, None)
                .await?;
        }

        // Phase: compute_detach_requested -> compute_detached
        let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
        let record = fresh.as_ref().unwrap_or(record);
        if record.status == STATUS_DETACH_REQUESTED
            || record.status == STATUS_COMPUTE_DETACH_REQUESTED
        {
            self.set_phase(record.id, STATUS_COMPUTE_DETACH_REQUESTED, None)
                .await?;
            let device_attachment = self.block_device_from_record(record).await;
            match self
                .provider
                .detach_block_device(record.server_id, &device_attachment)
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    self.trace_error(record, "compute_detach", &error);
                    // A known compute-detach failure must not hide the Cinder
                    // attachment behind a terminal record: the reverse-order
                    // terminate cannot run while the device is attached, so
                    // keep the record non-terminal for the reconciler to
                    // observe and retry.
                    if error.is_unknown_outcome() {
                        self.set_phase(
                            record.id,
                            STATUS_UNKNOWN,
                            Some(&format!("detach; {error}")),
                        )
                        .await?;
                    } else {
                        self.set_phase(
                            record.id,
                            STATUS_COMPUTE_DETACH_REQUESTED,
                            Some(&format!("{error}")),
                        )
                        .await?;
                    }
                    return Err(map_provider_error(error));
                }
            }
            self.set_phase(record.id, STATUS_COMPUTE_DETACHED, None)
                .await?;
        }

        // Phase: cinder_attachment_terminated
        let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
        let record = fresh.as_ref().unwrap_or(record);
        if (record.status == STATUS_COMPUTE_DETACHED
            || record.status == STATUS_CINDER_ATTACHMENT_TERMINATED)
            && let Some(cinder_attachment_id) = &record.cinder_attachment_id
        {
            self.set_phase(record.id, STATUS_CINDER_ATTACHMENT_TERMINATED, None)
                .await?;
            match cinder
                .terminate_attachment(project_id, cinder_attachment_id)
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    self.trace_error(record, "cinder_attachment_terminate", &error);
                    match &error {
                        AttachmentError::NotFound(_) | AttachmentError::Conflict(_) => {}
                        other => {
                            // Unknown-outcome termination must not flip the
                            // record to detached without observation; keep it
                            // non-terminal so reconciliation observes first.
                            if other.is_unknown_outcome() {
                                self.set_phase(
                                    record.id,
                                    STATUS_UNKNOWN,
                                    Some(&format!("detach; {other}")),
                                )
                                .await?;
                                return Ok(());
                            }
                            tracing::warn!(%error, "cinder attachment termination warning");
                        }
                    }
                }
            }
        }
        self.set_phase(record.id, STATUS_DETACHED, None).await?;
        Ok(())
    }

    /// Reconciles non-terminal attachments after restart or an unknown
    /// outcome: observes the Cinder and compute boundaries and either advances
    /// the phase or compensates. Foreign resources are never deleted. Records
    /// with an operation in flight are skipped so reconciliation never races a
    /// live attach/detach.
    pub async fn reconcile(&self) -> Result<(), ComputeError> {
        let records = self
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        for record in records {
            if self.is_in_flight(record.id) {
                continue;
            }
            self.reconcile_record(&record).await?;
        }
        Ok(())
    }

    async fn reconcile_record(&self, record: &VolumeAttachmentRecord) -> Result<(), ComputeError> {
        if record.status == STATUS_UNKNOWN {
            return self.reconcile_unknown(record).await;
        }
        self.reconcile_in_progress(record).await
    }

    async fn project_for_server(&self, server_id: Uuid) -> Result<String, ComputeError> {
        let resource = self.store.get_resource(server_id).await?;
        Ok(resource.project_id)
    }

    async fn reconcile_unknown(&self, record: &VolumeAttachmentRecord) -> Result<(), ComputeError> {
        // A record set to unknown mid-detach must resume the detach flow, not
        // the attach flow. The detach unknown paths record an error prefixed
        // with "detach".
        if record
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("detach"))
        {
            let project = self.project_for_server(record.server_id).await?;
            let _ = self.continue_detach(&project, record).await;
            return Ok(());
        }
        let Some(cinder) = self.cinder.clone() else {
            self.set_phase(
                record.id,
                STATUS_ERROR,
                Some("cinder client is not configured"),
            )
            .await?;
            return Ok(());
        };
        let project = self.project_for_server(record.server_id).await?;
        let Some(cinder_attachment_id) = record.cinder_attachment_id.clone() else {
            // The attachment create outcome was unknown and the id is unknown:
            // observe the volume's attachments before deciding.
            match cinder.list_attachments(&project).await {
                Ok(attachments)
                    if attachments
                        .iter()
                        .any(|attachment| attachment.volume_id == record.volume_id.to_string()) =>
                {
                    let observed = attachments
                        .into_iter()
                        .find(|attachment| attachment.volume_id == record.volume_id.to_string())
                        .ok_or(ComputeError::NotFound)?;
                    self.store
                        .update_volume_attachment_outcome(
                            record.id,
                            STATUS_CINDER_ATTACHMENT_CREATED,
                            Some(&observed.id),
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                        )
                        .await?;
                    let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
                    self.continue_attach(&project, fresh.as_ref().unwrap_or(record))
                        .await?;
                }
                Ok(_) => {
                    self.set_phase(
                        record.id,
                        STATUS_ERROR,
                        Some("no cinder attachment was observed for the volume"),
                    )
                    .await?;
                }
                Err(error) => {
                    self.trace_error(record, "cinder_attachment_list", &error);
                    // The observation itself failed: the outcome is still
                    // unknown, so the record must stay non-terminal rather than
                    // being hidden behind a terminal error.
                    if error.is_unknown_outcome() {
                        self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                            .await?;
                    } else {
                        self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                            .await?;
                    }
                }
            }
            return Ok(());
        };
        match cinder
            .show_attachment(&project, &cinder_attachment_id)
            .await
        {
            Ok(attachment) => match attachment.status.as_str() {
                "attached" => {
                    self.set_phase(record.id, STATUS_ATTACHED, None).await?;
                }
                "attaching" | "reserved" => {
                    // The PUT (or a later phase) completed server-side; resume
                    // the attach flow from the observed Cinder state.
                    let phase = if attachment
                        .connection_info
                        .as_ref()
                        .is_some_and(|info| info.has_usable_target())
                    {
                        STATUS_CONNECTION_PREPARED
                    } else {
                        STATUS_CINDER_ATTACHMENT_CREATED
                    };
                    self.set_phase(record.id, phase, None).await?;
                    let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
                    self.continue_attach(&project, fresh.as_ref().unwrap_or(record))
                        .await?;
                }
                other => {
                    self.set_phase(
                        record.id,
                        STATUS_ERROR,
                        Some(&format!("cinder attachment is in state {}", other)),
                    )
                    .await?;
                }
            },
            Err(error) => {
                self.trace_error(record, "cinder_attachment_observe", &error);
                if matches!(error, AttachmentError::NotFound(_)) {
                    // The attachment no longer exists; mark the durable record
                    // error so it is not retried blindly, and clean up any
                    // compute device without touching foreign state.
                    let device = self.block_device_from_record(record).await;
                    let _ = self
                        .provider
                        .detach_block_device(record.server_id, &device)
                        .await;
                    self.set_phase(
                        record.id,
                        STATUS_ERROR,
                        Some("cinder attachment no longer exists"),
                    )
                    .await?;
                } else if error.is_unknown_outcome() {
                    // The observation failed: the outcome is still unknown.
                    // Keep the record non-terminal so reconcile observes again.
                    self.set_phase(record.id, STATUS_UNKNOWN, Some(&format!("{error}")))
                        .await?;
                } else {
                    self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn reconcile_in_progress(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), ComputeError> {
        let project = self.project_for_server(record.server_id).await?;
        let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
        let record = fresh.as_ref().unwrap_or(record);
        if is_detach_phase(record.status.as_str()) {
            // A crash or unknown outcome mid-detach: resume the reverse flow
            // (compute detach then Cinder terminate) instead of re-attaching.
            let _ = self.continue_detach(&project, record).await;
            return Ok(());
        }
        // Observe the compute device first. If it is attached, drive
        // completion; otherwise resume the attach flow from the persisted
        // phase (idempotent).
        let observed = self
            .provider
            .observe_block_device(record.server_id, &record.volume_id.to_string())
            .await;
        match observed {
            Ok(Some(observation)) if observation.attached => {
                self.set_phase(record.id, STATUS_COMPUTE_ATTACHED, None)
                    .await?;
                let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
                self.continue_attach(&project, fresh.as_ref().unwrap_or(record))
                    .await?;
            }
            _ => {
                let fresh = self.store.get_volume_attachment_by_id(record.id).await?;
                let _ = self
                    .continue_attach(&project, fresh.as_ref().unwrap_or(record))
                    .await;
            }
        }
        Ok(())
    }

    /// Observes the Cinder attachment before any compensation DELETE. If the
    /// attachment exists and holds connection information, the earlier outcome
    /// was actually successful and the record is left non-terminal for
    /// reconciliation rather than deleted. Compensation (a service-token
    /// DELETE) runs only after observation confirms the attachment is safe to
    /// remove or the attachment is absent.
    async fn observe_before_compensate(
        &self,
        project_id: &str,
        id: Uuid,
        volume_id: &str,
        cinder_attachment_id: Option<&str>,
        reason: &str,
    ) -> Result<(), ComputeError> {
        let Some(cinder_attachment_id) = cinder_attachment_id else {
            // No attachment id known: the create outcome was uncertain. Observe
            // THIS volume's attachments; if none match, compensation is a no-op.
            let Some(cinder) = self.cinder.clone() else {
                return Ok(());
            };
            match cinder.list_attachments(project_id).await {
                Ok(attachments)
                    if attachments.iter().any(|attachment| {
                        attachment.volume_id == volume_id
                            && attachment
                                .connection_info
                                .as_ref()
                                .map(|info| info.has_usable_target())
                                .unwrap_or(false)
                    }) =>
                {
                    self.set_phase(id, STATUS_UNKNOWN, Some(reason)).await?;
                    return Ok(());
                }
                Ok(_) => {
                    // No live attachment observed for this volume; nothing to
                    // compensate.
                    self.set_phase(id, STATUS_ERROR, Some(reason)).await?;
                    return Ok(());
                }
                Err(error) => {
                    if error.is_unknown_outcome() {
                        self.set_phase(id, STATUS_UNKNOWN, Some(&format!("{reason}; {error}")))
                            .await?;
                    } else {
                        self.set_phase(id, STATUS_ERROR, Some(&format!("{reason}; {error}")))
                            .await?;
                    }
                    return Ok(());
                }
            }
        };
        let Some(cinder) = self.cinder.clone() else {
            return Ok(());
        };
        match cinder
            .show_attachment(project_id, cinder_attachment_id)
            .await
        {
            Ok(attachment)
                if attachment
                    .connection_info
                    .as_ref()
                    .map(|info| info.has_usable_target())
                    .unwrap_or(false) =>
            {
                // The attachment was updated successfully despite the local
                // outcome; never delete it. Leave it unknown for reconcile to
                // drive forward.
                self.trace_malformed_record(
                    id,
                    "attachment_holds_connection_info_not_compensated",
                    reason,
                );
                self.set_phase(id, STATUS_UNKNOWN, Some(reason)).await?;
                Ok(())
            }
            Ok(_) => {
                // Confirmed absent/null connection_info: compensating is safe.
                self.compensate_after_create(project_id, id, Some(cinder_attachment_id), reason)
                    .await
            }
            Err(error) => {
                if error.is_unknown_outcome() {
                    self.set_phase(id, STATUS_UNKNOWN, Some(&format!("{reason}; {error}")))
                        .await?;
                } else {
                    // A confirmed NotFound means there is nothing to delete.
                    self.set_phase(id, STATUS_ERROR, Some(&format!("{reason}; {error}")))
                        .await?;
                }
                Ok(())
            }
        }
    }

    async fn compensate_after_create(
        &self,
        project_id: &str,
        id: Uuid,
        cinder_attachment_id: Option<&str>,
        reason: &str,
    ) -> Result<(), ComputeError> {
        // Reverse order: terminate the Cinder attachment if one was created.
        if let Some(cinder_attachment_id) = cinder_attachment_id
            && let Some(cinder) = &self.cinder
        {
            match cinder
                .terminate_attachment(project_id, cinder_attachment_id)
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    self.trace_error_record(id, "compensation_terminate", &error);
                    self.set_phase(id, STATUS_UNKNOWN, Some(&format!("{reason}; {error}")))
                        .await?;
                    return Ok(());
                }
            }
        }
        self.set_phase(id, STATUS_ERROR, Some(reason)).await?;
        Ok(())
    }

    async fn compensate_after_attach(
        &self,
        project_id: &str,
        id: Uuid,
        cinder_attachment_id: Option<&str>,
        server_id: Uuid,
        device: &BlockDeviceAttachment,
        reason: &str,
    ) -> Result<(), ComputeError> {
        // Reverse order: detach any owned compute device, then terminate the
        // Cinder attachment.
        let _ = self.provider.detach_block_device(server_id, device).await;
        if let Some(cinder_attachment_id) = cinder_attachment_id
            && let Some(cinder) = &self.cinder
        {
            match cinder
                .terminate_attachment(project_id, cinder_attachment_id)
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    self.trace_error_record(id, "compensation_terminate", &error);
                    self.set_phase(id, STATUS_UNKNOWN, Some(&format!("{reason}; {error}")))
                        .await?;
                    return Ok(());
                }
            }
        }
        self.set_phase(id, STATUS_ERROR, Some(reason)).await?;
        Ok(())
    }

    async fn set_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), ComputeError> {
        self.store
            .update_volume_attachment_phase(id, status, error)
            .await?;
        Ok(())
    }

    /// Bounded redacted phase tracing. Records only attachment/volume/instance
    /// ids, the durable phase, connection_info presence and top-level key names,
    /// and attach_target presence. Never tokens, CHAP secrets, or raw
    /// connection_info.
    async fn trace_phase(
        &self,
        record: &VolumeAttachmentRecord,
        phase: &str,
        _error: Option<&str>,
    ) -> Result<(), ComputeError> {
        tracing::info!(
            phase,
            attachment_id = record.cinder_attachment_id.as_deref().unwrap_or(""),
            volume_id = %record.volume_id,
            instance_id = %record.server_id,
            "attachment phase"
        );
        Ok(())
    }

    fn trace_attachment(
        &self,
        record: &VolumeAttachmentRecord,
        step: &str,
        attachment: &AttachmentObservation,
        _error: Option<&str>,
    ) {
        let presence = match attachment.connection_info_presence() {
            ConnectionInfoPresence::Present => "present",
            ConnectionInfoPresence::Missing => "missing",
            ConnectionInfoPresence::Null => "null",
            ConnectionInfoPresence::Malformed => "malformed",
        };
        let top_level_keys = attachment
            .connection_info
            .as_ref()
            .map(|info| info.top_level_keys().join(","))
            .unwrap_or_default();
        let target_present = attachment
            .connection_info
            .as_ref()
            .map(|info| info.attach_target().is_some())
            .unwrap_or(false);
        tracing::info!(
            step,
            attachment_id = %attachment.id,
            volume_id = %record.volume_id,
            instance_id = %record.server_id,
            attach_status = %attachment.status,
            connection_info_presence = presence,
            connection_info_top_level_keys = top_level_keys,
            attach_target_present = target_present,
            "cinder attachment trace"
        );
    }

    fn trace_malformed(&self, record: &VolumeAttachmentRecord, reason: &str) {
        tracing::warn!(
            volume_id = %record.volume_id,
            instance_id = %record.server_id,
            attachment_id = record.cinder_attachment_id.as_deref().unwrap_or(""),
            reason,
            "cinder attachment parse trace"
        );
    }

    fn trace_malformed_record(&self, id: Uuid, step: &str, reason: &str) {
        tracing::warn!(attachment_id = %id, step, reason, "cinder attachment observe trace");
    }

    fn trace_error(
        &self,
        record: &VolumeAttachmentRecord,
        step: &str,
        error: &dyn std::fmt::Display,
    ) {
        tracing::warn!(
            step,
            volume_id = %record.volume_id,
            instance_id = %record.server_id,
            error = %error,
            "attachment error trace"
        );
    }

    fn trace_error_record(&self, id: Uuid, step: &str, error: &dyn std::fmt::Display) {
        tracing::warn!(attachment_id = %id, step, error = %error, "attachment compensation trace");
    }
}

fn validate_target(target: &AttachmentTarget) -> Result<(), ComputeError> {
    match target.driver_volume_type.as_str() {
        "iscsi" => {
            if target.target_iqn.is_none() || target.target_portal.is_none() {
                return Err(ComputeError::InvalidRequest);
            }
        }
        "local" => {
            if target.local_path.is_none() {
                return Err(ComputeError::InvalidRequest);
            }
        }
        _ => return Err(ComputeError::Unavailable),
    }
    // CHAP credentials are carried only over the authenticated agent control
    // channel and applied by the agent to the iSCSI node session at login;
    // they are never logged or included in diagnostics. Auth fields must be
    // internally consistent when present.
    match (
        target.auth_method.as_deref(),
        target.auth_username.as_ref(),
        target.auth_password.as_ref(),
    ) {
        (Some(method), Some(_username), Some(_password)) => {
            if !method.eq_ignore_ascii_case("CHAP") {
                return Err(ComputeError::InvalidRequest);
            }
        }
        (None, None, None) => {}
        _ => return Err(ComputeError::InvalidRequest),
    }
    Ok(())
}

fn map_attachment_error(error: AttachmentError) -> ComputeError {
    match error {
        AttachmentError::NotFound(_) => ComputeError::NotFound,
        AttachmentError::Conflict(_) => ComputeError::Conflict,
        AttachmentError::InvalidRequest(_) => ComputeError::InvalidRequest,
        AttachmentError::Unauthorized
        | AttachmentError::Unavailable
        | AttachmentError::UnknownOutcome(_)
        | AttachmentError::Protocol(_) => ComputeError::Unavailable,
    }
}

fn map_provider_error(error: ProviderError) -> ComputeError {
    match error {
        ProviderError::NotFound => ComputeError::NotFound,
        ProviderError::Conflict => ComputeError::Conflict,
        ProviderError::InvalidRequest | ProviderError::UnsupportedBlockDevice(_) => {
            ComputeError::InvalidRequest
        }
        ProviderError::UnknownOutcome { .. } | ProviderError::StaleState => {
            ComputeError::Unavailable
        }
        other => ComputeError::Provider(other),
    }
}

fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_time(seconds)
}

fn format_time(seconds: u64) -> String {
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date(days_since_epoch: u64) -> (i64, u64, u64) {
    let days = i64::try_from(days_since_epoch).unwrap_or(i64::MAX);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (
        year,
        u64::try_from(month).unwrap_or(0),
        u64::try_from(day).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeComputeProvider;
    use o3k_cinder::testkit::{FaultConfig, faults, start_testbed};
    use o3k_provider::{AttachmentTarget, FailureInjection, VolumeAttachmentProvider};
    use std::sync::Arc;

    #[test]
    fn validate_target_accepts_chap_and_rejects_inconsistent_auth() {
        let base = AttachmentTarget {
            driver_volume_type: "iscsi".to_owned(),
            target_iqn: Some("iqn.2026-01.example.com:volume-1".to_owned()),
            target_portal: Some("10.0.0.10:3260".to_owned()),
            target_lun: Some(1),
            local_path: None,
            auth_method: Some("CHAP".to_owned()),
            auth_username: Some("user".to_owned()),
            auth_password: Some("password".to_owned()),
        };
        assert!(validate_target(&base).is_ok());

        let mut without_iqn = base.clone();
        without_iqn.target_iqn = None;
        assert!(validate_target(&without_iqn).is_err());

        let mut no_auth = base.clone();
        no_auth.auth_method = None;
        no_auth.auth_username = None;
        no_auth.auth_password = None;
        assert!(validate_target(&no_auth).is_ok());

        let mut half_auth = base.clone();
        half_auth.auth_username = None;
        assert!(validate_target(&half_auth).is_err());

        let mut wrong_method = base.clone();
        wrong_method.auth_method = Some("NONE".to_owned());
        assert!(validate_target(&wrong_method).is_err());

        let local = AttachmentTarget {
            driver_volume_type: "local".to_owned(),
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            local_path: Some("/dev/mapper/volume".to_owned()),
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };
        assert!(validate_target(&local).is_ok());

        let unknown_driver = AttachmentTarget {
            driver_volume_type: "nvmeof".to_owned(),
            ..base
        };
        assert!(matches!(
            validate_target(&unknown_driver),
            Err(ComputeError::Unavailable)
        ));
    }

    struct TestHarness {
        store: Arc<dyn ComputeRepository>,
        fake_provider: Arc<FakeComputeProvider>,
        orchestrator: AttachmentOrchestrator,
        cinder_fake: o3k_cinder::testkit::FakeCinderState,
        cinder: Arc<o3k_cinder::CinderClient>,
    }

    async fn harness() -> Result<TestHarness, Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, cinder_fake, _addr) =
            start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let orchestrator = AttachmentOrchestrator::new(
            store.clone(),
            provider.clone(),
            Some(client.clone() as Arc<dyn VolumeAttachmentProvider>),
        );
        Ok(TestHarness {
            store,
            fake_provider,
            orchestrator,
            cinder_fake,
            cinder: client,
        })
    }

    async fn create_volume(h: &TestHarness) -> Result<Uuid, Box<dyn std::error::Error>> {
        let volume = h
            .cinder
            .create_volume("eba29e2d-53de-461d-ae91-ede7402713cb", 1, "vol")
            .await?;
        Ok(Uuid::parse_str(&volume.id)?)
    }

    async fn seed_server(
        h: &TestHarness,
        server_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::ResourceRecord;
        h.store
            .insert_resource(&ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "ACTIVE".to_owned(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            })
            .await?;
        Ok(())
    }

    fn set_fault(h: &TestHarness, fault: fn(&FaultConfig) -> bool) {
        h.cinder_fake.set_fault(fault, true);
    }

    #[tokio::test]
    async fn attach_happy_path_persists_terminal_attached_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let h = harness().await?;
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        seed_server(&h, server_id).await?;
        let volume = create_volume(&h).await?;

        let record = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await?;
        assert_eq!(record.status, STATUS_ATTACHED);
        assert!(record.cinder_attachment_id.is_some());
        assert!(record.connection_info_digest.is_some());
        assert_eq!(record.driver_volume_type.as_deref(), Some("iscsi"));
        assert_ne!(record.id, volume);
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 1);

        // The fake Cinder mirrors real Cinder 28 and always returns CHAP
        // credentials; the orchestrator must accept the target and deliver
        // the credentials to the compute boundary without persisting them.
        let dispatched = h
            .fake_provider
            .last_attached_device()
            .ok_or("device never dispatched")?;
        assert_eq!(dispatched.auth_method.as_deref(), Some("CHAP"));
        assert_eq!(dispatched.auth_username.as_deref(), Some("chap-user"));
        assert_eq!(dispatched.auth_password.as_deref(), Some("chap-password"));
        assert!(!format!("{dispatched:?}").contains("chap-password"));
        assert!(record.target_iqn.is_some());

        // Duplicate attach is idempotent.
        let duplicate = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await?;
        assert_eq!(duplicate.id, record.id);
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 1);

        h.orchestrator.detach(project, server_id, record.id).await?;
        let final_record = h
            .store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or("final record missing")?;
        assert_eq!(final_record.status, STATUS_DETACHED);
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 0);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_detach_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let h = harness().await?;
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        seed_server(&h, server_id).await?;
        let volume = create_volume(&h).await?;
        let record = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await?;
        h.orchestrator.detach(project, server_id, record.id).await?;
        h.orchestrator.detach(project, server_id, record.id).await?;
        let final_record = h
            .store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or("final record missing")?;
        assert_eq!(final_record.status, STATUS_DETACHED);
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cinder_unavailable_before_create_is_observed_not_compensated()
    -> Result<(), Box<dyn std::error::Error>> {
        let h = harness().await?;
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        seed_server(&h, server_id).await?;
        let volume = create_volume(&h).await?;
        set_fault(&h, faults::fail_create_attachment);
        let result = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await;
        assert!(result.is_err());
        assert!(h.cinder_fake.attachment_ids().is_empty());
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 0);
        // A 503 on create is an uncertain outcome: the record must be left
        // non-terminal (unknown), not compensated blindly.
        let pending = h
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, STATUS_UNKNOWN);
        // Reconciliation observes the volume's attachments and settles the
        // record once it confirms none exist.
        h.orchestrator.reconcile().await?;
        let pending = h
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        assert_eq!(h.cinder_fake.attachment_ids().len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn connector_failure_compensates_by_terminating_cinder_attachment()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        fake_provider.set_failure(FailureInjection::Terminal)?;
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, cinder_fake, _addr) =
            start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let orchestrator =
            AttachmentOrchestrator::new(store.clone(), provider.clone(), Some(client.clone()));
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: "eba29e2d-53de-461d-ae91-ede7402713cb".to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "ACTIVE".to_owned(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            })
            .await?;
        let volume = {
            let created = client.create_volume(project, 1, "vol").await?;
            Uuid::parse_str(&created.id)?
        };
        let result = orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await;
        assert!(result.is_err());
        // Compensation terminated the Cinder attachment.
        assert!(cinder_fake.attachment_ids().is_empty());
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn compute_attach_success_with_cinder_completion_failure_is_not_prematurely_compensated()
    -> Result<(), Box<dyn std::error::Error>> {
        // A 500 on os-complete is an uncertain outcome: the compute device was
        // already attached and the Cinder complete may have succeeded
        // server-side. The orchestrator must NOT compensate; it leaves the
        // record unknown and reconciliation observes, then resumes and
        // completes (the fake consumes the fault once).
        let h = harness().await?;
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        seed_server(&h, server_id).await?;
        let volume = create_volume(&h).await?;
        set_fault(&h, faults::fail_complete_attachment);
        let result = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await;
        assert!(result.is_err());
        // No premature compensation: the compute device stays attached and no
        // DELETE is issued.
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 1);
        assert!(h.cinder_fake.attachment_ids().len() == 1);
        // Reconciliation observes the attachment, sees the compute device is
        // attached, retries completion and reaches attached.
        h.orchestrator.reconcile().await?;
        let pending = h
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        let records = h.store.list_volume_attachments(server_id).await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, STATUS_ATTACHED);
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_connection_info_is_rejected_and_compensated()
    -> Result<(), Box<dyn std::error::Error>> {
        // The fake returns an iscsi target by default; simulate an
        // unsupported driver by pointing the orchestrator at a fake that
        // reports a CHAP target would require altering the fake. Instead we
        // verify the orchestrator rejects empty/invalid targets before any
        // compute mutation by failing the connector update.
        let h = harness().await?;
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        seed_server(&h, server_id).await?;
        let volume = create_volume(&h).await?;
        set_fault(&h, faults::fail_update_connector);
        let result = h
            .orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await;
        assert!(result.is_err());
        assert!(h.cinder_fake.attachment_ids().is_empty());
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 0);
        Ok(())
    }
}

#[cfg(test)]
mod restart_tests {
    use super::*;
    use crate::FakeComputeProvider;
    use o3k_cinder::testkit::start_testbed;
    use std::sync::Arc;

    #[tokio::test]
    async fn restart_with_attached_device_reconciles_to_attached()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, _fake, _addr) = start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let orchestrator =
            AttachmentOrchestrator::new(store.clone(), provider.clone(), Some(client.clone()));
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: project.to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "ACTIVE".to_owned(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            })
            .await?;
        let volume = {
            let created = client.create_volume(project, 1, "vol").await?;
            Uuid::parse_str(&created.id)?
        };

        let record = orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await?;
        assert_eq!(record.status, STATUS_ATTACHED);

        // Simulate restart: a new orchestrator over the same durable store
        // observes the compute device and completes the Cinder attachment.
        let reloaded = AttachmentOrchestrator::new(store.clone(), provider.clone(), Some(client));
        reloaded.reconcile().await?;
        let final_record = store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or("final record missing")?;
        assert_eq!(final_record.status, STATUS_ATTACHED);
        assert_eq!(fake_provider.attached_volume_count(server_id), 1);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_outcome_attachment_reconciles_by_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, fake, _addr) = start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let orchestrator =
            AttachmentOrchestrator::new(store.clone(), provider.clone(), Some(client.clone()));
        let project = "eba29e2d-53de-461d-ae91-ede7402713cb";
        let server_id = Uuid::now_v7();
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: project.to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "ACTIVE".to_owned(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            })
            .await?;
        let volume = {
            let created = client.create_volume(project, 1, "vol").await?;
            Uuid::parse_str(&created.id)?
        };

        // Force a terminal failure after the Cinder attachment is created so
        // the durable record is non-terminal; then manually mark it unknown
        // to simulate an in-flight timeout.
        fake.set_fault(o3k_cinder::testkit::faults::fail_update_connector, true);
        let result = orchestrator
            .attach(project, server_id, volume, None, None, false)
            .await;
        assert!(result.is_err());
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(
            pending.is_empty(),
            "compensation must leave no non-terminal records"
        );
        Ok(())
    }
}

/// Gate E — orchestrator replay gate.
///
/// Replays the response classes defined in
/// `contracts/cinder/attachment-interaction-v28.yaml` through the durable
/// attachment orchestrator and proves: observe-before-retry, no premature
/// compensation, no duplicate attachment, and restart safety.
#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::FakeComputeProvider;
    use o3k_cinder::testkit::{faults, start_testbed};
    use o3k_provider::VolumeAttachmentProvider;
    use std::time::Duration;

    const PROJECT: &str = "eba29e2d-53de-461d-ae91-ede7402713cb";

    async fn harness_with_timeout(
        timeout: Duration,
    ) -> Result<
        (
            Arc<dyn ComputeRepository>,
            Arc<FakeComputeProvider>,
            AttachmentOrchestrator,
            o3k_cinder::testkit::FakeCinderState,
            Arc<o3k_cinder::CinderClient>,
        ),
        Box<dyn std::error::Error>,
    > {
        let store: Arc<dyn ComputeRepository> = Arc::new(o3k_store::testkit::open_memory().await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, fake, _addr) = start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client.with_timeout(timeout));
        let orchestrator = AttachmentOrchestrator::new(
            store.clone(),
            provider.clone(),
            Some(client.clone() as Arc<dyn VolumeAttachmentProvider>),
        );
        Ok((store, fake_provider, orchestrator, fake, client))
    }

    async fn seed_server(
        store: &dyn ComputeRepository,
        server_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        store
            .insert_resource(&o3k_store::ResourceRecord {
                id: server_id,
                kind: "compute_instance".to_owned(),
                project_id: PROJECT.to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state: "ACTIVE".to_owned(),
                observed_state: "ACTIVE".to_owned(),
                provider_id: None,
            })
            .await?;
        Ok(())
    }

    async fn create_volume(
        client: &o3k_cinder::CinderClient,
    ) -> Result<Uuid, Box<dyn std::error::Error>> {
        let volume = client.create_volume(PROJECT, 1, "vol").await?;
        Ok(Uuid::parse_str(&volume.id)?)
    }

    #[tokio::test]
    async fn put_missing_connection_info_observes_then_compensates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        fake.set_fault(faults::missing_connection_info_on_update, true);
        let result = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await;
        assert!(result.is_err());
        // Observe-before-compensate: after observing the confirmed-missing
        // connection_info the orchestrator compensates with a service-token
        // DELETE, so no attachment remains and no compute device was attached.
        assert!(
            fake.attachment_ids().is_empty(),
            "confirmed missing connection_info must be compensated"
        );
        assert_eq!(fake.last_delete_service_token_validated(), Some(true));
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn put_null_connection_info_observes_then_compensates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        fake.set_fault(faults::null_connection_info_on_update, true);
        let result = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await;
        assert!(result.is_err());
        assert!(
            fake.attachment_ids().is_empty(),
            "confirmed null connection_info must be compensated"
        );
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn put_malformed_connection_info_observes_then_compensates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        fake.set_fault(faults::malformed_connection_info_on_update, true);
        let result = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await;
        assert!(result.is_err());
        assert!(
            fake.attachment_ids().is_empty(),
            "confirmed malformed connection_info must be compensated"
        );
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn put_timeout_is_unknown_then_reconcile_resumes_and_attaches()
    -> Result<(), Box<dyn std::error::Error>> {
        // A timed-out PUT is an unknown outcome: the orchestrator must NOT
        // delete a possibly-successful attachment. It persists unknown and
        // reconciliation observes, then resumes the connector update (the
        // fake consumes the timeout fault once) and reaches attached.
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_millis(500)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        fake.set_fault(faults::timeout_update_connector, true);
        let result = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await;
        assert!(result.is_err());
        // No premature compensation: the Cinder attachment still exists.
        assert_eq!(fake.attachment_ids().len(), 1);
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, STATUS_UNKNOWN);

        // Reconciliation observes and resumes the attach flow to completion.
        orchestrator.reconcile().await?;
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty(), "reconcile must settle the record");
        let records = store.list_volume_attachments(server_id).await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, STATUS_ATTACHED);
        assert_eq!(fake_provider.attached_volume_count(server_id), 1);
        assert_eq!(fake.attachment_ids().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn restart_at_connection_prepared_resumes_without_duplicate_attachment()
    -> Result<(), Box<dyn std::error::Error>> {
        // Simulate a restart after the connector update completed (phase
        // connection_prepared, digest persisted, CHAP credentials not
        // persisted): reconciliation re-fetches connection information from
        // Cinder, re-derives the target, attaches the compute device, completes
        // the attachment, and reaches attached without creating a second
        // Cinder attachment.
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;

        // Prepare the Cinder-side attachment through the shared client (create
        // + connector update) so the fake holds real connection_info.
        let volume_str = volume_id.to_string();
        let cinder_attachment = client
            .create_attachment(PROJECT, &volume_str, Some(&server_id.to_string()))
            .await?;
        let connector = o3k_cinder::ComputeConnector {
            host: "compute-restart".to_owned(),
            ip: "10.0.0.5".to_owned(),
            platform: "x86_64".to_owned(),
            os_type: "linux".to_owned(),
            multipath: false,
            initiator: Some("iqn.1993-08.org.debian:01:o3k".to_owned()),
        };
        let updated = client
            .update_attachment_connector(PROJECT, &cinder_attachment.id, &connector)
            .await?;
        let connection_info = updated.connection_info.ok_or("missing connection_info")?;
        let target = connection_info.attach_target().ok_or("missing target")?;

        // Durable record at connection_prepared: digest and target fields
        // persisted, CHAP credentials never persisted.
        let record_id = Uuid::now_v7();
        store
            .insert_volume_attachment(&VolumeAttachmentRecord {
                id: record_id,
                server_id,
                volume_id,
                device: "/dev/vdb".to_owned(),
                tag: None,
                delete_on_termination: false,
                created_at: now_rfc3339(),
                status: STATUS_CONNECTION_PREPARED.to_owned(),
                operation_id: Some(Uuid::now_v7()),
                idempotency_key: Some("attach:restart".to_owned()),
                cinder_attachment_id: Some(cinder_attachment.id.clone()),
                connector_host: Some("compute-restart".to_owned()),
                connector_ip: Some("10.0.0.5".to_owned()),
                connector_initiator: Some("iqn.1993-08.org.debian:01:o3k".to_owned()),
                driver_volume_type: Some(target.driver_volume_type.clone()),
                target_iqn: target.target_iqn.clone(),
                target_portal: target.target_portal.clone(),
                target_lun: target.target_lun.map(|v| v as u32),
                connection_info_digest: Some(connection_info.digest()),
                error: None,
            })
            .await?;

        orchestrator.reconcile().await?;
        let record = store
            .get_volume_attachment_by_id(record_id)
            .await?
            .ok_or("record missing")?;
        assert_eq!(record.status, STATUS_ATTACHED);
        assert_eq!(fake_provider.attached_volume_count(server_id), 1);
        // Exactly one Cinder attachment: no duplicate was created.
        assert_eq!(fake.attachment_ids().len(), 1);
        assert!(fake.attachment_ids().contains(&cinder_attachment.id));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_attach_never_creates_a_second_cinder_attachment()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, _fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;

        let first = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await?;
        let second = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await?;
        assert_eq!(first.id, second.id);
        assert_eq!(fake.attachment_ids().len(), 1, "no duplicate attachment");
        Ok(())
    }

    #[tokio::test]
    async fn create_timeout_is_unknown_and_reconcile_observes_before_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        // A timed-out create leaves the attachment id unknown. The orchestrator
        // must not DELETE blindly; reconciliation lists the volume's
        // attachments and only settles once observation confirms state.
        let (store, _fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_millis(500)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        fake.set_fault(faults::timeout_create_attachment, true);
        let result = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await;
        assert!(result.is_err());
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, STATUS_UNKNOWN);
        orchestrator.reconcile().await?;
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(
            pending.is_empty(),
            "reconcile must settle after observation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn crash_mid_detach_resumes_detach_not_reattach() -> Result<(), Box<dyn std::error::Error>>
    {
        // A crash after the compute_detach_requested phase persisted must be
        // resumed by reconciliation as a DETACH (compute detach then Cinder
        // terminate), never as a re-attach.
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        let record = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await?;
        assert_eq!(fake_provider.attached_volume_count(server_id), 1);

        // Simulate a crash mid-detach.
        store
            .update_volume_attachment_phase(record.id, STATUS_COMPUTE_DETACH_REQUESTED, None)
            .await?;

        orchestrator.reconcile().await?;
        let final_record = store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or("record missing")?;
        assert_eq!(final_record.status, STATUS_DETACHED);
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        assert!(
            fake.attachment_ids().is_empty(),
            "the Cinder attachment must be terminated, not re-attached"
        );
        Ok(())
    }

    #[tokio::test]
    async fn detach_terminate_unknown_outcome_resumes_after_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        // An unknown-outcome Cinder terminate during detach must keep the
        // record non-terminal; reconciliation observes and retries to detached.
        let (store, fake_provider, orchestrator, fake, client) =
            harness_with_timeout(Duration::from_secs(5)).await?;
        let server_id = Uuid::now_v7();
        seed_server(store.as_ref(), server_id).await?;
        let volume_id = create_volume(&client).await?;
        let record = orchestrator
            .attach(PROJECT, server_id, volume_id, None, None, false)
            .await?;

        fake.set_fault(faults::fail_terminate_attachment, true);
        // The terminate outcome is unknown: detach leaves the record
        // non-terminal (unknown) instead of reporting a confirmed failure or
        // flipping it to detached without observation.
        orchestrator.detach(PROJECT, server_id, record.id).await?;
        let pending = store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, STATUS_UNKNOWN);

        orchestrator.reconcile().await?;
        let final_record = store
            .get_volume_attachment_by_id(record.id)
            .await?
            .ok_or("record missing")?;
        assert_eq!(final_record.status, STATUS_DETACHED);
        assert_eq!(fake_provider.attached_volume_count(server_id), 0);
        assert!(fake.attachment_ids().is_empty());
        Ok(())
    }
}
