//! Durable native Volume, VolumeAttachment, Snapshot, and storage-backend
//! projections. The canonical payload is serialized only at this adapter
//! boundary; authorization and lifecycle semantics remain in `o3k-domain`.

use async_trait::async_trait;
use o3k_domain::{Snapshot, StorageBackend, Volume, VolumeAttachment};
use sqlx::{Row, postgres::PgRow, sqlite::SqliteRow};
use uuid::Uuid;

use crate::{SqliteStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageBackendRecord {
    pub backend: StorageBackend,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRecord {
    pub volume: Volume,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeAttachmentRecordV1 {
    pub attachment: VolumeAttachment,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub snapshot: Snapshot,
    pub created_at: String,
}

#[async_trait]
pub trait StorageRepository: Send + Sync {
    async fn insert_storage_backend(&self, record: &StorageBackendRecord)
    -> Result<(), StoreError>;
    async fn get_storage_backend(
        &self,
        id: &str,
    ) -> Result<Option<StorageBackendRecord>, StoreError>;
    async fn list_storage_backends(&self) -> Result<Vec<StorageBackendRecord>, StoreError>;

    async fn insert_volume(&self, record: &VolumeRecord) -> Result<(), StoreError>;
    async fn get_volume(&self, id: Uuid) -> Result<Option<VolumeRecord>, StoreError>;
    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeRecord>, StoreError>;
    async fn update_volume(
        &self,
        expected_generation: u64,
        record: &VolumeRecord,
    ) -> Result<VolumeRecord, StoreError>;
    async fn delete_volume(&self, project_id: &str, id: Uuid) -> Result<(), StoreError>;

    async fn insert_volume_attachment_v1(
        &self,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<(), StoreError>;
    async fn get_volume_attachment_v1(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecordV1>, StoreError>;
    async fn list_volume_attachments_v1(
        &self,
        project_id: &str,
    ) -> Result<Vec<VolumeAttachmentRecordV1>, StoreError>;
    async fn update_volume_attachment_v1(
        &self,
        expected_generation: u64,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<VolumeAttachmentRecordV1, StoreError>;
    async fn delete_volume_attachment_v1(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), StoreError>;

    async fn insert_snapshot(&self, record: &SnapshotRecord) -> Result<(), StoreError>;
    async fn get_snapshot(&self, id: Uuid) -> Result<Option<SnapshotRecord>, StoreError>;
    async fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotRecord>, StoreError>;
    async fn update_snapshot(
        &self,
        expected_generation: u64,
        record: &SnapshotRecord,
    ) -> Result<SnapshotRecord, StoreError>;
    async fn delete_snapshot(&self, project_id: &str, id: Uuid) -> Result<(), StoreError>;
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn parse<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, StoreError> {
    serde_json::from_str(payload).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn state_name<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    let value =
        serde_json::to_value(value).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StoreError::Corrupt("storage state is not a string".to_owned()))
}

fn scope_columns(scope: &o3k_domain::StorageExecutionScope) -> (&'static str, &str) {
    match scope {
        o3k_domain::StorageExecutionScope::Host(id) => ("host", id),
        o3k_domain::StorageExecutionScope::Backend(id) => ("backend", id),
    }
}

fn provider_columns(
    reference: Option<&o3k_domain::StorageProviderReference>,
) -> (Option<&str>, Option<&str>) {
    reference
        .map(|value| {
            (
                Some(value.provider.as_str()),
                Some(value.resource_id.as_str()),
            )
        })
        .unwrap_or((None, None))
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::Corrupt(format!("invalid {field}")))
}

fn validate_volume_row(
    payload: &str,
    id: &str,
    project_id: &str,
    generation: i64,
    state: &str,
) -> Result<VolumeRecord, StoreError> {
    let volume: Volume = parse(payload)?;
    if volume.id.as_uuid() != parse_uuid(id, "volume id")?
        || volume.project_id != project_id
        || volume.generation
            != u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("invalid volume generation".to_owned()))?
        || state_name(&volume.state)? != state
    {
        return Err(StoreError::Corrupt(
            "native volume index/payload mismatch".to_owned(),
        ));
    }
    volume
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(VolumeRecord {
        volume,
        created_at: String::new(),
    })
}

fn volume_from_row(row: &SqliteRow) -> Result<VolumeRecord, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let mut record = validate_volume_row(&payload, &id, &project_id, generation, &state)?;
    record.created_at = created_at;
    Ok(record)
}

fn attachment_from_row(row: &SqliteRow) -> Result<VolumeAttachmentRecordV1, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let attachment: VolumeAttachment = parse(&payload)?;
    if attachment.id.as_uuid() != parse_uuid(&id, "attachment id")?
        || attachment.project_id != project_id
        || attachment.generation
            != u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("invalid attachment generation".to_owned()))?
        || state_name(&attachment.state)? != state
    {
        return Err(StoreError::Corrupt(
            "native attachment index/payload mismatch".to_owned(),
        ));
    }
    attachment
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(VolumeAttachmentRecordV1 {
        attachment,
        created_at,
    })
}

fn snapshot_from_row(row: &SqliteRow) -> Result<SnapshotRecord, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let snapshot: Snapshot = parse(&payload)?;
    if snapshot.id.as_uuid() != parse_uuid(&id, "snapshot id")?
        || snapshot.project_id != project_id
        || snapshot.generation
            != u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("invalid snapshot generation".to_owned()))?
        || state_name(&snapshot.state)? != state
    {
        return Err(StoreError::Corrupt(
            "native snapshot index/payload mismatch".to_owned(),
        ));
    }
    snapshot
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(SnapshotRecord {
        snapshot,
        created_at,
    })
}

