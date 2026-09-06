//! Process-level composition for the bounded P14 migration.
//!
//! The runner owns workflow ordering and durable phase projection.  Source
//! and destination adapters remain explicit ports: this module never writes a
//! database and never treats provider state as canonical ownership evidence.

use crate::cutover::{
    CutoverAuthorization, commit_cutover, prepare_server_cutover, validate_cutover_authorization,
};
use crate::manifest::{
    FileManifestStore, ManifestError, ManifestNode, ManifestPhase, MigrationManifest,
    RollbackState, VerificationState, ensure_snapshot_unchanged, validate,
};
use crate::recovery::{RecoveryError, mark_rolled_back, plan_rollback};
use crate::{OpenStackSource, ResourceKind, SourceDocument, SourceSnapshot};
use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use thiserror::Error;

pub use crate::tofu::run_opentofu_noop;

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
    #[error("recovery error: {0}")]
    Recovery(#[from] RecoveryError),
    #[error("source error: {0}")]
    Source(String),
    #[error("destination error: {0}")]
    Destination(String),
    #[error("workflow is fenced: {0}")]
    Fenced(String),
    #[error("OpenTofu failed: {0}")]
    OpenTofu(String),
    #[error("invalid runner input: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationObservation {
    pub destination_id: String,
    pub migration_id: String,
    pub owner_scope_id: String,
    pub source_key: String,
    pub owned_by_migration: bool,
    pub complete: bool,
}

/// The destination port is deliberately expressed in canonical resource
/// terms. Implementations call O3K application APIs, not its database.
#[async_trait]
pub trait CanonicalDestination: Send + Sync {
    async fn observe(
        &self,
        migration_id: &str,
        node: &ManifestNode,
    ) -> Result<Option<DestinationObservation>, RunnerError>;
    async fn create(
        &self,
        migration_id: &str,
        node: &ManifestNode,
        source: &SourceDocument,
    ) -> Result<DestinationObservation, RunnerError>;
    async fn delete_owned(
        &self,
        migration_id: &str,
        node: &ManifestNode,
    ) -> Result<(), RunnerError>;
    async fn verify(&self, migration_id: &str, node: &ManifestNode) -> Result<(), RunnerError>;
}

/// Operations that are intentionally still owned by the source cloud until
/// the one-way cutover marker is committed.
#[async_trait]
pub trait SourceControl: Send + Sync {
    async fn quiesce(&self, server_source_id: &str) -> Result<(), RunnerError>;
    async fn final_sync(&self, snapshot: &SourceSnapshot) -> Result<(), RunnerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub migration_id: String,
    pub phase: ManifestPhase,
    pub created: usize,
    pub resumed: usize,
}

/// Durable process-level orchestrator. Each side effect is preceded by an
/// observation and each manifest projection is flushed before the next side
/// effect, so a process restart resumes from the last committed node.
pub struct MigrationRunner<S, D> {
    source: S,
    destination: D,
    journal: FileManifestStore,
}

