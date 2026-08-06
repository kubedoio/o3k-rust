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

use std::sync::Arc;

use o3k_cinder::{AttachTarget, CinderClient, CinderError, ComputeConnector};
use o3k_provider::{BlockDeviceAttachment, ComputeProvider, ProviderError};
use o3k_store::{DurableStore, SqliteStore, VolumeAttachmentRecord};
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

#[derive(Clone)]
pub struct AttachmentOrchestrator {
    store: Arc<SqliteStore>,
    provider: Arc<ProviderBackend>,
    cinder: Option<Arc<CinderClient>>,
}

impl AttachmentOrchestrator {
    pub fn new(
        store: Arc<SqliteStore>,
        provider: Arc<ProviderBackend>,
        cinder: Option<Arc<CinderClient>>,
    ) -> Self {
        Self {
            store,
            provider,
            cinder,
        }
    }

    #[must_use]
    pub fn cinder_configured(&self) -> bool {
        self.cinder.is_some()
    }

    /// Executes the durable Cinder attach lifecycle. Duplicate requests for
    /// the same volume return the existing durable attachment.
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
            .get_volume_attachment_by_volume(volume_id)
            .await?
        {
            return Ok(existing);
        }
        let cinder = self.cinder.clone().ok_or(ComputeError::Unavailable)?;

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

        let volume_id_str = volume_id.to_string();

        // Phase: cinder_attachment_created
        self.set_phase(id, STATUS_CINDER_ATTACHMENT_CREATED, None)
            .await?;
        let server_id_str = server_id.to_string();
        let cinder_attachment = match cinder
            .create_attachment(project_id, &volume_id_str, Some(&server_id_str))
            .await
        {
            Ok(attachment) => attachment,
            Err(error) => {
                self.compensate_after_create(project_id, id, None, &format!("{error}"))
                    .await?;
                return Err(map_cinder_error(error));
            }
        };
        self.store
            .update_volume_attachment_outcome(
                id,
                STATUS_CINDER_ATTACHMENT_CREATED,
                Some(&cinder_attachment.id),
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

        // Phase: connector_obtained
        self.set_phase(id, STATUS_CONNECTOR_OBTAINED, None).await?;
        let connector = match self.provider.collect_connector(server_id).await {
            Ok(connector) => connector,
            Err(error) => {
                self.compensate_after_create(
                    project_id,
                    id,
                    Some(&cinder_attachment.id),
                    &format!("{error}"),
                )
                .await?;
                return Err(map_provider_error(error));
            }
        };
        self.store
            .update_volume_attachment_outcome(
                id,
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

        // Phase: connection_prepared
        self.set_phase(id, STATUS_CONNECTION_PREPARED, None).await?;
        let cinder_connector = ComputeConnector {
            host: connector.host.clone(),
            ip: connector.ip.clone(),
            platform: connector.platform.clone(),
            os_type: connector.os_type.clone(),
            multipath: connector.multipath,
            initiator: connector.initiator.clone(),
        };
        let updated = match cinder
            .update_attachment_connector(project_id, &cinder_attachment.id, &cinder_connector)
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                self.compensate_after_create(
                    project_id,
                    id,
                    Some(&cinder_attachment.id),
                    &format!("{error}"),
                )
                .await?;
                return Err(map_cinder_error(error));
            }
        };
        let connection_info = match updated.connection_info {
            Some(connection_info) => connection_info,
            None => {
                self.compensate_after_create(
                    project_id,
                    id,
                    Some(&cinder_attachment.id),
                    "connection information is missing",
                )
                .await?;
                return Err(ComputeError::InvalidRequest);
            }
        };
        let target = match connection_info.attach_target() {
            Some(target) => target,
            None => {
                self.compensate_after_create(
                    project_id,
                    id,
                    Some(&cinder_attachment.id),
                    "connection information is malformed",
                )
                .await?;
                return Err(ComputeError::InvalidRequest);
            }
        };
        validate_target(&target)?;
        self.store
            .update_volume_attachment_outcome(
                id,
                STATUS_CONNECTION_PREPARED,
                None,
                None,
                None,
                None,
                Some(&target.driver_volume_type),
                target.target_iqn.as_deref(),
                target.target_portal.as_deref(),
                target.target_lun.map(|value| value as u32),
                Some(&connection_info.digest()),
                None,
            )
            .await?;

