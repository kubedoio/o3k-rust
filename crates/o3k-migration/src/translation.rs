//! Canonical network/policy/L3 translation plan.

use crate::{
    ResourceKind,
    manifest::{ManifestError, MigrationManifest},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TranslationError {
    #[error("manifest is invalid: {0}")]
    Manifest(String),
    #[error("network translation is blocked: {0}")]
    Blocked(String),
    #[error("network translation dependency is missing")]
    MissingDependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalKind {
    Network,
    AddressRealm,
    SubnetIntent,
    EndpointIntent,
    NetworkPolicy,
    L3Gateway,
    GatewayAttachment,
    PublicAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationNode {
    pub source_key: String,
    pub canonical_kind: CanonicalKind,
    pub canonical_id: String,
    pub owner_scope_id: String,
    pub depends_on: Vec<String>,
    pub source_fingerprint: String,
    pub ownership_marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkTranslationPlan {
    pub migration_id: String,
    pub destination_scope_id: String,
    pub nodes: Vec<TranslationNode>,
    pub plan_fingerprint: String,
    pub rollback_order: Vec<String>,
}

pub fn translate(manifest: &MigrationManifest) -> Result<NetworkTranslationPlan, TranslationError> {
    crate::manifest::validate(manifest).map_err(map_manifest)?;
    let mut nodes = Vec::new();
    for node in &manifest.nodes {
        let Some(kind) = canonical_kind(node.resource_type) else {
            continue;
        };
        if !node.unsupported.is_empty() || node.destination_id.is_none() {
            return Err(TranslationError::Blocked(node.key.clone()));
        }
        let canonical_id = node
            .destination_id
            .clone()
            .ok_or_else(|| TranslationError::Blocked(node.key.clone()))?;
        nodes.push(TranslationNode {
            source_key: node.key.clone(),
            canonical_kind: kind,
            canonical_id: canonical_id.clone(),
            owner_scope_id: node.owner_scope_id.clone(),
            depends_on: node.depends_on.clone(),
            source_fingerprint: node.source_fingerprint.clone(),
            ownership_marker: format!("o3k:migration:{}:{}", manifest.migration_id, canonical_id),
        });
    }
    let known = nodes
        .iter()
        .map(|node| node.source_key.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if nodes.iter().any(|node| {
        node.depends_on.iter().any(|dependency| {
            !known.contains(dependency) && relevant_dependency(dependency, manifest)
        })
    }) {
        return Err(TranslationError::MissingDependency);
    }
    let mut plan = NetworkTranslationPlan {
        migration_id: manifest.migration_id.clone(),
        destination_scope_id: manifest.destination.scope_id.clone(),
        rollback_order: nodes
            .iter()
            .rev()
            .map(|node| node.canonical_id.clone())
            .collect(),
        nodes,
        plan_fingerprint: String::new(),
    };
    plan.plan_fingerprint = digest(
        &serde_json::to_vec(&plan)
            .map_err(|error| TranslationError::Manifest(error.to_string()))?,
    );
    Ok(plan)
}

fn relevant_dependency(dependency: &str, manifest: &MigrationManifest) -> bool {
    manifest
        .nodes
        .iter()
        .any(|node| node.key == dependency && canonical_kind(node.resource_type).is_some())
}
fn canonical_kind(kind: ResourceKind) -> Option<CanonicalKind> {
    Some(match kind {
        ResourceKind::Network => CanonicalKind::Network,
        ResourceKind::Subnet => CanonicalKind::SubnetIntent,
        ResourceKind::Port => CanonicalKind::EndpointIntent,
        ResourceKind::SecurityGroup | ResourceKind::SecurityGroupRule => {
            CanonicalKind::NetworkPolicy
        }
        ResourceKind::Router => CanonicalKind::L3Gateway,
        ResourceKind::RouterInterface => CanonicalKind::GatewayAttachment,
        ResourceKind::FloatingIp => CanonicalKind::PublicAddress,
        _ => return None,
    })
}
fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
fn map_manifest(error: ManifestError) -> TranslationError {
    TranslationError::Manifest(error.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::manifest::{ManifestRequest, build_manifest};
    use crate::{Classification, ResourceKind, SourceResource, SourceSnapshot};
    fn fixture() -> MigrationManifest {
        let snapshot = SourceSnapshot {
            schema: "o3k.migration.source-snapshot/v1".into(),
            source_cloud_id: "cloud".into(),
            project_id: "source".into(),
            generation_inputs: vec![],
            resources: vec![
                SourceResource {
                    kind: ResourceKind::Network,
                    source_id: "net".into(),
                    project_id: Some("source".into()),
                    name: None,
                    dependencies: vec![],
                    fingerprint: "a".repeat(64),
                    classification: Classification::Supported,
                    reasons: vec![],
                },
                SourceResource {
                    kind: ResourceKind::Port,
                    source_id: "port".into(),
                    project_id: Some("source".into()),
                    name: None,
                    dependencies: vec!["net".into()],
                    fingerprint: "b".repeat(64),
                    classification: Classification::Supported,
                    reasons: vec![],
                },
            ],
            snapshot_fingerprint: "c".repeat(64),
        };
        build_manifest(
            &snapshot,
            &ManifestRequest {
                migration_id: "m".into(),
                endpoint_fingerprint: "d".repeat(64),
                profile: "p14-openstack-cold-migration-v1".into(),
                destination_scope_id: "destination".into(),
                destination_profile: "native-rust-testlab".into(),
                actor_principal_id: "actor".into(),
                authenticated_service_principal_id: "service".into(),
            },
        )
        .expect("manifest")
    }
    #[test]
    fn translates_to_canonical_kinds_and_reverse_rollback() {
        let plan = translate(&fixture()).expect("plan");
        assert_eq!(plan.nodes[0].canonical_kind, CanonicalKind::Network);
        assert_eq!(plan.nodes[1].canonical_kind, CanonicalKind::EndpointIntent);
        assert_eq!(plan.rollback_order[0], plan.nodes[1].canonical_id);
        assert_ne!(plan.nodes[0].canonical_id, "net");
        assert!(
            plan.nodes
                .iter()
                .all(|node| node.owner_scope_id == "destination")
        );
    }
    #[test]
    fn blocked_provider_extension_fails_closed() {
        let mut manifest = fixture();
        manifest.nodes[0]
            .unsupported
            .push("UNSUPPORTED_SEMANTIC:trunk".into());
        let mut unsigned = manifest.clone();
        unsigned.integrity.manifest_digest.clear();
        manifest.integrity.manifest_digest =
            digest(&serde_json::to_vec(&unsigned).expect("serialize"));
        assert!(matches!(
            translate(&manifest),
            Err(TranslationError::Blocked(_))
        ));
    }
}