impl<S, D> MigrationRunner<S, D>
where
    S: OpenStackSource,
    D: CanonicalDestination,
{
    pub fn new(source: S, destination: D, manifest_path: impl Into<PathBuf>) -> Self {
        Self {
            source,
            destination,
            journal: FileManifestStore::new(manifest_path),
        }
    }

    pub fn load(&self) -> Result<MigrationManifest, RunnerError> {
        Ok(self.journal.load()?)
    }

    fn save(&self, manifest: &mut MigrationManifest) -> Result<(), RunnerError> {
        crate::manifest::refresh_integrity(manifest)?;
        self.journal.save(manifest)?;
        Ok(())
    }

    pub async fn execute(
        &self,
        mut manifest: MigrationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<RunReport, RunnerError> {
        validate(&manifest)?;
        ensure_snapshot_unchanged(&manifest, snapshot)?;
        if matches!(
            manifest.phase,
            ManifestPhase::CutoverCommitted | ManifestPhase::Finalized
        ) {
            return Ok(RunReport {
                migration_id: manifest.migration_id,
                phase: manifest.phase,
                created: 0,
                resumed: 0,
            });
        }
        if manifest.phase == ManifestPhase::UnknownOutcome {
            return Err(RunnerError::Fenced(
                "unknown outcome must be observed before replay".into(),
            ));
        }
        manifest.phase = ManifestPhase::Transferring;
        self.save(&mut manifest)?;
        let documents = source_documents(&self.source, snapshot).await?;
        let order = dependency_order(&manifest)?;
        let mut created = 0;
        let mut resumed = 0;
        for index in order {
            let mut node = manifest
                .nodes
                .get(index)
                .cloned()
                .ok_or_else(|| RunnerError::Invalid("manifest node disappeared".into()))?;
            if !node.unsupported.is_empty() {
                continue;
            }
            let observed = self
                .destination
                .observe(&manifest.migration_id, &node)
                .await?;
            let destination = match observed {
                Some(value) if value.owned_by_migration && value.complete => {
                    if value.destination_id != node.destination_id.as_deref().unwrap_or_default() {
                        return Err(RunnerError::Fenced(format!(
                            "replayed destination mapping changed for {}",
                            node.key
                        )));
                    }
                    resumed += 1;
                    value
                }
                Some(_) => {
                    return Err(RunnerError::Fenced(format!(
                        "foreign or incomplete destination state for {}",
                        node.key
                    )));
                }
                None => {
                    let document = documents.get(&node.key).ok_or_else(|| {
                        RunnerError::Source(format!("source document missing for {}", node.key))
                    })?;
                    let expected_destination_id = node.destination_id.clone();
                    let value = self
                        .destination
                        .create(&manifest.migration_id, &node, document)
                        .await?;
                    if value.destination_id.trim().is_empty() {
                        return Err(RunnerError::Destination(format!(
                            "create omitted canonical destination id for {}",
                            node.key
                        )));
                    }
                    if expected_destination_id
                        .as_deref()
                        .is_some_and(|expected| expected != value.destination_id)
                    {
                        return Err(RunnerError::Fenced(format!(
                            "create returned a conflicting destination mapping for {}",
                            node.key
                        )));
                    }
                    node.destination_id = Some(value.destination_id.clone());
                    created += 1;
                    value
                }
            };
            if destination.destination_id != node.destination_id.as_deref().unwrap_or_default()
                || destination.owner_scope_id != manifest.destination.scope_id
                || destination.source_key != node.key
            {
                return Err(RunnerError::Fenced(format!(
                    "destination mapping or ownership mismatch for {}",
                    node.key
                )));
            }
            self.destination
                .verify(&manifest.migration_id, &node)
                .await?;
            let target = manifest
                .nodes
                .get_mut(index)
                .ok_or_else(|| RunnerError::Invalid("manifest node disappeared".into()))?;
            target.verification = VerificationState::Verified;
            target.rollback = RollbackState::Owned;
            target.destination_id = node.destination_id;
            self.save(&mut manifest)?;
        }
        manifest.phase = ManifestPhase::Validated;
        self.save(&mut manifest)?;
        Ok(RunReport {
            migration_id: manifest.migration_id,
            phase: manifest.phase,
            created,
            resumed,
        })
    }

    pub async fn rollback(&self, mut manifest: MigrationManifest) -> Result<(), RunnerError> {
        let plan = plan_rollback(&manifest)?;
        for destination_id in plan.resource_ids {
            let index = manifest
                .nodes
                .iter()
                .position(|node| node.destination_id.as_deref() == Some(destination_id.as_str()))
                .ok_or_else(|| RunnerError::Invalid("rollback mapping disappeared".into()))?;
            let node = manifest.nodes[index].clone();
            self.destination
                .delete_owned(&manifest.migration_id, &node)
                .await?;
            manifest.nodes[index].rollback = RollbackState::Removed;
            self.save(&mut manifest)?;
        }
        manifest = mark_rolled_back(&manifest)?;
        self.save(&mut manifest)?;
        Ok(())
    }
}

impl<S, D> MigrationRunner<S, D>
where
    S: SourceControl + OpenStackSource,
    D: CanonicalDestination,
{
    pub async fn cutover(
        &self,
        mut manifest: MigrationManifest,
        snapshot: &SourceSnapshot,
        authorization: &CutoverAuthorization,
        server_source_id: &str,
        commit_id: &str,
    ) -> Result<MigrationManifest, RunnerError> {
        validate(&manifest)?;
        ensure_snapshot_unchanged(&manifest, snapshot)?;
        if manifest.phase == ManifestPhase::CutoverCommitted {
            if manifest.cutover.committed_at.as_deref() == Some(commit_id) {
                return Ok(manifest);
            }
            return Err(RunnerError::Fenced(
                "cutover commit identity conflicts".into(),
            ));
        }
        if manifest.phase != ManifestPhase::Validated {
            return Err(RunnerError::Fenced("migration is not validated".into()));
        }
        validate_cutover_authorization(&manifest, authorization)
            .map_err(|error| RunnerError::Fenced(error.to_string()))?;
        self.source.quiesce(server_source_id).await?;
        manifest.cutover.source_quiesced = true;
        manifest.phase = ManifestPhase::CutoverPending;
        self.save(&mut manifest)?;
        self.source.final_sync(snapshot).await?;
        prepare_server_cutover(&manifest, authorization)
            .map_err(|error| RunnerError::Fenced(error.to_string()))?;
        manifest = commit_cutover(&manifest, authorization, commit_id)
            .map_err(|error| RunnerError::Fenced(error.to_string()))?;
        self.save(&mut manifest)?;
        Ok(manifest)
    }
}

async fn source_documents<S: OpenStackSource>(
    source: &S,
    snapshot: &SourceSnapshot,
) -> Result<BTreeMap<String, SourceDocument>, RunnerError> {
    let mut documents = BTreeMap::new();
    for kind in snapshot
        .resources
        .iter()
        .map(|resource| resource.kind)
        .collect::<BTreeSet<_>>()
    {
        let values = if kind == ResourceKind::Project {
            vec![
                source
                    .project()
                    .await
                    .map_err(|error| RunnerError::Source(error.to_string()))?,
            ]
        } else {
            source
                .list(kind)
                .await
                .map_err(|error| RunnerError::Source(error.to_string()))?
        };
        for document in values {
            let id = document
                .body
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| RunnerError::Source("source document has no id".into()))?;
            documents.insert(format!("{}/{}", kind_name(kind), id), document);
        }
    }
    Ok(documents)
}

