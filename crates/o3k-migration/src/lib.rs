//! Read-only source discovery and preflight for P14 OpenStack adoption.
//!
//! This crate deliberately stops at a redacted, deterministic source snapshot.
//! It has no destination client, persistence, provider-state adoption, or
//! migration/cutover side effect. Credentials live only in the in-memory
//! source client and are never part of a discovery result.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};
use thiserror::Error;
use url::Url;
pub mod manifest;

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESOURCES: usize = 50_000;
const MAX_NAME_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub struct SourceCloudConfig {
    pub cloud_id: String,
    pub auth_url: Url,
    pub username: String,
    pub password: Secret,
    pub user_domain_name: String,
    pub project_id: String,
    pub region: String,
    pub interface: EndpointInterface,
    pub allowed_hosts: BTreeSet<String>,
    pub allow_insecure_tls: bool,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Result<Self, DiscoveryError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DiscoveryError::InvalidConfig(
                "source password is empty".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointInterface {
    Public,
    Internal,
    Admin,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("invalid source configuration: {0}")]
    InvalidConfig(String),
    #[error("source endpoint host is not allowlisted")]
    HostNotAllowlisted,
    #[error("source endpoint must use HTTPS unless insecure TLS is explicitly enabled")]
    InsecureEndpoint,
    #[error("source response exceeded the bounded response limit")]
    ResponseTooLarge,
    #[error("source returned HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("source response was malformed: {0}")]
    MalformedResponse(String),
    #[error("source request failed: {0}")]
    Request(String),
    #[error("source inventory exceeded the bounded resource limit")]
    ResourceLimit,
    #[error("source resource is missing a stable id")]
    MissingId,
    #[error("source resource has an invalid name")]
    InvalidName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Project,
    Image,
    Flavor,
    Keypair,
    Network,
    Subnet,
    Port,
    SecurityGroup,
    SecurityGroupRule,
    Router,
    RouterInterface,
    FloatingIp,
    Server,
    Volume,
    VolumeAttachment,
}

impl ResourceKind {
    fn path(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Image => "images",
            Self::Flavor => "flavors",
            Self::Keypair => "os-keypairs",
            Self::Network => "networks",
            Self::Subnet => "subnets",
            Self::Port => "ports",
            Self::SecurityGroup => "security-groups",
            Self::SecurityGroupRule => "security-group-rules",
            Self::Router => "routers",
            Self::RouterInterface => "router-interfaces",
            Self::FloatingIp => "floatingips",
            Self::Server => "servers/detail",
            Self::Volume => "volumes/detail",
            Self::VolumeAttachment => "attachments/detail",
        }
    }

    fn service(self) -> &'static str {
        match self {
            Self::Project => "identity",
            Self::Image => "image",
            Self::Flavor | Self::Server | Self::Keypair => "compute",
            Self::Network
            | Self::Subnet
            | Self::Port
            | Self::SecurityGroup
            | Self::SecurityGroupRule
            | Self::Router
            | Self::RouterInterface
            | Self::FloatingIp => "network",
            Self::Volume | Self::VolumeAttachment => "volumev3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Classification {
    Supported,
    SupportedWithMapping,
    Blocked,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassificationReason {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceResource {
    pub kind: ResourceKind,
    pub source_id: String,
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub dependencies: Vec<String>,
    pub fingerprint: String,
    pub classification: Classification,
    pub reasons: Vec<ClassificationReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub schema: String,
    pub source_cloud_id: String,
    pub project_id: String,
    pub generation_inputs: Vec<String>,
    pub resources: Vec<SourceResource>,
    pub snapshot_fingerprint: String,
}

impl SourceSnapshot {
    #[must_use]
    pub fn has_blocked_resources(&self) -> bool {
        self.resources
            .iter()
            .any(|resource| resource.classification == Classification::Blocked)
    }
}

#[derive(Debug, Clone)]
pub struct SourceDocument {
    pub kind: ResourceKind,
    pub body: Value,
    pub generation_input: String,
}

#[async_trait]
pub trait OpenStackSource: Send + Sync {
    async fn project(&self) -> Result<SourceDocument, DiscoveryError>;
    async fn list(&self, kind: ResourceKind) -> Result<Vec<SourceDocument>, DiscoveryError>;
}

#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    pub selected_kinds: BTreeSet<ResourceKind>,
}

impl DiscoveryRequest {
    #[must_use]
    pub fn bounded_default() -> Self {
        Self {
            selected_kinds: [
                ResourceKind::Project,
                ResourceKind::Image,
                ResourceKind::Flavor,
                ResourceKind::Keypair,
                ResourceKind::Network,
                ResourceKind::Subnet,
                ResourceKind::Port,
                ResourceKind::SecurityGroup,
                ResourceKind::SecurityGroupRule,
                ResourceKind::Router,
                ResourceKind::RouterInterface,
                ResourceKind::FloatingIp,
                ResourceKind::Server,
                ResourceKind::Volume,
                ResourceKind::VolumeAttachment,
            ]
            .into_iter()
            .collect(),
        }
    }
}

pub async fn discover<S: OpenStackSource>(
    source: &S,
    config: &SourceCloudConfig,
    request: &DiscoveryRequest,
) -> Result<SourceSnapshot, DiscoveryError> {
    validate_config(config)?;
    if request.selected_kinds.is_empty() {
        return Err(DiscoveryError::InvalidConfig(
            "selected resource set is empty".to_owned(),
        ));
    }
    let project = source.project().await?;
    let mut documents = vec![project];
    for kind in request
        .selected_kinds
        .iter()
        .copied()
        .filter(|kind| *kind != ResourceKind::Project)
    {
        documents.extend(source.list(kind).await?);
        if documents.len() > MAX_RESOURCES {
            return Err(DiscoveryError::ResourceLimit);
        }
    }
    documents.sort_by_key(|document| (document.kind, id(&document.body)));
    let mut generation_inputs = documents
        .iter()
        .map(|document| document.generation_input.clone())
        .collect::<Vec<_>>();
    let resources = documents
        .into_iter()
        .map(|document| classify(document, &config.project_id))
        .collect::<Result<Vec<_>, _>>()?;
    generation_inputs.extend(
        resources
            .iter()
            .map(|resource| resource.fingerprint.clone()),
    );
    generation_inputs.sort();
    generation_inputs.dedup();
    let snapshot_fingerprint = digest(
        &serde_json::to_vec(&resources)
            .map_err(|error| DiscoveryError::MalformedResponse(error.to_string()))?,
    );
    Ok(SourceSnapshot {
        schema: "o3k.migration.source-snapshot/v1".to_owned(),
        source_cloud_id: config.cloud_id.clone(),
        project_id: config.project_id.clone(),
        generation_inputs,
        resources,
        snapshot_fingerprint,
    })
}

fn validate_config(config: &SourceCloudConfig) -> Result<(), DiscoveryError> {
    if config.cloud_id.trim().is_empty()
        || config.project_id.trim().is_empty()
        || config.username.trim().is_empty()
        || config.user_domain_name.trim().is_empty()
        || config.region.trim().is_empty()
    {
        return Err(DiscoveryError::InvalidConfig(
            "required source identity/configuration is missing".to_owned(),
        ));
    }
    validate_url(&config.auth_url, config)?;
    if config.timeout.is_zero() || config.timeout > Duration::from_secs(300) {
        return Err(DiscoveryError::InvalidConfig(
            "timeout must be between 1 and 300 seconds".to_owned(),
        ));
    }
    Ok(())
}

fn validate_url(url: &Url, config: &SourceCloudConfig) -> Result<(), DiscoveryError> {
    if url.username() != "" || url.password().is_some() || url.host_str().is_none() {
        return Err(DiscoveryError::InvalidConfig(
            "source URL contains invalid authority".to_owned(),
        ));
    }
    if url.scheme() != "https" && !(config.allow_insecure_tls && url.scheme() == "http") {
        return Err(DiscoveryError::InsecureEndpoint);
    }
    let host = url
        .host_str()
        .ok_or_else(|| DiscoveryError::InvalidConfig("source URL has no host".to_owned()))?;
    if !config.allowed_hosts.contains(host) {
        return Err(DiscoveryError::HostNotAllowlisted);
    }
    Ok(())
}

fn id(body: &Value) -> String {
    body.get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn classify(document: SourceDocument, project_id: &str) -> Result<SourceResource, DiscoveryError> {
    let source_id = id(&document.body);
    if source_id.trim().is_empty() {
        return Err(DiscoveryError::MissingId);
    }
    let name = document
        .body
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if name
        .as_ref()
        .is_some_and(|value| value.len() > MAX_NAME_BYTES)
    {
        return Err(DiscoveryError::InvalidName);
    }
    let (classification, mut reasons) = classify_semantics(document.kind, &document.body);
    let dependencies = dependencies(document.kind, &document.body);
    let project = document
        .body
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if project.as_deref().is_some_and(|value| value != project_id) {
        reasons.push(reason(
            "CROSS_PROJECT_RESOURCE",
            "resource is outside the selected project",
        ));
    }
    let scope_required = !matches!(
        document.kind,
        ResourceKind::Project | ResourceKind::Image | ResourceKind::Flavor
    );
    if scope_required && project.is_none() {
        reasons.push(reason(
            "SCOPE_UNVERIFIED",
            "project ownership is absent from the source representation",
        ));
    }
    let classification = if reasons.iter().any(|reason| {
        matches!(
            reason.code.as_str(),
            "CROSS_PROJECT_RESOURCE" | "SCOPE_UNVERIFIED"
        )
    }) {
        Classification::Blocked
    } else {
        classification
    };
    let canonical = canonical_body(&document.body);
    Ok(SourceResource {
        kind: document.kind,
        source_id,
        project_id: project,
        name,
        dependencies,
        fingerprint: digest(
            &serde_json::to_vec(&canonical)
                .map_err(|error| DiscoveryError::MalformedResponse(error.to_string()))?,
        ),
        classification,
        reasons,
    })
}

fn classify_semantics(
    kind: ResourceKind,
    body: &Value,
) -> (Classification, Vec<ClassificationReason>) {
    let mut reasons = Vec::new();
    let blocked_keys: BTreeSet<&str> = [
        "sriov",
        "trunk_details",
        "sub_ports",
        "pci_passthrough_devices",
        "pci_device",
        "gpu",
        "numa_topology",
        "hugepage",
        "hugepages",
        "kms_key_id",
        "encryption_key_id",
    ]
    .into_iter()
    .collect();
    for key in blocked_keys {
        if body.get(key).is_some_and(|value| {
            !value.is_null() && value != &Value::Bool(false) && value != &json!([])
        }) {
            reasons.push(reason("UNSUPPORTED_SEMANTIC", key));
        }
    }
    if body.get("multiattach").and_then(Value::as_bool) == Some(true) {
        reasons.push(reason("MULTIATTACH_UNSUPPORTED", "volume uses multiattach"));
    }
    if kind == ResourceKind::Volume && body.get("encrypted").and_then(Value::as_bool) == Some(true)
    {
        reasons.push(reason(
            "ENCRYPTED_VOLUME_UNSUPPORTED",
            "encrypted/KMS-backed volume",
        ));
    }
    if kind == ResourceKind::Server
        && body
            .get("block_device_mapping_v2")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        reasons.push(reason(
            "BOOT_FROM_VOLUME_UNSUPPORTED",
            "server has a volume-backed boot mapping",
        ));
    }
    if body.as_object().is_some_and(|object| {
        object
            .keys()
            .any(|key| key.starts_with("extension:") || key.starts_with("x-"))
    }) {
        reasons.push(reason(
            "UNDECLARED_EXTENSION",
            "resource contains an undeclared extension field",
        ));
    }
    let classification = if !reasons.is_empty() {
        Classification::Blocked
    } else if matches!(
        kind,
        ResourceKind::Flavor
            | ResourceKind::Keypair
            | ResourceKind::RouterInterface
            | ResourceKind::FloatingIp
            | ResourceKind::VolumeAttachment
    ) {
        Classification::SupportedWithMapping
    } else {
        Classification::Supported
    };
    (classification, reasons)
}

fn reason(code: &str, detail: &str) -> ClassificationReason {
    ClassificationReason {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
}

fn dependencies(kind: ResourceKind, body: &Value) -> Vec<String> {
    let keys: &[&str] = match kind {
        ResourceKind::Subnet => &["network_id"],
        ResourceKind::Port => &["network_id", "device_id"],
        ResourceKind::SecurityGroupRule => &["security_group_id", "remote_group_id"],
        ResourceKind::RouterInterface => &["router_id", "port_id", "subnet_id"],
        ResourceKind::FloatingIp => &["floating_network_id", "port_id"],
        ResourceKind::Server => &["image_id", "flavor_id", "key_name", "network_id"],
        ResourceKind::VolumeAttachment => &["volume_id", "server_id"],
        _ => &[],
    };
    let mut dependencies = keys
        .iter()
        .filter_map(|key| body.get(*key).and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    if kind == ResourceKind::Server
        && let Some(items) = body.get("networks").and_then(Value::as_array)
    {
        dependencies.extend(items.iter().flat_map(|item| {
            ["net-id", "port"]
                .into_iter()
                .filter_map(move |key| item.get(key).and_then(Value::as_str).map(str::to_owned))
        }));
    }
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

fn canonical_body(body: &Value) -> Value {
    match body {
        Value::Object(object) => {
            let mut sorted = Map::new();
            let mut entries = object
                .iter()
                .filter(|(key, _)| !is_secret_key(key))
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                sorted.insert(key.clone(), canonical_body(value));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_body).collect()),
        value => value.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "token",
        "secret",
        "private_key",
        "access_key",
        "credential",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone)]
pub struct ReqwestOpenStackSource {
    client: Client,
    token: String,
    catalog: BTreeMap<String, Url>,
    project_id: String,
}

impl ReqwestOpenStackSource {
    pub async fn authenticate(config: &SourceCloudConfig) -> Result<Self, DiscoveryError> {
        validate_config(config)?;
        let client = Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(config.allow_insecure_tls)
            .build()
            .map_err(|error| DiscoveryError::Request(error.to_string()))?;
        let auth_url = append_path(&config.auth_url, "auth/tokens")?;
        validate_url(&auth_url, config)?;
        let response = client.post(auth_url).json(&json!({"auth":{"identity":{"methods":["password"],"password":{"user":{"name":config.username,"domain":{"name":config.user_domain_name},"password":config.password.expose()}}},"scope":{"project":{"id":config.project_id}}}})).send().await.map_err(|error| DiscoveryError::Request(error.to_string()))?;
        let token = response
            .headers()
            .get("x-subject-token")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or(DiscoveryError::MalformedResponse(
                "authentication response omitted token".to_owned(),
            ))?
            .to_owned();
        let status = response.status();
        let body = bounded_json(response).await?;
        if !status.is_success() {
            return Err(DiscoveryError::HttpStatus {
                status: status.as_u16(),
            });
        }
        let catalog = parse_catalog(&body, config)?;
        Ok(Self {
            client,
            token,
            catalog,
            project_id: config.project_id.clone(),
        })
    }
}

#[async_trait]
impl OpenStackSource for ReqwestOpenStackSource {
    async fn project(&self) -> Result<SourceDocument, DiscoveryError> {
        let endpoint = self
            .catalog
            .get("identity")
            .ok_or(DiscoveryError::MalformedResponse(
                "catalog has no identity endpoint".to_owned(),
            ))?;
        let url = append_path(endpoint, &format!("projects/{}", self.project_id))?;
        let (body, generation_input) = self.get_json(url).await?;
        Ok(SourceDocument {
            kind: ResourceKind::Project,
            generation_input,
            body,
        })
    }

    async fn list(&self, kind: ResourceKind) -> Result<Vec<SourceDocument>, DiscoveryError> {
        let endpoint = self.catalog.get(kind.service()).ok_or_else(|| {
            DiscoveryError::MalformedResponse(format!("catalog has no {} endpoint", kind.service()))
        })?;
        let url = append_path(endpoint, kind.path())?;
        let (body, generation_input) = self.get_json(url).await?;
        let items = body
            .get(kind.path().split('/').next().unwrap_or(kind.path()))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| body.as_array().cloned().unwrap_or_default());
        Ok(items
            .into_iter()
            .map(|body| SourceDocument {
                kind,
                body,
                generation_input: generation_input.clone(),
            })
            .collect())
    }
}

impl ReqwestOpenStackSource {
    async fn get_json(&self, url: Url) -> Result<(Value, String), DiscoveryError> {
        let response = self
            .client
            .get(url.clone())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| DiscoveryError::Request(error.to_string()))?;
        let status = response.status();
        let generation_input = generation_input(&url, &response);
        let body = bounded_json(response).await?;
        if !status.is_success() {
            return Err(DiscoveryError::HttpStatus {
                status: status.as_u16(),
            });
        }
        Ok((body, generation_input))
    }
}

async fn bounded_json(response: reqwest::Response) -> Result<Value, DiscoveryError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(DiscoveryError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| DiscoveryError::Request(error.to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(DiscoveryError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| DiscoveryError::MalformedResponse(error.to_string()))
}

fn append_path(base: &Url, path: &str) -> Result<Url, DiscoveryError> {
    let mut url = base.clone();
    let base_path = base.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{path}"));
    Ok(url)
}

fn generation_input(url: &Url, response: &reqwest::Response) -> String {
    let mut input = url.to_string();
    for header in ["etag", "last-modified", "x-openstack-request-id"] {
        if let Some(value) = response
            .headers()
            .get(header)
            .and_then(|value| value.to_str().ok())
        {
            input.push('|');
            input.push_str(header);
            input.push('=');
            input.push_str(value);
        }
    }
    input
}

fn parse_catalog(
    body: &Value,
    config: &SourceCloudConfig,
) -> Result<BTreeMap<String, Url>, DiscoveryError> {
    let mut catalog = BTreeMap::new();
    let services = body
        .get("token")
        .and_then(|token| token.get("catalog"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DiscoveryError::MalformedResponse("authentication response omitted catalog".to_owned())
        })?;
    for service in services {
        let service_type = service
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(endpoint) =
            service
                .get("endpoints")
                .and_then(Value::as_array)
                .and_then(|endpoints| {
                    endpoints.iter().find(|endpoint| {
                        endpoint.get("interface").and_then(Value::as_str)
                            == Some(match config.interface {
                                EndpointInterface::Public => "public",
                                EndpointInterface::Internal => "internal",
                                EndpointInterface::Admin => "admin",
                            })
                            && endpoint.get("region").and_then(Value::as_str)
                                == Some(config.region.as_str())
                    })
                })
        else {
            continue;
        };
        let raw = endpoint.get("url").and_then(Value::as_str).ok_or_else(|| {
            DiscoveryError::MalformedResponse("catalog endpoint omitted URL".to_owned())
        })?;
        let url = Url::parse(raw)
            .map_err(|error| DiscoveryError::MalformedResponse(error.to_string()))?;
        validate_url(&url, config)?;
        catalog.insert(service_type.to_owned(), url);
    }
    Ok(catalog)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn config() -> SourceCloudConfig {
        SourceCloudConfig {
            cloud_id: "source-a".into(),
            auth_url: Url::parse("http://source.test/v3/").expect("literal URL"),
            username: "user".into(),
            password: Secret::new("pw").expect("literal secret"),
            user_domain_name: "Default".into(),
            project_id: "project-a".into(),
            region: "RegionOne".into(),
            interface: EndpointInterface::Public,
            allowed_hosts: ["source.test".to_owned()].into_iter().collect(),
            allow_insecure_tls: true,
            timeout: Duration::from_secs(10),
        }
    }

    #[derive(Debug)]
    struct FixtureSource {
        documents: BTreeMap<ResourceKind, Vec<Value>>,
        project: Value,
    }

    #[async_trait]
    impl OpenStackSource for FixtureSource {
        async fn project(&self) -> Result<SourceDocument, DiscoveryError> {
            Ok(SourceDocument {
                kind: ResourceKind::Project,
                body: self.project.clone(),
                generation_input: "fixture/project".into(),
            })
        }
        async fn list(&self, kind: ResourceKind) -> Result<Vec<SourceDocument>, DiscoveryError> {
            Ok(self
                .documents
                .get(&kind)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|body| SourceDocument {
                    kind,
                    body,
                    generation_input: format!("fixture/{kind:?}"),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn discovery_is_deterministic_and_scope_safe() {
        let source = FixtureSource {
            project: json!({"id":"project-a","name":"tenant-a"}),
            documents: [
                (
                    ResourceKind::Network,
                    vec![json!({"id":"network-a","project_id":"project-a","name":"net"})],
                ),
                (
                    ResourceKind::Port,
                    vec![json!({"id":"port-a","project_id":"project-a","network_id":"network-a"})],
                ),
            ]
            .into_iter()
            .collect(),
        };
        let request = DiscoveryRequest {
            selected_kinds: [
                ResourceKind::Project,
                ResourceKind::Network,
                ResourceKind::Port,
            ]
            .into_iter()
            .collect(),
        };
        let first = discover(&source, &config(), &request)
            .await
            .expect("fixture discovery");
        let second = discover(&source, &config(), &request)
            .await
            .expect("fixture discovery");
        assert_eq!(first, second);
        assert!(!first.has_blocked_resources());
        let port = first
            .resources
            .iter()
            .find(|resource| resource.kind == ResourceKind::Port)
            .expect("fixture includes port");
        assert_eq!(port.dependencies, vec!["network-a"]);
    }

    #[tokio::test]
    async fn unsupported_semantics_fail_closed_without_mutation() {
        let source = FixtureSource { project: json!({"id":"project-a"}), documents: [(ResourceKind::Volume, vec![json!({"id":"volume-a","project_id":"project-a","encrypted":true,"multiattach":true})]), (ResourceKind::Server, vec![json!({"id":"server-a","project_id":"project-a","block_device_mapping_v2":[{"uuid":"volume-a"}]})])].into_iter().collect() };
        let request = DiscoveryRequest {
            selected_kinds: [
                ResourceKind::Project,
                ResourceKind::Volume,
                ResourceKind::Server,
            ]
            .into_iter()
            .collect(),
        };
        let snapshot = discover(&source, &config(), &request)
            .await
            .expect("fixture discovery");
        assert!(snapshot.has_blocked_resources());
        let blocked = snapshot
            .resources
            .iter()
            .filter(|resource| resource.classification == Classification::Blocked)
            .count();
        assert_eq!(blocked, 2);
        assert!(
            snapshot
                .resources
                .iter()
                .flat_map(|resource| resource.reasons.iter())
                .any(|reason| reason.code == "ENCRYPTED_VOLUME_UNSUPPORTED")
        );
    }

    #[test]
    fn configuration_rejects_unallowlisted_or_insecure_endpoint() {
        let mut invalid = config();
        invalid.allowed_hosts.clear();
        assert_eq!(
            validate_config(&invalid),
            Err(DiscoveryError::HostNotAllowlisted)
        );
        let mut insecure = config();
        insecure.allow_insecure_tls = false;
        assert_eq!(
            validate_config(&insecure),
            Err(DiscoveryError::InsecureEndpoint)
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("do-not-log").expect("literal secret");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert!(!format!("{secret:?}").contains("do-not-log"));
    }
}
