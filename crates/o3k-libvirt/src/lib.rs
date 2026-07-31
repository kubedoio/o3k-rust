//! Typed local libvirt/KVM adapter for the compute agent.
//!
//! The public API is async and keeps all libvirt FFI behind `spawn_blocking`.
//! The `libvirt` feature enables the `virt` bindings; the default build keeps
//! the control plane usable on hosts where libvirt development libraries are
//! not installed and reports a clear readiness error instead.

use std::{fmt, sync::Arc};

use o3k_provider_contract::compute_proto as proto;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(feature = "libvirt")]
use virt::{connect::Connect, domain::Domain};

pub const LOCAL_SYSTEM_URI: &str = "qemu:///system";
pub const O3K_METADATA_NAMESPACE: &str = "urn:o3k:compute:domain";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainMetadata {
    pub server_id: String,
    pub project_id: String,
    pub generation: u64,
    pub operation_id: String,
    pub managed_by: String,
}

#[derive(Debug, Clone)]
pub struct DomainSpec {
    pub metadata: DomainMetadata,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub image_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltDomainXml {
    pub name: String,
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryResult {
    Owned {
        name: String,
        metadata: DomainMetadata,
    },
    Foreign {
        name: String,
    },
    Quarantined {
        name: String,
        reason: String,
    },
}

pub fn stable_domain_name(server_id: &str) -> String {
    let digest = Sha256::digest(server_id.as_bytes());
    let mut suffix = String::with_capacity(20);
    for byte in digest.iter().take(10) {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    format!("o3k-{suffix}")
}

pub fn build_domain_xml(spec: &DomainSpec) -> Result<BuiltDomainXml, LibvirtError> {
    validate_metadata(&spec.metadata)?;
    if spec.vcpus == 0
        || spec.vcpus > 512
        || spec.memory_mib == 0
        || spec.image_id.trim().is_empty()
    {
        return Err(LibvirtError::new(
            ErrorCategory::InvalidRequest,
            "domain resource values are invalid",
        ));
    }
    let name = stable_domain_name(&spec.metadata.server_id);
    let m = &spec.metadata;
    let xml = format!(
        "<domain type=\"kvm\"><name>{}</name><memory unit=\"MiB\">{}</memory><currentMemory unit=\"MiB\">{}</currentMemory><vcpu>{}</vcpu><metadata><o3k:domain xmlns:o3k=\"{}\" server_id=\"{}\" project_id=\"{}\" generation=\"{}\" operation_id=\"{}\" managed_by=\"{}\" /></metadata><os><type machine=\"pc\">hvm</type></os><devices><disk type=\"file\" device=\"disk\"><driver name=\"qemu\" type=\"qcow2\" /><source file=\"{}\" /><target dev=\"vda\" bus=\"virtio\" /></disk></devices></domain>",
        xml_escape(&name),
        spec.memory_mib,
        spec.memory_mib,
        spec.vcpus,
        O3K_METADATA_NAMESPACE,
        xml_escape(&m.server_id),
        xml_escape(&m.project_id),
        m.generation,
        xml_escape(&m.operation_id),
        xml_escape(&m.managed_by),
        xml_escape(&spec.image_id)
    );
    Ok(BuiltDomainXml { name, xml })
}

pub fn discover_domain_xml(name: &str, xml: &str) -> DiscoveryResult {
    let Some(metadata) = parse_metadata(xml) else {
        return DiscoveryResult::Foreign {
            name: name.to_owned(),
        };
    };
    match metadata {
        Ok(metadata) => DiscoveryResult::Owned {
            name: name.to_owned(),
            metadata,
        },
        Err(reason) => DiscoveryResult::Quarantined {
            name: name.to_owned(),
            reason,
        },
    }
}

pub fn discover_domain_xmls(domains: &[(String, String)]) -> Vec<DiscoveryResult> {
    let mut results = domains
        .iter()
        .map(|(name, xml)| discover_domain_xml(name, xml))
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    for result in &mut results {
        if let DiscoveryResult::Owned { metadata, .. } = result {
            if !seen.insert(metadata.server_id.clone()) {
                let name = match result {
                    DiscoveryResult::Owned { name, .. } => name.clone(),
                    _ => String::new(),
                };
                *result = DiscoveryResult::Quarantined {
                    name,
                    reason: "duplicate O3K server ID".to_owned(),
                };
            }
        }
    }
    results
}

fn validate_metadata(metadata: &DomainMetadata) -> Result<(), LibvirtError> {
    for value in [
        &metadata.server_id,
        &metadata.project_id,
        &metadata.operation_id,
        &metadata.managed_by,
    ] {
        if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(LibvirtError::new(
                ErrorCategory::InvalidRequest,
                "domain metadata is invalid",
            ));
        }
    }
    if metadata.managed_by != "o3k-compute" {
        return Err(LibvirtError::new(
            ErrorCategory::InvalidRequest,
            "domain managed-by value is invalid",
        ));
    }
    Ok(())
}

fn parse_metadata(xml: &str) -> Option<Result<DomainMetadata, String>> {
    let marker = "<o3k:domain";
    let start = xml.find(marker)?;
    let end = xml[start..].find('>')? + start;
    let tag = &xml[start..end];
    let namespace = attr(tag, "xmlns:o3k")?;
    if namespace != O3K_METADATA_NAMESPACE {
        return Some(Err("metadata namespace is invalid".to_owned()));
    }
    let generation = match attr(tag, "generation").and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => return Some(Err("generation is invalid".to_owned())),
    };
    let (Some(server_id), Some(project_id), Some(operation_id), Some(managed_by)) = (
        attr(tag, "server_id"),
        attr(tag, "project_id"),
        attr(tag, "operation_id"),
        attr(tag, "managed_by"),
    ) else {
        return Some(Err("metadata fields are incomplete".to_owned()));
    };
    let metadata = DomainMetadata {
        server_id,
        project_id,
        generation,
        operation_id,
        managed_by,
    };
    if validate_metadata(&metadata).is_err() {
        Some(Err("metadata fields are invalid".to_owned()))
    } else {
        Some(Ok(metadata))
    }
}

fn attr(tag: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = tag.find(&marker)? + marker.len();
    Some(
        tag[start..]
            .split_once('"')?
            .0
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">"),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Unavailable,
    ConnectionLost,
    NotFound,
    InvalidRequest,
    OperationFailed,
}

#[derive(Debug, Error)]
#[error("libvirt {category:?}: {message}")]
pub struct LibvirtError {
    pub category: ErrorCategory,
    message: String,
}

impl LibvirtError {
    fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LibvirtConfig {
    pub uri: String,
    pub managed_domain_prefix: String,
}

impl Default for LibvirtConfig {
    fn default() -> Self {
        Self {
            uri: LOCAL_SYSTEM_URI.to_owned(),
            managed_domain_prefix: "o3k-".to_owned(),
        }
    }
}

impl LibvirtConfig {
    fn validate(&self) -> Result<(), LibvirtError> {
        if self.uri != LOCAL_SYSTEM_URI {
            return Err(LibvirtError::new(
                ErrorCategory::InvalidRequest,
                "only the local qemu:///system URI is supported",
            ));
        }
        if self.managed_domain_prefix.is_empty() || self.managed_domain_prefix.len() > 64 {
            return Err(LibvirtError::new(
                ErrorCategory::InvalidRequest,
                "managed domain prefix is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct LibvirtCapabilities {
    pub uri: String,
    pub libvirt_version: Option<String>,
    pub hypervisor_version: Option<String>,
    pub architecture: Option<String>,
    pub cpu_model: Option<String>,
    pub total_memory_kib: Option<u64>,
    pub machine_types: Vec<String>,
    pub kvm_available: bool,
    pub supported_operations: Vec<String>,
}

impl LibvirtCapabilities {
    pub fn unavailable(uri: &str) -> Self {
        Self {
            uri: uri.to_owned(),
            supported_operations: Vec::new(),
            ..Self::default()
        }
    }

    pub fn to_protocol_capabilities(&self) -> proto::Capabilities {
        proto::Capabilities {
            architecture: self.architecture.clone().unwrap_or_default(),
            agent_provider_name: "o3k-libvirt".to_owned(),
            agent_provider_version: self.libvirt_version.clone().unwrap_or_default(),
            max_vcpus: 0,
            max_memory_mib: self.total_memory_kib.unwrap_or_default() / 1024,
            lifecycle_actions: self.supported_operations.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct DomainDefinition {
    pub name: String,
    pub xml: String,
}

#[derive(Debug, Clone)]
pub struct DomainInspection {
    pub name: String,
    pub active: bool,
    pub persistent: bool,
    pub state: String,
    pub max_memory_kib: u64,
    pub vcpus: u32,
    pub xml: String,
}

#[derive(Clone)]
pub struct LibvirtAdapter {
    config: Arc<LibvirtConfig>,
}

impl fmt::Debug for LibvirtAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LibvirtAdapter")
            .field("uri", &self.config.uri)
            .field("managed_domain_prefix", &self.config.managed_domain_prefix)
            .finish()
    }
}

impl LibvirtAdapter {
    pub fn new(config: LibvirtConfig) -> Result<Self, LibvirtError> {
        config.validate()?;
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub async fn capabilities(&self) -> Result<LibvirtCapabilities, LibvirtError> {
        let uri = self.config.uri.clone();
        run_blocking(move || backend_capabilities(&uri)).await
    }

    pub async fn define(&self, definition: DomainDefinition) -> Result<(), LibvirtError> {
        let uri = self.config.uri.clone();
        run_blocking(move || backend_define(&uri, &definition)).await
    }

    pub async fn start(&self, name: String) -> Result<(), LibvirtError> {
        self.domain_action(name, DomainAction::Start).await
    }

    pub async fn inspect(&self, name: String) -> Result<DomainInspection, LibvirtError> {
        let uri = self.config.uri.clone();
        run_blocking(move || backend_inspect(&uri, &name)).await
    }

    pub async fn shutdown(&self, name: String) -> Result<(), LibvirtError> {
        self.domain_action(name, DomainAction::Shutdown).await
    }

    pub async fn force_stop(&self, name: String) -> Result<(), LibvirtError> {
        self.domain_action(name, DomainAction::ForceStop).await
    }

    pub async fn reboot(&self, name: String) -> Result<(), LibvirtError> {
        self.domain_action(name, DomainAction::Reboot).await
    }

    pub async fn undefine(&self, name: String) -> Result<(), LibvirtError> {
        self.domain_action(name, DomainAction::Undefine).await
    }

    pub async fn list_managed_domains(&self) -> Result<Vec<String>, LibvirtError> {
        let uri = self.config.uri.clone();
        let prefix = self.config.managed_domain_prefix.clone();
        run_blocking(move || backend_list(&uri, &prefix)).await
    }

    async fn domain_action(&self, name: String, action: DomainAction) -> Result<(), LibvirtError> {
        let uri = self.config.uri.clone();
        run_blocking(move || backend_action(&uri, &name, action)).await
    }
}

#[derive(Clone, Copy)]
enum DomainAction {
    Start,
    Shutdown,
    ForceStop,
    Reboot,
    Undefine,
}

async fn run_blocking<T, F>(operation: F) -> Result<T, LibvirtError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, LibvirtError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation).await.map_err(|_| {
        LibvirtError::new(ErrorCategory::OperationFailed, "libvirt worker terminated")
    })?
}

#[cfg(not(feature = "libvirt"))]
fn backend_capabilities(uri: &str) -> Result<LibvirtCapabilities, LibvirtError> {
    Err(LibvirtError::new(
        ErrorCategory::Unavailable,
        format!(
            "local libvirt support is not compiled; install libvirt and enable the libvirt feature for {uri}"
        ),
    ))
}

#[cfg(not(feature = "libvirt"))]
fn backend_define(_: &str, _: &DomainDefinition) -> Result<(), LibvirtError> {
    unavailable()
}
#[cfg(not(feature = "libvirt"))]
fn backend_inspect(_: &str, _: &str) -> Result<DomainInspection, LibvirtError> {
    unavailable()
}
#[cfg(not(feature = "libvirt"))]
fn backend_action(_: &str, _: &str, _: DomainAction) -> Result<(), LibvirtError> {
    unavailable()
}
#[cfg(not(feature = "libvirt"))]
fn backend_list(_: &str, _: &str) -> Result<Vec<String>, LibvirtError> {
    unavailable()
}
#[cfg(not(feature = "libvirt"))]
fn unavailable<T>() -> Result<T, LibvirtError> {
    Err(LibvirtError::new(
        ErrorCategory::Unavailable,
        "local libvirt support is unavailable",
    ))
}

#[cfg(feature = "libvirt")]
fn open(uri: &str) -> Result<Connect, LibvirtError> {
    Connect::open(Some(uri)).map_err(|_| {
        LibvirtError::new(
            ErrorCategory::ConnectionLost,
            "qemu:///system connection failed",
        )
    })
}

#[cfg(feature = "libvirt")]
fn backend_capabilities(uri: &str) -> Result<LibvirtCapabilities, LibvirtError> {
    let connection = open(uri)?;
    let node = connection.get_node_info().map_err(|_| {
        LibvirtError::new(
            ErrorCategory::OperationFailed,
            "node capability discovery failed",
        )
    })?;
    let capabilities_xml = connection.get_capabilities().map_err(|_| {
        LibvirtError::new(
            ErrorCategory::OperationFailed,
            "capability discovery failed",
        )
    })?;
    Ok(LibvirtCapabilities {
        uri: uri.to_owned(),
        libvirt_version: connection.get_lib_version().ok().map(version),
        hypervisor_version: connection.get_hyp_version().ok().map(version),
        architecture: xml_attribute(&capabilities_xml, "<arch", "name"),
        cpu_model: Some(node.model),
        total_memory_kib: Some(node.memory),
        machine_types: xml_values(&capabilities_xml, "<machine", "name"),
        kvm_available: connection
            .get_type()
            .map(|kind| kind == "QEMU")
            .unwrap_or(false),
        supported_operations: [
            "define",
            "start",
            "inspect",
            "shutdown",
            "force-stop",
            "reboot",
            "undefine",
            "list",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    })
}

#[cfg(feature = "libvirt")]
fn version(value: u32) -> String {
    format!(
        "{}.{}.{}",
        value / 1_000_000,
        (value / 1_000) % 1_000,
        value % 1_000
    )
}

#[cfg(feature = "libvirt")]
fn backend_define(uri: &str, definition: &DomainDefinition) -> Result<(), LibvirtError> {
    let connection = open(uri)?;
    Domain::define_xml(&connection, &definition.xml).map_err(|_| {
        LibvirtError::new(ErrorCategory::OperationFailed, "domain definition failed")
    })?;
    Ok(())
}

#[cfg(feature = "libvirt")]
fn backend_inspect(uri: &str, name: &str) -> Result<DomainInspection, LibvirtError> {
    let connection = open(uri)?;
    let domain = Domain::lookup_by_name(&connection, name)
        .map_err(|_| LibvirtError::new(ErrorCategory::NotFound, "domain was not found"))?;
    let info = domain.get_info().map_err(|_| {
        LibvirtError::new(ErrorCategory::OperationFailed, "domain inspection failed")
    })?;
    Ok(DomainInspection {
        name: name.to_owned(),
        active: domain.is_active().unwrap_or(false),
        persistent: domain.is_persistent().unwrap_or(false),
        state: format!("{:?}", info.state),
        max_memory_kib: info.max_mem,
        vcpus: info.nr_virt_cpu,
        xml: domain.get_xml_desc(0).map_err(|_| {
            LibvirtError::new(
                ErrorCategory::OperationFailed,
                "domain XML inspection failed",
            )
        })?,
    })
}

#[cfg(feature = "libvirt")]
fn backend_action(uri: &str, name: &str, action: DomainAction) -> Result<(), LibvirtError> {
    let connection = open(uri)?;
    let domain = Domain::lookup_by_name(&connection, name)
        .map_err(|_| LibvirtError::new(ErrorCategory::NotFound, "domain was not found"))?;
    let result = match action {
        DomainAction::Start => domain.create().map(|_| ()),
        DomainAction::Shutdown => domain.shutdown().map(|_| ()),
        DomainAction::ForceStop => domain.destroy(),
        DomainAction::Reboot => domain.reboot(0),
        DomainAction::Undefine => domain.undefine(),
    };
    result.map_err(|_| {
        LibvirtError::new(
            ErrorCategory::OperationFailed,
            "domain lifecycle operation failed",
        )
    })
}

#[cfg(feature = "libvirt")]
fn backend_list(uri: &str, prefix: &str) -> Result<Vec<String>, LibvirtError> {
    let connection = open(uri)?;
    let domains = connection
        .list_all_domains(0)
        .map_err(|_| LibvirtError::new(ErrorCategory::OperationFailed, "domain listing failed"))?;
    Ok(domains
        .into_iter()
        .filter_map(|domain| domain.get_name().ok())
        .filter(|name| name.starts_with(prefix))
        .collect())
}

#[cfg(feature = "libvirt")]
fn xml_attribute(xml: &str, tag: &str, attribute: &str) -> Option<String> {
    xml_values(xml, tag, attribute).into_iter().next()
}

#[cfg(feature = "libvirt")]
fn xml_values(xml: &str, tag: &str, attribute: &str) -> Vec<String> {
    xml.split(tag)
        .skip(1)
        .filter_map(|part| part.split_once(&format!("{attribute}=\"")))
        .filter_map(|(_, value)| value.split_once('"').map(|(value, _)| value.to_owned()))
        .collect()
}

/// Compute-provider implementation backed by the local-system libvirt adapter.
#[derive(Clone)]
pub struct LibvirtProvider {
    adapter: LibvirtAdapter,
    operations: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<uuid::Uuid, o3k_provider::Operation>>,
    >,
}

impl LibvirtProvider {
    pub fn new(adapter: LibvirtAdapter) -> Self {
        Self {
            adapter,
            operations: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        }
    }

    fn operation(
        &self,
        request: uuid::Uuid,
        resource: Option<String>,
    ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
        let operation = o3k_provider::Operation {
            provider_operation_id: uuid::Uuid::now_v7(),
            o3k_operation_id: request,
            state: o3k_provider::OperationState::Succeeded,
            error_category: None,
            provider_resource_id: resource,
        };
        self.operations
            .lock()
            .map_err(|_| o3k_provider::ProviderError::Storage)?
            .insert(operation.provider_operation_id, operation.clone());
        Ok(operation)
    }
}

fn provider_error(error: LibvirtError) -> o3k_provider::ProviderError {
    match error.category {
        ErrorCategory::NotFound => o3k_provider::ProviderError::NotFound,
        ErrorCategory::InvalidRequest => o3k_provider::ProviderError::InvalidRequest,
        ErrorCategory::ConnectionLost => o3k_provider::ProviderError::Retryable,
        ErrorCategory::Unavailable | ErrorCategory::OperationFailed => {
            o3k_provider::ProviderError::Terminal
        }
    }
}

fn owned_metadata(
    inspection: &DomainInspection,
    expected_server_id: Option<&str>,
) -> Result<DomainMetadata, o3k_provider::ProviderError> {
    let DiscoveryResult::Owned { metadata, .. } =
        discover_domain_xml(&inspection.name, &inspection.xml)
    else {
        // Foreign or malformed domains are never eligible for provider mutations.
        return Err(o3k_provider::ProviderError::NotFound);
    };
    if expected_server_id.is_some_and(|id| metadata.server_id != id) {
        return Err(o3k_provider::ProviderError::NotFound);
    }
    Ok(metadata)
}

#[async_trait::async_trait]
impl o3k_provider::ComputeProvider for LibvirtProvider {
    async fn capabilities(
        &self,
    ) -> Result<o3k_provider::Capabilities, o3k_provider::ProviderError> {
        let value = self.adapter.capabilities().await.map_err(provider_error)?;
        Ok(o3k_provider::Capabilities {
            provider_name: "o3k-libvirt".into(),
            provider_version: value.libvirt_version.unwrap_or_else(|| "unknown".into()),
            capabilities: value.supported_operations,
        })
    }

    async fn create_instance(
        &self,
        request: o3k_provider::CreateInstanceRequest,
    ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
        let image_id = request
            .image_id
            .clone()
            .ok_or(o3k_provider::ProviderError::InvalidRequest)?;
        let definition = build_domain_xml(&DomainSpec {
            metadata: DomainMetadata {
                server_id: request.o3k_server_id.to_string(),
                project_id: request.project_id,
                generation: 1,
                operation_id: request.operation_id.to_string(),
                managed_by: "o3k-compute".into(),
            },
            vcpus: request.vcpus,
            memory_mib: request.memory_mib,
            image_id,
        })
        .map_err(provider_error)?;
        self.adapter
            .define(DomainDefinition {
                name: definition.name.clone(),
                xml: definition.xml,
            })
            .await
            .map_err(provider_error)?;
        if let Err(error) = self.adapter.start(definition.name.clone()).await {
            let _ = self.adapter.undefine(definition.name.clone()).await;
            return Err(provider_error(error));
        }
        self.operation(request.operation_id, Some(definition.name))
    }

    async fn get_instance(
        &self,
        provider_instance_id: &str,
    ) -> Result<o3k_provider::Instance, o3k_provider::ProviderError> {
        let inspection = self
            .adapter
            .inspect(provider_instance_id.to_owned())
            .await
            .map_err(provider_error)?;
        let metadata = owned_metadata(&inspection, None)?;
        Ok(o3k_provider::Instance {
            provider_instance_id: inspection.name,
            o3k_server_id: uuid::Uuid::parse_str(&metadata.server_id)
                .map_err(|_| o3k_provider::ProviderError::Terminal)?,
            state: if inspection.active {
                o3k_provider::InstanceState::Running
            } else {
                o3k_provider::InstanceState::Stopped
            },
            observed_message: Some(inspection.state),
        })
    }

    async fn delete_instance(
        &self,
        request: o3k_provider::DeleteInstanceRequest,
    ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
        let name = request.provider_instance_id;
        match self.adapter.inspect(name.clone()).await {
            Ok(inspection) => {
                owned_metadata(&inspection, None)?;
                if inspection.active {
                    self.adapter
                        .force_stop(name.clone())
                        .await
                        .map_err(provider_error)?;
                }
                self.adapter.undefine(name).await.map_err(provider_error)?;
            }
            Err(error) if error.category == ErrorCategory::NotFound => {}
            Err(error) => return Err(provider_error(error)),
        }
        self.operation(request.operation_id, None)
    }

    async fn action_instance(
        &self,
        provider_instance_id: &str,
        action: o3k_provider::InstanceAction,
        operation_id: uuid::Uuid,
        _idempotency_key: &str,
    ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
        let name = provider_instance_id.to_owned();
        let inspection = self
            .adapter
            .inspect(name.clone())
            .await
            .map_err(provider_error)?;
        owned_metadata(&inspection, None)?;
        match action {
            o3k_provider::InstanceAction::Start => self.adapter.start(name).await,
            o3k_provider::InstanceAction::Stop => self.adapter.shutdown(name).await,
            o3k_provider::InstanceAction::Reboot => self.adapter.reboot(name).await,
        }
        .map_err(provider_error)?;
        self.operation(operation_id, Some(provider_instance_id.to_owned()))
    }

    async fn get_operation(
        &self,
        id: uuid::Uuid,
    ) -> Result<o3k_provider::Operation, o3k_provider::ProviderError> {
        self.operations
            .lock()
            .map_err(|_| o3k_provider::ProviderError::Storage)?
            .get(&id)
            .cloned()
            .ok_or(o3k_provider::ProviderError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_build_reports_missing_libvirt_without_blocking() -> Result<(), LibvirtError> {
        let _adapter = LibvirtAdapter::new(LibvirtConfig::default())?;
        #[cfg(not(feature = "libvirt"))]
        let result = _adapter.capabilities().await;
        assert!(matches!(
            result,
            Err(LibvirtError {
                category: ErrorCategory::Unavailable,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn only_local_system_uri_is_accepted() {
        assert!(
            LibvirtAdapter::new(LibvirtConfig {
                uri: "qemu:///session".to_owned(),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn domain_xml_is_deterministic_escaped_and_recoverable() -> Result<(), LibvirtError> {
        let spec = DomainSpec {
            metadata: DomainMetadata {
                server_id: "server-1".to_owned(),
                project_id: "project&1".to_owned(),
                generation: 7,
                operation_id: "op-1".to_owned(),
                managed_by: "o3k-compute".to_owned(),
            },
            vcpus: 2,
            memory_mib: 512,
            image_id: "/var/lib/o3k/disk&1.qcow2".to_owned(),
        };
        let first = build_domain_xml(&spec)?;
        let second = build_domain_xml(&spec)?;
        assert_eq!(first, second);
        assert!(first.xml.contains("project&amp;1"));
        assert_eq!(
            discover_domain_xml(&first.name, &first.xml),
            DiscoveryResult::Owned {
                name: first.name,
                metadata: spec.metadata
            }
        );
        Ok(())
    }

    #[test]
    fn malformed_or_duplicate_metadata_is_quarantined() {
        let malformed = "<domain><metadata><o3k:domain xmlns:o3k=\"urn:o3k:compute:domain\" generation=\"x\" /></metadata></domain>";
        assert!(matches!(
            discover_domain_xml("o3k-bad", malformed),
            DiscoveryResult::Quarantined { .. }
        ));
        let foreign = "<domain><name>foreign</name></domain>";
        assert_eq!(
            discover_domain_xml("foreign", foreign),
            DiscoveryResult::Foreign {
                name: "foreign".to_owned()
            }
        );
    }

    #[test]
    fn provider_ownership_guard_rejects_foreign_or_malformed_domains() -> Result<(), LibvirtError> {
        let foreign = DomainInspection {
            name: "o3k-same-prefix".to_owned(),
            active: true,
            persistent: true,
            state: "running".to_owned(),
            max_memory_kib: 512,
            vcpus: 1,
            xml: "<domain><name>o3k-same-prefix</name></domain>".to_owned(),
        };
        assert_eq!(
            owned_metadata(&foreign, None),
            Err(o3k_provider::ProviderError::NotFound)
        );

        let spec = DomainSpec {
            metadata: DomainMetadata {
                server_id: "server-guard".to_owned(),
                project_id: "project".to_owned(),
                generation: 1,
                operation_id: "operation".to_owned(),
                managed_by: "o3k-compute".to_owned(),
            },
            vcpus: 1,
            memory_mib: 128,
            image_id: "/var/lib/o3k/image.qcow2".to_owned(),
        };
        let built = build_domain_xml(&spec)?;
        let owned = DomainInspection {
            name: built.name,
            active: false,
            persistent: true,
            state: "shutoff".to_owned(),
            max_memory_kib: 128,
            vcpus: 1,
            xml: built.xml,
        };
        assert_eq!(
            owned_metadata(&owned, Some("different-server")),
            Err(o3k_provider::ProviderError::NotFound)
        );
        Ok(())
    }
}
