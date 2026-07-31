//! Typed local libvirt/KVM adapter for the compute agent.
//!
//! The public API is async and keeps all libvirt FFI behind `spawn_blocking`.
//! The `libvirt` feature enables the `virt` bindings; the default build keeps
//! the control plane usable on hosts where libvirt development libraries are
//! not installed and reports a clear readiness error instead.

use std::{fmt, sync::Arc};

use o3k_provider_contract::compute_proto as proto;
use thiserror::Error;

#[cfg(feature = "libvirt")]
use virt::{connect::Connect, domain::Domain};

pub const LOCAL_SYSTEM_URI: &str = "qemu:///system";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_build_reports_missing_libvirt_without_blocking() -> Result<(), LibvirtError> {
        let _adapter = LibvirtAdapter::new(LibvirtConfig::default())?;
        #[cfg(not(feature = "libvirt"))]
        assert_eq!(
            _adapter.capabilities().await.unwrap_err().category,
            ErrorCategory::Unavailable
        );
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
}