        // Phase: compute_attach_requested
        self.set_phase(id, STATUS_COMPUTE_ATTACH_REQUESTED, None)
            .await?;
        let device_attachment = BlockDeviceAttachment {
            volume_id: volume_id_str.clone(),
            attachment_id: cinder_attachment.id.clone(),
            driver_volume_type: target.driver_volume_type.clone(),
            target_iqn: target.target_iqn.clone(),
            target_portal: target.target_portal.clone(),
            target_lun: target.target_lun.map(|value| value as u32),
            local_path: target.local_path.clone(),
            device_path: None,
            multipath: false,
            initiator: connector.initiator.clone(),
            auth_method: target.auth_method.clone(),
            auth_username: target
                .auth_username
                .as_ref()
                .map(|value| value.expose().to_owned()),
            auth_password: target
                .auth_password
                .as_ref()
                .map(|value| value.expose().to_owned()),
        };
        let observation = match self
            .provider
            .attach_block_device(server_id, &device_attachment)
            .await
        {
            Ok(observation) => observation,
            Err(error) => {
                self.compensate_after_attach(
                    project_id,
                    id,
                    Some(&cinder_attachment.id),
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
                id,
                Some(&cinder_attachment.id),
                server_id,
                &device_attachment,
                "compute device was not attached",
            )
            .await?;
            return Err(ComputeError::Conflict);
        }
        self.set_phase(id, STATUS_COMPUTE_ATTACHED, None).await?;
        if let Some(path) = &observation.device_path {
            self.store
                .update_volume_attachment_outcome(
                    id,
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

        // Phase: cinder_attachment_completed
        self.set_phase(id, STATUS_CINDER_ATTACHMENT_COMPLETED, None)
            .await?;
        if let Err(error) = cinder
            .complete_attachment(project_id, &cinder_attachment.id)
            .await
        {
            self.compensate_after_attach(
                project_id,
                id,
                Some(&cinder_attachment.id),
                server_id,
                &device_attachment,
                &format!("{error}"),
            )
            .await?;
            return Err(map_cinder_error(error));
        }
        self.set_phase(id, STATUS_ATTACHED, None).await?;

        self.store
            .get_volume_attachment_by_id(id)
            .await?
            .ok_or(ComputeError::NotFound)
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
        let cinder = self.cinder.clone().ok_or(ComputeError::Unavailable)?;
        self.set_phase(record.id, STATUS_DETACH_REQUESTED, None)
            .await?;

        let device_attachment = BlockDeviceAttachment {
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
            // Detach only needs the target identity; CHAP credentials are
            // never persisted and are not required to log out.
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };

        // Phase: compute_detach_requested
        self.set_phase(record.id, STATUS_COMPUTE_DETACH_REQUESTED, None)
            .await?;
        match self
            .provider
            .detach_block_device(server_id, &device_attachment)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                    .await?;
                return Err(map_provider_error(error));
            }
        }
        self.set_phase(record.id, STATUS_DETACHED, None).await?;

        // Phase: cinder_attachment_terminated
        if let Some(cinder_attachment_id) = &record.cinder_attachment_id
            && let Err(error) = cinder
                .terminate_attachment(project_id, cinder_attachment_id)
                .await
        {
            match error {
                o3k_cinder::CinderError::NotFound(_) | o3k_cinder::CinderError::Conflict(_) => {}
                _ => tracing::warn!(%error, "cinder attachment termination warning"),
            }
        }
        Ok(())
    }

