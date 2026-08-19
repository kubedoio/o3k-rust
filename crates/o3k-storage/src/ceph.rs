//! Bounded Ceph RBD execution provider.
//!
//! Ceph identifiers, device mappings, monitors, and authentication paths stay
//! inside this execution adapter.  The provider returns only the same
//! technology-independent observations used by the LVM provider; device names
//! are transient values for the compute hand-off.

use crate::{
    PreparedAttachment, StorageAttachmentObservation, StorageAttachmentRequest, StorageProvider,
    StorageProviderError, StorageSnapshotObservation, StorageSnapshotRequest,
    StorageVolumeObservation, StorageVolumeRequest,
};
use async_trait::async_trait;
use o3k_domain::{SnapshotConsistency, StorageCapabilities, StorageProviderReference};
use serde::Deserialize;
use std::{fmt, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{process::Command, time::timeout};

const OWNER_KEY: &str = "o3k.owner";

#[derive(Clone, PartialEq, Eq)]
pub struct CephRbdConfig {
    pub pool: String,
    pub namespace: Option<String>,
    pub provider_namespace: String,
    pub conf_path: Option<String>,
    pub client_id: Option<String>,
    pub keyring_path: Option<String>,
    pub capacity_bytes: u64,
}

impl fmt::Debug for CephRbdConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CephRbdConfig")
            .field("pool", &self.pool)
            .field("namespace", &self.namespace)
            .field("provider_namespace", &self.provider_namespace)
            .field(
                "conf_path",
                &self.conf_path.as_ref().map(|_| "<configured>"),
            )
            .field(
                "client_id",
                &self.client_id.as_ref().map(|_| "<configured>"),
            )
            .field(
                "keyring_path",
                &self.keyring_path.as_ref().map(|_| "<configured>"),
            )
            .field("capacity_bytes", &self.capacity_bytes)
            .finish()
    }
}

