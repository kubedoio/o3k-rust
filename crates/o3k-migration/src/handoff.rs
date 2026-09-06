//! Bounded OpenTofu handoff inputs and exact NO-OP proof.

use crate::manifest::MigrationManifest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandoffError {
    #[error("handoff manifest is invalid: {0}")]
    Manifest(String),
    #[error("handoff mapping is inconsistent: {0}")]
    Mapping(String),
    #[error("handoff observed state drifted")]
    Drift,
    #[error("handoff contains a forbidden value")]
    ForbiddenValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IaCResource {
    pub address: String,
    pub resource_type: String,
    pub canonical_id: String,
    pub owner_scope_id: String,
    pub source_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffState {
    pub migration_id: String,
    pub destination_scope_id: String,
    pub resources: Vec<IaCResource>,
    pub state_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoOpProof {
    pub migration_id: String,
    pub state_fingerprint: String,
    pub resource_count: usize,
}

pub fn build_handoff(manifest: &MigrationManifest) -> Result<HandoffState, HandoffError> {
    crate::manifest::validate(manifest)
        .map_err(|error| HandoffError::Manifest(error.to_string()))?;
    let mut resources = Vec::new();
    for node in &manifest.nodes {
        if !node.unsupported.is_empty() {
            continue;
        }
        let Some(canonical_id) = &node.destination_id else {
            return Err(HandoffError::Mapping(node.key.clone()));
        };
        if node.owner_scope_id != manifest.destination.scope_id
            || contains_forbidden(&node.key)
            || contains_forbidden(canonical_id)
        {
            return Err(HandoffError::ForbiddenValue);
        }
        resources.push(IaCResource {
            address: format!("o3k_migration.{}", node.key.replace('/', "_")),
            resource_type: node.resource_type_name().into(),
            canonical_id: canonical_id.clone(),
            owner_scope_id: node.owner_scope_id.clone(),
            source_fingerprint: node.source_fingerprint.clone(),
        });
    }
    resources.sort_by(|left, right| left.address.cmp(&right.address));
    let mut state = HandoffState {
        migration_id: manifest.migration_id.clone(),
        destination_scope_id: manifest.destination.scope_id.clone(),
        resources,
        state_fingerprint: String::new(),
    };
    state.state_fingerprint = digest(
        &serde_json::to_vec(&state).map_err(|error| HandoffError::Manifest(error.to_string()))?,
    );
    Ok(state)
}

pub fn refresh_noop(
    state: &HandoffState,
    observed: &[IaCResource],
) -> Result<NoOpProof, HandoffError> {
    let mut sorted = observed.to_vec();
    sorted.sort_by(|left, right| left.address.cmp(&right.address));
    if sorted != state.resources
        || sorted
            .iter()
            .any(|resource| resource.owner_scope_id != state.destination_scope_id)
    {
        return Err(HandoffError::Drift);
    }
    Ok(NoOpProof {
        migration_id: state.migration_id.clone(),
        state_fingerprint: state.state_fingerprint.clone(),
        resource_count: sorted.len(),
    })
}

fn contains_forbidden(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password",
        "token",
        "secret",
        "private",
        "backend",
        "credential",
    ]
    .iter()
    .any(|part| lower.contains(part))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

trait ResourceTypeName {
    fn resource_type_name(&self) -> &'static str;
}

impl ResourceTypeName for crate::manifest::ManifestNode {
    fn resource_type_name(&self) -> &'static str {
        match self.resource_type {
            crate::ResourceKind::Project => "project",
            crate::ResourceKind::Image => "image",
            crate::ResourceKind::Flavor => "flavor",
            crate::ResourceKind::Keypair => "keypair",
            crate::ResourceKind::Network => "network",
            crate::ResourceKind::Subnet => "subnet",
            crate::ResourceKind::Port => "port",
            crate::ResourceKind::SecurityGroup => "security_group",
            crate::ResourceKind::SecurityGroupRule => "security_group_rule",
            crate::ResourceKind::Router => "router",
            crate::ResourceKind::RouterInterface => "router_interface",
            crate::ResourceKind::FloatingIp => "floating_ip",
            crate::ResourceKind::Server => "server",
            crate::ResourceKind::Volume => "volume",
            crate::ResourceKind::VolumeAttachment => "volume_attachment",
        }
    }
}
