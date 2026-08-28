use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditSink, AuthContext, AuthorizationRequest, Authorizer,
    LimitKey, NoopAuditSink, OwnershipScope, ResourceAmount, ResourceId, ResourceTarget,
    ResourceType, ScopeId, ServiceNamespace, StaticAuthorizer,
};
use o3k_store::{ImageMetadataRecord, ImageRepository, StoreError};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::internal::{
    TemporaryKind, ensure_managed_directory, remove_temporary_files, validate_qcow2_structure,
};
use crate::qemu_img::is_checksum;
use crate::record::{ImageArtifact, ImageError, ImageRecord, ImageStatus};

/// Image service: CRUD, upload, download, authorization, audit.
#[derive(Clone)]
pub struct ImageService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
    max_upload_bytes: usize,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn ImageRepository>,
}

impl ImageService {
    pub async fn open(
        root: impl Into<PathBuf>,
        max_upload_bytes: usize,
        repository: Arc<dyn ImageRepository>,
    ) -> Result<Self, ImageError> {
        let root = root.into();
        ensure_managed_directory(&root)?;
        let content = root.join("content");
        ensure_managed_directory(&content)?;
        remove_temporary_files(&content, TemporaryKind::Upload)?;
        Ok(Self {
            inner: Arc::new(Inner { root, repository }),
            lock: Arc::new(tokio::sync::Mutex::new(())),
            max_upload_bytes,
            authorizer: Arc::new(StaticAuthorizer::standard()),
            audit_sink: Arc::new(NoopAuditSink),
        })
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_audit_sink(mut self, audit_sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = audit_sink;
        self
    }

    pub async fn create(
        &self,
        auth: &AuthContext,
        name: String,
        visibility: String,
        container_format: String,
        disk_format: String,
    ) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "CreateImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "CreateImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::Unauthorized);
        }
        match self
            .create_for_project(
                auth.effective_scope().id().as_str(),
                name,
                visibility,
                container_format,
                disk_format,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_for_project(
        &self,
        project_id: &str,
        name: String,
        visibility: String,
        container_format: String,
        disk_format: String,
    ) -> Result<ImageRecord, ImageError> {
        if name.trim().is_empty()
            || container_format.trim().is_empty()
            || disk_format.trim().is_empty()
            || container_format != "bare"
            || !matches!(disk_format.as_str(), "raw" | "qcow2")
            || visibility != "private"
        {
            return Err(ImageError::InvalidMetadata);
        }
        let record = ImageMetadataRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "queued".to_owned(),
            visibility,
            container_format,
            disk_format,
            size: None,
            checksum: None,
        };
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(LimitKey::image_images(), 1)];
        let op_id = format!("o3k:image:create:{}:{}", project_id, record.id);
        let quota_res = self
            .inner
            .repository
            .reserve_quota(&scope, &op_id, &amounts)
            .await
            .map_err(|err| match err {
                StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => ImageError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                StoreError::ReservationConflict(_) => ImageError::Conflict,
                other => ImageError::Store(other),
            })?;

        match self.inner.repository.insert_image(&record).await {
            Ok(()) => {
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                image_from_store(record)
            }
            Err(error) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(Self::map_store_error(error))
            }
        }
    }

    pub async fn list(&self, auth: &AuthContext) -> Result<Vec<ImageRecord>, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "ListImages").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "ListImages".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::Unauthorized);
        }
        self.list_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_for_project(&self, project_id: &str) -> Result<Vec<ImageRecord>, ImageError> {
        self.inner
            .repository
            .list_images(project_id)
            .await
            .map_err(Self::map_store_error)?
            .into_iter()
            .map(image_from_store)
            .collect()
    }

    pub async fn get(&self, auth: &AuthContext, id: Uuid) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "ReadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "ReadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        self.get_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<ImageRecord, ImageError> {
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        image_from_store(record)
    }

    pub async fn resolve_artifact(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<ImageArtifact, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "DownloadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "DownloadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        self.resolve_artifact_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn resolve_artifact_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<ImageArtifact, ImageError> {
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        if record.status != "active" {
            return Err(ImageError::NotFound);
        }
        let checksum = record.checksum.ok_or(ImageError::NotFound)?;
        let size = record
            .size
            .map(|value| u64::try_from(value).map_err(|_| ImageError::NotFound))
            .transpose()?
            .ok_or(ImageError::NotFound)?;
        if !matches!(record.disk_format.as_str(), "raw" | "qcow2") {
            return Err(ImageError::UnsupportedFormat);
        }
        let path = content_path(&self.inner.root, id);
        if path.is_symlink() || !path.is_file() {
            return Err(ImageError::NotFound);
        }
        let mut file = fs::File::open(&path).map_err(ImageError::Storage)?;
        let actual_size = file.metadata().map_err(ImageError::Storage)?.len();
        if actual_size > self.max_upload_bytes as u64 {
            return Err(ImageError::TooLarge);
        }
        let mut content = Vec::with_capacity(actual_size as usize);
        file.read_to_end(&mut content)
            .map_err(ImageError::Storage)?;
        if content.len() as u64 != size
            || !is_checksum(&checksum)
            || format!("{:x}", Sha256::digest(&content)) != checksum
        {
            return Err(ImageError::ChecksumMismatch);
        }
        Ok(ImageArtifact {
            id,
            checksum,
            format: record.disk_format,
            size,
            content,
        })
    }

    pub async fn upload(
        &self,
        auth: &AuthContext,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "UploadImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "UploadImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        match self
            .upload_for_project(auth.effective_scope().id().as_str(), id, content)
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn upload_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        content: &[u8],
    ) -> Result<ImageRecord, ImageError> {
        if content.len() > self.max_upload_bytes {
            return Err(ImageError::TooLarge);
        }
        let _guard = self.lock.lock().await;
        let record = self
            .inner
            .repository
            .get_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?
            .ok_or(ImageError::NotFound)?;
        let record = image_from_store(record)?;
        if record.status == ImageStatus::Active {
            return Err(ImageError::Conflict);
        }
        if record.disk_format == "qcow2" {
            let mut reader = std::io::Cursor::new(content);
            validate_qcow2_structure(&mut reader, content.len() as u64)?;
        }
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(
            LimitKey::image_bytes(),
            content.len() as u64,
        )];
        let op_id = format!("o3k:image:upload:{}:{}", project_id, id);
        let quota_res = self
            .inner
            .repository
            .reserve_quota(&scope, &op_id, &amounts)
            .await
            .map_err(|err| match err {
                StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => ImageError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                StoreError::ReservationConflict(_) => ImageError::Conflict,
                other => ImageError::Store(other),
            })?;

        let content_path = content_path(&self.inner.root, id);
        let temporary_path = content_path.with_extension(format!("upload-{}", Uuid::now_v7()));
        if let Err(error) = fs::write(&temporary_path, content) {
            let _ = fs::remove_file(&temporary_path);
            let _ = self
                .inner
                .repository
                .release_reservation(&quota_res.id)
                .await;
            return Err(ImageError::Storage(error));
        }
        if let Err(error) = fs::rename(&temporary_path, &content_path) {
            let _ = fs::remove_file(&temporary_path);
            let _ = self
                .inner
                .repository
                .release_reservation(&quota_res.id)
                .await;
            return Err(ImageError::Storage(error));
        }
        let checksum = format!("{:x}", Sha256::digest(content));
        match self
            .inner
            .repository
            .activate_image(project_id, &id, content.len() as u64, &checksum)
            .await
        {
            Ok(record) => {
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                image_from_store(record)
            }
            Err(error) => {
                let _ = fs::remove_file(&content_path);
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(Self::map_store_error(error))
            }
        }
    }

    pub async fn delete(&self, auth: &AuthContext, id: Uuid) -> Result<(), ImageError> {
        let ns = ServiceNamespace::new("image")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("image".to_owned()));
        let act = ActionId::new("image", "DeleteImage").unwrap_or_else(|_| {
            ActionId::new_unchecked("image".to_owned(), "DeleteImage".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("image", "image").map_err(|_| ImageError::InvalidMetadata)?,
                ResourceId::new(id.to_string()).map_err(|_| ImageError::InvalidMetadata)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ImageError::NotFound);
        }
        match self
            .delete_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("image", "image").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("image".to_owned(), "image".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_for_project(&self, project_id: &str, id: Uuid) -> Result<(), ImageError> {
        let _guard = self.lock.lock().await;
        self.inner
            .repository
            .delete_image(project_id, &id)
            .await
            .map_err(Self::map_store_error)?;
        let content = content_path(&self.inner.root, id);
        if content.exists() {
            fs::remove_file(content).map_err(ImageError::Storage)?;
        }
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:image:create:{}:{}", project_id, id))
            .await;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:image:upload:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    fn map_store_error(error: StoreError) -> ImageError {
        match error {
            StoreError::ImageNotFound => ImageError::NotFound,
            StoreError::ImageAlreadyActive => ImageError::Conflict,
            other => ImageError::Store(other),
        }
    }
}

fn content_path(root: &Path, id: Uuid) -> PathBuf {
    root.join("content").join(id.to_string())
}

fn image_from_store(record: ImageMetadataRecord) -> Result<ImageRecord, ImageError> {
    let status = match record.status.as_str() {
        "queued" => ImageStatus::Queued,
        "active" => ImageStatus::Active,
        // An unknown status is corrupt durable state; fail closed instead of
        // inventing a status projection.
        _ => {
            return Err(ImageError::Store(StoreError::Corrupt(format!(
                "image {} has unknown status `{}`",
                record.id, record.status
            ))));
        }
    };
    Ok(ImageRecord {
        id: record.id,
        name: record.name,
        project_id: record.project_id,
        status,
        visibility: record.visibility,
        container_format: record.container_format,
        disk_format: record.disk_format,
        size: record
            .size
            .map(|size| {
                u64::try_from(size).map_err(|_| {
                    StoreError::Corrupt(format!("image {} has invalid size", record.id))
                })
            })
            .transpose()
            .map_err(ImageError::Store)?,
        checksum: record.checksum,
    })
}
