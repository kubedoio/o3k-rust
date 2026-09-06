//! Cold server reconstruction and explicit, one-way cutover contracts.

use crate::ResourceKind;
use crate::manifest::{
    ManifestError, ManifestPhase, MigrationManifest, RollbackState, VerificationState,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CutoverError {
    #[error("cutover manifest is invalid: {0}")]
    Manifest(String),
    #[error("cutover is not ready: {0}")]
    NotReady(String),
    #[error("cutover authorization is invalid")]
    Unauthorized,
    #[error("cutover commit conflicts with an existing commit")]
    CommitConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutoverAuthorization {
    pub principal_id: String,
    pub destination_scope_id: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCutoverPlan {
    pub migration_id: String,
    pub destination_scope_id: String,
    pub server_ids: Vec<String>,
    pub dependency_ids: Vec<String>,
    pub ownership_marker: String,
}

pub fn prepare_server_cutover(
    manifest: &MigrationManifest,
    authorization: &CutoverAuthorization,
) -> Result<ServerCutoverPlan, CutoverError> {
    crate::manifest::validate(manifest).map_err(map_manifest)?;
    if authorization.principal_id != manifest.actor.principal_id
        || authorization.destination_scope_id != manifest.destination.scope_id
        || authorization.action != "migrate:commit"
    {
        return Err(CutoverError::Unauthorized);
    }
    if manifest.phase != ManifestPhase::CutoverPending || !manifest.cutover.source_quiesced {
        return Err(CutoverError::NotReady(
            "manifest is not quiesced and cutover-pending".into(),
        ));
    }
    if manifest.nodes.iter().any(|node| {
        node.unsupported.is_empty()
            && (node.destination_id.is_none()
                || node.verification != VerificationState::Verified
                || node.rollback != RollbackState::Owned)
    }) {
        return Err(CutoverError::NotReady(
            "supported dependency is not verified and durably owned".into(),
        ));
    }
    if manifest.nodes.iter().any(|node| {
        node.unsupported.is_empty()
            && node.resource_type == ResourceKind::Server
            && node.destination_id.is_none()
    }) {
        return Err(CutoverError::NotReady(
            "server has no canonical destination".into(),
        ));
    }
    let server_ids = manifest
        .nodes
        .iter()
        .filter(|node| node.resource_type == ResourceKind::Server && node.unsupported.is_empty())
        .filter_map(|node| node.destination_id.clone())
        .collect::<Vec<_>>();
    if server_ids.is_empty() {
        return Err(CutoverError::NotReady(
            "no supported server is present".into(),
        ));
    }
    let dependency_ids = manifest
        .nodes
        .iter()
        .filter(|node| node.resource_type != ResourceKind::Server && node.unsupported.is_empty())
        .filter_map(|node| node.destination_id.clone())
        .collect::<Vec<_>>();
    Ok(ServerCutoverPlan {
        migration_id: manifest.migration_id.clone(),
        destination_scope_id: manifest.destination.scope_id.clone(),
        server_ids,
        dependency_ids,
        ownership_marker: format!("o3k:migration:{}:cutover", manifest.migration_id),
    })
}

pub fn commit_cutover(
    manifest: &MigrationManifest,
    authorization: &CutoverAuthorization,
    commit_id: &str,
) -> Result<MigrationManifest, CutoverError> {
    if commit_id.trim().is_empty() {
        return Err(CutoverError::NotReady("commit identity is empty".into()));
    }
    if manifest.phase == ManifestPhase::CutoverCommitted {
        return if manifest.cutover.committed_at.as_deref() == Some(commit_id) {
            Ok(manifest.clone())
        } else {
            Err(CutoverError::CommitConflict)
        };
    }
    prepare_server_cutover(manifest, authorization)?;
    let mut committed = manifest.clone();
    committed.phase = ManifestPhase::CutoverCommitted;
    committed.cutover.committed_at = Some(commit_id.to_owned());
    Ok(committed)
}

fn map_manifest(error: ManifestError) -> CutoverError {
    CutoverError::Manifest(error.to_string())
}
