use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use md5::{Digest as Md5Digest, Md5};
use sqlx::{
    Row, SqlitePool,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
    },
};
use thiserror::Error;
use uuid::Uuid;

mod artifact_transfer;
mod server_state;

/// Maximum attempts for an observation update contended by a concurrent
/// SQLite writer. BEGIN IMMEDIATE makes the configured busy_timeout apply, so
/// retries only absorb contention bursts that outlast it; the update is
/// idempotent, so a retry never double-applies.
const SQLITE_BUSY_MAX_ATTEMPTS: u32 = 5;

/// Reports whether a sqlx error is a SQLite lock-contention failure:
/// SQLITE_BUSY (extended code 5) or SQLITE_BUSY_SNAPSHOT (517). sqlx preserves
/// the extended code, so both variants are matchable here.
fn is_sqlite_busy(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            matches!(database.code().as_deref(), Some("5") | Some("517"))
        }
        _ => false,
    }
}

pub use artifact_transfer::{
    ArtifactTransferRecord, ArtifactTransferState, ArtifactTransferUpdate,
    MAX_ARTIFACT_TRANSFER_BYTES, MAX_ARTIFACT_TRANSFER_CHUNK_BYTES, MAX_ARTIFACT_TRANSFER_RETRIES,
};
pub use server_state::{server_state_from_storage, server_state_to_storage};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseHealth {
    pub status: String,
    pub journal_mode: String,
    pub foreign_keys: bool,
    pub integrity_check: String,
    pub page_count: i64,
    pub page_size: i64,
    pub wal_checkpoint_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalCheckpointMode {
    Passive,
    Full,
    Restart,
    Truncate,
}

impl WalCheckpointMode {
    #[must_use]
    pub fn as_pragma_str(&self) -> &'static str {
        match self {
            Self::Passive => "PASSIVE",
            Self::Full => "FULL",
            Self::Restart => "RESTART",
            Self::Truncate => "TRUNCATE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeypairRecord {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: String,
    pub name: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VolumeAttachmentRecord {
    pub id: Uuid,
    pub server_id: Uuid,
    pub volume_id: Uuid,
    pub device: String,
    pub tag: Option<String>,
    pub delete_on_termination: bool,
    pub created_at: String,
    pub status: String,
    pub operation_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub cinder_attachment_id: Option<String>,
    pub connector_host: Option<String>,
    pub connector_ip: Option<String>,
    pub connector_initiator: Option<String>,
    pub driver_volume_type: Option<String>,
    pub target_iqn: Option<String>,
    pub target_portal: Option<String>,
    pub target_lun: Option<u32>,
    pub connection_info_digest: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneDomainRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneProjectRecord {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneUserRecord {
    pub id: String,
    pub domain_id: String,
    pub name: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneRoleRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneRoleAssignmentRecord {
    pub id: String,
    pub user_id: String,
    pub project_id: String,
    pub role_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneServiceRecord {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneEndpointRecord {
    pub id: String,
    pub service_id: String,
    pub interface: String,
    pub url: String,
    pub region: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeystoneRegionRecord {
    pub id: String,
    pub description: Option<String>,
    pub parent_region_id: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Running,
    Succeeded,
    Retryable,
    UnknownOutcome,
    Failed,
}

impl OperationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Retryable => "retryable",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "retryable" => Ok(Self::Retryable),
            "unknown_outcome" => Ok(Self::UnknownOutcome),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown operation state `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    pub id: Uuid,
    pub kind: String,
    pub project_id: String,
    pub generation: i64,
    pub observed_generation: i64,
    pub desired_state: String,
    pub observed_state: String,
    pub provider_id: Option<String>,
}

pub struct ObservationUpdate<'a> {
    pub expected_generation: i64,
    pub desired_state: &'a str,
    pub observed_state: &'a str,
    pub observed_generation: i64,
    pub provider_id: Option<&'a str>,
    pub agent_epoch: &'a str,
    pub observation_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub id: Uuid,
    pub resource_id: Uuid,
    pub kind: String,
    pub state: OperationState,
    pub provider_operation_id: Option<String>,
    pub error_category: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReference {
    pub resource_id: Uuid,
    pub provider_name: String,
    pub provider_resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOverlayIdentity {
    pub resource_id: Uuid,
    pub operation_id: Uuid,
    pub command_id: String,
    pub agent_id: String,
    pub agent_epoch: String,
    pub base_sha256: String,
    pub base_format: String,
    pub overlay_format: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOverlayState {
    Pending,
    Materializing,
    Ready,
    Deleting,
    Deleted,
    Failed,
}

impl ImageOverlayState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Materializing => "materializing",
            Self::Ready => "ready",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "materializing" => Ok(Self::Materializing),
            "ready" => Ok(Self::Ready),
            "deleting" => Ok(Self::Deleting),
            "deleted" => Ok(Self::Deleted),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown image overlay state `{value}`"
            ))),
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Deleted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOverlayOwnershipRecord {
    pub overlay_id: String,
    pub identity: ImageOverlayIdentity,
    pub state: ImageOverlayState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageOverlayUpdate {
    pub state: ImageOverlayState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCommandState {
    Pending,
    Accepted,
    Running,
    Succeeded,
    Retryable,
    UnknownOutcome,
    Failed,
}

impl AgentCommandState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Retryable => "retryable",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "retryable" => Ok(Self::Retryable),
            "unknown_outcome" => Ok(Self::UnknownOutcome),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown agent command state `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommandRecord {
    pub command_id: String,
    pub idempotency_key: String,
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub agent_id: String,
    pub agent_epoch: String,
    pub payload_fingerprint_sha256: String,
    pub payload: Vec<u8>,
    pub state: AgentCommandState,
    pub accepted_sequence: u64,
    pub last_sequence: u64,
    pub provider_operation_id: Option<String>,
    pub provider_resource_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error")]
    Database(#[source] sqlx::Error),
    #[error("database migration error")]
    Migration(#[source] sqlx::migrate::MigrateError),
    #[error("resource not found")]
    ResourceNotFound,
    #[error("operation not found")]
    OperationNotFound,
    #[error("resource generation is stale")]
    StaleGeneration,
    #[error("resource already exists")]
    ResourceAlreadyExists,
    #[error("provider reference already exists")]
    ProviderReferenceAlreadyExists,
    #[error("provider reference not found")]
    ProviderReferenceNotFound,
    #[error("cannot create data directory {path}: {source}")]
    CreateDataDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid UUID in durable state")]
    InvalidUuid(#[source] uuid::Error),
    #[error("corrupt durable state: {0}")]
    Corrupt(String),
    #[error("keypair not found")]
    KeypairNotFound,
    #[error("keypair already exists")]
    KeypairAlreadyExists,
    #[error("invalid keypair: {0}")]
    InvalidKeypair(String),
    #[error("keypair is still attached to a server")]
    KeypairInUse,
    #[error("keypair and server ownership do not match")]
    KeypairOwnershipConflict,
    #[error("artifact transfer not found")]
    ArtifactTransferNotFound,
    #[error("artifact transfer epoch does not match durable state")]
    ArtifactTransferEpochConflict,
    #[error("artifact transfer conflict: {0}")]
    ArtifactTransferConflict(String),
    #[error("invalid artifact transfer: {0}")]
    InvalidArtifactTransfer(String),
    #[error("image overlay ownership not found")]
    ImageOverlayNotFound,
    #[error("image overlay ownership epoch does not match durable state")]
    ImageOverlayEpochConflict,
    #[error("image overlay ownership conflict: {0}")]
    ImageOverlayConflict(String),
    #[error("invalid image overlay ownership: {0}")]
    InvalidImageOverlay(String),
}

#[async_trait]
pub trait DurableStore: Send + Sync {
    async fn insert_resource(&self, resource: &ResourceRecord) -> Result<(), StoreError>;
    async fn get_resource(&self, id: Uuid) -> Result<ResourceRecord, StoreError>;
    async fn list_resources(
        &self,
        project_id: &str,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError>;
    async fn update_resource(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError>;
    async fn update_resource_from_observation(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError>;
    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError>;
    async fn get_operation(&self, id: Uuid) -> Result<OperationRecord, StoreError>;
    async fn update_operation(
        &self,
        id: Uuid,
        state: OperationState,
        provider_operation_id: Option<&str>,
        error_category: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationRecord, StoreError>;
    async fn attach_provider_reference(
        &self,
        reference: &ProviderReference,
    ) -> Result<(), StoreError>;
    async fn get_provider_reference(
        &self,
        resource_id: Uuid,
        provider_name: &str,
    ) -> Result<ProviderReference, StoreError>;
    async fn insert_agent_command(
        &self,
        command: &AgentCommandRecord,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command(&self, command_id: &str) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command_by_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn update_agent_command(
        &self,
        command_id: &str,
        state: AgentCommandState,
        accepted_sequence: u64,
        last_sequence: u64,
        provider_operation_id: Option<&str>,
        provider_resource_id: Option<&str>,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn list_recoverable_agent_commands(&self) -> Result<Vec<AgentCommandRecord>, StoreError>;
    async fn insert_artifact_transfer(
        &self,
        transfer: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn get_artifact_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn rebind_artifact_transfer_epoch(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        new_agent_epoch: &str,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError>;
    async fn insert_image_overlay(
        &self,
        overlay: &ImageOverlayOwnershipRecord,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn get_image_overlay(
        &self,
        overlay_id: &str,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn update_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
        update: ImageOverlayUpdate,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn list_image_overlays(
        &self,
        resource_id: Uuid,
    ) -> Result<Vec<ImageOverlayOwnershipRecord>, StoreError>;
    async fn count_image_overlay_references(
        &self,
        base_sha256: &str,
        base_format: &str,
    ) -> Result<u64, StoreError>;
    async fn delete_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError>;
    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
    ) -> Result<(), StoreError>;
    async fn readiness_check(&self) -> Result<(), StoreError>;
}

/// Durable Keystone-compatible identity records used by the identity
/// application service: deterministic bootstrap seeding (upserts) and the
/// one-time snapshot load that feeds token issuance and the catalog.
///
/// This is a narrow port around the identity use cases, not a generic
/// persistence surface. Application code depends on this trait (or a broader
/// combined port) instead of on the concrete `SqliteStore` adapter.
#[async_trait]
pub trait IdentityRepository: Send + Sync {
    async fn insert_keystone_domain(&self, domain: &KeystoneDomainRecord)
    -> Result<(), StoreError>;
    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError>;
    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError>;
    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError>;
    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError>;
    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError>;
    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError>;
    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError>;
    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError>;
    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError>;
    async fn insert_keystone_region(&self, region: &KeystoneRegionRecord)
    -> Result<(), StoreError>;
    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError>;
}

/// Durable keypair records owned by the compute service. The trait keeps the
/// scoped uniqueness, attach, and delete semantics available to application
/// code without naming the concrete adapter.
#[async_trait]
pub trait KeypairRepository: Send + Sync {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError>;
    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError>;
    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError>;
    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError>;
    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError>;
    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError>;
    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError>;
}

/// Durable Nova volume-attachment records owned by the compute attachment
/// orchestrator. Phase and outcome updates carry the frozen Cinder attachment
/// lifecycle; the port exposes the exact transitions the orchestrator uses.
#[async_trait]
pub trait VolumeAttachmentRepository: Send + Sync {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError>;
    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError>;
    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError>;
    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError>;
}

/// The persistence surface of the compute application service.
///
/// Combines the reconciler's `DurableStore` semantics (resources, operations,
/// agent commands, artifact transfers, image overlays, provider references —
/// already consumed generically by `OperationJournal`) with the keypair,
/// volume-attachment, and recovery-list capabilities the compute service uses.
/// Application code depends on this port; the composition root chooses the
/// concrete adapter.
#[async_trait]
pub trait ComputeRepository: DurableStore + KeypairRepository + VolumeAttachmentRepository {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError>;
}

/// Test-only construction helpers for the SQLite adapter.
///
/// Application crate tests build adapters through this module so the concrete
/// `SqliteStore` symbol never appears in application sources: the
/// architecture-boundary ratchet scans `src/**/*.rs` of application crates for
/// that literal symbol, and the adapter is an infrastructure detail that tests
/// of application behavior should not depend on by name.
pub mod testkit {
    use std::path::Path;

    use super::{SqliteStore, StoreError};

    /// Concrete SQLite adapter type used by application-crate tests. Named
    /// here so application sources never spell out `SqliteStore`; the
    /// architecture-boundary ratchet scans application `src/**/*.rs` for that
    /// literal symbol.
    pub type TestStore = SqliteStore;

    /// Opens a fresh in-memory SQLite adapter. Each call owns a private
    /// connection pool with the memory journal; it is not shared across
    /// stores.
    pub async fn open_memory() -> Result<TestStore, StoreError> {
        SqliteStore::connect("sqlite::memory:").await
    }

    /// Opens (creating when missing) a file-backed SQLite adapter with the
    /// production WAL posture, migrations, and integrity verification.
    pub async fn open_file(path: &Path) -> Result<TestStore, StoreError> {
        SqliteStore::connect_file(path).await
    }
}

#[derive(Clone, Debug)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let is_memory = database_url == "sqlite::memory:" || database_url == "sqlite://:memory:";
        let mut options =
            SqliteConnectOptions::from_str(database_url).map_err(StoreError::Database)?;
        let max_connections = if is_memory { 1 } else { 5 };

        options = options
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));

        if !is_memory {
            options = options
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal);
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(StoreError::Database)?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(StoreError::Migration)?;

        let store = Self { pool };
        store.verify_integrity().await?;
        Ok(store)
    }

    pub async fn connect_file(path: &Path) -> Result<Self, StoreError> {
        if path.as_os_str().is_empty() {
            return Err(StoreError::Database(sqlx::Error::Configuration(
                "database path cannot be empty".into(),
            )));
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| StoreError::CreateDataDirectory {
                path: parent.to_owned(),
                source,
            })?;
        }
        let url = format!("sqlite://{}", path.display());
        Self::connect(&url).await
    }

    pub async fn journal_mode(&self) -> Result<String, StoreError> {
        let row = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let mode: String = row.get(0);
        Ok(mode)
    }

    pub async fn checkpoint(&self, mode: WalCheckpointMode) -> Result<(), StoreError> {
        let sql = format!("PRAGMA wal_checkpoint({})", mode.as_pragma_str());
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn database_health(&self) -> Result<DatabaseHealth, StoreError> {
        let journal_mode = self.journal_mode().await?;

        let fk_row = sqlx::query("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let fk_int: i64 = fk_row.get(0);
        let foreign_keys = fk_int != 0;

        let integrity_row = sqlx::query("PRAGMA quick_check")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let integrity_check: String = integrity_row.get(0);

        let page_count_row = sqlx::query("PRAGMA page_count")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let page_count: i64 = page_count_row.get(0);

        let page_size_row = sqlx::query("PRAGMA page_size")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        let page_size: i64 = page_size_row.get(0);

        let wal_status = if journal_mode.eq_ignore_ascii_case("wal") {
            Some("active".to_owned())
        } else {
            None
        };

        let status = if integrity_check.eq_ignore_ascii_case("ok") {
            "ok".to_owned()
        } else {
            "degraded".to_owned()
        };

        Ok(DatabaseHealth {
            status,
            journal_mode,
            foreign_keys,
            integrity_check,
            page_count,
            page_size,
            wal_checkpoint_status: wal_status,
        })
    }

    pub async fn backup_to_file(&self, destination: &Path) -> Result<(), StoreError> {
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| StoreError::CreateDataDirectory {
                path: parent.to_owned(),
                source,
            })?;
        }
        let dest_str = destination.display().to_string();
        let query_str = format!("VACUUM INTO '{}'", dest_str.replace('\'', "''"));
        sqlx::query(&query_str)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    /// Lists all resources of one kind across projects for restart
    /// reconciliation. Callers must apply their own authorization checks
    /// before exposing the returned project-scoped records.
    pub async fn list_resources_by_kind(
        &self,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE kind = ? ORDER BY id",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(resource_from_row).collect()
    }

    fn volume_attachment_from_row(row: &SqliteRow) -> Result<VolumeAttachmentRecord, StoreError> {
        let id_str: String = row.try_get("id").map_err(StoreError::Database)?;
        let server_id_str: String = row.try_get("server_id").map_err(StoreError::Database)?;
        let volume_id_str: String = row.try_get("volume_id").map_err(StoreError::Database)?;
        let device: String = row.try_get("device").map_err(StoreError::Database)?;
        let tag: Option<String> = row.try_get("tag").map_err(StoreError::Database)?;
        let delete_on_termination_int: i32 = row
            .try_get("delete_on_termination")
            .map_err(StoreError::Database)?;
        let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
        let status: String = row.try_get("status").map_err(StoreError::Database)?;
        let operation_id: Option<String> =
            row.try_get("operation_id").map_err(StoreError::Database)?;
        let idempotency_key: Option<String> = row
            .try_get("idempotency_key")
            .map_err(StoreError::Database)?;
        let cinder_attachment_id: Option<String> = row
            .try_get("cinder_attachment_id")
            .map_err(StoreError::Database)?;
        let connector_host: Option<String> = row
            .try_get("connector_host")
            .map_err(StoreError::Database)?;
        let connector_ip: Option<String> =
            row.try_get("connector_ip").map_err(StoreError::Database)?;
        let connector_initiator: Option<String> = row
            .try_get("connector_initiator")
            .map_err(StoreError::Database)?;
        let driver_volume_type: Option<String> = row
            .try_get("driver_volume_type")
            .map_err(StoreError::Database)?;
        let target_iqn: Option<String> = row.try_get("target_iqn").map_err(StoreError::Database)?;
        let target_portal: Option<String> =
            row.try_get("target_portal").map_err(StoreError::Database)?;
        let target_lun: Option<i64> = row.try_get("target_lun").map_err(StoreError::Database)?;
        let connection_info_digest: Option<String> = row
            .try_get("connection_info_digest")
            .map_err(StoreError::Database)?;
        let error: Option<String> = row.try_get("error").map_err(StoreError::Database)?;

        let id = Uuid::parse_str(&id_str)
            .map_err(|_| StoreError::Corrupt("invalid volume attachment id".to_owned()))?;
        let server_id = Uuid::parse_str(&server_id_str)
            .map_err(|_| StoreError::Corrupt("invalid server id".to_owned()))?;
        let volume_id = Uuid::parse_str(&volume_id_str)
            .map_err(|_| StoreError::Corrupt("invalid volume id".to_owned()))?;

        Ok(VolumeAttachmentRecord {
            id,
            server_id,
            volume_id,
            device,
            tag,
            delete_on_termination: delete_on_termination_int != 0,
            created_at,
            status,
            operation_id: operation_id
                .as_deref()
                .map(Uuid::parse_str)
                .transpose()
                .map_err(|_| StoreError::Corrupt("invalid attachment operation id".to_owned()))?,
            idempotency_key,
            cinder_attachment_id,
            connector_host,
            connector_ip,
            connector_initiator,
            driver_volume_type,
            target_iqn,
            target_portal,
            target_lun: target_lun
                .map(|value| {
                    u32::try_from(value)
                        .map_err(|_| StoreError::Corrupt("invalid attachment lun".to_owned()))
                })
                .transpose()?,
            connection_info_digest,
            error,
        })
    }

    async fn verify_integrity(&self) -> Result<(), StoreError> {
        let result: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result != "ok" {
            return Err(StoreError::Corrupt(result));
        }
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('resources', 'operations', 'provider_refs', 'keypairs', 'server_keypairs', 'agent_commands', 'operation_retry_state', 'artifact_transfers', 'image_overlay_ownership', 'volume_attachments', 'keystone_domains', 'keystone_projects', 'keystone_users', 'keystone_roles', 'keystone_role_assignments', 'keystone_services', 'keystone_endpoints', 'keystone_regions')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if table_count != 18 {
            return Err(StoreError::Corrupt("required table is missing".to_owned()));
        }

        Ok(())
    }

    pub async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "INSERT INTO volume_attachments (id, server_id, volume_id, device, tag, delete_on_termination, created_at, status, operation_id, idempotency_key, cinder_attachment_id, connector_host, connector_ip, connector_initiator, driver_volume_type, target_iqn, target_portal, target_lun, connection_info_digest, error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.to_string())
        .bind(record.server_id.to_string())
        .bind(record.volume_id.to_string())
        .bind(&record.device)
        .bind(&record.tag)
        .bind(if record.delete_on_termination { 1 } else { 0 })
        .bind(&record.created_at)
        .bind(&record.status)
        .bind(record.operation_id.map(|id| id.to_string()))
        .bind(&record.idempotency_key)
        .bind(&record.cinder_attachment_id)
        .bind(&record.connector_host)
        .bind(&record.connector_ip)
        .bind(&record.connector_initiator)
        .bind(&record.driver_volume_type)
        .bind(&record.target_iqn)
        .bind(&record.target_portal)
        .bind(record.target_lun.map(i64::from))
        .bind(&record.connection_info_digest)
        .bind(&record.error)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    /// Advances (or regresses) an attachment's durable phase. Phase is
    /// persisted before the matching external side effect.
    pub async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        sqlx::query("UPDATE volume_attachments SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    /// Persists the non-secret outcome data observed after an external step.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        // Phase transition persistence: None leaves the durable field untouched
        // (COALESCE), so a transition that only updates status/device/one field
        // never wipes the connector or connection-information data persisted by
        // an earlier phase.
        sqlx::query(
            "UPDATE volume_attachments SET status = ?, cinder_attachment_id = COALESCE(?, cinder_attachment_id), connector_host = COALESCE(?, connector_host), connector_ip = COALESCE(?, connector_ip), connector_initiator = COALESCE(?, connector_initiator), driver_volume_type = COALESCE(?, driver_volume_type), target_iqn = COALESCE(?, target_iqn), target_portal = COALESCE(?, target_portal), target_lun = COALESCE(?, target_lun), connection_info_digest = COALESCE(?, connection_info_digest), device = COALESCE(?, device) WHERE id = ?",
        )
        .bind(status)
        .bind(cinder_attachment_id)
        .bind(connector_host)
        .bind(connector_ip)
        .bind(connector_initiator)
        .bind(driver_volume_type)
        .bind(target_iqn)
        .bind(target_portal)
        .bind(target_lun.map(i64::from))
        .bind(connection_info_digest)
        .bind(device)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        self.get_volume_attachment_by_id(id)
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    pub async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE volume_id = ?")
            .bind(volume_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE idempotency_key = ?")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    /// Lists non-terminal attachments for restart reconciliation.
    pub async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        if terminal.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = terminal.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let query = format!(
            "SELECT * FROM volume_attachments WHERE status NOT IN ({placeholders}) ORDER BY created_at"
        );
        let mut builder = sqlx::query(&query);
        for status in terminal {
            builder = builder.bind(status);
        }
        let rows = builder
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter().map(Self::volume_attachment_from_row).collect()
    }

    pub async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM volume_attachments WHERE server_id = ? ORDER BY created_at")
                .bind(server_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(StoreError::Database)?;

        rows.iter().map(Self::volume_attachment_from_row).collect()
    }

    pub async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        let row = sqlx::query("SELECT * FROM volume_attachments WHERE server_id = ? AND id = ?")
            .bind(server_id.to_string())
            .bind(attachment_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        row.map(|r| Self::volume_attachment_from_row(&r))
            .transpose()
    }

    pub async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM volume_attachments WHERE server_id = ? AND id = ?")
            .bind(server_id.to_string())
            .bind(attachment_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        if result.rows_affected() == 0 {
            Err(StoreError::ResourceNotFound)
        } else {
            Ok(())
        }
    }

    pub async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_domains (id, name, description, enabled, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description, enabled=excluded.enabled")
            .bind(&domain.id)
            .bind(&domain.name)
            .bind(&domain.description)
            .bind(if domain.enabled { 1 } else { 0 })
            .bind(&domain.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_domain_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneDomainRecord>, StoreError> {
        let row = sqlx::query("SELECT id, name, description, enabled, created_at FROM keystone_domains WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneDomainRecord {
            id: r.get("id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_projects (id, domain_id, name, description, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description, enabled=excluded.enabled")
            .bind(&project.id)
            .bind(&project.domain_id)
            .bind(&project.name)
            .bind(&project.description)
            .bind(if project.enabled { 1 } else { 0 })
            .bind(&project.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_project_by_name(
        &self,
        domain_id: &str,
        name: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE domain_id = ? AND name = ?")
            .bind(domain_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneProjectRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_users (id, domain_id, name, password_hash, email, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET password_hash=excluded.password_hash, enabled=excluded.enabled")
            .bind(&user.id)
            .bind(&user.domain_id)
            .bind(&user.name)
            .bind(&user.password_hash)
            .bind(&user.email)
            .bind(if user.enabled { 1 } else { 0 })
            .bind(&user.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_user_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneUserRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneUserRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            password_hash: r.get("password_hash"),
            email: r.get("email"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_roles (id, name, description, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description")
            .bind(&role.id)
            .bind(&role.name)
            .bind(&role.description)
            .bind(&role.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn get_keystone_role_by_name(
        &self,
        name: &str,
    ) -> Result<Option<KeystoneRoleRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at FROM keystone_roles WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?;

        Ok(row.map(|r| KeystoneRoleRecord {
            id: r.get("id"),
            name: r.get("name"),
            description: r.get("description"),
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_role_assignments (id, user_id, project_id, role_id, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(user_id, project_id, role_id) DO NOTHING")
            .bind(&assignment.id)
            .bind(&assignment.user_id)
            .bind(&assignment.project_id)
            .bind(&assignment.role_id)
            .bind(&assignment.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_user_role_names(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query("SELECT r.name FROM keystone_roles r JOIN keystone_role_assignments ra ON r.id = ra.role_id WHERE ra.user_id = ? AND ra.project_id = ?")
            .bind(user_id)
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows.into_iter().map(|r| r.get("name")).collect())
    }

    pub async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_services (id, name, type, description, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name=excluded.name, type=excluded.type, enabled=excluded.enabled")
            .bind(&service.id)
            .bind(&service.name)
            .bind(&service.r#type)
            .bind(&service.description)
            .bind(if service.enabled { 1 } else { 0 })
            .bind(&service.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, type, description, enabled, created_at FROM keystone_services WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneServiceRecord {
                id: r.get("id"),
                name: r.get("name"),
                r#type: r.get("type"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_endpoints (id, service_id, interface, url, region, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET url=excluded.url, enabled=excluded.enabled")
            .bind(&endpoint.id)
            .bind(&endpoint.service_id)
            .bind(&endpoint.interface)
            .bind(&endpoint.url)
            .bind(&endpoint.region)
            .bind(if endpoint.enabled { 1 } else { 0 })
            .bind(&endpoint.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, service_id, interface, url, region, enabled, created_at FROM keystone_endpoints WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneEndpointRecord {
                id: r.get("id"),
                service_id: r.get("service_id"),
                interface: r.get("interface"),
                url: r.get("url"),
                region: r.get("region"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, description, parent_region_id, enabled, created_at FROM keystone_regions WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;

        Ok(rows
            .into_iter()
            .map(|r| KeystoneRegionRecord {
                id: r.get("id"),
                description: r.get("description"),
                parent_region_id: r.get("parent_region_id"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO keystone_regions (id, description, parent_region_id, enabled, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET description=excluded.description, parent_region_id=excluded.parent_region_id, enabled=excluded.enabled")
            .bind(&region.id)
            .bind(&region.description)
            .bind(&region.parent_region_id)
            .bind(if region.enabled { 1 } else { 0 })
            .bind(&region.created_at)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(())
    }

    pub async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, description, enabled, created_at FROM keystone_domains WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneDomainRecord {
                id: r.get("id"),
                name: r.get("name"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneProjectRecord {
                id: r.get("id"),
                domain_id: r.get("domain_id"),
                name: r.get("name"),
                description: r.get("description"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE enabled = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneUserRecord {
                id: r.get("id"),
                domain_id: r.get("domain_id"),
                name: r.get("name"),
                password_hash: r.get("password_hash"),
                email: r.get("email"),
                enabled: r.get::<i32, _>("enabled") != 0,
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, name, description, created_at FROM keystone_roles")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneRoleRecord {
                id: r.get("id"),
                name: r.get("name"),
                description: r.get("description"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, user_id, project_id, role_id, created_at FROM keystone_role_assignments",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        Ok(rows
            .into_iter()
            .map(|r| KeystoneRoleAssignmentRecord {
                id: r.get("id"),
                user_id: r.get("user_id"),
                project_id: r.get("project_id"),
                role_id: r.get("role_id"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn get_keystone_user_by_id(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneUserRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, password_hash, email, enabled, created_at FROM keystone_users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(row.map(|r| KeystoneUserRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            password_hash: r.get("password_hash"),
            email: r.get("email"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn get_keystone_project_by_id(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneProjectRecord>, StoreError> {
        let row = sqlx::query("SELECT id, domain_id, name, description, enabled, created_at FROM keystone_projects WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        Ok(row.map(|r| KeystoneProjectRecord {
            id: r.get("id"),
            domain_id: r.get("domain_id"),
            name: r.get("name"),
            description: r.get("description"),
            enabled: r.get::<i32, _>("enabled") != 0,
            created_at: r.get("created_at"),
        }))
    }

    pub async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        let (key_type, fingerprint, canonical) = validate_public_key(&keypair.public_key)?;
        if keypair.key_type != key_type
            || keypair.fingerprint != fingerprint
            || keypair.public_key != canonical
        {
            return Err(StoreError::InvalidKeypair(
                "keypair record is not canonical".to_owned(),
            ));
        }
        let result = sqlx::query("INSERT INTO keypairs (id, user_id, project_id, name, key_type, public_key, fingerprint, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(keypair.id.to_string()).bind(&keypair.user_id).bind(&keypair.project_id)
            .bind(&keypair.name).bind(&keypair.key_type).bind(&keypair.public_key)
            .bind(&keypair.fingerprint).bind(&keypair.created_at).execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::KeypairAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    pub async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, user_id, project_id, name, key_type, public_key, fingerprint, created_at FROM keypairs WHERE user_id = ? AND project_id = ? ORDER BY name")
            .bind(user_id).bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(keypair_from_row).collect()
    }

    pub async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        let row = sqlx::query("SELECT id, user_id, project_id, name, key_type, public_key, fingerprint, created_at FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?")
            .bind(user_id).bind(project_id).bind(name).fetch_optional(&self.pool).await.map_err(StoreError::Database)?
            .ok_or(StoreError::KeypairNotFound)?;
        keypair_from_row(&row)
    }

    pub async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let attached: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM server_keypairs WHERE keypair_id = (SELECT id FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?)")
            .bind(user_id).bind(project_id).bind(name).fetch_one(&mut *transaction).await.map_err(StoreError::Database)?;
        if attached > 0 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairInUse);
        }
        let pending_reference: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resources WHERE project_id = ? AND kind = 'compute_instance' AND observed_state != 'DELETED' AND EXISTS (SELECT 1 FROM operations WHERE operations.resource_id = resources.id AND operations.kind = 'create' AND operations.state IN ('pending', 'running', 'unknown_outcome')) AND (json_extract(desired_state, '$.keypair_id') = (SELECT id FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?) OR (json_extract(desired_state, '$.keypair_id') IS NULL AND json_extract(desired_state, '$.key_name') = ?))",
        )
        .bind(project_id)
        .bind(user_id)
        .bind(project_id)
        .bind(name)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::Database)?;
        if pending_reference > 0 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairInUse);
        }
        let result =
            sqlx::query("DELETE FROM keypairs WHERE user_id = ? AND project_id = ? AND name = ?")
                .bind(user_id)
                .bind(project_id)
                .bind(name)
                .execute(&mut *transaction)
                .await
                .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            Err(StoreError::KeypairNotFound)
        } else {
            Ok(())
        }
    }

    pub async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let owned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resources JOIN keypairs ON keypairs.project_id = resources.project_id WHERE resources.id = ? AND resources.kind = 'compute_instance' AND keypairs.id = ?")
            .bind(server_id.to_string()).bind(keypair_id.to_string()).fetch_one(&mut *transaction).await.map_err(StoreError::Database)?;
        if owned != 1 {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Err(StoreError::KeypairOwnershipConflict);
        }
        sqlx::query("INSERT INTO server_keypairs (server_id, keypair_id) VALUES (?, ?)")
            .bind(server_id.to_string())
            .bind(keypair_id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)
    }

    pub async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM server_keypairs WHERE server_id = ?")
            .bind(server_id.to_string())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(StoreError::Database)
    }

    pub async fn get_server_keypair_name(
        &self,
        server_id: Uuid,
    ) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT keypairs.name FROM server_keypairs JOIN keypairs ON keypairs.id = server_keypairs.keypair_id WHERE server_keypairs.server_id = ?")
            .bind(server_id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)
    }
    /// Runs one attempt of the observation update inside a BEGIN IMMEDIATE
    /// transaction. Errors are rolled back best-effort; the original error
    /// stays authoritative.
    async fn apply_observation_update(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError> {
        let mut connection = self.pool.acquire().await.map_err(StoreError::Database)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(StoreError::Database)?;
        let outcome = self
            .observation_update_in_transaction(&mut connection, id, update)
            .await;
        match outcome {
            Ok(record) => match sqlx::query("COMMIT").execute(&mut *connection).await {
                Ok(_) => Ok(record),
                Err(error) => {
                    let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                    Err(StoreError::Database(error))
                }
            },
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    async fn observation_update_in_transaction(
        &self,
        connection: &mut sqlx::sqlite::SqliteConnection,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError> {
        let ObservationUpdate {
            expected_generation,
            desired_state,
            observed_state,
            observed_generation,
            provider_id,
            agent_epoch,
            observation_sequence,
        } = update;
        let transaction = connection;
        let resource_row = sqlx::query("SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        let current = resource_from_row(&resource_row)?;
        if current.generation != *expected_generation {
            return Err(StoreError::StaleGeneration);
        }
        let watermark = sqlx::query(
            "SELECT agent_epoch, observation_sequence FROM observation_watermarks WHERE resource_id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::Database)?;
        if let Some(watermark) = watermark {
            let previous_epoch: String = watermark.get("agent_epoch");
            let previous_sequence: i64 = watermark.get("observation_sequence");
            if previous_epoch == *agent_epoch
                && *observation_sequence <= u64::try_from(previous_sequence).unwrap_or(u64::MAX)
            {
                // Already applied: committing the read-only transaction is
                // equivalent to the previous explicit rollback.
                return Ok(current);
            }
        }
        sqlx::query("UPDATE resources SET generation = generation + 1, desired_state = ?, observed_state = ?, observed_generation = ?, provider_id = ? WHERE id = ? AND generation = ?")
            .bind(*desired_state)
            .bind(*observed_state)
            .bind(*observed_generation)
            .bind(*provider_id)
            .bind(id.to_string())
            .bind(*expected_generation)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        sqlx::query("INSERT INTO observation_watermarks (resource_id, agent_epoch, observation_sequence) VALUES (?, ?, ?) ON CONFLICT(resource_id) DO UPDATE SET agent_epoch = excluded.agent_epoch, observation_sequence = excluded.observation_sequence")
            .bind(id.to_string())
            .bind(*agent_epoch)
            .bind(i64::try_from(*observation_sequence).map_err(|_| StoreError::Corrupt("observation sequence exceeds SQLite range".to_owned()))?)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        let updated_row = sqlx::query("SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        let updated = resource_from_row(&updated_row)?;
        Ok(updated)
    }
}

fn keypair_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<KeypairRecord, StoreError> {
    Ok(KeypairRecord {
        id: parse_uuid(row.get("id"))?,
        user_id: row.get("user_id"),
        project_id: row.get("project_id"),
        name: row.get("name"),
        key_type: row.get("key_type"),
        public_key: row.get("public_key"),
        fingerprint: row.get("fingerprint"),
        created_at: row.get("created_at"),
    })
}

/// Validate the public OpenSSH key form accepted by the TestLab profile.
/// This deliberately imports public material only; private-key generation is not supported.
pub fn validate_public_key(value: &str) -> Result<(String, String, String), StoreError> {
    let value = value.trim();
    if value.chars().any(char::is_control) {
        return Err(StoreError::InvalidKeypair(
            "public key contains control characters".to_owned(),
        ));
    }
    let mut fields = value.split_whitespace();
    let key_type = fields
        .next()
        .ok_or_else(|| StoreError::InvalidKeypair("public key is empty".to_owned()))?;
    if !matches!(key_type, "ssh-ed25519" | "ssh-rsa" | "ecdsa-sha2-nistp256") {
        return Err(StoreError::InvalidKeypair(
            "unsupported public key type".to_owned(),
        ));
    }
    let encoded = fields
        .next()
        .ok_or_else(|| StoreError::InvalidKeypair("public key data is missing".to_owned()))?;
    let comment = fields.collect::<Vec<_>>().join(" ");
    if comment.len() > 256 || encoded.len() > 16_384 {
        return Err(StoreError::InvalidKeypair(
            "public key is too large".to_owned(),
        ));
    }
    let decoded = BASE64
        .decode(encoded)
        .map_err(|_| StoreError::InvalidKeypair("public key data is not base64".to_owned()))?;
    if decoded.is_empty() {
        return Err(StoreError::InvalidKeypair(
            "public key data is empty".to_owned(),
        ));
    }
    let mut cursor = 0;
    let embedded_type = ssh_string(&decoded, &mut cursor)?;
    if embedded_type != key_type.as_bytes() {
        return Err(StoreError::InvalidKeypair(
            "key type does not match public key data".to_owned(),
        ));
    }
    match key_type {
        "ssh-ed25519" => {
            let key_data = ssh_string(&decoded, &mut cursor)?;
            if key_data.len() != 32 || cursor != decoded.len() {
                return Err(StoreError::InvalidKeypair(
                    "ed25519 key data has the wrong length".to_owned(),
                ));
            }
        }
        "ssh-rsa" => {
            let exponent = ssh_string(&decoded, &mut cursor)?;
            let modulus = ssh_string(&decoded, &mut cursor)?;
            if exponent.is_empty() || modulus.is_empty() || cursor != decoded.len() {
                return Err(StoreError::InvalidKeypair(
                    "rsa key data is invalid".to_owned(),
                ));
            }
        }
        "ecdsa-sha2-nistp256" => {
            let curve = ssh_string(&decoded, &mut cursor)?;
            let point = ssh_string(&decoded, &mut cursor)?;
            if curve != b"nistp256"
                || point.len() != 65
                || point.first() != Some(&4)
                || cursor != decoded.len()
            {
                return Err(StoreError::InvalidKeypair(
                    "ecdsa key data is invalid".to_owned(),
                ));
            }
        }
        _ => unreachable!(),
    }
    let digest = Md5::digest(&decoded);
    let fingerprint = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    Ok((
        key_type.to_owned(),
        fingerprint,
        format!("{key_type} {}", BASE64.encode(decoded)),
    ))
}

fn ssh_string<'a>(data: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], StoreError> {
    let header_end = cursor
        .checked_add(4)
        .ok_or_else(|| StoreError::InvalidKeypair("truncated public key data".to_owned()))?;
    let header = data
        .get(*cursor..header_end)
        .ok_or_else(|| StoreError::InvalidKeypair("truncated public key data".to_owned()))?;
    let length = u32::from_be_bytes(
        header
            .try_into()
            .map_err(|_| StoreError::InvalidKeypair("invalid public key length".to_owned()))?,
    ) as usize;
    let end = header_end
        .checked_add(length)
        .ok_or_else(|| StoreError::InvalidKeypair("truncated public key data".to_owned()))?;
    if end > data.len() {
        return Err(StoreError::InvalidKeypair(
            "truncated public key data".to_owned(),
        ));
    }
    let value = &data[header_end..end];
    *cursor = end;
    Ok(value)
}

#[async_trait]
impl DurableStore for SqliteStore {
    async fn insert_resource(&self, resource: &ResourceRecord) -> Result<(), StoreError> {
        let result = sqlx::query(
            "INSERT INTO resources (id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(resource.id.to_string())
        .bind(&resource.kind)
        .bind(&resource.project_id)
        .bind(resource.generation)
        .bind(resource.observed_generation)
        .bind(&resource.desired_state)
        .bind(&resource.observed_state)
        .bind(&resource.provider_id)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_resource(&self, id: Uuid) -> Result<ResourceRecord, StoreError> {
        let row = sqlx::query("SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ResourceNotFound)?;
        resource_from_row(&row)
    }

    async fn list_resources(
        &self,
        project_id: &str,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id FROM resources WHERE project_id = ? AND kind = ? ORDER BY id")
            .bind(project_id)
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter().map(resource_from_row).collect()
    }

    async fn update_resource(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError> {
        let result = sqlx::query("UPDATE resources SET generation = generation + 1, desired_state = ?, observed_state = ?, observed_generation = ?, provider_id = ? WHERE id = ? AND generation = ?")
            .bind(desired_state)
            .bind(observed_state)
            .bind(observed_generation)
            .bind(provider_id)
            .bind(id.to_string())
            .bind(expected_generation)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return match self.get_resource(id).await {
                Ok(_) => Err(StoreError::StaleGeneration),
                Err(StoreError::ResourceNotFound) => Err(StoreError::ResourceNotFound),
                Err(error) => Err(error),
            };
        }
        self.get_resource(id).await
    }

    async fn update_resource_from_observation(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError> {
        // A deferred read-then-write transaction in WAL mode can fail
        // immediately with SQLITE_BUSY when a concurrent connection holds the
        // write lock: SQLite declines to invoke the busy handler when waiting
        // would deadlock a lock promotion (proven by run local-1785957445,
        // issue #487, where the observation update failed 6ms after start and
        // the resource stayed `requested` forever). BEGIN IMMEDIATE acquires
        // the write lock up front so the configured busy_timeout is honoured,
        // and the bounded retry below absorbs any residual busy window. The
        // update is transactional and idempotent (generation check plus
        // watermark dedup), so retrying a failed attempt is safe.
        let mut backoff = Duration::from_millis(10);
        for attempt in 0..SQLITE_BUSY_MAX_ATTEMPTS {
            match self.apply_observation_update(id, update).await {
                Err(StoreError::Database(error))
                    if is_sqlite_busy(&error) && attempt + 1 < SQLITE_BUSY_MAX_ATTEMPTS =>
                {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                outcome => return outcome,
            }
        }
        unreachable!("the loop returns on the final attempt")
    }

    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO operations (id, resource_id, kind, state, provider_operation_id, error_category, error_message) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(operation.id.to_string())
            .bind(operation.resource_id.to_string())
            .bind(&operation.kind)
            .bind(operation.state.as_str())
            .bind(&operation.provider_operation_id)
            .bind(&operation.error_category)
            .bind(&operation.error_message)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(StoreError::Database)
    }

    async fn get_operation(&self, id: Uuid) -> Result<OperationRecord, StoreError> {
        let row = sqlx::query("SELECT id, resource_id, kind, state, provider_operation_id, error_category, error_message FROM operations WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        operation_from_row(&row)
    }

    async fn update_operation(
        &self,
        id: Uuid,
        state: OperationState,
        provider_operation_id: Option<&str>,
        error_category: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationRecord, StoreError> {
        let result = sqlx::query("UPDATE operations SET state = ?, provider_operation_id = ?, error_category = ?, error_message = ? WHERE id = ?")
            .bind(state.as_str())
            .bind(provider_operation_id)
            .bind(error_category)
            .bind(error_message)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::OperationNotFound);
        }
        self.get_operation(id).await
    }

    async fn attach_provider_reference(
        &self,
        reference: &ProviderReference,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("INSERT INTO provider_refs (resource_id, provider_name, provider_resource_id) VALUES (?, ?, ?)")
            .bind(reference.resource_id.to_string())
            .bind(&reference.provider_name)
            .bind(&reference.provider_resource_id)
            .execute(&self.pool)
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ProviderReferenceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_provider_reference(
        &self,
        resource_id: Uuid,
        provider_name: &str,
    ) -> Result<ProviderReference, StoreError> {
        let row = sqlx::query("SELECT resource_id, provider_name, provider_resource_id FROM provider_refs WHERE resource_id = ? AND provider_name = ?")
            .bind(resource_id.to_string())
            .bind(provider_name)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ProviderReferenceNotFound)?;
        Ok(ProviderReference {
            resource_id: parse_uuid(row.get("resource_id"))?,
            provider_name: row.get("provider_name"),
            provider_resource_id: row.get("provider_resource_id"),
        })
    }

    async fn insert_agent_command(
        &self,
        command: &AgentCommandRecord,
    ) -> Result<AgentCommandRecord, StoreError> {
        let result = sqlx::query(
            "INSERT INTO agent_commands (command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&command.command_id)
        .bind(&command.idempotency_key)
        .bind(command.operation_id.to_string())
        .bind(command.resource_id.to_string())
        .bind(&command.agent_id)
        .bind(&command.agent_epoch)
        .bind(&command.payload_fingerprint_sha256)
        .bind(&command.payload)
        .bind(command.state.as_str())
        .bind(sqlite_sequence(command.accepted_sequence)?)
        .bind(sqlite_sequence(command.last_sequence)?)
        .bind(&command.provider_operation_id)
        .bind(&command.provider_resource_id)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(command.clone()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                let existing = self
                    .get_agent_command_by_idempotency_key(&command.idempotency_key)
                    .await?;
                if existing.command_id == command.command_id
                    && existing.operation_id == command.operation_id
                    && existing.resource_id == command.resource_id
                    && existing.agent_id == command.agent_id
                    && existing.agent_epoch == command.agent_epoch
                    && existing.payload_fingerprint_sha256 == command.payload_fingerprint_sha256
                    && existing.payload == command.payload
                {
                    Ok(existing)
                } else {
                    Err(StoreError::Corrupt(
                        "agent command idempotency identity conflicts with durable state"
                            .to_owned(),
                    ))
                }
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_agent_command(&self, command_id: &str) -> Result<AgentCommandRecord, StoreError> {
        let row = sqlx::query("SELECT command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id FROM agent_commands WHERE command_id = ?")
            .bind(command_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        agent_command_from_row(&row)
    }

    async fn get_agent_command_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AgentCommandRecord, StoreError> {
        let row = sqlx::query("SELECT command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id FROM agent_commands WHERE idempotency_key = ?")
            .bind(idempotency_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        agent_command_from_row(&row)
    }

    async fn get_agent_command_by_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<AgentCommandRecord, StoreError> {
        let row = sqlx::query("SELECT command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id FROM agent_commands WHERE operation_id = ? ORDER BY created_at DESC LIMIT 1")
            .bind(operation_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        agent_command_from_row(&row)
    }

    async fn update_agent_command(
        &self,
        command_id: &str,
        state: AgentCommandState,
        accepted_sequence: u64,
        last_sequence: u64,
        provider_operation_id: Option<&str>,
        provider_resource_id: Option<&str>,
    ) -> Result<AgentCommandRecord, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let row = sqlx::query("SELECT command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id FROM agent_commands WHERE command_id = ?")
            .bind(command_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::OperationNotFound)?;
        let current = agent_command_from_row(&row)?;
        if last_sequence < current.last_sequence {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Ok(current);
        }
        if last_sequence == current.last_sequence {
            if current.state == state
                && current.accepted_sequence == accepted_sequence
                && provider_operation_id
                    .is_none_or(|value| current.provider_operation_id.as_deref() == Some(value))
                && provider_resource_id
                    .is_none_or(|value| current.provider_resource_id.as_deref() == Some(value))
            {
                transaction.rollback().await.map_err(StoreError::Database)?;
                return Ok(current);
            }
            return Err(StoreError::Corrupt(
                "conflicting agent command evidence at one sequence".to_owned(),
            ));
        }
        let accepted_sequence = accepted_sequence.max(current.accepted_sequence);
        let provider_operation_id =
            provider_operation_id.or(current.provider_operation_id.as_deref());
        let provider_resource_id = provider_resource_id.or(current.provider_resource_id.as_deref());
        let result = sqlx::query("UPDATE agent_commands SET state = ?, accepted_sequence = ?, last_sequence = ?, provider_operation_id = ?, provider_resource_id = ?, updated_at = CURRENT_TIMESTAMP WHERE command_id = ? AND last_sequence = ?")
            .bind(state.as_str())
            .bind(sqlite_sequence(accepted_sequence)?)
            .bind(sqlite_sequence(last_sequence)?)
            .bind(provider_operation_id)
            .bind(provider_resource_id)
            .bind(command_id)
            .bind(sqlite_sequence(current.last_sequence)?)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::OperationNotFound);
        }
        transaction.commit().await.map_err(StoreError::Database)?;
        self.get_agent_command(command_id).await
    }

    async fn list_recoverable_agent_commands(&self) -> Result<Vec<AgentCommandRecord>, StoreError> {
        let rows = sqlx::query("SELECT command_id, idempotency_key, operation_id, resource_id, agent_id, agent_epoch, payload_fingerprint_sha256, payload, state, accepted_sequence, last_sequence, provider_operation_id, provider_resource_id FROM agent_commands WHERE state IN ('pending', 'accepted', 'running', 'retryable', 'unknown_outcome') ORDER BY created_at ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter().map(agent_command_from_row).collect()
    }

    async fn insert_artifact_transfer(
        &self,
        transfer: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        artifact_transfer::insert(&self.pool, transfer).await
    }

    async fn get_artifact_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        artifact_transfer::get(&self.pool, transfer_id).await
    }

    async fn rebind_artifact_transfer_epoch(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        new_agent_epoch: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        artifact_transfer::rebind_epoch(
            &self.pool,
            transfer_id,
            expected_agent_epoch,
            new_agent_epoch,
        )
        .await
    }

    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        artifact_transfer::update(&self.pool, transfer_id, expected_agent_epoch, update).await
    }

    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError> {
        artifact_transfer::list_recoverable(&self.pool).await
    }

    async fn insert_image_overlay(
        &self,
        overlay: &ImageOverlayOwnershipRecord,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        validate_image_overlay(overlay)?;
        let result = sqlx::query(
            "INSERT INTO image_overlay_ownership (overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&overlay.overlay_id)
        .bind(overlay.identity.resource_id.to_string())
        .bind(overlay.identity.operation_id.to_string())
        .bind(&overlay.identity.command_id)
        .bind(&overlay.identity.agent_id)
        .bind(&overlay.identity.agent_epoch)
        .bind(&overlay.identity.base_sha256)
        .bind(&overlay.identity.base_format)
        .bind(&overlay.identity.overlay_format)
        .bind(overlay.state.as_str())
        .bind(&overlay.created_at)
        .bind(&overlay.updated_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => self.get_image_overlay(&overlay.overlay_id).await,
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                let existing = self.get_image_overlay(&overlay.overlay_id).await;
                match existing {
                    Ok(existing) if image_overlay_identity_matches(&existing, overlay) => {
                        Ok(existing)
                    }
                    Ok(_) => Err(StoreError::ImageOverlayConflict(
                        "overlay identity conflicts with durable state".to_owned(),
                    )),
                    Err(StoreError::ImageOverlayNotFound) => {
                        let identity_exists: i64 = sqlx::query_scalar(
                            "SELECT COUNT(*) FROM image_overlay_ownership WHERE resource_id = ? AND operation_id = ? AND command_id = ?",
                        )
                        .bind(overlay.identity.resource_id.to_string())
                        .bind(overlay.identity.operation_id.to_string())
                        .bind(&overlay.identity.command_id)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(StoreError::Database)?;
                        if identity_exists != 0 {
                            Err(StoreError::ImageOverlayConflict(
                                "resource operation already owns an overlay".to_owned(),
                            ))
                        } else {
                            Err(StoreError::ImageOverlayNotFound)
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_image_overlay(
        &self,
        overlay_id: &str,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let row = sqlx::query(
            "SELECT overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at FROM image_overlay_ownership WHERE overlay_id = ?",
        )
        .bind(overlay_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::Database)?
        .ok_or(StoreError::ImageOverlayNotFound)?;
        image_overlay_from_row(&row)
    }

    async fn update_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
        update: ImageOverlayUpdate,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        validate_image_overlay_identity(expected_identity)?;
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let row = sqlx::query(
            "SELECT overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at FROM image_overlay_ownership WHERE overlay_id = ?",
        )
        .bind(overlay_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::Database)?
        .ok_or(StoreError::ImageOverlayNotFound)?;
        let current = image_overlay_from_row(&row)?;
        ensure_image_overlay_identity(&current, expected_identity)?;
        validate_image_overlay_transition(current.state, update.state)?;
        if current.state == update.state {
            transaction.rollback().await.map_err(StoreError::Database)?;
            return Ok(current);
        }
        let result = sqlx::query(
            "UPDATE image_overlay_ownership SET state = ?, updated_at = CURRENT_TIMESTAMP WHERE overlay_id = ? AND resource_id = ? AND operation_id = ? AND command_id = ? AND agent_id = ? AND agent_epoch = ? AND base_sha256 = ? AND base_format = ? AND overlay_format = ? AND state = ?",
        )
        .bind(update.state.as_str())
        .bind(overlay_id)
        .bind(expected_identity.resource_id.to_string())
        .bind(expected_identity.operation_id.to_string())
        .bind(&expected_identity.command_id)
        .bind(&expected_identity.agent_id)
        .bind(&expected_identity.agent_epoch)
        .bind(&expected_identity.base_sha256)
        .bind(&expected_identity.base_format)
        .bind(&expected_identity.overlay_format)
        .bind(current.state.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::Database)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::ImageOverlayConflict(
                "concurrent overlay state change".to_owned(),
            ));
        }
        transaction.commit().await.map_err(StoreError::Database)?;
        self.get_image_overlay(overlay_id).await
    }

    async fn list_image_overlays(
        &self,
        resource_id: Uuid,
    ) -> Result<Vec<ImageOverlayOwnershipRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT overlay_id, resource_id, operation_id, command_id, agent_id, agent_epoch, base_sha256, base_format, overlay_format, state, created_at, updated_at FROM image_overlay_ownership WHERE resource_id = ? ORDER BY overlay_id",
        )
        .bind(resource_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        rows.iter().map(image_overlay_from_row).collect()
    }

    async fn count_image_overlay_references(
        &self,
        base_sha256: &str,
        base_format: &str,
    ) -> Result<u64, StoreError> {
        validate_base_identity(base_sha256, base_format)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM image_overlay_ownership WHERE base_sha256 = ? AND base_format = ? AND state != 'deleted'",
        )
        .bind(base_sha256)
        .bind(base_format)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        u64::try_from(count)
            .map_err(|_| StoreError::Corrupt("negative overlay reference count".to_owned()))
    }

    async fn delete_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        let current = self.get_image_overlay(overlay_id).await?;
        if current.state == ImageOverlayState::Deleted {
            ensure_image_overlay_identity(&current, expected_identity)?;
            return Ok(current);
        }
        let deleting = self
            .update_image_overlay(
                overlay_id,
                expected_identity,
                ImageOverlayUpdate {
                    state: ImageOverlayState::Deleting,
                },
            )
            .await?;
        if deleting.state == ImageOverlayState::Deleted {
            return Ok(deleting);
        }
        self.update_image_overlay(
            overlay_id,
            expected_identity,
            ImageOverlayUpdate {
                state: ImageOverlayState::Deleted,
            },
        )
        .await
    }

    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT attempts FROM operation_retry_state WHERE operation_id = ?")
                .bind(operation_id.to_string())
                .fetch_optional(&mut *transaction)
                .await
                .map_err(StoreError::Database)?;
        let attempts = current.unwrap_or(0).saturating_add(1);
        if current.is_some() {
            sqlx::query("UPDATE operation_retry_state SET attempts = ?, updated_at = CURRENT_TIMESTAMP WHERE operation_id = ?")
                .bind(attempts)
                .bind(operation_id.to_string())
                .execute(&mut *transaction)
                .await
                .map_err(StoreError::Database)?;
        } else {
            sqlx::query("INSERT INTO operation_retry_state (operation_id, attempts) VALUES (?, ?)")
                .bind(operation_id.to_string())
                .bind(attempts)
                .execute(&mut *transaction)
                .await
                .map_err(StoreError::Database)?;
        }
        transaction.commit().await.map_err(StoreError::Database)?;
        u8::try_from(attempts)
            .map_err(|_| StoreError::Corrupt("operation retry count exceeds limit".to_owned()))
    }

    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        let insert_resource = sqlx::query("INSERT INTO resources (id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(resource.id.to_string())
            .bind(&resource.kind)
            .bind(&resource.project_id)
            .bind(resource.generation)
            .bind(resource.observed_generation)
            .bind(&resource.desired_state)
            .bind(&resource.observed_state)
            .bind(&resource.provider_id)
            .execute(&mut *transaction)
            .await;
        match insert_resource {
            Ok(_) => {}
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                return Err(StoreError::ResourceAlreadyExists);
            }
            Err(error) => return Err(StoreError::Database(error)),
        }
        sqlx::query("INSERT INTO operations (id, resource_id, state, provider_operation_id, error_category, error_message) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(operation.id.to_string())
            .bind(operation.resource_id.to_string())
            .bind(operation.state.as_str())
            .bind(&operation.provider_operation_id)
            .bind(&operation.error_category)
            .bind(&operation.error_message)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
        transaction.commit().await.map_err(StoreError::Database)
    }

    async fn readiness_check(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(StoreError::Database)
    }
}

// The port implementations delegate to the inherent adapter methods, which
// remain the canonical SQL bodies. Inherent methods take name-resolution
// precedence over trait methods, so `self.method(...)` inside these bodies
// resolves to the inherent implementation and does not recurse into the trait.
//
// Hazard: silent infinite recursion (stack exhaustion at runtime, not a
// compile error) is still possible in exactly two cases:
//   1. the inherent method is removed or renamed without deleting its
//      delegation below — the trait method is then the only candidate, and
//      the delegation calls itself (application code calls these methods
//      through the ports, so removal is not guaranteed to error elsewhere);
//   2. the inherent receiver changes from `&self` to `&mut self` — through
//      `&SqliteStore` only the `&self` trait method is applicable, so the
//      delegation resolves to itself.
// An argument or return-type drift, by contrast, is a compile error at the
// delegation site: once name resolution commits to the inherent method there
// is no fallback to the trait. Keep each delegation paired with its inherent
// method; the port conformance tests exercise every method and turn any
// recursion into a loud test failure, but they are the safety net, not the
// primary guard.

#[async_trait]
impl IdentityRepository for SqliteStore {
    async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_domain(domain).await
    }

    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        self.list_keystone_domains().await
    }

    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_project(project).await
    }

    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        self.list_keystone_projects().await
    }

    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        self.insert_keystone_user(user).await
    }

    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        self.list_keystone_users().await
    }

    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        self.insert_keystone_role(role).await
    }

    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        self.list_keystone_roles().await
    }

    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_role_assignment(assignment).await
    }

    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        self.list_keystone_role_assignments().await
    }

    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_service(service).await
    }

    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        self.list_keystone_services().await
    }

    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_endpoint(endpoint).await
    }

    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        self.list_keystone_endpoints().await
    }

    async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        self.insert_keystone_region(region).await
    }

    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        self.list_keystone_regions().await
    }
}

#[async_trait]
impl KeypairRepository for SqliteStore {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        self.insert_keypair(keypair).await
    }

    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        self.list_keypairs(user_id, project_id).await
    }

    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        self.get_keypair(user_id, project_id, name).await
    }

    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        self.delete_keypair(user_id, project_id, name).await
    }

    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        self.attach_server_keypair(server_id, keypair_id).await
    }

    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        self.detach_server_keypair(server_id).await
    }

    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError> {
        self.get_server_keypair_name(server_id).await
    }
}

#[async_trait]
impl VolumeAttachmentRepository for SqliteStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        self.insert_volume_attachment(record).await
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        self.update_volume_attachment_phase(id, status, error).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        self.update_volume_attachment_outcome(
            id,
            status,
            cinder_attachment_id,
            connector_host,
            connector_ip,
            connector_initiator,
            driver_volume_type,
            target_iqn,
            target_portal,
            target_lun,
            connection_info_digest,
            device,
        )
        .await
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_id(id).await
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_volume(volume_id).await
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment_by_idempotency(idempotency_key)
            .await
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        self.list_volume_attachments_by_status(terminal).await
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        self.list_volume_attachments(server_id).await
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        self.get_volume_attachment(server_id, attachment_id).await
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        self.delete_volume_attachment(server_id, attachment_id)
            .await
    }
}

#[async_trait]
impl ComputeRepository for SqliteStore {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError> {
        self.list_resources_by_kind(kind).await
    }
}

fn resource_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ResourceRecord, StoreError> {
    Ok(ResourceRecord {
        id: parse_uuid(row.get("id"))?,
        kind: row.get("kind"),
        project_id: row.get("project_id"),
        generation: row.get("generation"),
        observed_generation: row.get("observed_generation"),
        desired_state: row.get("desired_state"),
        observed_state: row.get("observed_state"),
        provider_id: row.get("provider_id"),
    })
}

fn operation_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<OperationRecord, StoreError> {
    Ok(OperationRecord {
        id: parse_uuid(row.get("id"))?,
        resource_id: parse_uuid(row.get("resource_id"))?,
        kind: row.get("kind"),
        state: OperationState::parse(row.get("state"))?,
        provider_operation_id: row.get("provider_operation_id"),
        error_category: row.get("error_category"),
        error_message: row.get("error_message"),
    })
}

fn sqlite_sequence(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::Corrupt("agent command sequence exceeds SQLite range".to_owned()))
}

fn agent_command_from_row(row: &SqliteRow) -> Result<AgentCommandRecord, StoreError> {
    let accepted_sequence: i64 = row.get("accepted_sequence");
    let last_sequence: i64 = row.get("last_sequence");
    Ok(AgentCommandRecord {
        command_id: row.get("command_id"),
        idempotency_key: row.get("idempotency_key"),
        operation_id: parse_uuid(row.get("operation_id"))?,
        resource_id: parse_uuid(row.get("resource_id"))?,
        agent_id: row.get("agent_id"),
        agent_epoch: row.get("agent_epoch"),
        payload_fingerprint_sha256: row.get("payload_fingerprint_sha256"),
        payload: row.get("payload"),
        state: AgentCommandState::parse(row.get::<String, _>("state").as_str())?,
        accepted_sequence: u64::try_from(accepted_sequence)
            .map_err(|_| StoreError::Corrupt("negative agent command sequence".to_owned()))?,
        last_sequence: u64::try_from(last_sequence)
            .map_err(|_| StoreError::Corrupt("negative agent command sequence".to_owned()))?,
        provider_operation_id: row.get("provider_operation_id"),
        provider_resource_id: row.get("provider_resource_id"),
    })
}

fn parse_uuid(value: String) -> Result<Uuid, StoreError> {
    Uuid::parse_str(&value).map_err(StoreError::InvalidUuid)
}

fn validate_image_overlay(overlay: &ImageOverlayOwnershipRecord) -> Result<(), StoreError> {
    bounded_overlay_text("overlay_id", &overlay.overlay_id, 128)?;
    validate_image_overlay_identity(&overlay.identity)?;
    if overlay.state.is_terminal() {
        return Err(StoreError::InvalidImageOverlay(
            "a new overlay cannot start in deleted state".to_owned(),
        ));
    }
    Ok(())
}

fn validate_image_overlay_identity(identity: &ImageOverlayIdentity) -> Result<(), StoreError> {
    bounded_overlay_text("command_id", &identity.command_id, 128)?;
    bounded_overlay_text("agent_id", &identity.agent_id, 128)?;
    bounded_overlay_text("agent_epoch", &identity.agent_epoch, 256)?;
    validate_base_identity(&identity.base_sha256, &identity.base_format)?;
    if identity.overlay_format != "qcow2" {
        return Err(StoreError::InvalidImageOverlay(
            "overlay format must be qcow2".to_owned(),
        ));
    }
    Ok(())
}

fn validate_base_identity(sha256: &str, format: &str) -> Result<(), StoreError> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidImageOverlay(
            "base checksum must be 64 hexadecimal characters".to_owned(),
        ));
    }
    if !matches!(format, "raw" | "qcow2") {
        return Err(StoreError::InvalidImageOverlay(
            "base format must be raw or qcow2".to_owned(),
        ));
    }
    Ok(())
}

fn bounded_overlay_text(name: &str, value: &str, max: usize) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(StoreError::InvalidImageOverlay(format!(
            "{name} is empty, too long, or contains control characters"
        )));
    }
    Ok(())
}

fn image_overlay_identity_matches(
    left: &ImageOverlayOwnershipRecord,
    right: &ImageOverlayOwnershipRecord,
) -> bool {
    left.overlay_id == right.overlay_id && left.identity == right.identity
}

fn ensure_image_overlay_identity(
    current: &ImageOverlayOwnershipRecord,
    expected: &ImageOverlayIdentity,
) -> Result<(), StoreError> {
    if current.identity.agent_epoch != expected.agent_epoch {
        return Err(StoreError::ImageOverlayEpochConflict);
    }
    if current.identity != *expected {
        return Err(StoreError::ImageOverlayConflict(
            "overlay identity conflicts with durable state".to_owned(),
        ));
    }
    Ok(())
}

fn validate_image_overlay_transition(
    current: ImageOverlayState,
    next: ImageOverlayState,
) -> Result<(), StoreError> {
    let allowed = match current {
        ImageOverlayState::Pending => matches!(
            next,
            ImageOverlayState::Pending
                | ImageOverlayState::Materializing
                | ImageOverlayState::Deleting
                | ImageOverlayState::Failed
        ),
        ImageOverlayState::Materializing => matches!(
            next,
            ImageOverlayState::Materializing
                | ImageOverlayState::Ready
                | ImageOverlayState::Deleting
                | ImageOverlayState::Failed
        ),
        ImageOverlayState::Ready => {
            matches!(next, ImageOverlayState::Ready | ImageOverlayState::Deleting)
        }
        ImageOverlayState::Deleting => {
            matches!(
                next,
                ImageOverlayState::Deleting | ImageOverlayState::Deleted
            )
        }
        ImageOverlayState::Deleted => next == ImageOverlayState::Deleted,
        ImageOverlayState::Failed => matches!(
            next,
            ImageOverlayState::Failed
                | ImageOverlayState::Materializing
                | ImageOverlayState::Deleting
        ),
    };
    if allowed {
        Ok(())
    } else {
        Err(StoreError::ImageOverlayConflict(format!(
            "invalid overlay state transition from {current:?} to {next:?}"
        )))
    }
}

fn image_overlay_from_row(row: &SqliteRow) -> Result<ImageOverlayOwnershipRecord, StoreError> {
    let record = ImageOverlayOwnershipRecord {
        overlay_id: row.get("overlay_id"),
        identity: ImageOverlayIdentity {
            resource_id: parse_uuid(row.get("resource_id"))?,
            operation_id: parse_uuid(row.get("operation_id"))?,
            command_id: row.get("command_id"),
            agent_id: row.get("agent_id"),
            agent_epoch: row.get("agent_epoch"),
            base_sha256: row.get("base_sha256"),
            base_format: row.get("base_format"),
            overlay_format: row.get("overlay_format"),
        },
        state: ImageOverlayState::parse(row.get::<String, _>("state").as_str())?,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    };
    bounded_overlay_text("overlay_id", &record.overlay_id, 128)?;
    validate_image_overlay_identity(&record.identity)?;
    Ok(record)
}

/// Runs the behavior shared by every durable store adapter.
pub async fn run_conformance<S: DurableStore>(store: &S) -> Result<(), StoreError> {
    let resource = ResourceRecord {
        id: Uuid::now_v7(),
        kind: "server".to_owned(),
        project_id: "project-a".to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "requested".to_owned(),
        observed_state: "unknown".to_owned(),
        provider_id: Some("provider-1".to_owned()),
    };
    store.insert_resource(&resource).await?;
    assert_eq!(store.get_resource(resource.id).await?, resource);
    assert_eq!(store.list_resources("project-a", "server").await?.len(), 1);
    assert!(matches!(
        store
            .update_resource(resource.id, 0, "active", "running", 1, Some("provider-1"))
            .await,
        Err(StoreError::StaleGeneration)
    ));
    let updated = store
        .update_resource(resource.id, 1, "active", "running", 1, Some("provider-1"))
        .await?;
    assert_eq!(updated.generation, 2);
    let operation = OperationRecord {
        id: Uuid::now_v7(),
        resource_id: resource.id,
        kind: "test".to_owned(),
        state: OperationState::UnknownOutcome,
        provider_operation_id: Some("provider-op-1".to_owned()),
        error_category: Some("unknown_outcome".to_owned()),
        error_message: Some("acceptance could not be confirmed".to_owned()),
    };
    store.insert_operation(&operation).await?;
    assert_eq!(store.get_operation(operation.id).await?, operation);
    let updated_operation = store
        .update_operation(
            operation.id,
            OperationState::Succeeded,
            Some("provider-op-1"),
            None,
            None,
        )
        .await?;
    assert_eq!(updated_operation.state, OperationState::Succeeded);
    let reference = ProviderReference {
        resource_id: resource.id,
        provider_name: "fake".to_owned(),
        provider_resource_id: "instance-1".to_owned(),
    };
    store.attach_provider_reference(&reference).await?;
    assert_eq!(
        store.get_provider_reference(resource.id, "fake").await?,
        reference
    );
    Ok(())
}

/// Runs the behavior shared by every identity repository adapter: each record
/// kind round-trips through its insert/list pair, and the deterministic
/// bootstrap upserts are idempotent for the same identity.
pub async fn run_identity_repository_conformance<S: IdentityRepository>(
    store: &S,
) -> Result<(), StoreError> {
    let now = "2026-08-07T00:00:00Z".to_owned();
    let domain = KeystoneDomainRecord {
        id: "default".to_owned(),
        name: "Default".to_owned(),
        description: Some("Default domain".to_owned()),
        enabled: true,
        created_at: now.clone(),
    };
    store.insert_keystone_domain(&domain).await?;
    store.insert_keystone_domain(&domain).await?;
    assert_eq!(store.list_keystone_domains().await?, vec![domain.clone()]);

    let project = KeystoneProjectRecord {
        id: "project-a".to_owned(),
        domain_id: domain.id.clone(),
        name: "admin".to_owned(),
        description: None,
        enabled: true,
        created_at: now.clone(),
    };
    store.insert_keystone_project(&project).await?;
    store.insert_keystone_project(&project).await?;
    assert_eq!(store.list_keystone_projects().await?, vec![project.clone()]);

    let user = KeystoneUserRecord {
        id: "user-a".to_owned(),
        domain_id: domain.id.clone(),
        name: "admin".to_owned(),
        password_hash: "pbkdf2_sha256$1$test".to_owned(),
        email: None,
        enabled: true,
        created_at: now.clone(),
    };
    store.insert_keystone_user(&user).await?;
    store.insert_keystone_user(&user).await?;
    assert_eq!(store.list_keystone_users().await?, vec![user.clone()]);

    let role = KeystoneRoleRecord {
        id: "role-a".to_owned(),
        name: "admin".to_owned(),
        description: None,
        created_at: now.clone(),
    };
    store.insert_keystone_role(&role).await?;
    store.insert_keystone_role(&role).await?;
    assert_eq!(store.list_keystone_roles().await?, vec![role.clone()]);

    let assignment = KeystoneRoleAssignmentRecord {
        id: "assignment-0".to_owned(),
        user_id: user.id.clone(),
        project_id: project.id.clone(),
        role_id: role.id.clone(),
        created_at: now.clone(),
    };
    store.insert_keystone_role_assignment(&assignment).await?;
    store.insert_keystone_role_assignment(&assignment).await?;
    assert_eq!(
        store.list_keystone_role_assignments().await?,
        vec![assignment]
    );

    let service = KeystoneServiceRecord {
        id: "service-a".to_owned(),
        name: "identity".to_owned(),
        r#type: "identity".to_owned(),
        description: None,
        enabled: true,
        created_at: now.clone(),
    };
    store.insert_keystone_service(&service).await?;
    store.insert_keystone_service(&service).await?;
    assert_eq!(store.list_keystone_services().await?, vec![service.clone()]);

    let endpoint = KeystoneEndpointRecord {
        id: "endpoint-a".to_owned(),
        service_id: service.id.clone(),
        interface: "public".to_owned(),
        url: "http://127.0.0.1:8080/v3".to_owned(),
        region: "RegionOne".to_owned(),
        enabled: true,
        created_at: now.clone(),
    };
    store.insert_keystone_endpoint(&endpoint).await?;
    store.insert_keystone_endpoint(&endpoint).await?;
    assert_eq!(store.list_keystone_endpoints().await?, vec![endpoint]);

    let region = KeystoneRegionRecord {
        id: "RegionOne".to_owned(),
        description: None,
        parent_region_id: None,
        enabled: true,
        created_at: now,
    };
    store.insert_keystone_region(&region).await?;
    store.insert_keystone_region(&region).await?;
    assert_eq!(store.list_keystone_regions().await?, vec![region]);
    Ok(())
}

/// Runs the behavior shared by every keypair repository adapter: scoped
/// uniqueness, canonical record acceptance, attach/detach against a durable
/// server, in-use protection, and scoped delete.
pub async fn run_keypair_repository_conformance<S: KeypairRepository + DurableStore>(
    store: &S,
) -> Result<(), StoreError> {
    let resource = ResourceRecord {
        id: Uuid::now_v7(),
        kind: "compute_instance".to_owned(),
        project_id: "project-a".to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "{\"key_name\": \"other\"}".to_owned(),
        observed_state: "BUILD".to_owned(),
        provider_id: None,
    };
    store.insert_resource(&resource).await?;

    let blob = [
        0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9', 0, 0, 0, 32,
    ]
    .into_iter()
    .chain([9_u8; 32])
    .collect::<Vec<_>>();
    let (key_type, fingerprint, canonical) =
        validate_public_key(&format!("ssh-ed25519 {}", BASE64.encode(blob)))?;
    let keypair = KeypairRecord {
        id: Uuid::now_v7(),
        user_id: "user-a".to_owned(),
        project_id: "project-a".to_owned(),
        name: "test-key".to_owned(),
        key_type,
        public_key: canonical,
        fingerprint,
        created_at: "1".to_owned(),
    };
    store.insert_keypair(&keypair).await?;
    assert!(matches!(
        store.insert_keypair(&keypair).await,
        Err(StoreError::KeypairAlreadyExists)
    ));
    assert_eq!(
        store.get_keypair("user-a", "project-a", "test-key").await?,
        keypair
    );
    assert!(matches!(
        store.get_keypair("user-b", "project-a", "test-key").await,
        Err(StoreError::KeypairNotFound)
    ));
    assert_eq!(store.list_keypairs("user-a", "project-a").await?.len(), 1);

    store.attach_server_keypair(resource.id, keypair.id).await?;
    assert_eq!(
        store.get_server_keypair_name(resource.id).await?,
        Some(keypair.name.clone())
    );
    assert!(matches!(
        store
            .delete_keypair("user-a", "project-a", "test-key")
            .await,
        Err(StoreError::KeypairInUse)
    ));
    store.detach_server_keypair(resource.id).await?;
    assert_eq!(store.get_server_keypair_name(resource.id).await?, None);
    store
        .delete_keypair("user-a", "project-a", "test-key")
        .await?;
    assert!(matches!(
        store
            .delete_keypair("user-a", "project-a", "test-key")
            .await,
        Err(StoreError::KeypairNotFound)
    ));
    Ok(())
}

/// Runs the behavior shared by every volume-attachment repository adapter:
/// phase and outcome persistence with COALESCE field preservation, status
/// filtering, server-scoped reads, and delete.
pub async fn run_volume_attachment_repository_conformance<
    S: VolumeAttachmentRepository + DurableStore,
>(
    store: &S,
) -> Result<(), StoreError> {
    let resource = ResourceRecord {
        id: Uuid::now_v7(),
        kind: "compute_instance".to_owned(),
        project_id: "project-a".to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "requested".to_owned(),
        observed_state: "BUILD".to_owned(),
        provider_id: None,
    };
    store.insert_resource(&resource).await?;

    let attachment = VolumeAttachmentRecord {
        id: Uuid::now_v7(),
        server_id: resource.id,
        volume_id: Uuid::now_v7(),
        device: "/dev/vdb".to_owned(),
        tag: None,
        delete_on_termination: false,
        created_at: "2026-08-07T00:00:00Z".to_owned(),
        status: "validated".to_owned(),
        operation_id: None,
        idempotency_key: Some("idem-attach-1".to_owned()),
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
    store.insert_volume_attachment(&attachment).await?;
    assert_eq!(
        store
            .get_volume_attachment_by_id(attachment.id)
            .await?
            .ok_or(StoreError::Corrupt(
                "conformance attachment missing after insert".to_owned()
            ))?,
        attachment
    );
    assert_eq!(
        store
            .get_volume_attachment_by_volume(attachment.volume_id)
            .await?
            .ok_or(StoreError::Corrupt(
                "conformance attachment missing by volume".to_owned()
            ))?,
        attachment
    );
    assert_eq!(
        store
            .get_volume_attachment_by_idempotency("idem-attach-1")
            .await?
            .ok_or(StoreError::Corrupt(
                "conformance attachment missing by idempotency".to_owned()
            ))?,
        attachment
    );

    let phased = store
        .update_volume_attachment_phase(attachment.id, "cinder_attachment_created", None)
        .await?;
    assert_eq!(phased.status, "cinder_attachment_created");
    assert!(phased.error.is_none());

    let outcome = store
        .update_volume_attachment_outcome(
            attachment.id,
            "connector_obtained",
            Some("cinder-att-1"),
            Some("compute-1"),
            Some("10.0.0.5"),
            Some("iqn.2026-08.org.o3k:node"),
            Some("iscsi"),
            None,
            None,
            None,
            None,
            None,
        )
        .await?;
    assert_eq!(
        outcome.cinder_attachment_id.as_deref(),
        Some("cinder-att-1")
    );
    assert_eq!(outcome.connector_host.as_deref(), Some("compute-1"));
    // COALESCE semantics: a later phase that only reports status/device must
    // not wipe the connector fields persisted by an earlier phase.
    let later = store
        .update_volume_attachment_outcome(
            attachment.id,
            "attached",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("/dev/vdb"),
        )
        .await?;
    assert_eq!(later.status, "attached");
    assert_eq!(later.connector_host.as_deref(), Some("compute-1"));
    assert_eq!(later.cinder_attachment_id.as_deref(), Some("cinder-att-1"));
    assert_eq!(later.device, "/dev/vdb");

    assert_eq!(store.list_volume_attachments(resource.id).await?.len(), 1);
    assert!(
        store
            .list_volume_attachments_by_status(&["attached", "detached", "error"])
            .await?
            .is_empty()
    );
    assert_eq!(
        store
            .get_volume_attachment(resource.id, attachment.id)
            .await?
            .ok_or(StoreError::Corrupt(
                "conformance attachment missing by server".to_owned()
            ))?
            .id,
        attachment.id
    );
    store
        .delete_volume_attachment(resource.id, attachment.id)
        .await?;
    assert_eq!(
        store.get_volume_attachment_by_id(attachment.id).await?,
        None
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[tokio::test]
    async fn sqlite_store_passes_conformance() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        run_conformance(&store).await
    }

    #[tokio::test]
    async fn sqlite_store_passes_extracted_repository_port_conformance() -> Result<(), StoreError> {
        let identity_store = SqliteStore::connect("sqlite::memory:").await?;
        run_identity_repository_conformance(&identity_store).await?;
        let keypair_store = SqliteStore::connect("sqlite::memory:").await?;
        run_keypair_repository_conformance(&keypair_store).await?;
        let attachment_store = SqliteStore::connect("sqlite::memory:").await?;
        run_volume_attachment_repository_conformance(&attachment_store).await?;
        let compute_store = SqliteStore::connect("sqlite::memory:").await?;
        run_keypair_repository_conformance(&compute_store).await?;
        run_volume_attachment_repository_conformance(&compute_store).await?;
        run_conformance(&compute_store).await?;
        assert_eq!(
            compute_store
                .list_resources_by_kind("compute_instance")
                .await?
                .len(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn transaction_rolls_back_when_operation_insert_fails() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "server".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: Uuid::now_v7(),
            resource_id: Uuid::now_v7(),
            kind: "test".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        assert!(
            store
                .insert_resource_and_operation(&resource, &operation)
                .await
                .is_err()
        );
        assert!(matches!(
            store.get_resource(resource.id).await,
            Err(StoreError::ResourceNotFound)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_resource_is_rejected() -> Result<(), StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "image".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: None,
        };
        store.insert_resource(&resource).await?;
        assert!(matches!(
            store.insert_resource(&resource).await,
            Err(StoreError::ResourceAlreadyExists)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn image_overlay_ownership_is_fenced_restart_safe_and_reference_counted()
    -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-image-overlay-ownership-{}.sqlite",
            std::process::id()
        ));
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "server".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: Uuid::now_v7(),
            resource_id: resource.id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let identity = ImageOverlayIdentity {
            resource_id: resource.id,
            operation_id: operation.id,
            command_id: "command-image-1".to_owned(),
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            base_sha256: "a".repeat(64),
            base_format: "qcow2".to_owned(),
            overlay_format: "qcow2".to_owned(),
        };
        let record = ImageOverlayOwnershipRecord {
            overlay_id: "overlay-1".to_owned(),
            identity: identity.clone(),
            state: ImageOverlayState::Pending,
            created_at: String::new(),
            updated_at: String::new(),
        };
        {
            let store = SqliteStore::connect_file(&path).await?;
            store
                .insert_resource_and_operation(&resource, &operation)
                .await?;
            assert_eq!(
                store.insert_image_overlay(&record).await?,
                store.get_image_overlay("overlay-1").await?
            );
            assert_eq!(
                store.insert_image_overlay(&record).await?.overlay_id,
                "overlay-1"
            );
            assert_eq!(
                store
                    .count_image_overlay_references(&"a".repeat(64), "qcow2")
                    .await?,
                1
            );
            store
                .update_image_overlay(
                    "overlay-1",
                    &identity,
                    ImageOverlayUpdate {
                        state: ImageOverlayState::Materializing,
                    },
                )
                .await?;
            store
                .update_image_overlay(
                    "overlay-1",
                    &identity,
                    ImageOverlayUpdate {
                        state: ImageOverlayState::Ready,
                    },
                )
                .await?;
            let mut stale = identity.clone();
            stale.agent_epoch = "epoch-2".to_owned();
            assert!(matches!(
                store.delete_image_overlay("overlay-1", &stale).await,
                Err(StoreError::ImageOverlayEpochConflict)
            ));
            assert_eq!(
                store
                    .delete_image_overlay("overlay-1", &identity)
                    .await?
                    .state,
                ImageOverlayState::Deleted
            );
            assert_eq!(
                store
                    .count_image_overlay_references(&"a".repeat(64), "qcow2")
                    .await?,
                0
            );
        }
        let reopened = SqliteStore::connect_file(&path).await?;
        assert_eq!(
            reopened.get_image_overlay("overlay-1").await?.state,
            ImageOverlayState::Deleted
        );
        fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_command_identity_is_idempotent_and_survives_restart()
    -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-agent-commands-{}.sqlite",
            std::process::id()
        ));
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "server".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: None,
        };
        let operation = OperationRecord {
            id: Uuid::now_v7(),
            resource_id: resource.id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let command = AgentCommandRecord {
            command_id: "command-1".to_owned(),
            idempotency_key: "create-1".to_owned(),
            operation_id: operation.id,
            resource_id: resource.id,
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            payload_fingerprint_sha256: "a".repeat(64),
            payload: b"command-payload".to_vec(),
            state: AgentCommandState::Pending,
            accepted_sequence: 0,
            last_sequence: 0,
            provider_operation_id: None,
            provider_resource_id: None,
        };
        {
            let store = SqliteStore::connect_file(&path).await?;
            store
                .insert_resource_and_operation(&resource, &operation)
                .await?;
            assert_eq!(store.insert_agent_command(&command).await?, command);
            assert_eq!(store.insert_agent_command(&command).await?, command);
            let updated = store
                .update_agent_command(
                    &command.command_id,
                    AgentCommandState::Accepted,
                    1,
                    1,
                    Some("provider-op-1"),
                    Some("domain-1"),
                )
                .await?;
            assert_eq!(updated.accepted_sequence, 1);
            assert_eq!(
                updated.provider_operation_id.as_deref(),
                Some("provider-op-1")
            );
            assert_eq!(updated.provider_resource_id.as_deref(), Some("domain-1"));
            assert_eq!(store.increment_operation_retry(operation.id).await?, 1);
            assert_eq!(store.increment_operation_retry(operation.id).await?, 2);
            assert_eq!(
                store
                    .update_agent_command(
                        &command.command_id,
                        AgentCommandState::Pending,
                        0,
                        0,
                        None,
                        None,
                    )
                    .await?
                    .state,
                AgentCommandState::Accepted
            );
            assert!(matches!(
                store
                    .update_agent_command(
                        &command.command_id,
                        AgentCommandState::Failed,
                        1,
                        1,
                        Some("provider-op-1"),
                        Some("domain-1"),
                    )
                    .await,
                Err(StoreError::Corrupt(_))
            ));
        }
        let reopened = SqliteStore::connect_file(&path).await?;
        assert_eq!(
            reopened.get_agent_command(&command.command_id).await?.state,
            AgentCommandState::Accepted
        );
        assert_eq!(reopened.increment_operation_retry(operation.id).await?, 3);
        fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn file_database_survives_restart() -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!("/tmp/o3k-store-{}.sqlite", std::process::id()));
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "server".to_owned(),
            project_id: "project-a".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: Some("provider-1".to_owned()),
        };
        {
            let store = SqliteStore::connect_file(&path).await?;
            store.insert_resource(&resource).await?;
        }
        let reopened = SqliteStore::connect_file(&path).await?;
        assert_eq!(reopened.get_resource(resource.id).await?, resource);
        fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_database_is_rejected_without_repair() -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-store-corrupt-{}.sqlite",
            std::process::id()
        ));
        fs::write(&path, b"not a sqlite database")?;
        let result = SqliteStore::connect_file(&path).await;
        assert!(result.is_err());
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn public_key_validation_is_canonical_and_rejects_mismatches() -> Result<(), StoreError> {
        let blob = [
            0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9', 0, 0, 0,
            32,
        ]
        .into_iter()
        .chain([7_u8; 32])
        .collect::<Vec<_>>();
        let encoded = BASE64.encode(&blob);
        let (key_type, fingerprint, canonical) =
            validate_public_key(&format!("ssh-ed25519 {encoded} comment"))?;
        assert_eq!(key_type, "ssh-ed25519");
        assert_eq!(fingerprint.len(), 47);
        assert_eq!(canonical, format!("ssh-ed25519 {encoded}"));
        assert!(validate_public_key(&format!("ssh-ed25519 {encoded}\n")).is_ok());
        assert!(validate_public_key(&format!("ssh-rsa {encoded}")).is_err());
        assert!(validate_public_key("ssh-ed25519 !!!").is_err());
        assert!(validate_public_key("ssh-dss AAAA").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn keypairs_are_scoped_unique_and_survive_restart() -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!("/tmp/o3k-keypairs-{}.sqlite", std::process::id()));
        let blob = [
            0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9', 0, 0, 0,
            32,
        ]
        .into_iter()
        .chain([9_u8; 32])
        .collect::<Vec<_>>();
        let public_key = format!("ssh-ed25519 {}", BASE64.encode(blob));
        let (key_type, fingerprint, canonical) = validate_public_key(&public_key)?;
        let record = KeypairRecord {
            id: Uuid::now_v7(),
            user_id: "user-a".to_owned(),
            project_id: "project-a".to_owned(),
            name: "test-key".to_owned(),
            key_type,
            public_key: canonical,
            fingerprint,
            created_at: "1".to_owned(),
        };
        {
            let store = SqliteStore::connect_file(&path).await?;
            store.insert_keypair(&record).await?;
            assert!(matches!(
                store.insert_keypair(&record).await,
                Err(StoreError::KeypairAlreadyExists)
            ));
            assert!(
                store
                    .get_keypair("user-b", "project-a", "test-key")
                    .await
                    .is_err()
            );
            assert_eq!(store.list_keypairs("user-a", "project-a").await?.len(), 1);
        }
        let reopened = SqliteStore::connect_file(&path).await?;
        assert_eq!(
            reopened
                .get_keypair("user-a", "project-a", "test-key")
                .await?,
            record
        );
        reopened
            .delete_keypair("user-a", "project-a", "test-key")
            .await?;
        assert!(matches!(
            reopened
                .delete_keypair("user-a", "project-a", "test-key")
                .await,
            Err(StoreError::KeypairNotFound)
        ));
        fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn wal_mode_and_foreign_keys_enabled_for_persistent_database()
    -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!("/tmp/o3k-store-wal-{}.sqlite", Uuid::now_v7()));
        let store = SqliteStore::connect_file(&path).await?;
        assert_eq!(store.journal_mode().await?, "wal");

        let health = store.database_health().await?;
        assert_eq!(health.status, "ok");
        assert_eq!(health.journal_mode, "wal");
        assert!(health.foreign_keys);
        assert_eq!(health.integrity_check, "ok");
        assert_eq!(health.wal_checkpoint_status.as_deref(), Some("active"));

        store.checkpoint(WalCheckpointMode::Passive).await?;
        store.checkpoint(WalCheckpointMode::Truncate).await?;

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_database_uses_memory_journal_mode() -> Result<(), Box<dyn Error>> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        assert_eq!(store.journal_mode().await?, "memory");
        let health = store.database_health().await?;
        assert_eq!(health.journal_mode, "memory");
        assert!(health.wal_checkpoint_status.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_writers_and_wal_lock_contention() -> Result<(), Box<dyn Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-store-wal-concurrent-{}.sqlite",
            Uuid::now_v7()
        ));
        let store = std::sync::Arc::new(SqliteStore::connect_file(&path).await?);
        let blob = [
            0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9', 0, 0, 0,
            32,
        ]
        .into_iter()
        .chain([7_u8; 32])
        .collect::<Vec<_>>();
        let encoded = BASE64.encode(&blob);

        let mut handles = Vec::new();

        for i in 0..5 {
            let store = store.clone();
            let encoded = encoded.clone();
            let handle = tokio::spawn(async move {
                let (_, fingerprint, canonical) =
                    validate_public_key(&format!("ssh-ed25519 {encoded} user-{i}"))?;
                let keypair = KeypairRecord {
                    id: Uuid::now_v7(),
                    user_id: format!("user-{i}"),
                    project_id: "project-concurrent".to_owned(),
                    name: format!("key-{i}"),
                    key_type: "ssh-ed25519".to_owned(),
                    public_key: canonical,
                    fingerprint,
                    created_at: "2024-01-01T00:00:00Z".to_owned(),
                };
                store.insert_keypair(&keypair).await
            });
            handles.push(handle);
        }

        for handle in handles {
            let res = handle.await?;
            assert!(res.is_ok());
        }

        let health = store.database_health().await?;
        assert_eq!(health.status, "ok");

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observation_update_waits_out_a_concurrent_writer() -> Result<(), Box<dyn Error>> {
        // Regression test for issue #487 (run local-1785957445): a deferred
        // read-then-write transaction failed immediately with SQLITE_BUSY
        // when a concurrent connection held the write lock, and the dropped
        // observation left the resource stuck in `requested`. BEGIN IMMEDIATE
        // honours the configured busy_timeout instead of failing immediately.
        let path = PathBuf::from(format!(
            "/tmp/o3k-store-observation-busy-{}.sqlite",
            Uuid::now_v7()
        ));
        let store = SqliteStore::connect_file(&path).await?;
        let resource = ResourceRecord {
            id: Uuid::now_v7(),
            kind: "server".to_owned(),
            project_id: "project-busy".to_owned(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".to_owned(),
            observed_state: "unknown".to_owned(),
            provider_id: Some("provider-1".to_owned()),
        };
        store.insert_resource(&resource).await?;

        // Hold the WAL write lock on a second connection long enough that an
        // immediate-failure implementation would return SQLITE_BUSY first.
        let lock_url = format!("sqlite://{}", path.display());
        let holder = tokio::spawn(async move {
            use sqlx::Connection as _;
            let mut connection = sqlx::sqlite::SqliteConnection::connect(&lock_url).await?;
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut connection)
                .await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            sqlx::query("COMMIT").execute(&mut connection).await?;
            Ok::<(), sqlx::Error>(())
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let update = ObservationUpdate {
            expected_generation: 1,
            desired_state: "active",
            observed_state: "running",
            observed_generation: 1,
            provider_id: Some("provider-1"),
            agent_epoch: "epoch-1",
            observation_sequence: 1,
        };
        let updated = store
            .update_resource_from_observation(resource.id, &update)
            .await?;
        assert_eq!(updated.observed_state, "running");
        assert_eq!(updated.generation, 2);
        holder.await??;

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn backup_and_restore_produces_consistent_database() -> Result<(), Box<dyn Error>> {
        let src_path = PathBuf::from(format!(
            "/tmp/o3k-store-backup-src-{}.sqlite",
            Uuid::now_v7()
        ));
        let backup_path = PathBuf::from(format!(
            "/tmp/o3k-store-backup-dst-{}.sqlite",
            Uuid::now_v7()
        ));

        let store = SqliteStore::connect_file(&src_path).await?;
        let blob = [
            0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9', 0, 0, 0,
            32,
        ]
        .into_iter()
        .chain([7_u8; 32])
        .collect::<Vec<_>>();
        let encoded = BASE64.encode(&blob);

        let (_, fingerprint, canonical) =
            validate_public_key(&format!("ssh-ed25519 {encoded} user-backup"))?;
        let keypair = KeypairRecord {
            id: Uuid::now_v7(),
            user_id: "user-backup".to_owned(),
            project_id: "project-backup".to_owned(),
            name: "key-backup".to_owned(),
            key_type: "ssh-ed25519".to_owned(),
            public_key: canonical,
            fingerprint,
            created_at: "2024-01-01T00:00:00Z".to_owned(),
        };
        store.insert_keypair(&keypair).await?;

        store.backup_to_file(&backup_path).await?;

        let restored_store = SqliteStore::connect_file(&backup_path).await?;
        let fetched = restored_store
            .get_keypair("user-backup", "project-backup", "key-backup")
            .await?;
        assert_eq!(fetched, keypair);

        let _ = fs::remove_file(&src_path);
        let _ = fs::remove_file(format!("{}-wal", src_path.display()));
        let _ = fs::remove_file(format!("{}-shm", src_path.display()));
        let _ = fs::remove_file(&backup_path);
        let _ = fs::remove_file(format!("{}-wal", backup_path.display()));
        let _ = fs::remove_file(format!("{}-shm", backup_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn empty_database_path_returns_error() {
        let result = SqliteStore::connect_file(Path::new("")).await;
        assert!(result.is_err());
    }
}
