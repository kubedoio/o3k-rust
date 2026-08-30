use std::sync::Arc;

use o3k_kernel::Authorizer;
use o3k_native_api::{error::NativeReadError, volume::VolumeItem};
use o3k_store::storage::StorageRepository;
use uuid::Uuid;

use super::helpers::{authorize_collection, authorize_instance};

// ── VolumeReader ──────────────────────────────────────────────────────────

pub struct VolumeReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub authorizer: Arc<dyn Authorizer>,
}

#[async_trait::async_trait]
impl o3k_native_api::volume::VolumeReader for VolumeReaderAdapter {
    async fn list_volumes(
        &self,
        auth: &o3k_kernel::AuthContext,
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
        match self.store.list_volumes(project_id).await {
            Ok(records) => Ok(records
                .into_iter()
                .map(|r| VolumeItem {
                    id: r.volume.id.to_string(),
                    project_id: r.volume.project_id.clone(),
                    name: r.volume.name.clone(),
                    description: r.volume.description.clone(),
                    metadata: serde_json::to_value(&r.volume.metadata)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    availability_zone: r.volume.availability_zone.clone(),
                    size_bytes: r.volume.size_bytes,
                    volume_type: r.volume.volume_type.clone(),
                    state: serde_json::to_value(r.volume.state)
                        .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                        .unwrap_or_else(|_| "unknown".to_owned()),
                    created_at: Some(r.created_at.clone()),
                    generation: r.volume.generation as i64,
                })
                .collect()),
            Err(e) => {
                tracing::error!(error = %e, project_id = %project_id, "native volume list failed");
                Err(NativeReadError::Internal)
            }
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
    use super::authorize_collection;

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
}