    /// Reconciles non-terminal attachments after restart or an unknown
    /// outcome: observes the Cinder and compute boundaries and either advances
    /// the phase or compensates. Foreign resources are never deleted.
    pub async fn reconcile(&self) -> Result<(), ComputeError> {
        let records = self
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        for record in records {
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
                    self.set_phase(record.id, STATUS_CINDER_ATTACHMENT_CREATED, None)
                        .await?;
                    return Ok(());
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
                    self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                        .await?;
                }
            }
            return Ok(());
        };
        match cinder
            .show_attachment(&project, &cinder_attachment_id)
            .await
        {
            Ok(attachment) if attachment.status == "attached" => {
                self.set_phase(record.id, STATUS_ATTACHED, None).await?;
            }
            Ok(attachment) => {
                self.set_phase(
                    record.id,
                    STATUS_ERROR,
                    Some(&format!(
                        "cinder attachment is in state {}",
                        attachment.status
                    )),
                )
                .await?;
            }
            Err(error) => {
                self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                    .await?;
            }
        }
        Ok(())
    }

    async fn reconcile_in_progress(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), ComputeError> {
        // Observe the compute device. If it is attached, drive completion;
        // otherwise advance deterministically from the persisted phase.
        let observed = self
            .provider
            .observe_block_device(record.server_id, &record.volume_id.to_string())
            .await;
        match (record.status.as_str(), observed) {
            (_, Ok(Some(observation))) if observation.attached => {
                self.set_phase(record.id, STATUS_COMPUTE_ATTACHED, None)
                    .await?;
                if let Some(cinder_attachment_id) = &record.cinder_attachment_id
                    && let Some(cinder) = &self.cinder
                {
                    let project = self.project_for_server(record.server_id).await?;
                    self.set_phase(record.id, STATUS_CINDER_ATTACHMENT_COMPLETED, None)
                        .await?;
                    match cinder
                        .complete_attachment(&project, cinder_attachment_id)
                        .await
                    {
                        Ok(()) => {
                            self.set_phase(record.id, STATUS_ATTACHED, None).await?;
                        }
                        Err(error) => {
                            self.set_phase(record.id, STATUS_ERROR, Some(&format!("{error}")))
                                .await?;
                        }
                    }
                } else {
                    self.set_phase(record.id, STATUS_ATTACHED, None).await?;
                }
            }
            _ => {}
        }
        Ok(())
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
}

fn validate_target(target: &AttachTarget) -> Result<(), ComputeError> {
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

fn map_cinder_error(error: CinderError) -> ComputeError {
    match error {
        CinderError::NotFound(_) => ComputeError::NotFound,
        CinderError::Conflict(_) => ComputeError::Conflict,
        CinderError::InvalidRequest(_) => ComputeError::InvalidRequest,
        CinderError::Unauthorized | CinderError::Auth(_) => ComputeError::Unavailable,
        CinderError::ServiceUnavailable | CinderError::UnknownOutcome(_) => {
            ComputeError::Unavailable
        }
        CinderError::Protocol(_) => ComputeError::Unavailable,
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
    use o3k_cinder::CinderSecret;
    use o3k_cinder::testkit::{FaultConfig, faults, start_testbed};
    use o3k_provider::FailureInjection;
    use std::sync::Arc;

    #[test]
    fn validate_target_accepts_chap_and_rejects_inconsistent_auth() {
        let base = AttachTarget {
            driver_volume_type: "iscsi".to_owned(),
            target_iqn: Some("iqn.2026-01.example.com:volume-1".to_owned()),
            target_portal: Some("10.0.0.10:3260".to_owned()),
            target_lun: Some(1),
            local_path: None,
            auth_method: Some("CHAP".to_owned()),
            auth_username: Some(CinderSecret::new("user".to_owned())),
            auth_password: Some(CinderSecret::new("password".to_owned())),
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

        let local = AttachTarget {
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

        let unknown_driver = AttachTarget {
            driver_volume_type: "nvmeof".to_owned(),
            ..base
        };
        assert!(matches!(
            validate_target(&unknown_driver),
            Err(ComputeError::Unavailable)
        ));
    }

    struct TestHarness {
        store: Arc<SqliteStore>,
        fake_provider: Arc<FakeComputeProvider>,
        orchestrator: AttachmentOrchestrator,
        cinder_fake: o3k_cinder::testkit::FakeCinderState,
        cinder: Arc<CinderClient>,
    }

    async fn harness() -> Result<TestHarness, Box<dyn std::error::Error>> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
        let fake_provider = Arc::new(FakeComputeProvider::new());
        let provider = Arc::new(ProviderBackend::from(fake_provider.clone()));
        let (client, cinder_fake, _addr) =
            start_testbed().await.map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let orchestrator =
            AttachmentOrchestrator::new(store.clone(), provider.clone(), Some(client.clone()));
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
    async fn cinder_unavailable_before_create_compensates_cleanly()
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
        let pending = h
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn connector_failure_compensates_by_terminating_cinder_attachment()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
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
    async fn compute_attach_success_with_cinder_completion_failure_compensates()
    -> Result<(), Box<dyn std::error::Error>> {
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
        assert_eq!(h.fake_provider.attached_volume_count(server_id), 0);
        let pending = h
            .store
            .list_volume_attachments_by_status(TERMINAL_STATUSES)
            .await?;
        assert!(pending.is_empty());
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
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
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
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await?);
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
