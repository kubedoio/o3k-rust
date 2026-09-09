use std::sync::Arc;

use o3k_kernel::Authorizer;
use o3k_native_api::{error::NativeReadError, volume::VolumeItem};
use o3k_store::storage::StorageRepository;
use uuid::Uuid;

use super::helpers::{authorize_collection, authorize_instance};
use super::resource::sanitize_public_spec;

// ── VolumeReader ──────────────────────────────────────────────────────────

pub struct VolumeReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub authorizer: Arc<dyn Authorizer>,
}

fn volume_item(r: o3k_store::storage::VolumeRecord) -> VolumeItem {
    VolumeItem {
        id: r.volume.id.to_string(),
        project_id: r.volume.project_id.clone(),
        name: r.volume.name.clone(),
        description: r.volume.description.clone(),
        metadata: serde_json::to_value(&r.volume.metadata)
            .map(sanitize_public_spec)
            .unwrap_or_else(|_| serde_json::json!({})),
        availability_zone: r.volume.availability_zone.clone(),
        size_bytes: r.volume.size_bytes,
        volume_type: r.volume.volume_type.clone(),
        state: serde_json::to_value(r.volume.state)
            .map(|v| v.as_str().unwrap_or("unknown").to_owned())
            .unwrap_or_else(|_| "unknown".to_owned()),
        created_at: Some(r.created_at.clone()),
        generation: r.volume.generation as i64,
    }
}

#[async_trait::async_trait]
impl o3k_native_api::volume::VolumeReader for VolumeReaderAdapter {
    async fn list_volumes_page(
        &self,
        auth: &o3k_kernel::AuthContext,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<VolumeItem>, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_collection(
            auth,
            "volume:ListVolumes",
            "volume",
            "volume",
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        self.store
            .list_volumes_page(project_id, after_id, limit)
            .await
            .map(|records| records.into_iter().map(volume_item).collect())
            .map_err(|error| {
                tracing::error!(%error, "native bounded volume list failed");
                NativeReadError::Internal
            })
    }
    async fn list_volumes(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<VolumeItem>, NativeReadError> {
        if !authorize_collection(
            auth,
            "volume:ListVolumes",
            "volume",
            "volume",
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        const MAX_LEGACY_ITEMS: usize = 10_000;
        match self
            .list_volumes_page(auth, None, MAX_LEGACY_ITEMS + 1)
            .await
        {
            Ok(items) if items.len() <= MAX_LEGACY_ITEMS => Ok(items),
            Ok(_) => Err(NativeReadError::Internal),
            Err(error) => Err(error),
        }
    }

    async fn show_volume(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<VolumeItem, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_instance(
            auth,
            "volume:ReadVolume",
            "volume",
            "volume",
            id,
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        match self.store.get_volume(id).await {
            Ok(Some(r)) if r.volume.project_id == project_id => Ok(VolumeItem {
                id: r.volume.id.to_string(),
                project_id: r.volume.project_id.clone(),
                name: r.volume.name.clone(),
                description: r.volume.description.clone(),
                metadata: serde_json::to_value(&r.volume.metadata)
                    .map(sanitize_public_spec)
                    .unwrap_or_else(|_| serde_json::json!({})),
                availability_zone: r.volume.availability_zone.clone(),
                size_bytes: r.volume.size_bytes,
                volume_type: r.volume.volume_type.clone(),
                state: serde_json::to_value(r.volume.state)
                    .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                    .unwrap_or_else(|_| "unknown".to_owned()),
                created_at: Some(r.created_at.clone()),
                generation: r.volume.generation as i64,
            }),
            Ok(_) => Err(NativeReadError::NotFound),
            Err(e) => {
                tracing::error!(error = %e, volume_id = %id, "native volume show failed");
                Err(NativeReadError::Internal)
            }
        }
    }
}
#[cfg(test)]
mod volume_reader_tests {
    use super::{authorize_collection, volume_item};
    use o3k_domain::{StorageExecutionScope, Volume, VolumeId, VolumeState};
    use o3k_store::storage::VolumeRecord;
    use std::collections::BTreeMap;

    #[test]
    fn denied_canonical_volume_action_blocks_matching_scope() {
        let auth = o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("user-b"),
                "user-b",
                None,
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked("project-b"),
                None,
                None,
            ),
            vec!["member".into()],
            1,
            2,
            "audit",
            "request",
            None,
        );
        assert!(!authorize_collection(
            &auth,
            "volume:ListVolumes",
            "volume",
            "volume",
            &o3k_kernel::StaticAuthorizer::empty(),
        ));
    }

    #[test]
    fn volume_projection_redacts_secret_and_provider_metadata() {
        let id = uuid::Uuid::new_v4();
        let mut metadata = BTreeMap::new();
        metadata.insert("chap_password".to_owned(), "do-not-return".to_owned());
        metadata.insert("provider_id".to_owned(), "backend-secret".to_owned());
        metadata.insert("source_key".to_owned(), "migration-source".to_owned());
        let record = VolumeRecord {
            volume: Volume {
                id: VolumeId::from_uuid(id),
                project_id: "project-a".to_owned(),
                name: "safe".to_owned(),
                description: String::new(),
                metadata,
                availability_zone: None,
                size_bytes: 1,
                volume_type: "thin".to_owned(),
                backend_id: "local".to_owned(),
                execution_scope: StorageExecutionScope::Host("local".to_owned()),
                state: VolumeState::Available,
                generation: 1,
                operation_id: None,
                provider_reference: None,
            },
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let item = volume_item(record);
        let rendered = item.metadata.to_string();
        assert!(!rendered.contains("do-not-return"));
        assert!(!rendered.contains("backend-secret"));
        assert!(rendered.contains("migration-source"));
    }
}