impl CephRbdConfig {
    pub fn validate(&self) -> Result<(), StorageProviderError> {
        for value in [&self.pool, &self.provider_namespace] {
            if !valid_identifier(value) {
                return Err(StorageProviderError::InvalidConfiguration);
            }
        }
        if let Some(namespace) = &self.namespace
            && !namespace.is_empty()
            && !valid_identifier(namespace)
        {
            return Err(StorageProviderError::InvalidConfiguration);
        }
        if self.capacity_bytes == 0 {
            return Err(StorageProviderError::InvalidConfiguration);
        }
        for path in [&self.conf_path, &self.keyring_path].into_iter().flatten() {
            if path.is_empty() || path.len() > 4096 || path.contains('\0') {
                return Err(StorageProviderError::InvalidConfiguration);
            }
        }
        if let Some(id) = &self.client_id
            && (id.is_empty() || id.len() > 128 || id.contains('\0'))
        {
            return Err(StorageProviderError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CephCommandOutput {
    pub status: i32,
    pub stdout: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CephCommandError {
    #[error("command unavailable")]
    Unavailable,
    #[error("command timed out")]
    Timeout,
    #[error("command failed")]
    Failed,
}

#[async_trait]
pub trait CephCommandRunner: Send + Sync {
    async fn run(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<CephCommandOutput, CephCommandError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCephCommandRunner;

#[async_trait]
impl CephCommandRunner for SystemCephCommandRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<CephCommandOutput, CephCommandError> {
        let output = timeout(
            Duration::from_secs(30),
            Command::new(program).args(args).output(),
        )
        .await
        .map_err(|_| CephCommandError::Timeout)?
        .map_err(|_| CephCommandError::Unavailable)?;
        Ok(CephCommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8(output.stdout).map_err(|_| CephCommandError::Failed)?,
        })
    }
}

#[derive(Clone)]
pub struct CephRbdStorageProvider<R = SystemCephCommandRunner> {
    config: CephRbdConfig,
    runner: Arc<R>,
}

impl<R> fmt::Debug for CephRbdStorageProvider<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CephRbdStorageProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl CephRbdStorageProvider<SystemCephCommandRunner> {
    pub fn new(config: CephRbdConfig) -> Result<Self, StorageProviderError> {
        Self::with_runner(config, SystemCephCommandRunner)
    }
}

impl<R: CephCommandRunner> CephRbdStorageProvider<R> {
    pub fn with_runner(config: CephRbdConfig, runner: R) -> Result<Self, StorageProviderError> {
        config.validate()?;
        Ok(Self {
            config,
            runner: Arc::new(runner),
        })
    }

    #[must_use]
    pub fn config(&self) -> &CephRbdConfig {
        &self.config
    }

    fn auth_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(conf) = &self.config.conf_path {
            args.extend(["--conf".to_owned(), conf.clone()]);
        }
        if let Some(id) = &self.config.client_id {
            args.extend(["--id".to_owned(), id.clone()]);
        }
        if let Some(keyring) = &self.config.keyring_path {
            args.extend(["--keyring".to_owned(), keyring.clone()]);
        }
        args
    }

    fn rbd_args(&self) -> Vec<String> {
        let mut args = vec!["--pool".to_owned(), self.config.pool.clone()];
        if let Some(namespace) = &self.config.namespace
            && !namespace.is_empty()
        {
            args.extend(["--namespace".to_owned(), namespace.clone()]);
        }
        args.extend(self.auth_args());
        args
    }

    fn image_name(&self, id: o3k_domain::VolumeId) -> String {
        format!("o3k-v-{}", id.as_uuid().simple())
    }

    fn snapshot_name(&self, id: o3k_domain::SnapshotId) -> String {
        format!("o3k-s-{}", id.as_uuid().simple())
    }

    fn owner_marker(
        &self,
        volume_id: o3k_domain::VolumeId,
        project_id: &str,
        generation: u64,
    ) -> String {
        let mut digest = sha2::Sha256::new();
        use sha2::Digest;
        digest.update(self.config.provider_namespace.as_bytes());
        digest.update([0]);
        digest.update(volume_id.as_uuid().as_bytes());
        digest.update([0]);
        digest.update(project_id.as_bytes());
        digest.update([0]);
        digest.update(generation.to_be_bytes());
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn run(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<CephCommandOutput, StorageProviderError>
    where
        R: CephCommandRunner + Sync,
    {
        self.runner
            .run(program, &args)
            .await
            .map_err(map_command_error)
    }

    async fn checked(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<CephCommandOutput, StorageProviderError>
    where
        R: CephCommandRunner + Sync,
    {
        let output = self.run(program, args).await?;
        if output.status != 0 {
            return Err(StorageProviderError::CommandFailed);
        }
        Ok(output)
    }

    async fn owned_image(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<(String, u64), StorageProviderError>
    where
        R: CephCommandRunner + Sync,
    {
        validate_volume(request)?;
        let image = self.image_name(request.volume_id);
        let mut info_args = self.rbd_args();
        info_args.extend([
            "info".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            image.clone(),
        ]);
        let info = self.run("rbd", info_args).await?;
        if info.status != 0 {
            return Err(StorageProviderError::NotFound);
        }
        let parsed: RbdInfo =
            serde_json::from_str(&info.stdout).map_err(|_| StorageProviderError::CommandFailed)?;
        let mut meta_args = self.rbd_args();
        meta_args.extend([
            "image-meta".to_owned(),
            "get".to_owned(),
            image.clone(),
            OWNER_KEY.to_owned(),
        ]);
        let metadata = self.run("rbd", meta_args).await?;
        if metadata.status != 0 {
            return Err(StorageProviderError::ForeignResource);
        }
        if metadata.stdout.trim()
            != self.owner_marker(request.volume_id, &request.project_id, request.generation)
        {
            return Err(StorageProviderError::ForeignResource);
        }
        Ok((image, parsed.size))
    }

    async fn snapshot_exists(
        &self,
        image: &str,
        snapshot: &str,
    ) -> Result<bool, StorageProviderError>
    where
        R: CephCommandRunner + Sync,
    {
        let mut args = self.rbd_args();
        args.extend([
            "snap".to_owned(),
            "ls".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            image.to_owned(),
        ]);
        let output = self.checked("rbd", args).await?;
        let snapshots: Vec<RbdSnapshot> = serde_json::from_str(&output.stdout)
            .map_err(|_| StorageProviderError::CommandFailed)?;
        Ok(snapshots.iter().any(|entry| entry.name == snapshot))
    }
}

#[async_trait]
impl<R> StorageProvider for CephRbdStorageProvider<R>
where
    R: CephCommandRunner + Sync,
{
    async fn capabilities(&self) -> Result<StorageCapabilities, StorageProviderError> {
        let mut args = self.auth_args();
        args.extend(["df".to_owned(), "--format".to_owned(), "json".to_owned()]);
        let output = self.checked("ceph", args).await?;
        let report: CephDf = serde_json::from_str(&output.stdout)
            .map_err(|_| StorageProviderError::CommandFailed)?;
        Ok(StorageCapabilities {
            create_volume: true,
            snapshots: true,
            attachment: true,
            capacity_bytes: self.config.capacity_bytes.min(report.stats.total_bytes),
            allocated_bytes: self
                .config
                .capacity_bytes
                .min(report.stats.total_used_bytes),
            allocation_unit_bytes: 4096,
        })
    }

    async fn create_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<StorageVolumeObservation, StorageProviderError> {
        validate_volume(request)?;
        match self.owned_image(request).await {
            Ok((_, size)) => {
                return Ok(observation(self.image_name(request.volume_id), size));
            }
            Err(StorageProviderError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let image = self.image_name(request.volume_id);
        let mut args = self.rbd_args();
        args.extend([
            "create".to_owned(),
            "--image-format".to_owned(),
            "2".to_owned(),
            "--size".to_owned(),
            request.size_bytes.to_string(),
            image.clone(),
        ]);
        self.checked("rbd", args).await?;
        let mut metadata = self.rbd_args();
        metadata.extend([
            "image-meta".to_owned(),
            "set".to_owned(),
            image.clone(),
            OWNER_KEY.to_owned(),
            self.owner_marker(request.volume_id, &request.project_id, request.generation),
        ]);
        self.checked("rbd", metadata).await?;
        self.inspect_volume(request).await
    }

    async fn inspect_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<StorageVolumeObservation, StorageProviderError> {
        let (image, size) = self.owned_image(request).await?;
        Ok(observation(image, size))
    }

    async fn delete_volume(
        &self,
        request: &StorageVolumeRequest,
    ) -> Result<(), StorageProviderError> {
        let (image, _) = self.owned_image(request).await?;
        let mut args = self.rbd_args();
        args.extend(["rm".to_owned(), image]);
        self.checked("rbd", args).await?;
        Ok(())
    }

    async fn prepare_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<PreparedAttachment, StorageProviderError> {
        if request.project_id.is_empty()
            || request.host_id.is_empty()
            || request.volume_generation == 0
        {
            return Err(StorageProviderError::InvalidRequest);
        }
        let (image, _) = self
            .owned_image(&StorageVolumeRequest {
                volume_id: request.volume_id,
                project_id: request.project_id.clone(),
                size_bytes: 1,
                generation: request.volume_generation,
            })
            .await?;
        let mut args = self.rbd_args();
        args.push("device".to_owned());
        args.push("map".to_owned());
        if matches!(
            request.access_mode,
            o3k_domain::AttachmentAccessMode::ReadOnly
        ) {
            args.push("--read-only".to_owned());
        }
        args.push(image.clone());
        let output = self.checked("rbd", args).await?;
        let device = output.stdout.trim();
        if !device.starts_with("/dev/") || device.len() > 512 || device.contains('\0') {
            return Err(StorageProviderError::CommandFailed);
        }
        PreparedAttachment::from_provider(
            StorageProviderReference {
                provider: "ceph-rbd".to_owned(),
                resource_id: image,
            },
            device.to_owned(),
            request.attachment_id,
            request.volume_id,
        )
    }

    async fn inspect_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<StorageAttachmentObservation, StorageProviderError> {
        let (image, _) = self
            .owned_image(&StorageVolumeRequest {
                volume_id: request.volume_id,
                project_id: request.project_id.clone(),
                size_bytes: 1,
                generation: request.volume_generation,
            })
            .await?;
        let mut args = self.rbd_args();
        args.extend([
            "device".to_owned(),
            "list".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ]);
        let output = self.checked("rbd", args).await?;
        let mappings: Vec<RbdMapping> = serde_json::from_str(&output.stdout)
            .map_err(|_| StorageProviderError::CommandFailed)?;
        let attached = mappings.iter().any(|mapping| mapping.image == image);
        Ok(StorageAttachmentObservation {
            attachment_id: request.attachment_id,
            volume_id: request.volume_id,
            host_id: request.host_id.clone(),
            attached,
            provider_reference: StorageProviderReference {
                provider: "ceph-rbd".to_owned(),
                resource_id: image,
            },
        })
    }

    async fn terminate_attachment(
        &self,
        request: &StorageAttachmentRequest,
    ) -> Result<StorageAttachmentObservation, StorageProviderError> {
        let (image, _) = self
            .owned_image(&StorageVolumeRequest {
                volume_id: request.volume_id,
                project_id: request.project_id.clone(),
                size_bytes: 1,
                generation: request.volume_generation,
            })
            .await?;
        let mut args = self.rbd_args();
        args.extend(["device".to_owned(), "unmap".to_owned(), image.clone()]);
        let output = self.run("rbd", args).await?;
        if output.status != 0 {
            return Err(StorageProviderError::UnknownOutcome);
        }
        Ok(StorageAttachmentObservation {
            attachment_id: request.attachment_id,
            volume_id: request.volume_id,
            host_id: request.host_id.clone(),
            attached: false,
            provider_reference: StorageProviderReference {
                provider: "ceph-rbd".to_owned(),
                resource_id: image,
            },
        })
    }

    async fn create_snapshot(
        &self,
        request: &StorageSnapshotRequest,
    ) -> Result<StorageSnapshotObservation, StorageProviderError> {
        let (image, _) = self
            .owned_image(&StorageVolumeRequest {
                volume_id: request.volume_id,
                project_id: request.project_id.clone(),
                size_bytes: 1,
                generation: request.source_generation,
            })
            .await?;
        let snapshot = self.snapshot_name(request.snapshot_id);
        let mut args = self.rbd_args();
        args.extend([
            "snap".to_owned(),
            "create".to_owned(),
            format!("{image}@{snapshot}"),
        ]);
        self.checked("rbd", args).await?;
        Ok(StorageSnapshotObservation {
            provider_reference: StorageProviderReference {
                provider: "ceph-rbd".to_owned(),
                resource_id: format!("{image}@{snapshot}"),
            },
            consistency: SnapshotConsistency::CrashConsistent,
            available: true,
        })
    }

    async fn delete_snapshot(
        &self,
        request: &StorageSnapshotRequest,
    ) -> Result<(), StorageProviderError> {
        let (image, _) = self
            .owned_image(&StorageVolumeRequest {
                volume_id: request.volume_id,
                project_id: request.project_id.clone(),
                size_bytes: 1,
                generation: request.source_generation,
            })
            .await?;
        let snapshot = self.snapshot_name(request.snapshot_id);
        if !self.snapshot_exists(&image, &snapshot).await? {
            return Err(StorageProviderError::NotFound);
        }
        let mut args = self.rbd_args();
        args.extend([
            "snap".to_owned(),
            "rm".to_owned(),
            format!("{image}@{snapshot}"),
        ]);
        self.checked("rbd", args).await?;
        Ok(())
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn validate_volume(request: &StorageVolumeRequest) -> Result<(), StorageProviderError> {
    if request.project_id.is_empty()
        || request.project_id.len() > 256
        || request.size_bytes == 0
        || request.generation == 0
    {
        return Err(StorageProviderError::InvalidRequest);
    }
    Ok(())
}

fn observation(image: String, size_bytes: u64) -> StorageVolumeObservation {
    StorageVolumeObservation {
        provider_reference: StorageProviderReference {
            provider: "ceph-rbd".to_owned(),
            resource_id: image,
        },
        size_bytes,
        owned: true,
        available: true,
    }
}

fn map_command_error(error: CephCommandError) -> StorageProviderError {
    match error {
        CephCommandError::Unavailable => StorageProviderError::Unavailable,
        CephCommandError::Timeout => StorageProviderError::UnknownOutcome,
        CephCommandError::Failed => StorageProviderError::CommandFailed,
    }
}

#[derive(Debug, Deserialize)]
struct RbdInfo {
    size: u64,
}

#[derive(Debug, Deserialize)]
struct RbdSnapshot {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RbdMapping {
    #[serde(alias = "name", alias = "image_name")]
    image: String,
}

#[derive(Debug, Deserialize)]
struct CephDf {
    stats: CephDfStats,
}

#[derive(Debug, Deserialize)]
struct CephDfStats {
    total_bytes: u64,
    total_used_bytes: u64,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::StorageProviderError;
    use o3k_domain::VolumeId;
    use std::{collections::VecDeque, sync::Mutex};
    use uuid::Uuid;

    #[derive(Default)]
    struct FakeRunner {
        outputs: Mutex<VecDeque<Result<CephCommandOutput, CephCommandError>>>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn with_outputs(
            outputs: impl IntoIterator<Item = Result<CephCommandOutput, CephCommandError>>,
        ) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn output(status: i32, stdout: &str) -> Result<CephCommandOutput, CephCommandError> {
            Ok(CephCommandOutput {
                status,
                stdout: stdout.to_owned(),
            })
        }
    }

    #[async_trait]
    impl CephCommandRunner for FakeRunner {
        async fn run(
            &self,
            program: &str,
            args: &[String],
        ) -> Result<CephCommandOutput, CephCommandError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((program.to_owned(), args.to_vec()));
            self.outputs
                .lock()
                .expect("outputs lock")
                .pop_front()
                .expect("scripted Ceph response")
        }
    }

    fn config() -> CephRbdConfig {
        CephRbdConfig {
            pool: "o3k".to_owned(),
            namespace: Some("testlab".to_owned()),
            provider_namespace: "p10-test".to_owned(),
            conf_path: Some("/etc/ceph/ceph.conf".to_owned()),
            client_id: Some("client.o3k".to_owned()),
            keyring_path: Some("/etc/ceph/client.o3k.keyring".to_owned()),
            capacity_bytes: 1 << 30,
        }
    }

    fn volume() -> StorageVolumeRequest {
        StorageVolumeRequest {
            volume_id: VolumeId::from_uuid(Uuid::from_u128(7)),
            project_id: "project-a".to_owned(),
            size_bytes: 64 << 20,
            generation: 1,
        }
    }

    #[test]
    fn config_debug_redacts_auth_paths_and_identifiers() {
        let rendered = format!("{:?}", config());
        assert!(!rendered.contains("client.o3k"));
        assert!(!rendered.contains("client.o3k.keyring"));
        assert!(rendered.contains("<configured>"));
    }

    #[tokio::test]
    async fn create_proves_owner_marker_before_reporting_owned() {
        let request = volume();
        let provider = CephRbdStorageProvider::with_runner(
            config(),
            FakeRunner::with_outputs([
                FakeRunner::output(1, ""),
                FakeRunner::output(0, ""),
                FakeRunner::output(0, ""),
                FakeRunner::output(0, r#"{"size":67108864}"#),
                FakeRunner::output(0, &format!("{}\n", provider_marker(&request))),
            ]),
        )
        .expect("valid config");

        let observed = provider
            .create_volume(&request)
            .await
            .expect("create converges");
        assert!(observed.owned);
        assert_eq!(observed.size_bytes, request.size_bytes);
        assert_eq!(observed.provider_reference.provider, "ceph-rbd");
    }

    #[tokio::test]
    async fn foreign_image_is_rejected_before_mutation() {
        let provider = CephRbdStorageProvider::with_runner(
            config(),
            FakeRunner::with_outputs([
                FakeRunner::output(0, r#"{"size":67108864}"#),
                FakeRunner::output(0, "foreign-owner\n"),
            ]),
        )
        .expect("valid config");
        let error = provider
            .inspect_volume(&volume())
            .await
            .expect_err("foreign image");
        assert_eq!(error, StorageProviderError::ForeignResource);
    }

    #[tokio::test]
    async fn timeout_is_unknown_outcome() {
        let provider = CephRbdStorageProvider::with_runner(
            config(),
            FakeRunner::with_outputs([Err(CephCommandError::Timeout)]),
        )
        .expect("valid config");
        let error = provider
            .inspect_volume(&volume())
            .await
            .expect_err("timeout");
        assert_eq!(error, StorageProviderError::UnknownOutcome);
    }

    fn provider_marker(request: &StorageVolumeRequest) -> String {
        let provider = CephRbdStorageProvider::with_runner(config(), FakeRunner::default())
            .expect("valid config");
        provider.owner_marker(request.volume_id, &request.project_id, request.generation)
    }
}
