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
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};
use thiserror::Error;
use uuid::Uuid;

mod artifact_transfer;

pub use artifact_transfer::{
    ArtifactTransferRecord, ArtifactTransferState, ArtifactTransferUpdate,
    MAX_ARTIFACT_TRANSFER_BYTES, MAX_ARTIFACT_TRANSFER_CHUNK_BYTES, MAX_ARTIFACT_TRANSFER_RETRIES,
};

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
    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError>;
    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError>;
    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
    ) -> Result<(), StoreError>;
    async fn readiness_check(&self) -> Result<(), StoreError>;
}

#[derive(Clone, Debug)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(database_url).map_err(StoreError::Database)?;
        let max_connections = if database_url == "sqlite::memory:" {
            1
        } else {
            5
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(
                options
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(5)),
            )
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

    async fn verify_integrity(&self) -> Result<(), StoreError> {
        let result: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result != "ok" {
            return Err(StoreError::Corrupt(result));
        }
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('resources', 'operations', 'provider_refs', 'keypairs', 'server_keypairs', 'agent_commands', 'operation_retry_state', 'artifact_transfers')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if table_count != 8 {
            return Err(StoreError::Corrupt("required table is missing".to_owned()));
        }
        Ok(())
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
        let ObservationUpdate {
            expected_generation,
            desired_state,
            observed_state,
            observed_generation,
            provider_id,
            agent_epoch,
            observation_sequence,
        } = update;
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
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
                transaction.rollback().await.map_err(StoreError::Database)?;
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
        transaction.commit().await.map_err(StoreError::Database)?;
        Ok(updated)
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
}