fn volume_from_pg_row(row: &PgRow) -> Result<VolumeRecord, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let mut record = validate_volume_row(&payload, &id, &project_id, generation, &state)?;
    record.created_at = created_at;
    Ok(record)
}

fn attachment_from_pg_row(row: &PgRow) -> Result<VolumeAttachmentRecordV1, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let attachment: VolumeAttachment = parse(&payload)?;
    if attachment.id.as_uuid() != parse_uuid(&id, "attachment id")?
        || attachment.project_id != project_id
        || attachment.generation
            != u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("invalid attachment generation".to_owned()))?
        || state_name(&attachment.state)? != state
    {
        return Err(StoreError::Corrupt(
            "native attachment index/payload mismatch".to_owned(),
        ));
    }
    attachment
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(VolumeAttachmentRecordV1 {
        attachment,
        created_at,
    })
}

fn snapshot_from_pg_row(row: &PgRow) -> Result<SnapshotRecord, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::Database)?;
    let project_id: String = row.try_get("project_id").map_err(StoreError::Database)?;
    let generation: i64 = row.try_get("generation").map_err(StoreError::Database)?;
    let state: String = row.try_get("state").map_err(StoreError::Database)?;
    let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
    let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
    let snapshot: Snapshot = parse(&payload)?;
    if snapshot.id.as_uuid() != parse_uuid(&id, "snapshot id")?
        || snapshot.project_id != project_id
        || snapshot.generation
            != u64::try_from(generation)
                .map_err(|_| StoreError::Corrupt("invalid snapshot generation".to_owned()))?
        || state_name(&snapshot.state)? != state
    {
        return Err(StoreError::Corrupt(
            "native snapshot index/payload mismatch".to_owned(),
        ));
    }
    snapshot
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(SnapshotRecord {
        snapshot,
        created_at,
    })
}

#[async_trait]
impl StorageRepository for SqliteStore {
    async fn insert_storage_backend(
        &self,
        record: &StorageBackendRecord,
    ) -> Result<(), StoreError> {
        record
            .backend
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.backend)?;
        let (scope_kind, scope_id) = scope_columns(&record.backend.scope);
        let result = sqlx::query(
            "INSERT INTO native_storage_backends (id, scope_kind, scope_id, generation, available, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.backend.id)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(i64::try_from(record.backend.generation).map_err(|_| StoreError::Corrupt("backend generation overflow".to_owned()))?)
        .bind(if record.backend.available { 1 } else { 0 })
        .bind(payload)
        .bind(&record.created_at)
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

    async fn get_storage_backend(
        &self,
        id: &str,
    ) -> Result<Option<StorageBackendRecord>, StoreError> {
        let row =
            sqlx::query("SELECT payload, created_at FROM native_storage_backends WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        row.map(|row| {
            let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
            let backend: StorageBackend = parse(&payload)?;
            backend
                .validate()
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
            Ok(StorageBackendRecord {
                backend,
                created_at,
            })
        })
        .transpose()
    }

