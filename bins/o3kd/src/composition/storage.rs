use async_trait::async_trait;
use o3k_api::NativeAttachmentWorkflow;
use o3k_provider::BlockDeviceAttachment;
use o3k_reconciler;
use o3k_reconciler::storage_workflow::{
    ComputeAttachmentExecutor, StorageAttachmentWorkflow, StorageControllerFence,
    StorageWorkflowError,
};
use o3k_storage;
use o3k_store;
use o3k_store::{
    ControllerEpoch, ControllerId, CoordinationRepository, DurableStore, FencingToken,
    LeaseAcquireOutcome, StorageRepository,
};
use rustix::fs::{FlockOperation, flock};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tracing;
use uuid::Uuid;

pub(crate) struct LocalStorageFence {
    pub(crate) coordination: Arc<dyn o3k_store::CoordinationRepository>,
    pub(crate) controller_id: o3k_store::ControllerId,
    pub(crate) controller_epoch: o3k_store::ControllerEpoch,
    pub(crate) intent_epoch: u64,
    pub(crate) execution_lock_path: PathBuf,
    pub(crate) attempt: Arc<tokio::sync::Mutex<Option<StorageLeaseAttempt>>>,
}

pub(crate) struct StorageLeaseAttempt {
    pub(crate) fencing_token: o3k_store::FencingToken,
    pub(crate) stop: tokio::sync::oneshot::Sender<()>,
    pub(crate) valid: Arc<AtomicBool>,
    pub(crate) _execution_lock: File,
}

#[async_trait]
impl o3k_reconciler::storage_workflow::StorageControllerFence for LocalStorageFence {
    async fn begin(
        &self,
        controller_epoch: u64,
    ) -> Result<(), o3k_reconciler::storage_workflow::StorageWorkflowError> {
        if controller_epoch == 0 || controller_epoch != self.intent_epoch {
            return Err(
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
            );
        }
        let mut attempt = self.attempt.lock().await;
        if attempt.is_some() {
            return Err(
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
            );
        }
        let lease = match self
            .coordination
            .acquire_work_lease(
                "o3k-native-storage-controller",
                "storage-attachment",
                &self.controller_id,
                &self.controller_epoch,
                Duration::from_secs(15),
            )
            .await
            .map_err(|_| {
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence
            })? {
            o3k_store::LeaseAcquireOutcome::Acquired { lease } => lease,
            o3k_store::LeaseAcquireOutcome::Busy { .. } => {
                return Err(
                    o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
                );
            }
        };
        let execution_lock = match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.execution_lock_path)
        {
            Ok(file) => file,
            Err(_) => {
                let _ = self
                    .coordination
                    .release_work_lease(
                        "o3k-native-storage-controller",
                        &self.controller_id,
                        &self.controller_epoch,
                        lease.fencing_token,
                    )
                    .await;
                return Err(
                    o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
                );
            }
        };
        if flock(&execution_lock, FlockOperation::NonBlockingLockExclusive).is_err() {
            let _ = self
                .coordination
                .release_work_lease(
                    "o3k-native-storage-controller",
                    &self.controller_id,
                    &self.controller_epoch,
                    lease.fencing_token,
                )
                .await;
            return Err(
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
            );
        }
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let valid = Arc::new(AtomicBool::new(true));
        let coordination = self.coordination.clone();
        let controller_id = self.controller_id.clone();
        let controller_epoch_value = self.controller_epoch.clone();
        let fencing_token = lease.fencing_token;
        let renewal_valid = valid.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = interval.tick() => {
                        match coordination.renew_work_lease(
                            "o3k-native-storage-controller",
                            &controller_id,
                            &controller_epoch_value,
                            fencing_token,
                            Duration::from_secs(15),
                        ).await {
                            Ok(true) => {}
                            Ok(false) | Err(_) => {
                                renewal_valid.store(false, Ordering::Release);
                                break;
                            }
                        }
                    }
                }
            }
        });
        *attempt = Some(StorageLeaseAttempt {
            fencing_token,
            stop,
            valid,
            _execution_lock: execution_lock,
        });
        Ok(())
    }

    async fn assert_current(
        &self,
        controller_epoch: u64,
    ) -> Result<(), o3k_reconciler::storage_workflow::StorageWorkflowError> {
        if controller_epoch == 0 || controller_epoch != self.intent_epoch {
            return Err(
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
            );
        }
        let fencing_token = self
            .attempt
            .lock()
            .await
            .as_ref()
            .filter(|attempt| attempt.valid.load(Ordering::Acquire))
            .map(|attempt| attempt.fencing_token)
            .ok_or(o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence)?;
        let renewed = self
            .coordination
            .renew_work_lease(
                "o3k-native-storage-controller",
                &self.controller_id,
                &self.controller_epoch,
                fencing_token,
                Duration::from_secs(15),
            )
            .await
            .map_err(|_| {
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence
            })?;
        if renewed {
            Ok(())
        } else {
            Err(o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence)
        }
    }

    async fn end(
        &self,
        controller_epoch: u64,
    ) -> Result<(), o3k_reconciler::storage_workflow::StorageWorkflowError> {
        if controller_epoch != self.intent_epoch {
            return Err(
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence,
            );
        }
        let Some(attempt) = self.attempt.lock().await.take() else {
            return Ok(());
        };
        let _ = attempt.stop.send(());
        let released = self
            .coordination
            .release_work_lease(
                "o3k-native-storage-controller",
                &self.controller_id,
                &self.controller_epoch,
                attempt.fencing_token,
            )
            .await
            .map_err(|_| {
                o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence
            })?;
        if released {
            Ok(())
        } else {
            Err(o3k_reconciler::storage_workflow::StorageWorkflowError::StaleControllerFence)
        }
    }
}