fn dependency_order(manifest: &MigrationManifest) -> Result<Vec<usize>, RunnerError> {
    let indexes = manifest
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.key.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut pending = manifest
        .nodes
        .iter()
        .enumerate()
        .collect::<BTreeMap<_, _>>();
    let mut resolved = BTreeSet::new();
    let mut result = Vec::with_capacity(manifest.nodes.len());
    while !pending.is_empty() {
        let next = pending
            .iter()
            .find(|(_, node)| node.depends_on.iter().all(|key| resolved.contains(key)))
            .map(|(index, _)| *index);
        let Some(index) = next else {
            return Err(RunnerError::Invalid("manifest dependency cycle".into()));
        };
        let node = pending
            .remove(&index)
            .ok_or_else(|| RunnerError::Invalid("selected pending node disappeared".into()))?;
        resolved.insert(node.key.clone());
        result.push(indexes[&node.key]);
    }
    Ok(result)
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

/// Native O3K HTTP adapter. It only speaks the declared application API and
/// requires an explicit bearer capability supplied by the caller.
pub struct HttpNativeDestination {
    client: Client,
    endpoint: Url,
    bearer: String,
}

impl HttpNativeDestination {
    pub fn new(endpoint: Url, bearer: String) -> Result<Self, RunnerError> {
        if endpoint.scheme() != "https" && endpoint.host_str() != Some("127.0.0.1") {
            return Err(RunnerError::Invalid(
                "destination endpoint must use HTTPS".into(),
            ));
        }
        if bearer.trim().is_empty() {
            return Err(RunnerError::Invalid(
                "destination bearer capability is empty".into(),
            ));
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| RunnerError::Destination(error.to_string()))?,
            endpoint,
            bearer,
        })
    }

    fn collection_url(&self, node: &ManifestNode) -> Result<Url, RunnerError> {
        let (namespace, collection) = match node.resource_type {
            ResourceKind::Server => ("compute", "servers"),
            ResourceKind::Volume => ("volume", "volumes"),
            ResourceKind::Network => ("network", "networks"),
            ResourceKind::Subnet => ("network", "subnets"),
            ResourceKind::Port => ("network", "ports"),
            ResourceKind::SecurityGroup => ("network", "security-groups"),
            ResourceKind::SecurityGroupRule => ("network", "security-group-rules"),
            ResourceKind::Router => ("network", "routers"),
            ResourceKind::FloatingIp => ("network", "floating-ips"),
            ResourceKind::Image => ("image", "images"),
            _ => {
                return Err(RunnerError::Invalid(format!(
                    "no native collection for {}",
                    node.key
                )));
            }
        };
        let mut url = self.endpoint.clone();
        let base = url.path().trim_end_matches('/');
        url.set_path(&format!("{base}/o3k/v1/{namespace}/{collection}"));
        Ok(url)
    }

    fn resource_url(&self, node: &ManifestNode) -> Result<Url, RunnerError> {
        let mut url = self.collection_url(node)?;
        let id = node
            .destination_id
            .as_deref()
            .ok_or_else(|| RunnerError::Invalid(format!("{} has no destination id", node.key)))?;
        url.set_path(&format!("{}/{}", url.path().trim_end_matches('/'), id));
        Ok(url)
    }
}

