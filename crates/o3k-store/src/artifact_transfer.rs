use std::fmt;

use sqlx::{Row, SqlitePool, sqlite::SqliteRow};
use uuid::Uuid;

use crate::{StoreError, parse_uuid, sqlite_sequence};

pub const MAX_ARTIFACT_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARTIFACT_TRANSFER_CHUNK_BYTES: u64 = 256 * 1024;
pub const MAX_ARTIFACT_TRANSFER_RETRIES: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactTransferState {
    Offered,
    Receiving,
    Committed,
    Rejected,
    Expired,
}

impl ArtifactTransferState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Receiving => "receiving",
            Self::Committed => "committed",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "offered" => Ok(Self::Offered),
            "receiving" => Ok(Self::Receiving),
            "committed" => Ok(Self::Committed),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            _ => Err(StoreError::Corrupt(format!(
                "unknown artifact transfer state `{value}`"
            ))),
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Rejected | Self::Expired)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactTransferRecord {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub agent_id: String,
    pub agent_epoch: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub expires_at_unix_ms: i64,
    pub format: String,
    pub chunk_size_bytes: u64,
    pub chunk_count: u64,
    pub state: ArtifactTransferState,
    pub contiguous_bytes: u64,
    pub next_chunk_index: u64,
    pub retry_count: u8,
    pub created_at: String,
    pub updated_at: String,
}

