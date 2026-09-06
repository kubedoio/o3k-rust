//! Failure, compensation, and explicit source-finalization contracts.

use crate::manifest::{ManifestPhase, MigrationManifest, RollbackState};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecoveryError {
    #[error("recovery manifest is not valid for this action: {0}")]
    InvalidState(String),
    #[error("cleanup is blocked because ownership cannot be proven")]
    OwnershipBlocked,
    #[error("source finalization requires explicit operator authorization")]
    FinalizationNotAuthorized,
    #[error("source identity or fingerprint no longer matches")]
    SourceDrift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPlan {
    pub migration_id: String,
    pub resource_ids: Vec<String>,
    pub ownership_marker: String,
    pub source_restore_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFinalizationRequest {
    pub operator_principal_id: String,
    pub migration_id: String,
    pub source_cloud_id: String,
    pub source_snapshot_fingerprint: String,
    pub explicit_destructive_action: bool,
}

pub fn plan_rollback(manifest: &MigrationManifest) -> Result<RollbackPlan, RecoveryError> {
    crate::manifest::validate(manifest)
        .map_err(|error| RecoveryError::InvalidState(error.to_string()))?;
    if manifest.phase == ManifestPhase::CutoverCommitted
        || manifest.phase == ManifestPhase::Finalized
    {
        return Err(RecoveryError::InvalidState(
            "O3K authority is already committed".into(),
        ));
    }
    if manifest.phase == ManifestPhase::UnknownOutcome {
        return Err(RecoveryError::InvalidState(
            "observe unknown outcome before rollback".into(),
        ));
    }
    let owned = manifest
        .nodes
        .iter()
        .filter(|node| node.rollback == RollbackState::Owned)
        .collect::<Vec<_>>();
    if manifest
        .nodes
        .iter()
        .any(|node| node.rollback == RollbackState::Owned && node.destination_id.is_none())
    {
        return Err(RecoveryError::OwnershipBlocked);
    }
    Ok(RollbackPlan {
        migration_id: manifest.migration_id.clone(),
        resource_ids: owned
            .into_iter()
            .rev()
            .filter_map(|node| node.destination_id.clone())
            .collect(),
        ownership_marker: format!("o3k:migration:{}", manifest.migration_id),
        source_restore_required: manifest.cutover.source_quiesced,
    })
}

pub fn mark_rolled_back(manifest: &MigrationManifest) -> Result<MigrationManifest, RecoveryError> {
    let _ = plan_rollback(manifest)?;
    let mut rolled_back = manifest.clone();
    rolled_back.phase = ManifestPhase::RolledBack;
    Ok(rolled_back)
}

pub fn authorize_source_finalization(
    manifest: &MigrationManifest,
    request: &SourceFinalizationRequest,
) -> Result<(), RecoveryError> {
    if manifest.phase != ManifestPhase::CutoverCommitted {
        return Err(RecoveryError::InvalidState(
            "source finalization requires committed O3K authority".into(),
        ));
    }
    if !request.explicit_destructive_action
        || request.migration_id != manifest.migration_id
        || request.source_cloud_id != manifest.source.cloud_id
        || request.source_snapshot_fingerprint != manifest.source.snapshot_fingerprint
        || request.operator_principal_id.trim().is_empty()
    {
        return Err(RecoveryError::FinalizationNotAuthorized);
    }
    Ok(())
}

pub fn retry_backoff_ms(attempt: u32) -> u64 {
    100_u64
        .saturating_mul(2_u64.saturating_pow(attempt.min(6)))
        .min(5_000)
}
