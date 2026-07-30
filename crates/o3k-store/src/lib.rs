use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use async_trait::async_trait;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use thiserror::Error;
use uuid::Uuid;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub id: Uuid,
    pub resource_id: Uuid,
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
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('resources', 'operations', 'provider_refs')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Database)?;
        if table_count != 3 {
            return Err(StoreError::Corrupt("required table is missing".to_owned()));
        }
        Ok(())
    }
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

    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO operations (id, resource_id, state, provider_operation_id, error_category, error_message) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(operation.id.to_string())
            .bind(operation.resource_id.to_string())
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
        let row = sqlx::query("SELECT id, resource_id, state, provider_operation_id, error_category, error_message FROM operations WHERE id = ?")
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

    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::Database)?;
        sqlx::query("INSERT INTO resources (id, kind, project_id, generation, observed_generation, desired_state, observed_state, provider_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(resource.id.to_string())
            .bind(&resource.kind)
            .bind(&resource.project_id)
            .bind(resource.generation)
            .bind(resource.observed_generation)
            .bind(&resource.desired_state)
            .bind(&resource.observed_state)
            .bind(&resource.provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::Database)?;
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
        state: OperationState::parse(row.get("state"))?,
        provider_operation_id: row.get("provider_operation_id"),
        error_category: row.get("error_category"),
        error_message: row.get("error_message"),
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
}