impl ArtifactTransferRecord {
    pub fn validate(&self) -> Result<(), StoreError> {
        bounded_text("transfer_id", &self.transfer_id, 128)?;
        bounded_text("command_id", &self.command_id, 128)?;
        bounded_text("agent_id", &self.agent_id, 128)?;
        bounded_text("agent_epoch", &self.agent_epoch, 256)?;
        bounded_text("artifact_id", &self.artifact_id, 256)?;
        bounded_text("artifact_kind", &self.artifact_kind, 64)?;
        bounded_text("format", &self.format, 32)?;
        if self.expires_at_unix_ms <= 0 {
            return Err(StoreError::InvalidArtifactTransfer(
                "artifact expiry must be positive".to_owned(),
            ));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidArtifactTransfer(
                "sha256 must be 64 hexadecimal characters".to_owned(),
            ));
        }
        if self.size_bytes == 0 || self.size_bytes > MAX_ARTIFACT_TRANSFER_BYTES {
            return Err(StoreError::InvalidArtifactTransfer(
                "artifact size exceeds the transfer bound".to_owned(),
            ));
        }
        if self.chunk_size_bytes == 0 || self.chunk_size_bytes > MAX_ARTIFACT_TRANSFER_CHUNK_BYTES {
            return Err(StoreError::InvalidArtifactTransfer(
                "chunk size exceeds the transfer bound".to_owned(),
            ));
        }
        let minimum_chunks = self.size_bytes.div_ceil(self.chunk_size_bytes);
        if self.chunk_count != minimum_chunks {
            return Err(StoreError::InvalidArtifactTransfer(
                "chunk count is inconsistent with artifact size".to_owned(),
            ));
        }
        if self.contiguous_bytes > self.size_bytes || self.next_chunk_index > self.chunk_count {
            return Err(StoreError::InvalidArtifactTransfer(
                "transfer progress exceeds offer metadata".to_owned(),
            ));
        }
        if self.retry_count > MAX_ARTIFACT_TRANSFER_RETRIES {
            return Err(StoreError::InvalidArtifactTransfer(
                "transfer retry count exceeds the bound".to_owned(),
            ));
        }
        if self.state == ArtifactTransferState::Committed
            && (self.contiguous_bytes != self.size_bytes
                || self.next_chunk_index != self.chunk_count)
        {
            return Err(StoreError::InvalidArtifactTransfer(
                "committed transfer does not contain complete progress".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactTransferUpdate {
    pub state: ArtifactTransferState,
    pub contiguous_bytes: u64,
    pub next_chunk_index: u64,
    pub retry_count: u8,
}

impl ArtifactTransferUpdate {
    pub(crate) fn validate_against(
        self,
        current: &ArtifactTransferRecord,
    ) -> Result<(), StoreError> {
        if self.retry_count > MAX_ARTIFACT_TRANSFER_RETRIES
            || self.contiguous_bytes > current.size_bytes
            || self.next_chunk_index > current.chunk_count
        {
            return Err(StoreError::InvalidArtifactTransfer(
                "transfer update exceeds durable bounds".to_owned(),
            ));
        }
        if current.state.is_terminal() {
            if self.state != current.state
                || self.contiguous_bytes != current.contiguous_bytes
                || self.next_chunk_index != current.next_chunk_index
                || self.retry_count != current.retry_count
            {
                return Err(StoreError::ArtifactTransferConflict(
                    "terminal artifact transfer cannot change".to_owned(),
                ));
            }
            return Ok(());
        }
        if self.contiguous_bytes < current.contiguous_bytes
            || self.next_chunk_index < current.next_chunk_index
            || self.retry_count < current.retry_count
        {
            return Err(StoreError::ArtifactTransferConflict(
                "artifact transfer progress cannot regress".to_owned(),
            ));
        }
        if matches!(self.state, ArtifactTransferState::Offered)
            && current.state != ArtifactTransferState::Offered
        {
            return Err(StoreError::ArtifactTransferConflict(
                "artifact transfer state cannot regress".to_owned(),
            ));
        }
        if self.state == ArtifactTransferState::Offered
            && (self.contiguous_bytes != 0 || self.next_chunk_index != 0)
        {
            return Err(StoreError::InvalidArtifactTransfer(
                "offered transfer cannot have received progress".to_owned(),
            ));
        }
        if matches!(self.state, ArtifactTransferState::Receiving) && current.state.is_terminal() {
            return Err(StoreError::ArtifactTransferConflict(
                "artifact transfer state cannot regress".to_owned(),
            ));
        }
        if self.state == ArtifactTransferState::Committed
            && (self.contiguous_bytes != current.size_bytes
                || self.next_chunk_index != current.chunk_count)
        {
            return Err(StoreError::InvalidArtifactTransfer(
                "commit requires complete transfer progress".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) async fn insert(
    pool: &SqlitePool,
    transfer: &ArtifactTransferRecord,
) -> Result<ArtifactTransferRecord, StoreError> {
    transfer.validate()?;
    let result = sqlx::query(
        "INSERT INTO artifact_transfers (transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&transfer.transfer_id)
    .bind(&transfer.command_id)
    .bind(transfer.operation_id.to_string())
    .bind(transfer.resource_id.to_string())
    .bind(&transfer.agent_id)
    .bind(&transfer.agent_epoch)
    .bind(&transfer.artifact_id)
    .bind(&transfer.artifact_kind)
        .bind(&transfer.sha256)
        .bind(sqlite_sequence(transfer.size_bytes)?)
        .bind(transfer.expires_at_unix_ms)
        .bind(&transfer.format)
    .bind(sqlite_sequence(transfer.chunk_size_bytes)?)
    .bind(sqlite_sequence(transfer.chunk_count)?)
    .bind(transfer.state.as_str())
    .bind(sqlite_sequence(transfer.contiguous_bytes)?)
    .bind(sqlite_sequence(transfer.next_chunk_index)?)
    .bind(i64::from(transfer.retry_count))
    .execute(pool)
    .await;
    match result {
        Ok(_) => get(pool, &transfer.transfer_id).await,
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            let existing = get(pool, &transfer.transfer_id).await?;
            if same_identity(&existing, transfer) {
                Ok(existing)
            } else {
                Err(StoreError::ArtifactTransferConflict(
                    "transfer identity conflicts with durable state".to_owned(),
                ))
            }
        }
        Err(error) => Err(StoreError::Database(error)),
    }
}

pub(crate) async fn get(
    pool: &SqlitePool,
    transfer_id: &str,
) -> Result<ArtifactTransferRecord, StoreError> {
    let row = sqlx::query("SELECT transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count, created_at, updated_at FROM artifact_transfers WHERE transfer_id = ?")
        .bind(transfer_id)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::Database)?
        .ok_or(StoreError::ArtifactTransferNotFound)?;
    from_row(&row)
}

pub(crate) async fn update(
    pool: &SqlitePool,
    transfer_id: &str,
    expected_agent_epoch: &str,
    update: ArtifactTransferUpdate,
) -> Result<ArtifactTransferRecord, StoreError> {
    // Acquire the write lock before reading the row.  A deferred transaction
    // can take a read snapshot and then fail with SQLITE_BUSY_SNAPSHOT when
    // the commit update upgrades it after another transfer heartbeat/write.
    // The store's bounded busy timeout applies to BEGIN IMMEDIATE, making the
    // durable commit path retryable instead of surfacing a false provider
    // conflict.
    let mut connection = pool.acquire().await.map_err(StoreError::Database)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(StoreError::Database)?;
    let outcome: Result<Option<ArtifactTransferRecord>, StoreError> = async {
        let row = sqlx::query("SELECT transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count, created_at, updated_at FROM artifact_transfers WHERE transfer_id = ?")
            .bind(transfer_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(StoreError::Database)?
            .ok_or(StoreError::ArtifactTransferNotFound)?;
        let current = from_row(&row)?;
        if current.agent_epoch != expected_agent_epoch {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }
        update.validate_against(&current)?;
        if update
            == (ArtifactTransferUpdate {
                state: current.state,
                contiguous_bytes: current.contiguous_bytes,
                next_chunk_index: current.next_chunk_index,
                retry_count: current.retry_count,
            })
        {
            return Ok(Some(current));
        }
        let result = sqlx::query("UPDATE artifact_transfers SET state = ?, contiguous_bytes = ?, next_chunk_index = ?, retry_count = ?, updated_at = CURRENT_TIMESTAMP WHERE transfer_id = ? AND agent_epoch = ?")
            .bind(update.state.as_str())
            .bind(sqlite_sequence(update.contiguous_bytes)?)
            .bind(sqlite_sequence(update.next_chunk_index)?)
            .bind(i64::from(update.retry_count))
            .bind(transfer_id)
            .bind(expected_agent_epoch)
            .execute(&mut *connection)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }
        Ok(None)
    }
    .await;
    match outcome {
        Ok(Some(current)) => {
            sqlx::query("ROLLBACK")
                .execute(&mut *connection)
                .await
                .map_err(StoreError::Database)?;
            Ok(current)
        }
        Ok(None) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(StoreError::Database)?;
            drop(connection);
            get(pool, transfer_id).await
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

pub(crate) async fn rebind_epoch(
    pool: &SqlitePool,
    transfer_id: &str,
    expected_agent_epoch: &str,
    new_agent_epoch: &str,
) -> Result<ArtifactTransferRecord, StoreError> {
    bounded_text("expected_agent_epoch", expected_agent_epoch, 256)?;
    bounded_text("new_agent_epoch", new_agent_epoch, 256)?;
    if expected_agent_epoch == new_agent_epoch {
        return get(pool, transfer_id).await;
    }
    let result = sqlx::query(
        "UPDATE artifact_transfers SET agent_epoch = ?, updated_at = CURRENT_TIMESTAMP WHERE transfer_id = ? AND agent_epoch = ? AND state IN ('offered', 'receiving')",
    )
    .bind(new_agent_epoch)
    .bind(transfer_id)
    .bind(expected_agent_epoch)
    .execute(pool)
    .await
    .map_err(StoreError::Database)?;
    if result.rows_affected() != 1 {
        let current = get(pool, transfer_id).await?;
        if current.agent_epoch != expected_agent_epoch {
            return Err(StoreError::ArtifactTransferEpochConflict);
        }
        return Err(StoreError::ArtifactTransferConflict(
            "terminal artifact transfer cannot be rebound".to_owned(),
        ));
    }
    get(pool, transfer_id).await
}

pub(crate) async fn list_recoverable(
    pool: &SqlitePool,
) -> Result<Vec<ArtifactTransferRecord>, StoreError> {
    let rows = sqlx::query("SELECT transfer_id, command_id, operation_id, resource_id, agent_id, agent_epoch, artifact_id, artifact_kind, sha256, size_bytes, expires_at_unix_ms, format, chunk_size_bytes, chunk_count, state, contiguous_bytes, next_chunk_index, retry_count, created_at, updated_at FROM artifact_transfers WHERE state IN ('offered', 'receiving') ORDER BY created_at ASC")
        .fetch_all(pool)
        .await
        .map_err(StoreError::Database)?;
    rows.iter().map(from_row).collect()
}

/// Marks every artifact transfer whose owning operation has already reached
/// a terminal state as `expired`, and returns the number of rows expired.
///
/// Operations terminalize independently of their artifact handshakes: an
/// agent crash can leave `offered`/`receiving` rows behind, and a terminalized
/// operation is never driven again, so no per-operation path ever advances
/// those rows (issue #88). The terminal predicate is the reconciler's
/// (`OperationState::Succeeded | OperationState::Failed` — `succeeded`/
/// `failed`); every other stored operation state is non-terminal. The sweep
/// is idempotent (a second run finds no matching rows) and never touches
/// `committed`, `rejected`, or `expired` rows: a committed transfer of a
/// terminal operation is durable cache/evidence that must survive the
/// operation, and the other two states are already terminal.
pub(crate) async fn expire_transfers_of_terminal_operations(
    pool: &SqlitePool,
) -> Result<u64, StoreError> {
    let result = sqlx::query(
        "UPDATE artifact_transfers \
         SET state = 'expired', updated_at = CURRENT_TIMESTAMP \
         WHERE state NOT IN ('committed', 'rejected', 'expired') \
           AND operation_id IN (SELECT id FROM operations WHERE state IN ('succeeded', 'failed'))",
    )
    .execute(pool)
    .await
    .map_err(StoreError::Database)?;
    Ok(result.rows_affected())
}

fn same_identity(left: &ArtifactTransferRecord, right: &ArtifactTransferRecord) -> bool {
    left.transfer_id == right.transfer_id
        && left.command_id == right.command_id
        && left.operation_id == right.operation_id
        && left.resource_id == right.resource_id
        && left.agent_id == right.agent_id
        && left.agent_epoch == right.agent_epoch
        && left.artifact_id == right.artifact_id
        && left.artifact_kind == right.artifact_kind
        && left.sha256 == right.sha256
        && left.size_bytes == right.size_bytes
        && left.expires_at_unix_ms == right.expires_at_unix_ms
        && left.format == right.format
        && left.chunk_size_bytes == right.chunk_size_bytes
        && left.chunk_count == right.chunk_count
}

fn from_row(row: &SqliteRow) -> Result<ArtifactTransferRecord, StoreError> {
    let record = ArtifactTransferRecord {
        transfer_id: row.get("transfer_id"),
        command_id: row.get("command_id"),
        operation_id: parse_uuid(row.get("operation_id"))?,
        resource_id: parse_uuid(row.get("resource_id"))?,
        agent_id: row.get("agent_id"),
        agent_epoch: row.get("agent_epoch"),
        artifact_id: row.get("artifact_id"),
        artifact_kind: row.get("artifact_kind"),
        sha256: row.get("sha256"),
        size_bytes: u64::try_from(row.get::<i64, _>("size_bytes"))
            .map_err(|_| StoreError::Corrupt("negative artifact size".to_owned()))?,
        expires_at_unix_ms: row.get::<Option<i64>, _>("expires_at_unix_ms").ok_or(
            StoreError::Corrupt("artifact transfer expiry is missing".to_owned()),
        )?,
        format: row.get("format"),
        chunk_size_bytes: u64::try_from(row.get::<i64, _>("chunk_size_bytes"))
            .map_err(|_| StoreError::Corrupt("negative artifact chunk size".to_owned()))?,
        chunk_count: u64::try_from(row.get::<i64, _>("chunk_count"))
            .map_err(|_| StoreError::Corrupt("negative artifact chunk count".to_owned()))?,
        state: ArtifactTransferState::parse(row.get::<String, _>("state").as_str())?,
        contiguous_bytes: u64::try_from(row.get::<i64, _>("contiguous_bytes"))
            .map_err(|_| StoreError::Corrupt("negative artifact progress".to_owned()))?,
        next_chunk_index: u64::try_from(row.get::<i64, _>("next_chunk_index"))
            .map_err(|_| StoreError::Corrupt("negative artifact chunk index".to_owned()))?,
        retry_count: u8::try_from(row.get::<i64, _>("retry_count"))
            .map_err(|_| StoreError::Corrupt("invalid artifact retry count".to_owned()))?,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    };
    record.validate()?;
    Ok(record)
}

fn bounded_text(name: &str, value: &str, max: usize) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(StoreError::InvalidArtifactTransfer(format!(
            "{name} is empty, too long, or contains control characters"
        )));
    }
    Ok(())
}

impl fmt::Display for ArtifactTransferState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurableStore, OperationRecord, OperationState, ResourceRecord, SqliteStore};

    fn transfer(operation_id: Uuid, resource_id: Uuid) -> ArtifactTransferRecord {
        transfer_with(
            "transfer-1",
            operation_id,
            resource_id,
            ArtifactTransferState::Offered,
            0,
            0,
        )
    }

    fn transfer_with(
        transfer_id: &str,
        operation_id: Uuid,
        resource_id: Uuid,
        state: ArtifactTransferState,
        contiguous_bytes: u64,
        next_chunk_index: u64,
    ) -> ArtifactTransferRecord {
        ArtifactTransferRecord {
            transfer_id: transfer_id.to_owned(),
            command_id: format!("command-{transfer_id}"),
            operation_id,
            resource_id,
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            artifact_id: "image-1".to_owned(),
            artifact_kind: "image_base".to_owned(),
            sha256: "a".repeat(64),
            size_bytes: 512 * 1024,
            expires_at_unix_ms: i64::MAX,
            format: "qcow2".to_owned(),
            chunk_size_bytes: 256 * 1024,
            chunk_count: 2,
            state,
            contiguous_bytes,
            next_chunk_index,
            retry_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    async fn setup() -> Result<(SqliteStore, ArtifactTransferRecord), StoreError> {
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
            resource_id: resource.id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        store
            .insert_resource_and_operation(&resource, &operation)
            .await?;
        Ok((store, transfer(operation.id, resource.id)))
    }

    #[tokio::test]
    async fn insert_is_idempotent_and_updates_are_epoch_fenced() -> Result<(), StoreError> {
        let (store, record) = setup().await?;
        assert_eq!(
            store.insert_artifact_transfer(&record).await?,
            record_for_assert(&store, &record).await?
        );
        let duplicate = store.insert_artifact_transfer(&record).await?;
        assert_eq!(duplicate.transfer_id, record.transfer_id);
        assert!(!duplicate.created_at.is_empty());
        let mut conflicting = record.clone();
        conflicting.sha256 = "b".repeat(64);
        assert!(matches!(
            store.insert_artifact_transfer(&conflicting).await,
            Err(StoreError::ArtifactTransferConflict(_))
        ));
        let mut expiry_conflict = record.clone();
        expiry_conflict.expires_at_unix_ms -= 1;
        assert!(matches!(
            store.insert_artifact_transfer(&expiry_conflict).await,
            Err(StoreError::ArtifactTransferConflict(_))
        ));

        let receiving = ArtifactTransferUpdate {
            state: ArtifactTransferState::Receiving,
            contiguous_bytes: 256 * 1024,
            next_chunk_index: 1,
            retry_count: 1,
        };
        assert_eq!(
            store
                .update_artifact_transfer(&record.transfer_id, "epoch-1", receiving)
                .await?
                .state,
            ArtifactTransferState::Receiving
        );
        assert!(matches!(
            store
                .update_artifact_transfer(&record.transfer_id, "stale-epoch", receiving)
                .await,
            Err(StoreError::ArtifactTransferEpochConflict)
        ));
        assert!(matches!(
            store
                .update_artifact_transfer(
                    &record.transfer_id,
                    "epoch-1",
                    ArtifactTransferUpdate {
                        state: ArtifactTransferState::Receiving,
                        contiguous_bytes: 0,
                        next_chunk_index: 0,
                        retry_count: 1,
                    },
                )
                .await,
            Err(StoreError::ArtifactTransferConflict(_))
        ));
        let rebound = store
            .rebind_artifact_transfer_epoch(&record.transfer_id, "epoch-1", "epoch-2")
            .await?;
        assert_eq!(rebound.agent_epoch, "epoch-2");
        assert!(matches!(
            store
                .rebind_artifact_transfer_epoch(&record.transfer_id, "epoch-1", "epoch-3")
                .await,
            Err(StoreError::ArtifactTransferEpochConflict)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn completion_is_idempotent_and_not_recoverable() -> Result<(), StoreError> {
        let (store, record) = setup().await?;
        store.insert_artifact_transfer(&record).await?;
        let committed = ArtifactTransferUpdate {
            state: ArtifactTransferState::Committed,
            contiguous_bytes: record.size_bytes,
            next_chunk_index: record.chunk_count,
            retry_count: 1,
        };
        let first = store
            .update_artifact_transfer(&record.transfer_id, &record.agent_epoch, committed)
            .await?;
        let second = store
            .update_artifact_transfer(&record.transfer_id, &record.agent_epoch, committed)
            .await?;
        assert_eq!(first, second);
        assert!(
            store
                .list_recoverable_artifact_transfers()
                .await?
                .is_empty()
        );
        assert!(matches!(
            store
                .update_artifact_transfer(
                    &record.transfer_id,
                    &record.agent_epoch,
                    ArtifactTransferUpdate {
                        state: ArtifactTransferState::Receiving,
                        contiguous_bytes: record.size_bytes,
                        next_chunk_index: record.chunk_count,
                        retry_count: 1,
                    },
                )
                .await,
            Err(StoreError::ArtifactTransferConflict(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn transfer_metadata_and_progress_survive_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("o3k-artifact-transfer-{}.sqlite", Uuid::now_v7()));
        let record;
        {
            let (store, value) = {
                let store = SqliteStore::connect_file(&path).await?;
                let resource = ResourceRecord {
                    id: Uuid::now_v7(),
                    kind: "server".to_owned(),
                    project_id: "p".to_owned(),
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
                store
                    .insert_resource_and_operation(&resource, &operation)
                    .await?;
                (store, transfer(operation.id, resource.id))
            };
            store.insert_artifact_transfer(&value).await?;
            record = store
                .update_artifact_transfer(
                    &value.transfer_id,
                    &value.agent_epoch,
                    ArtifactTransferUpdate {
                        state: ArtifactTransferState::Receiving,
                        contiguous_bytes: 256 * 1024,
                        next_chunk_index: 1,
                        retry_count: 2,
                    },
                )
                .await?;
        }
        let reopened = SqliteStore::connect_file(&path).await?;
        assert_eq!(
            reopened.get_artifact_transfer(&record.transfer_id).await?,
            record
        );
        assert_eq!(
            reopened.list_recoverable_artifact_transfers().await?.len(),
            1
        );
        std::fs::remove_file(path)?;
        Ok(())
    }

    async fn record_for_assert(
        store: &SqliteStore,
        record: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        let stored = store.get_artifact_transfer(&record.transfer_id).await?;
        assert_eq!(stored.transfer_id, record.transfer_id);
        Ok(stored)
    }

    /// Issue #88: an operation can reach a terminal state while its artifact
    /// handshake rows are still non-terminal (an agent crash can leave
    /// `offered` or `receiving` rows behind, and a terminalized operation is
    /// never driven again), so no per-operation path ever advances them. The
    /// store sweep expires exactly those rows: non-terminal transfers whose
    /// operation is terminal (`succeeded`/`failed`, the reconciler's terminal
    /// predicate), leaving `committed`/`rejected`/`expired` rows and transfers
    /// of non-terminal operations untouched. Idempotent: a second run expires
    /// nothing.
    #[tokio::test]
    async fn sweep_expires_non_terminal_transfers_of_terminal_operations() -> Result<(), StoreError>
    {
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
        let failed = OperationRecord {
            id: Uuid::now_v7(),
            resource_id: resource.id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        let running = OperationRecord {
            id: Uuid::now_v7(),
            resource_id: resource.id,
            kind: "create".to_owned(),
            state: OperationState::Pending,
            provider_operation_id: None,
            error_category: None,
            error_message: None,
        };
        store
            .insert_resource_and_operation(&resource, &failed)
            .await?;
        store.insert_operation(&running).await?;
        store
            .update_operation(failed.id, OperationState::Failed, None, None, None)
            .await?;
        store
            .update_operation(running.id, OperationState::Running, None, None, None)
            .await?;
        for transfer in [
            transfer_with(
                "t-offered",
                failed.id,
                resource.id,
                ArtifactTransferState::Offered,
                0,
                0,
            ),
            transfer_with(
                "t-receiving",
                failed.id,
                resource.id,
                ArtifactTransferState::Receiving,
                256 * 1024,
                1,
            ),
            transfer_with(
                "t-committed",
                failed.id,
                resource.id,
                ArtifactTransferState::Committed,
                512 * 1024,
                2,
            ),
            transfer_with(
                "t-rejected",
                failed.id,
                resource.id,
                ArtifactTransferState::Rejected,
                0,
                0,
            ),
            transfer_with(
                "t-running",
                running.id,
                resource.id,
                ArtifactTransferState::Offered,
                0,
                0,
            ),
        ] {
            store.insert_artifact_transfer(&transfer).await?;
        }

        // The sweep expires the failed operation's abandoned offers/receives.
        assert_eq!(store.expire_transfers_of_terminal_operations().await?, 2);
        assert_eq!(
            store.get_artifact_transfer("t-offered").await?.state,
            ArtifactTransferState::Expired
        );
        assert_eq!(
            store.get_artifact_transfer("t-receiving").await?.state,
            ArtifactTransferState::Expired
        );
        // ...but never touches committed or rejected rows (a committed
        // transfer of a terminal operation is durable cache/evidence), nor
        // transfers of an operation that is still non-terminal.
        assert_eq!(
            store.get_artifact_transfer("t-committed").await?.state,
            ArtifactTransferState::Committed
        );
        assert_eq!(
            store.get_artifact_transfer("t-rejected").await?.state,
            ArtifactTransferState::Rejected
        );
        assert_eq!(
            store.get_artifact_transfer("t-running").await?.state,
            ArtifactTransferState::Offered
        );
        // Repeated runs are idempotent and expire nothing.
        assert_eq!(store.expire_transfers_of_terminal_operations().await?, 0);
        let recoverable = store.list_recoverable_artifact_transfers().await?;
        assert_eq!(recoverable.len(), 1);
        assert_eq!(recoverable[0].transfer_id, "t-running");
        Ok(())
    }
}