    async fn list_storage_backends(&self) -> Result<Vec<StorageBackendRecord>, StoreError> {
        let rows =
            sqlx::query("SELECT payload, created_at FROM native_storage_backends ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        rows.into_iter()
            .map(|row| {
                let payload: String = row.try_get("payload").map_err(StoreError::Database)?;
                let backend: StorageBackend = parse(&payload)?;
                backend
                    .validate()
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                let created_at: String = row.try_get("created_at").map_err(StoreError::Database)?;
                Ok(StorageBackendRecord {
                    backend,
                    created_at,
                })
            })
            .collect()
    }

    async fn insert_volume(&self, record: &VolumeRecord) -> Result<(), StoreError> {
        record
            .volume
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.volume)?;
        let state = state_name(&record.volume.state)?;
        let provider = provider_columns(record.volume.provider_reference.as_ref());
        let result = sqlx::query(
            "INSERT INTO native_volumes (id, project_id, generation, state, payload, provider_name, provider_resource_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.volume.id.to_string())
        .bind(&record.volume.project_id)
        .bind(i64::try_from(record.volume.generation).map_err(|_| StoreError::Corrupt("volume generation overflow".to_owned()))?)
        .bind(state)
        .bind(payload)
        .bind(provider.0)
        .bind(provider.1)
        .bind(&record.created_at)
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

    async fn get_volume(&self, id: Uuid) -> Result<Option<VolumeRecord>, StoreError> {
        let row = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volumes WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        row.map(|row| volume_from_row(&row)).transpose()
    }

    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volumes WHERE project_id = ? ORDER BY id")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        rows.iter().map(volume_from_row).collect()
    }

    async fn update_volume(
        &self,
        expected_generation: u64,
        record: &VolumeRecord,
    ) -> Result<VolumeRecord, StoreError> {
        record
            .volume
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let current =
            i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?;
        let next = i64::try_from(record.volume.generation)
            .map_err(|_| StoreError::Corrupt("volume generation overflow".to_owned()))?;
        let payload = json(&record.volume)?;
        let state = state_name(&record.volume.state)?;
        let provider = provider_columns(record.volume.provider_reference.as_ref());
        let result = sqlx::query("UPDATE native_volumes SET generation = ?, state = ?, payload = ?, provider_name = ?, provider_resource_id = ? WHERE id = ? AND generation = ?")
            .bind(next).bind(state).bind(payload).bind(provider.0).bind(provider.1)
            .bind(record.volume.id.to_string()).bind(current).execute(&self.pool).await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_volume(record.volume.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_volume(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM native_volumes WHERE id = ? AND project_id = ?")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    async fn insert_volume_attachment_v1(
        &self,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<(), StoreError> {
        record
            .attachment
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.attachment)?;
        let state = state_name(&record.attachment.state)?;
        let (scope_kind, scope_id) = scope_columns(&record.attachment.execution_scope);
        let result = sqlx::query("INSERT INTO native_volume_attachments (id, project_id, volume_id, server_id, scope_kind, scope_id, generation, state, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(record.attachment.id.to_string()).bind(&record.attachment.project_id)
            .bind(record.attachment.volume_id.to_string()).bind(record.attachment.server_id.to_string())
            .bind(scope_kind).bind(scope_id)
            .bind(i64::try_from(record.attachment.generation).map_err(|_| StoreError::Corrupt("attachment generation overflow".to_owned()))?)
            .bind(state).bind(payload).bind(&record.created_at)
            .execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_volume_attachment_v1(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecordV1>, StoreError> {
        let row = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volume_attachments WHERE id = ?")
            .bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.map(|row| attachment_from_row(&row)).transpose()
    }

    async fn list_volume_attachments_v1(
        &self,
        project_id: &str,
    ) -> Result<Vec<VolumeAttachmentRecordV1>, StoreError> {
        let rows = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volume_attachments WHERE project_id = ? ORDER BY id")
            .bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(attachment_from_row).collect()
    }

    async fn update_volume_attachment_v1(
        &self,
        expected_generation: u64,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<VolumeAttachmentRecordV1, StoreError> {
        record
            .attachment
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.attachment)?;
        let state = state_name(&record.attachment.state)?;
        let result = sqlx::query("UPDATE native_volume_attachments SET generation = ?, state = ?, payload = ? WHERE id = ? AND project_id = ? AND generation = ?")
            .bind(i64::try_from(record.attachment.generation).map_err(|_| StoreError::Corrupt("attachment generation overflow".to_owned()))?)
            .bind(state).bind(payload)
            .bind(record.attachment.id.to_string()).bind(&record.attachment.project_id)
            .bind(i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_volume_attachment_v1(record.attachment.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_volume_attachment_v1(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("DELETE FROM native_volume_attachments WHERE id = ? AND project_id = ?")
                .bind(id.to_string())
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    async fn insert_snapshot(&self, record: &SnapshotRecord) -> Result<(), StoreError> {
        record
            .snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.snapshot)?;
        let state = state_name(&record.snapshot.state)?;
        let (scope_kind, scope_id) = scope_columns(&record.snapshot.execution_scope);
        let provider = provider_columns(record.snapshot.provider_reference.as_ref());
        let result = sqlx::query("INSERT INTO native_snapshots (id, project_id, volume_id, scope_kind, scope_id, source_generation, generation, state, payload, provider_name, provider_resource_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(record.snapshot.id.to_string()).bind(&record.snapshot.project_id).bind(record.snapshot.volume_id.to_string())
            .bind(scope_kind).bind(scope_id)
            .bind(i64::try_from(record.snapshot.source_generation).map_err(|_| StoreError::Corrupt("snapshot source generation overflow".to_owned()))?)
            .bind(i64::try_from(record.snapshot.generation).map_err(|_| StoreError::Corrupt("snapshot generation overflow".to_owned()))?)
            .bind(state).bind(payload).bind(provider.0).bind(provider.1).bind(&record.created_at).execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_snapshot(&self, id: Uuid) -> Result<Option<SnapshotRecord>, StoreError> {
        let row = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_snapshots WHERE id = ?")
            .bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?;
        row.map(|row| snapshot_from_row(&row)).transpose()
    }

    async fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotRecord>, StoreError> {
        let rows = sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_snapshots WHERE project_id = ? ORDER BY id")
            .bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?;
        rows.iter().map(snapshot_from_row).collect()
    }

    async fn update_snapshot(
        &self,
        expected_generation: u64,
        record: &SnapshotRecord,
    ) -> Result<SnapshotRecord, StoreError> {
        record
            .snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let payload = json(&record.snapshot)?;
        let state = state_name(&record.snapshot.state)?;
        let provider = provider_columns(record.snapshot.provider_reference.as_ref());
        let result = sqlx::query("UPDATE native_snapshots SET generation = ?, state = ?, payload = ?, provider_name = ?, provider_resource_id = ? WHERE id = ? AND project_id = ? AND generation = ?")
            .bind(i64::try_from(record.snapshot.generation).map_err(|_| StoreError::Corrupt("snapshot generation overflow".to_owned()))?)
            .bind(state).bind(payload).bind(provider.0).bind(provider.1)
            .bind(record.snapshot.id.to_string()).bind(&record.snapshot.project_id)
            .bind(i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?).execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_snapshot(record.snapshot.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_snapshot(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM native_snapshots WHERE id = ? AND project_id = ?")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }
}

#[async_trait]
impl StorageRepository for crate::PostgresStore {
    async fn insert_storage_backend(
        &self,
        record: &StorageBackendRecord,
    ) -> Result<(), StoreError> {
        record
            .backend
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let (scope_kind, scope_id) = scope_columns(&record.backend.scope);
        let result = sqlx::query("INSERT INTO native_storage_backends (id, scope_kind, scope_id, generation, available, payload, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(&record.backend.id).bind(scope_kind).bind(scope_id)
            .bind(i64::try_from(record.backend.generation).map_err(|_| StoreError::Corrupt("backend generation overflow".to_owned()))?)
            .bind(record.backend.available).bind(json(&record.backend)?).bind(&record.created_at)
            .execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_storage_backend(
        &self,
        id: &str,
    ) -> Result<Option<StorageBackendRecord>, StoreError> {
        sqlx::query("SELECT payload, created_at FROM native_storage_backends WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .map(|row| {
                let backend: StorageBackend =
                    parse(row.try_get("payload").map_err(StoreError::Database)?)?;
                backend
                    .validate()
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                Ok(StorageBackendRecord {
                    backend,
                    created_at: row.try_get("created_at").map_err(StoreError::Database)?,
                })
            })
            .transpose()
    }

    async fn list_storage_backends(&self) -> Result<Vec<StorageBackendRecord>, StoreError> {
        sqlx::query("SELECT payload, created_at FROM native_storage_backends ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Database)?
            .into_iter()
            .map(|row| {
                let backend: StorageBackend =
                    parse(row.try_get("payload").map_err(StoreError::Database)?)?;
                backend
                    .validate()
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                Ok(StorageBackendRecord {
                    backend,
                    created_at: row.try_get("created_at").map_err(StoreError::Database)?,
                })
            })
            .collect()
    }

    async fn insert_volume(&self, record: &VolumeRecord) -> Result<(), StoreError> {
        record
            .volume
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let provider = provider_columns(record.volume.provider_reference.as_ref());
        let result = sqlx::query("INSERT INTO native_volumes (id, project_id, generation, state, payload, provider_name, provider_resource_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(record.volume.id.to_string()).bind(&record.volume.project_id)
            .bind(i64::try_from(record.volume.generation).map_err(|_| StoreError::Corrupt("volume generation overflow".to_owned()))?)
            .bind(state_name(&record.volume.state)?).bind(json(&record.volume)?).bind(provider.0).bind(provider.1).bind(&record.created_at)
            .execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_volume(&self, id: Uuid) -> Result<Option<VolumeRecord>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volumes WHERE id = $1")
            .bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?.map(|row| volume_from_pg_row(&row)).transpose()
    }

    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeRecord>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volumes WHERE project_id = $1 ORDER BY id")
            .bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?.iter().map(volume_from_pg_row).collect()
    }

    async fn update_volume(
        &self,
        expected_generation: u64,
        record: &VolumeRecord,
    ) -> Result<VolumeRecord, StoreError> {
        record
            .volume
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let provider = provider_columns(record.volume.provider_reference.as_ref());
        let result = sqlx::query("UPDATE native_volumes SET generation = $1, state = $2, payload = $3, provider_name = $4, provider_resource_id = $5 WHERE id = $6 AND generation = $7")
            .bind(i64::try_from(record.volume.generation).map_err(|_| StoreError::Corrupt("volume generation overflow".to_owned()))?)
            .bind(state_name(&record.volume.state)?).bind(json(&record.volume)?).bind(provider.0).bind(provider.1)
            .bind(record.volume.id.to_string()).bind(i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?)
            .execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_volume(record.volume.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_volume(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM native_volumes WHERE id = $1 AND project_id = $2")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    async fn insert_volume_attachment_v1(
        &self,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<(), StoreError> {
        record
            .attachment
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let (scope_kind, scope_id) = scope_columns(&record.attachment.execution_scope);
        let result = sqlx::query("INSERT INTO native_volume_attachments (id, project_id, volume_id, server_id, scope_kind, scope_id, generation, state, payload, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(record.attachment.id.to_string()).bind(&record.attachment.project_id).bind(record.attachment.volume_id.to_string()).bind(record.attachment.server_id.to_string())
            .bind(scope_kind).bind(scope_id).bind(i64::try_from(record.attachment.generation).map_err(|_| StoreError::Corrupt("attachment generation overflow".to_owned()))?)
            .bind(state_name(&record.attachment.state)?).bind(json(&record.attachment)?).bind(&record.created_at)
            .execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_volume_attachment_v1(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecordV1>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volume_attachments WHERE id = $1").bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?.map(|row| attachment_from_pg_row(&row)).transpose()
    }

    async fn list_volume_attachments_v1(
        &self,
        project_id: &str,
    ) -> Result<Vec<VolumeAttachmentRecordV1>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_volume_attachments WHERE project_id = $1 ORDER BY id").bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?.iter().map(attachment_from_pg_row).collect()
    }

    async fn update_volume_attachment_v1(
        &self,
        expected_generation: u64,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<VolumeAttachmentRecordV1, StoreError> {
        record
            .attachment
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let result = sqlx::query("UPDATE native_volume_attachments SET generation = $1, state = $2, payload = $3 WHERE id = $4 AND project_id = $5 AND generation = $6")
            .bind(i64::try_from(record.attachment.generation).map_err(|_| StoreError::Corrupt("attachment generation overflow".to_owned()))?).bind(state_name(&record.attachment.state)?).bind(json(&record.attachment)?)
            .bind(record.attachment.id.to_string()).bind(&record.attachment.project_id).bind(i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?)
            .execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_volume_attachment_v1(record.attachment.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_volume_attachment_v1(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("DELETE FROM native_volume_attachments WHERE id = $1 AND project_id = $2")
                .bind(id.to_string())
                .bind(project_id)
                .execute(&self.pool)
                .await
                .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }

    async fn insert_snapshot(&self, record: &SnapshotRecord) -> Result<(), StoreError> {
        record
            .snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let (scope_kind, scope_id) = scope_columns(&record.snapshot.execution_scope);
        let provider = provider_columns(record.snapshot.provider_reference.as_ref());
        let result = sqlx::query("INSERT INTO native_snapshots (id, project_id, volume_id, scope_kind, scope_id, source_generation, generation, state, payload, provider_name, provider_resource_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)")
            .bind(record.snapshot.id.to_string()).bind(&record.snapshot.project_id).bind(record.snapshot.volume_id.to_string()).bind(scope_kind).bind(scope_id)
            .bind(i64::try_from(record.snapshot.source_generation).map_err(|_| StoreError::Corrupt("snapshot source generation overflow".to_owned()))?).bind(i64::try_from(record.snapshot.generation).map_err(|_| StoreError::Corrupt("snapshot generation overflow".to_owned()))?)
            .bind(state_name(&record.snapshot.state)?).bind(json(&record.snapshot)?).bind(provider.0).bind(provider.1).bind(&record.created_at).execute(&self.pool).await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(StoreError::ResourceAlreadyExists)
            }
            Err(error) => Err(StoreError::Database(error)),
        }
    }

    async fn get_snapshot(&self, id: Uuid) -> Result<Option<SnapshotRecord>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_snapshots WHERE id = $1").bind(id.to_string()).fetch_optional(&self.pool).await.map_err(StoreError::Database)?.map(|row| snapshot_from_pg_row(&row)).transpose()
    }

    async fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotRecord>, StoreError> {
        sqlx::query("SELECT id, project_id, generation, state, payload, created_at FROM native_snapshots WHERE project_id = $1 ORDER BY id").bind(project_id).fetch_all(&self.pool).await.map_err(StoreError::Database)?.iter().map(snapshot_from_pg_row).collect()
    }

    async fn update_snapshot(
        &self,
        expected_generation: u64,
        record: &SnapshotRecord,
    ) -> Result<SnapshotRecord, StoreError> {
        record
            .snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let provider = provider_columns(record.snapshot.provider_reference.as_ref());
        let result = sqlx::query("UPDATE native_snapshots SET generation = $1, state = $2, payload = $3, provider_name = $4, provider_resource_id = $5 WHERE id = $6 AND project_id = $7 AND generation = $8")
            .bind(i64::try_from(record.snapshot.generation).map_err(|_| StoreError::Corrupt("snapshot generation overflow".to_owned()))?).bind(state_name(&record.snapshot.state)?).bind(json(&record.snapshot)?).bind(provider.0).bind(provider.1)
            .bind(record.snapshot.id.to_string()).bind(&record.snapshot.project_id).bind(i64::try_from(expected_generation).map_err(|_| StoreError::StaleGeneration)?)
            .execute(&self.pool).await.map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::StaleGeneration);
        }
        self.get_snapshot(record.snapshot.id.as_uuid())
            .await?
            .ok_or(StoreError::ResourceNotFound)
    }

    async fn delete_snapshot(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM native_snapshots WHERE id = $1 AND project_id = $2")
            .bind(id.to_string())
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Database)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::ResourceNotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
// These tests use named assertions to keep database failures easy to diagnose;
// production paths remain free of panic-on-error handling.
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use o3k_domain::{
        AttachmentAccessMode, SnapshotConsistency, SnapshotId, SnapshotState,
        StorageExecutionScope, VolumeId, VolumeState,
    };

    fn volume() -> VolumeRecord {
        VolumeRecord {
            volume: Volume {
                id: VolumeId::from_uuid(Uuid::from_u128(11)),
                project_id: "project-a".to_owned(),
                name: "volume-a".to_owned(),
                description: String::new(),
                metadata: std::collections::BTreeMap::new(),
                availability_zone: None,
                size_bytes: 4096,
                volume_type: "lvm-thin".to_owned(),
                backend_id: "backend-a".to_owned(),
                execution_scope: StorageExecutionScope::Host("host-a".to_owned()),
                state: VolumeState::Requested,
                generation: 1,
                operation_id: None,
                provider_reference: None,
            },
            created_at: "2026-08-19T00:00:00Z".to_owned(),
        }
    }

    #[tokio::test]
    async fn sqlite_round_trip_preserves_canonical_volume_and_generation_fence() {
        let store = crate::testkit::open_memory().await.expect("store");
        let record = volume();
        store.insert_volume(&record).await.expect("insert");
        assert_eq!(
            store
                .get_volume(record.volume.id.as_uuid())
                .await
                .expect("get"),
            Some(record.clone())
        );
        let mut updated = record.clone();
        updated.volume.state = VolumeState::Creating;
        updated.volume.generation = 2;
        assert!(store.update_volume(1, &updated).await.is_ok());
        assert!(matches!(
            store.update_volume(1, &updated).await,
            Err(StoreError::StaleGeneration)
        ));
    }

    #[tokio::test]
    async fn sqlite_attachment_and_snapshot_have_typed_state() {
        let store = crate::testkit::open_memory().await.expect("store");
        let volume = volume();
        store.insert_volume(&volume).await.expect("insert volume");
        let attachment = VolumeAttachmentRecordV1 {
            attachment: VolumeAttachment {
                id: o3k_domain::VolumeAttachmentId::from_uuid(Uuid::from_u128(12)),
                project_id: "project-a".to_owned(),
                volume_id: volume.volume.id,
                server_id: Uuid::from_u128(13),
                execution_scope: StorageExecutionScope::Host("host-a".to_owned()),
                access_mode: AttachmentAccessMode::ReadWrite,
                delete_on_termination: false,
                state: o3k_domain::VolumeAttachmentState::Reserved,
                generation: 1,
                operation_id: None,
            },
            created_at: "2026-08-19T00:00:00Z".to_owned(),
        };
        store
            .insert_volume_attachment_v1(&attachment)
            .await
            .expect("insert attachment");
        assert_eq!(
            store
                .list_volume_attachments_v1("project-a")
                .await
                .expect("list")
                .len(),
            1
        );
        let snapshot = SnapshotRecord {
            snapshot: Snapshot {
                id: SnapshotId::from_uuid(Uuid::from_u128(14)),
                project_id: "project-a".to_owned(),
                volume_id: volume.volume.id,
                source_generation: 1,
                execution_scope: StorageExecutionScope::Host("host-a".to_owned()),
                consistency: SnapshotConsistency::CrashConsistent,
                state: SnapshotState::Requested,
                generation: 1,
                operation_id: None,
                provider_reference: None,
            },
            created_at: "2026-08-19T00:00:00Z".to_owned(),
        };
        store
            .insert_snapshot(&snapshot)
            .await
            .expect("insert snapshot");
        assert_eq!(
            store
                .get_snapshot(snapshot.snapshot.id.as_uuid())
                .await
                .expect("get")
                .expect("snapshot"),
            snapshot
        );
    }
}