pub(crate) struct LocalComputeAttachmentExecutor {
    pub(crate) compute: Arc<o3k_compute::ComputeService>,
}

#[async_trait]
impl o3k_reconciler::storage_workflow::ComputeAttachmentExecutor
    for LocalComputeAttachmentExecutor
{
    async fn attach(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
        prepared: &o3k_storage::PreparedAttachment,
    ) -> Result<(), o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        let device = BlockDeviceAttachment {
            volume_id: attachment.volume_id.to_string(),
            attachment_id: attachment.id.to_string(),
            driver_volume_type: "local".to_owned(),
            target_iqn: None,
            target_portal: None,
            target_lun: None,
            local_path: Some(prepared.device_path().to_owned()),
            device_path: None,
            multipath: false,
            initiator: None,
            auth_method: None,
            auth_username: None,
            auth_password: None,
        };
        self.compute
            .provider()
            .attach_block_device(attachment.server_id, &device)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }

    async fn inspect(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
    ) -> Result<bool, o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        self.compute
            .provider()
            .observe_block_device(attachment.server_id, &attachment.volume_id.to_string())
            .await
            .map(|observation| observation.is_some_and(|value| value.attached))
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }

    async fn detach(
        &self,
        attachment: &o3k_domain::VolumeAttachment,
    ) -> Result<(), o3k_reconciler::storage_workflow::ComputeAttachmentError> {
        let device = BlockDeviceAttachment {
            volume_id: attachment.volume_id.to_string(),
            attachment_id: attachment.id.to_string(),
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
        self.compute
            .provider()
            .detach_block_device(attachment.server_id, &device)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.is_unknown_outcome() {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::UnknownOutcome
                } else {
                    o3k_reconciler::storage_workflow::ComputeAttachmentError::Failed
                }
            })
    }
}

pub(crate) struct NativeStorageAttachmentWorkflow {
    pub(crate) store: Arc<o3k_store::O3kStore>,
    pub(crate) controller_epoch: u64,
    pub(crate) workflow: o3k_reconciler::storage_workflow::StorageAttachmentWorkflow<
        o3k_store::O3kStore,
        o3k_storage::LvmStorageProvider,
        LocalComputeAttachmentExecutor,
        LocalStorageFence,
    >,
}

// Native LVM execution is intentionally in-process in this bounded profile;
// there is no independently restartable storage-agent session to register.
// The durable controller work lease above is the real storage mutation fence.
// This protocol value is therefore only the required target-agent field and
// must never be treated as storage-agent ownership or restart identity.
const LOCAL_STORAGE_TARGET_AGENT_EPOCH: u64 = 1;

#[async_trait]
impl o3k_api::NativeAttachmentWorkflow for NativeStorageAttachmentWorkflow {
    async fn attach(&self, attachment_id: Uuid) -> Result<(), String> {
        let record = self
            .store
            .get_volume_attachment_v1(attachment_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native attachment disappeared".to_owned())?;
        let attachment = record.attachment;
        let intent = native_storage_intent(&attachment, "attach", self.controller_epoch);
        self.workflow
            .attach(intent)
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok(())
    }

    async fn detach(&self, attachment_id: Uuid) -> Result<(), String> {
        let record = self
            .store
            .get_volume_attachment_v1(attachment_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native attachment disappeared".to_owned())?;
        let attachment = record.attachment;
        self.workflow
            .detach(native_storage_intent(
                &attachment,
                "detach",
                self.controller_epoch,
            ))
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok(())
    }

    async fn recover(&self) -> Result<(), String> {
        let mut first_error = None;
        for command in self
            .store
            .list_recoverable_agent_commands()
            .await
            .map_err(|error| error.to_string())?
        {
            let Ok(_envelope) =
                serde_json::from_slice::<o3k_domain::StorageCommandEnvelope>(&command.payload)
            else {
                continue;
            };
            if self
                .store
                .get_volume_attachment_v1(command.resource_id)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
            {
                continue;
            }
            // The envelope epoch is immutable request provenance.  Recovery
            // is executed by the current controller session after the
            // storage work lease has authorized a takeover.
            if let Err(error) = self
                .workflow
                .reconcile(&command.command_id, self.controller_epoch)
                .await
            {
                // Recovery is per command.  A busy/unknown/provider-failed
                // attachment must not head-of-line block unrelated durable
                // commands in the same startup pass; the next scheduled pass
                // retries the unresolved command automatically.
                tracing::warn!(
                    command_id = %command.command_id,
                    %error,
                    "native storage command recovery deferred"
                );
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

pub(crate) fn native_storage_intent(
    attachment: &o3k_domain::VolumeAttachment,
    operation: &str,
    controller_epoch: u64,
) -> o3k_reconciler::storage_workflow::StorageAttachmentIntent {
    o3k_reconciler::storage_workflow::StorageAttachmentIntent {
        attachment_id: attachment.id,
        volume_id: attachment.volume_id,
        server_id: attachment.server_id,
        project_id: attachment.project_id.clone(),
        access_mode: attachment.access_mode,
        delete_on_termination: attachment.delete_on_termination,
        controller_epoch,
        target_agent_id: "local".to_owned(),
        target_agent_epoch: LOCAL_STORAGE_TARGET_AGENT_EPOCH,
        idempotency_key: format!("native-{operation}:{}", attachment.id),
        trace_id: format!("native-{operation}:{}", attachment.id),
        deadline: "2099-01-01T00:00:00.000".to_owned(),
    }
}

pub(crate) fn storage_intent_epoch(epoch: &o3k_store::ControllerEpoch) -> u64 {
    epoch.0.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        hash.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(byte))
    })
}