#[async_trait]
impl CanonicalDestination for HttpNativeDestination {
    async fn observe(
        &self,
        migration_id: &str,
        node: &ManifestNode,
    ) -> Result<Option<DestinationObservation>, RunnerError> {
        let url = if node.destination_id.is_some() {
            self.resource_url(node)?
        } else {
            let mut url = self.collection_url(node)?;
            url.query_pairs_mut()
                .append_pair("migration_id", migration_id)
                .append_pair("source_key", &node.key);
            url
        };
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.bearer)
            .header(
                "Idempotency-Key",
                format!("{migration_id}:observe:{}", node.key),
            )
            .send()
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(RunnerError::Destination(format!(
                "observe returned {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        let resource = if node.destination_id.is_none() {
            body.get("resources")
                .and_then(Value::as_array)
                .and_then(|resources| {
                    resources.iter().find(|resource| {
                        resource
                            .pointer("/metadata/migration_id")
                            .and_then(Value::as_str)
                            == Some(migration_id)
                            && resource
                                .pointer("/metadata/source_key")
                                .and_then(Value::as_str)
                                == Some(node.key.as_str())
                    })
                })
                .or_else(|| body.get("resource"))
                .unwrap_or(&body)
        } else {
            &body
        };
        let id = resource
            .pointer("/metadata/id")
            .and_then(Value::as_str)
            .or_else(|| resource.get("id").and_then(Value::as_str))
            .ok_or_else(|| RunnerError::Destination("observe omitted canonical id".into()))?;
        Ok(Some(DestinationObservation {
            destination_id: id.into(),
            migration_id: migration_id.into(),
            owner_scope_id: resource
                .pointer("/metadata/owner_scope")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            source_key: node.key.clone(),
            owned_by_migration: resource
                .pointer("/metadata/migration_id")
                .and_then(Value::as_str)
                == Some(migration_id),
            complete: true,
        }))
    }

    async fn create(
        &self,
        migration_id: &str,
        node: &ManifestNode,
        source: &SourceDocument,
    ) -> Result<DestinationObservation, RunnerError> {
        let body = json!({"api_version":"o3k.io/v1", "kind": kind_name(node.resource_type), "spec": {"migration_id": migration_id, "source_key": node.key, "source": redact_json(&source.body), "canonical_id": node.destination_id}});
        let response = self
            .client
            .post(self.collection_url(node)?)
            .bearer_auth(&self.bearer)
            .header(
                "Idempotency-Key",
                format!("{migration_id}:create:{}", node.key),
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        if !response.status().is_success() {
            return Err(RunnerError::Destination(format!(
                "create returned {}",
                response.status()
            )));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        let id = value
            .pointer("/resource/metadata/id")
            .and_then(Value::as_str)
            .or_else(|| value.get("resource_id").and_then(Value::as_str))
            .or(node.destination_id.as_deref())
            .ok_or_else(|| RunnerError::Destination("create omitted canonical id".into()))?;
        Ok(DestinationObservation {
            destination_id: id.into(),
            migration_id: migration_id.into(),
            owner_scope_id: node.owner_scope_id.clone(),
            source_key: node.key.clone(),
            owned_by_migration: true,
            complete: true,
        })
    }

    async fn delete_owned(
        &self,
        migration_id: &str,
        node: &ManifestNode,
    ) -> Result<(), RunnerError> {
        let url = self.resource_url(node)?;
        let response = self
            .client
            .delete(url)
            .bearer_auth(&self.bearer)
            .header(
                "Idempotency-Key",
                format!("{migration_id}:delete:{}", node.key),
            )
            .send()
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(RunnerError::Destination(format!(
                "delete returned {}",
                response.status()
            )));
        }
        Ok(())
    }

    async fn verify(&self, migration_id: &str, node: &ManifestNode) -> Result<(), RunnerError> {
        let observed = self.observe(migration_id, node).await?.ok_or_else(|| {
            RunnerError::Destination(format!("{} disappeared after create", node.key))
        })?;
        if observed.destination_id != node.destination_id.as_deref().unwrap_or_default()
            || !observed.owned_by_migration
            || !observed.complete
            || observed.owner_scope_id != node.owner_scope_id
        {
            return Err(RunnerError::Fenced(format!(
                "verification failed for {}",
                node.key
            )));
        }
        Ok(())
    }
}

fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    let key = key.to_ascii_lowercase();
                    !["password", "token", "secret", "private_key", "credential"]
                        .iter()
                        .any(|part| key.contains(part))
                })
                .map(|(key, value)| (key.clone(), redact_json(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_json).collect()),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Classification, SourceResource};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct Source {
        quiesces: Arc<Mutex<usize>>,
        final_syncs: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl OpenStackSource for Source {
        async fn project(&self) -> Result<SourceDocument, crate::DiscoveryError> {
            Ok(SourceDocument {
                kind: ResourceKind::Project,
                body: json!({"id":"project-a"}),
                generation_input: "project".into(),
            })
        }

        async fn list(
            &self,
            kind: ResourceKind,
        ) -> Result<Vec<SourceDocument>, crate::DiscoveryError> {
            let id = format!("{}-a", kind_name(kind));
            Ok(vec![SourceDocument {
                kind,
                body: json!({"id":id,"project_id":"project-a"}),
                generation_input: "network".into(),
            }])
        }
    }

    #[async_trait]
    impl SourceControl for Source {
        async fn quiesce(&self, _server_source_id: &str) -> Result<(), RunnerError> {
            *self
                .quiesces
                .lock()
                .map_err(|_| RunnerError::Source("poisoned test lock".into()))? += 1;
            Ok(())
        }

        async fn final_sync(&self, _snapshot: &SourceSnapshot) -> Result<(), RunnerError> {
            *self
                .final_syncs
                .lock()
                .map_err(|_| RunnerError::Source("poisoned test lock".into()))? += 1;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct Destination {
        creates: Arc<Mutex<usize>>,
        observations: Arc<Mutex<usize>>,
        deletes: Arc<Mutex<Vec<String>>>,
        forced_create_id: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl CanonicalDestination for Destination {
        async fn observe(
            &self,
            migration_id: &str,
            node: &ManifestNode,
        ) -> Result<Option<DestinationObservation>, RunnerError> {
            *self
                .observations
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))? += 1;
            if *self
                .creates
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?
                == 0
            {
                return Ok(None);
            }
            Ok(Some(DestinationObservation {
                destination_id: node.destination_id.clone().unwrap_or_default(),
                migration_id: migration_id.into(),
                owner_scope_id: node.owner_scope_id.clone(),
                source_key: node.key.clone(),
                owned_by_migration: true,
                complete: true,
            }))
        }

        async fn create(
            &self,
            migration_id: &str,
            node: &ManifestNode,
            _source: &SourceDocument,
        ) -> Result<DestinationObservation, RunnerError> {
            *self
                .creates
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))? += 1;
            let destination_id = self
                .forced_create_id
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?
                .clone()
                .or_else(|| node.destination_id.clone())
                .unwrap_or_default();
            Ok(DestinationObservation {
                destination_id,
                migration_id: migration_id.into(),
                owner_scope_id: node.owner_scope_id.clone(),
                source_key: node.key.clone(),
                owned_by_migration: true,
                complete: true,
            })
        }

        async fn delete_owned(
            &self,
            _migration_id: &str,
            node: &ManifestNode,
        ) -> Result<(), RunnerError> {
            self.deletes
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?
                .push(node.destination_id.clone().unwrap_or_default());
            Ok(())
        }

        async fn verify(
            &self,
            _migration_id: &str,
            _node: &ManifestNode,
        ) -> Result<(), RunnerError> {
            Ok(())
        }
    }

    fn fixture() -> Result<(SourceSnapshot, MigrationManifest), RunnerError> {
        let snapshot = SourceSnapshot {
            schema: "o3k.migration.source-snapshot/v1".into(),
            source_cloud_id: "source-a".into(),
            project_id: "project-a".into(),
            generation_inputs: vec!["network".into()],
            resources: vec![SourceResource {
                kind: ResourceKind::Network,
                source_id: "network-a".into(),
                project_id: Some("project-a".into()),
                name: Some("network".into()),
                dependencies: vec![],
                fingerprint: "fingerprint-a".into(),
                classification: Classification::Supported,
                reasons: vec![],
            }],
            snapshot_fingerprint: "snapshot-a".into(),
        };
        let request = crate::manifest::ManifestRequest {
            migration_id: "migration-a".into(),
            endpoint_fingerprint: "endpoint-a".into(),
            profile: "p14-openstack-cold-migration-v1".into(),
            destination_scope_id: "destination-a".into(),
            destination_profile: "native-rust-testlab".into(),
            actor_principal_id: "actor-a".into(),
            authenticated_service_principal_id: "service-a".into(),
        };
        let manifest = crate::manifest::build_manifest(&snapshot, &request)?;
        Ok((snapshot, manifest))
    }

    fn server_fixture() -> Result<(SourceSnapshot, MigrationManifest), RunnerError> {
        let snapshot = SourceSnapshot {
            schema: "o3k.migration.source-snapshot/v1".into(),
            source_cloud_id: "source-a".into(),
            project_id: "project-a".into(),
            generation_inputs: vec!["server".into()],
            resources: vec![SourceResource {
                kind: ResourceKind::Server,
                source_id: "server-a".into(),
                project_id: Some("project-a".into()),
                name: Some("server".into()),
                dependencies: vec![],
                fingerprint: "fingerprint-server".into(),
                classification: Classification::Supported,
                reasons: vec![],
            }],
            snapshot_fingerprint: "snapshot-server".into(),
        };
        let request = crate::manifest::ManifestRequest {
            migration_id: "migration-server".into(),
            endpoint_fingerprint: "endpoint-a".into(),
            profile: "p14-openstack-cold-migration-v1".into(),
            destination_scope_id: "destination-a".into(),
            destination_profile: "native-rust-testlab".into(),
            actor_principal_id: "actor-a".into(),
            authenticated_service_principal_id: "service-a".into(),
        };
        Ok((
            snapshot.clone(),
            crate::manifest::build_manifest(&snapshot, &request)?,
        ))
    }

    #[tokio::test]
    async fn restart_resumes_owned_nodes_without_duplicate_create() -> Result<(), RunnerError> {
        let (snapshot, manifest) = fixture()?;
        let destination = Destination::default();
        let path = std::env::temp_dir().join(format!("o3k-p14-9a-{}.json", uuid::Uuid::new_v4()));
        let runner = MigrationRunner::new(Source::default(), destination.clone(), &path);
        let first = runner.execute(manifest, &snapshot).await?;
        let reopened = runner.load()?;
        let second = runner.execute(reopened, &snapshot).await?;
        assert_eq!(first.created, 1);
        assert_eq!(second.resumed, 1);
        assert_eq!(
            *destination
                .creates
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?,
            1
        );
        assert!(
            *destination
                .observations
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?
                >= 2
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn conflicting_replay_is_fenced() -> Result<(), RunnerError> {
        let (snapshot, manifest) = fixture()?;
        let destination = Destination::default();
        let path = std::env::temp_dir().join(format!("o3k-p14-9a-{}.json", uuid::Uuid::new_v4()));
        let conflicting = manifest.clone();
        *destination
            .forced_create_id
            .lock()
            .map_err(|_| RunnerError::Destination("poisoned test lock".into()))? =
            Some("foreign-id".into());
        let runner = MigrationRunner::new(Source::default(), destination, &path);
        let error = match runner.execute(conflicting, &snapshot).await {
            Err(error) => error,
            Ok(_) => return Err(RunnerError::Invalid("conflict was not fenced".into())),
        };
        assert!(matches!(error, RunnerError::Fenced(_)));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_outcome_is_fenced_before_replay_or_rollback() -> Result<(), RunnerError> {
        let (snapshot, mut manifest) = fixture()?;
        manifest.phase = ManifestPhase::UnknownOutcome;
        crate::manifest::refresh_integrity(&mut manifest)?;
        let destination = Destination::default();
        let path = std::env::temp_dir().join(format!("o3k-p14-9a-{}.json", uuid::Uuid::new_v4()));
        let runner = MigrationRunner::new(Source::default(), destination, &path);
        assert!(matches!(
            runner.execute(manifest.clone(), &snapshot).await,
            Err(RunnerError::Fenced(_))
        ));
        assert!(matches!(
            runner.rollback(manifest).await,
            Err(RunnerError::Recovery(RecoveryError::InvalidState(_)))
        ));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn rollback_deletes_only_owned_resources_in_reverse_order() -> Result<(), RunnerError> {
        let (_snapshot, mut manifest) = fixture()?;
        manifest.nodes[0].destination_id = Some("owned-network".into());
        manifest.nodes[0].rollback = RollbackState::Owned;
        manifest.nodes[0].verification = VerificationState::Verified;
        crate::manifest::refresh_integrity(&mut manifest)?;
        let destination = Destination::default();
        let path = std::env::temp_dir().join(format!("o3k-p14-9a-{}.json", uuid::Uuid::new_v4()));
        let runner = MigrationRunner::new(Source::default(), destination.clone(), &path);
        runner.rollback(manifest).await?;
        assert_eq!(
            *destination
                .deletes
                .lock()
                .map_err(|_| RunnerError::Destination("poisoned test lock".into()))?,
            vec!["owned-network"]
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn cutover_requires_authorization_and_keeps_source_deletion_separate()
    -> Result<(), RunnerError> {
        let (snapshot, manifest) = server_fixture()?;
        let destination = Destination::default();
        let source = Source::default();
        let path = std::env::temp_dir().join(format!("o3k-p14-9a-{}.json", uuid::Uuid::new_v4()));
        let runner = MigrationRunner::new(source.clone(), destination, &path);
        runner.execute(manifest, &snapshot).await?;
        let validated = runner.load()?;
        let unauthorized = CutoverAuthorization {
            principal_id: "wrong-actor".into(),
            destination_scope_id: "destination-a".into(),
            action: "migrate:commit".into(),
        };
        let error = match runner
            .cutover(validated, &snapshot, &unauthorized, "server-a", "commit-a")
            .await
        {
            Err(error) => error,
            Ok(_) => {
                return Err(RunnerError::Invalid(
                    "unauthorized cutover succeeded".into(),
                ));
            }
        };
        assert!(matches!(error, RunnerError::Fenced(_)));
        assert_eq!(runner.load()?.phase, ManifestPhase::Validated);
        assert_eq!(
            *source
                .quiesces
                .lock()
                .map_err(|_| RunnerError::Source("poisoned test lock".into()))?,
            0
        );
        assert_eq!(
            *source
                .final_syncs
                .lock()
                .map_err(|_| RunnerError::Source("poisoned test lock".into()))?,
            0
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn native_http_adapter_composes_observe_create_and_verify_without_db_access()
    -> Result<(), RunnerError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| RunnerError::Destination(error.to_string()))?;
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response_body in [
                None,
                Some(r#"{"resource":{"metadata":{"id":"dest-1"}}}"#),
                Some(
                    r#"{"metadata":{"id":"dest-1","migration_id":"migration-a","owner_scope":"destination-a","source_key":"network/network-a"}}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .map_err(|error| error.to_string())?;
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                if let Some(body) = response_body {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .await
                        .map_err(|error| error.to_string())?;
                } else {
                    stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok::<_, String>(requests)
        });

        let endpoint = Url::parse(&format!("http://{address}/")).map_err(|error| {
            RunnerError::Invalid(format!("test endpoint URL is invalid: {error}"))
        })?;
        let destination = HttpNativeDestination::new(endpoint, "capability-a".into())?;
        let (_snapshot, manifest) = fixture()?;
        let mut node = manifest.nodes[0].clone();
        node.destination_id = None;
        assert!(destination.observe("migration-a", &node).await?.is_none());
        let created = destination
            .create(
                "migration-a",
                &node,
                &SourceDocument {
                    kind: ResourceKind::Network,
                    body: json!({"id":"network-a","token":"must-not-cross-boundary"}),
                    generation_input: "network".into(),
                },
            )
            .await?;
        assert_eq!(created.destination_id, "dest-1");
        node.destination_id = Some(created.destination_id);
        destination.verify("migration-a", &node).await?;
        let requests = server
            .await
            .map_err(|error| RunnerError::Destination(error.to_string()))?
            .map_err(RunnerError::Destination)?;
        assert!(requests[0].contains(
            "GET /o3k/v1/network/networks?migration_id=migration-a&source_key=network%2Fnetwork-a"
        ));
        assert!(requests[0].contains("authorization: Bearer capability-a"));
        assert!(requests[1].contains("POST /o3k/v1/network/networks"));
        assert!(requests[1].contains("idempotency-key: migration-a:create:network/network-a"));
        assert!(!requests[1].contains("must-not-cross-boundary"));
        assert!(requests[2].contains("GET /o3k/v1/network/networks/dest-1"));
        Ok(())
    }
}
