//! Manifest v1: durable source-to-destination identity and dependency mapping.
//!
//! This module stores migration intent only. It does not create destination
//! resources, call providers, transfer bytes, or perform cutover.

use crate::{Classification, ResourceKind, SourceSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};
use thiserror::Error;
use uuid::{Uuid, uuid};

const SCHEMA: &str = "o3k.migration.manifest/v1";
const PROFILE: &str = "p14-openstack-cold-migration-v1";
const MANIFEST_NAMESPACE: Uuid = uuid!("2b2e1c01-6f84-4f3a-9de2-9c5325c6cc44");

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest input is invalid: {0}")]
    Invalid(String),
    #[error("manifest integrity digest does not match content")]
    Integrity,
    #[error("manifest contains a secret-bearing field")]
    SecretField,
    #[error("manifest persistence failed: {0}")]
    Persistence(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRequest {
    pub migration_id: String,
    pub endpoint_fingerprint: String,
    pub profile: String,
    pub destination_scope_id: String,
    pub destination_profile: String,
    pub actor_principal_id: String,
    pub authenticated_service_principal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationManifest {
    pub schema: String,
    pub migration_id: String,
    pub profile_id: String,
    pub generation: u64,
    pub source: SourceIdentity,
    pub destination: DestinationIdentity,
    pub actor: ActorIdentity,
    pub phase: ManifestPhase,
    pub nodes: Vec<ManifestNode>,
    pub cutover: CutoverState,
    pub integrity: Integrity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub cloud_id: String,
    pub endpoint_fingerprint: String,
    pub snapshot_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationIdentity {
    pub profile: String,
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorIdentity {
    pub principal_id: String,
    pub authenticated_service_principal_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestPhase {
    Planned,
    Preflighted,
    Manifested,
    Transferring,
    Validated,
    CutoverPending,
    CutoverCommitted,
    Finalized,
    Blocked,
    RolledBack,
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestNode {
    pub key: String,
    pub resource_type: ResourceKind,
    pub source_id: String,
    pub destination_id: Option<String>,
    pub owner_scope_id: String,
    pub depends_on: Vec<String>,
    pub source_fingerprint: String,
    pub transfer: TransferPlan,
    pub unsupported: Vec<String>,
    pub verification: VerificationState,
    pub rollback: RollbackState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPlan {
    pub mode: TransferMode,
    pub artifact_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferMode {
    Api,
    Adapter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationState {
    Pending,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RollbackState {
    NotOwned,
    Owned,
    Removed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutoverState {
    pub source_quiesced: bool,
    pub committed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integrity {
    pub manifest_digest: String,
    pub algorithm: String,
}

pub fn build_manifest(
    snapshot: &SourceSnapshot,
    request: &ManifestRequest,
) -> Result<MigrationManifest, ManifestError> {
    nonempty(request.migration_id.as_str(), "migration id")?;
    nonempty(
        request.endpoint_fingerprint.as_str(),
        "endpoint fingerprint",
    )?;
    nonempty(request.destination_scope_id.as_str(), "destination scope")?;
    nonempty(request.actor_principal_id.as_str(), "actor principal")?;
    nonempty(
        request.authenticated_service_principal_id.as_str(),
        "service principal",
    )?;
    if request.profile != PROFILE {
        return Err(ManifestError::Invalid(
            "unsupported migration profile".into(),
        ));
    }
    if request.destination_profile != "native-rust-testlab"
        && request.destination_profile != "small-edge-cloud"
    {
        return Err(ManifestError::Invalid(
            "unsupported destination profile".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut source_to_key = BTreeMap::new();
    for resource in &snapshot.resources {
        let key = format!("{}/{}", resource.kind_string(), resource.source_id);
        if !seen.insert(key.clone()) {
            return Err(ManifestError::Invalid(
                "duplicate source resource key".into(),
            ));
        }
        source_to_key.insert(resource.source_id.clone(), key);
    }
    let mut nodes = Vec::with_capacity(snapshot.resources.len());
    for resource in &snapshot.resources {
        let key = format!("{}/{}", resource.kind_string(), resource.source_id);
        let depends_on = resource
            .dependencies
            .iter()
            .filter_map(|dependency| source_to_key.get(dependency).cloned())
            .collect::<Vec<_>>();
        let destination_id = if resource.classification == Classification::Blocked {
            None
        } else {
            Some(
                Uuid::new_v5(
                    &MANIFEST_NAMESPACE,
                    format!("{}:{}", request.migration_id, key).as_bytes(),
                )
                .to_string(),
            )
        };
        let unsupported = resource
            .reasons
            .iter()
            .map(|reason| format!("{}:{}", reason.code, reason.detail))
            .collect::<Vec<_>>();
        nodes.push(ManifestNode {
            key,
            resource_type: resource.kind,
            source_id: resource.source_id.clone(),
            destination_id,
            owner_scope_id: request.destination_scope_id.clone(),
            depends_on,
            source_fingerprint: resource.fingerprint.clone(),
            transfer: TransferPlan {
                mode: TransferMode::Api,
                artifact_digest: None,
            },
            unsupported,
            verification: VerificationState::Pending,
            rollback: RollbackState::NotOwned,
        });
    }
    nodes.sort_by(|left, right| left.key.cmp(&right.key));
    let mut manifest = MigrationManifest {
        schema: SCHEMA.into(),
        migration_id: request.migration_id.clone(),
        profile_id: request.profile.clone(),
        generation: 1,
        source: SourceIdentity {
            cloud_id: snapshot.source_cloud_id.clone(),
            endpoint_fingerprint: request.endpoint_fingerprint.clone(),
            snapshot_fingerprint: snapshot.snapshot_fingerprint.clone(),
        },
        destination: DestinationIdentity {
            profile: request.destination_profile.clone(),
            scope_id: request.destination_scope_id.clone(),
        },
        actor: ActorIdentity {
            principal_id: request.actor_principal_id.clone(),
            authenticated_service_principal_id: request.authenticated_service_principal_id.clone(),
        },
        phase: ManifestPhase::Manifested,
        nodes,
        cutover: CutoverState {
            source_quiesced: false,
            committed_at: None,
        },
        integrity: Integrity {
            manifest_digest: String::new(),
            algorithm: "sha256".into(),
        },
    };
    manifest.integrity.manifest_digest = digest_without_integrity(&manifest)?;
    validate(&manifest)?;
    Ok(manifest)
}

pub fn validate(manifest: &MigrationManifest) -> Result<(), ManifestError> {
    if manifest.schema != SCHEMA
        || manifest.profile_id != PROFILE
        || manifest.integrity.algorithm != "sha256"
    {
        return Err(ManifestError::Invalid(
            "unsupported manifest schema/profile/algorithm".into(),
        ));
    }
    for value in [
        manifest.migration_id.as_str(),
        manifest.source.cloud_id.as_str(),
        manifest.source.endpoint_fingerprint.as_str(),
        manifest.source.snapshot_fingerprint.as_str(),
        manifest.destination.scope_id.as_str(),
        manifest.actor.principal_id.as_str(),
        manifest.actor.authenticated_service_principal_id.as_str(),
    ] {
        nonempty(value, "manifest identity")?;
    }
    let serialized = serde_json::to_string(manifest)
        .map_err(|error| ManifestError::Invalid(error.to_string()))?;
    if contains_secret(&serialized) {
        return Err(ManifestError::SecretField);
    }
    let mut keys = BTreeSet::new();
    let mut destination_ids = BTreeSet::new();
    for node in &manifest.nodes {
        if !keys.insert(node.key.clone())
            || node.owner_scope_id != manifest.destination.scope_id
            || node.key != format!("{}/{}", kind_name(node.resource_type), node.source_id)
        {
            return Err(ManifestError::Invalid(
                "manifest node identity or ownership is invalid".into(),
            ));
        }
        if let Some(destination_id) = &node.destination_id
            && !destination_ids.insert(destination_id.clone())
        {
            return Err(ManifestError::Invalid(
                "destination mapping is not unique".into(),
            ));
        }
        if node.unsupported.is_empty() && node.destination_id.is_none() {
            return Err(ManifestError::Invalid(
                "supported node has no destination mapping".into(),
            ));
        }
        if contains_secret(node.key.as_str())
            || node.unsupported.iter().any(|value| contains_secret(value))
        {
            return Err(ManifestError::SecretField);
        }
    }
    if manifest.nodes.iter().any(|node| {
        node.depends_on
            .iter()
            .any(|dependency| !keys.contains(dependency))
    }) {
        return Err(ManifestError::Invalid(
            "dependency references an unknown node".into(),
        ));
    }
    if digest_without_integrity(manifest)? != manifest.integrity.manifest_digest {
        return Err(ManifestError::Integrity);
    }
    Ok(())
}

pub fn ensure_snapshot_unchanged(
    manifest: &MigrationManifest,
    snapshot: &SourceSnapshot,
) -> Result<(), ManifestError> {
    if manifest.source.cloud_id != snapshot.source_cloud_id
        || manifest.source.snapshot_fingerprint != snapshot.snapshot_fingerprint
    {
        return Err(ManifestError::Invalid("source snapshot changed".into()));
    }
    let fingerprints = snapshot
        .resources
        .iter()
        .map(|resource| {
            (
                format!("{}/{}", resource.kind_string(), resource.source_id),
                resource.fingerprint.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if manifest
        .nodes
        .iter()
        .any(|node| fingerprints.get(&node.key) != Some(&node.source_fingerprint.as_str()))
    {
        return Err(ManifestError::Invalid(
            "source resource fingerprint changed".into(),
        ));
    }
    Ok(())
}

fn digest_without_integrity(manifest: &MigrationManifest) -> Result<String, ManifestError> {
    let mut copy = manifest.clone();
    copy.integrity.manifest_digest.clear();
    let bytes =
        serde_json::to_vec(&copy).map_err(|error| ManifestError::Invalid(error.to_string()))?;
    Ok(digest(&bytes))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
fn nonempty(value: &str, label: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        Err(ManifestError::Invalid(format!("{label} is empty")))
    } else {
        Ok(())
    }
}
fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password",
        "token",
        "secret",
        "private_key",
        "private-key",
        "credential",
        "cookie",
    ]
    .iter()
    .any(|part| lower.contains(part))
}

pub struct FileManifestStore {
    path: PathBuf,
}
impl FileManifestStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn save(&self, manifest: &MigrationManifest) -> Result<(), ManifestError> {
        validate(manifest)?;
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|error| ManifestError::Persistence(error.to_string()))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ManifestError::Persistence(error.to_string()))?;
        }
        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| ManifestError::Persistence(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| ManifestError::Persistence(error.to_string()))?;
        }
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(ManifestError::Persistence(error.to_string()));
        }
        fs::rename(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            ManifestError::Persistence(error.to_string())
        })
    }
    pub fn load(&self) -> Result<MigrationManifest, ManifestError> {
        let bytes =
            fs::read(&self.path).map_err(|error| ManifestError::Persistence(error.to_string()))?;
        let manifest = serde_json::from_slice(&bytes)
            .map_err(|error| ManifestError::Persistence(error.to_string()))?;
        validate(&manifest)?;
        Ok(manifest)
    }
}

trait ResourceKindName {
    fn kind_string(&self) -> &'static str;
}

fn kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Project => "project",
        ResourceKind::Image => "image",
        ResourceKind::Flavor => "flavor",
        ResourceKind::Keypair => "keypair",
        ResourceKind::Network => "network",
        ResourceKind::Subnet => "subnet",
        ResourceKind::Port => "port",
        ResourceKind::SecurityGroup => "security_group",
        ResourceKind::SecurityGroupRule => "security_group_rule",
        ResourceKind::Router => "router",
        ResourceKind::RouterInterface => "router_interface",
        ResourceKind::FloatingIp => "floating_ip",
        ResourceKind::Server => "server",
        ResourceKind::Volume => "volume",
        ResourceKind::VolumeAttachment => "volume_attachment",
    }
}
impl ResourceKindName for crate::SourceResource {
    fn kind_string(&self) -> &'static str {
        match self.kind {
            ResourceKind::Project => "project",
            ResourceKind::Image => "image",
            ResourceKind::Flavor => "flavor",
            ResourceKind::Keypair => "keypair",
            ResourceKind::Network => "network",
            ResourceKind::Subnet => "subnet",
            ResourceKind::Port => "port",
            ResourceKind::SecurityGroup => "security_group",
            ResourceKind::SecurityGroupRule => "security_group_rule",
            ResourceKind::Router => "router",
            ResourceKind::RouterInterface => "router_interface",
            ResourceKind::FloatingIp => "floating_ip",
            ResourceKind::Server => "server",
            ResourceKind::Volume => "volume",
            ResourceKind::VolumeAttachment => "volume_attachment",
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{ClassificationReason, SourceResource};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn snapshot() -> SourceSnapshot {
        SourceSnapshot {
            schema: "o3k.migration.source-snapshot/v1".into(),
            source_cloud_id: "cloud-a".into(),
            project_id: "source-project".into(),
            generation_inputs: vec!["generation-a".into()],
            resources: vec![
                SourceResource {
                    kind: ResourceKind::Network,
                    source_id: "network-a".into(),
                    project_id: Some("source-project".into()),
                    name: Some("net".into()),
                    dependencies: vec![],
                    fingerprint: "a".repeat(64),
                    classification: Classification::Supported,
                    reasons: vec![],
                },
                SourceResource {
                    kind: ResourceKind::Port,
                    source_id: "port-a".into(),
                    project_id: Some("source-project".into()),
                    name: None,
                    dependencies: vec!["network-a".into()],
                    fingerprint: "b".repeat(64),
                    classification: Classification::Supported,
                    reasons: vec![],
                },
            ],
            snapshot_fingerprint: "c".repeat(64),
        }
    }
    fn request() -> ManifestRequest {
        ManifestRequest {
            migration_id: "migration-a".into(),
            endpoint_fingerprint: "e".repeat(64),
            profile: PROFILE.into(),
            destination_scope_id: "destination-project".into(),
            destination_profile: "native-rust-testlab".into(),
            actor_principal_id: "actor-a".into(),
            authenticated_service_principal_id: "migration-service".into(),
        }
    }
    #[test]
    fn mapping_is_deterministic_and_dependency_ordered() {
        let first = build_manifest(&snapshot(), &request()).expect("manifest");
        let second = build_manifest(&snapshot(), &request()).expect("manifest");
        assert_eq!(first, second);
        assert_eq!(first.nodes[1].depends_on, vec!["network/network-a"]);
        assert_ne!(first.nodes[0].destination_id.as_deref(), Some("network-a"));
    }
    #[test]
    fn tampering_and_secret_fields_fail_closed() {
        let mut manifest = build_manifest(&snapshot(), &request()).expect("manifest");
        manifest.nodes[0].source_fingerprint = "changed".into();
        assert_eq!(validate(&manifest), Err(ManifestError::Integrity));
        let mut manifest = build_manifest(&snapshot(), &request()).expect("manifest");
        manifest.nodes[0].unsupported.push("token=secret".into());
        manifest.integrity.manifest_digest = digest_without_integrity(&manifest).expect("digest");
        assert_eq!(validate(&manifest), Err(ManifestError::SecretField));
    }
    #[test]
    fn file_store_reconstructs_after_restart() {
        let path = std::env::temp_dir().join(format!(
            "o3k-manifest-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let store = FileManifestStore::new(&path);
        let manifest = build_manifest(&snapshot(), &request()).expect("manifest");
        store.save(&manifest).expect("save");
        let reopened = FileManifestStore::new(&path);
        assert_eq!(reopened.load().expect("load"), manifest);
        let _ = fs::remove_file(path);
    }
    #[test]
    fn blocked_resource_has_no_destination_mapping() {
        let mut value = snapshot();
        value.resources[0].classification = Classification::Blocked;
        value.resources[0].reasons = vec![ClassificationReason {
            code: "UNSUPPORTED_SEMANTIC".into(),
            detail: "trunk".into(),
        }];
        let manifest = build_manifest(&value, &request()).expect("manifest");
        assert!(manifest.nodes[1].destination_id.is_some());
        assert!(
            manifest
                .nodes
                .iter()
                .find(|node| node.key == "network/network-a")
                .and_then(|node| node.destination_id.as_ref())
                .is_none()
        );
    }
}
