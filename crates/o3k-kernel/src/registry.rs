//! Canonical Cloud Kernel service registry, resource/action metadata, and
//! compatibility catalog projections.

use std::{collections::HashMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{action::ActionId, error::KernelError, resource::ResourceType};

/// Strongly typed identifier for an O3K service (e.g. "identity", "image", "compute").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServiceId(String);

impl ServiceId {
    /// Creates and validates a new `ServiceId`.
    pub fn new(id: impl Into<String>) -> Result<Self, KernelError> {
        let s = id.into();
        if s.trim().is_empty() {
            return Err(KernelError::InvalidIdentifier(
                "service ID cannot be empty".to_owned(),
            ));
        }
        Ok(Self(s))
    }

    /// Creates a `ServiceId` without validation (for constants / static init).
    #[must_use]
    pub const fn new_unchecked(id: String) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Strongly typed service namespace (e.g. "identity", "image", "network", "compute", "placement", "volume").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServiceNamespace(String);

impl ServiceNamespace {
    /// Creates and validates a new `ServiceNamespace`.
    pub fn new(ns: impl Into<String>) -> Result<Self, KernelError> {
        let s = ns.into();
        if s.trim().is_empty() {
            return Err(KernelError::InvalidIdentifier(
                "service namespace cannot be empty".to_owned(),
            ));
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(KernelError::InvalidIdentifier(format!(
                "invalid characters in service namespace: {s}"
            )));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    /// Creates a `ServiceNamespace` without validation (for constants / static init).
    #[must_use]
    pub const fn new_unchecked(ns: String) -> Self {
        Self(ns)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn compute() -> Self {
        Self::new_unchecked("compute".to_owned())
    }

    #[must_use]
    pub fn image() -> Self {
        Self::new_unchecked("image".to_owned())
    }

    #[must_use]
    pub fn network() -> Self {
        Self::new_unchecked("network".to_owned())
    }

    #[must_use]
    pub fn identity() -> Self {
        Self::new_unchecked("identity".to_owned())
    }

    #[must_use]
    pub fn placement() -> Self {
        Self::new_unchecked("placement".to_owned())
    }

    #[must_use]
    pub fn volume() -> Self {
        Self::new_unchecked("volume".to_owned())
    }

    #[must_use]
    pub fn database() -> Self {
        Self::new_unchecked("database".to_owned())
    }
}

impl fmt::Display for ServiceNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Service ownership mode in O3K Cloud OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceOwnership {
    /// Service is implemented natively within the O3K Cloud Kernel / runtime.
    O3kImplemented,
    /// Service is hosted externally (e.g. standalone Cinder testbed).
    ExternalHosted,
}

impl fmt::Display for ServiceOwnership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::O3kImplemented => write!(f, "o3k-implemented"),
            Self::ExternalHosted => write!(f, "external-hosted"),
        }
    }
}

/// Metadata describing an API surface exposed by a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiSurface {
    /// Public name of the API surface (e.g. "OpenStack Identity API", "OpenStack Compute API").
    pub name: String,
    /// URL prefix / mount point (e.g. "/v3", "/v2", "/v2.0", "/v2.1", "/placement").
    pub prefix: String,
    /// Major/minor or microversion advertised (e.g. "3.14", "2.1-2.89", "2.0").
    pub version: String,
    /// Whether this API surface is currently enabled.
    pub enabled: bool,
}

/// Template describing a service endpoint in a region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointTemplate {
    /// OpenStack interface ("public", "internal", "admin").
    pub interface: String,
    /// Region identifier ("RegionOne").
    pub region: String,
    /// URL template with placeholders like `{base_url}` and `{project_id}`.
    pub url_template: String,
}

/// Canonical descriptor for an O3K service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    /// Canonical service ID (e.g. "identity", "image", "network", "compute", "placement", "cinder").
    pub id: ServiceId,
    /// Human-readable service name.
    pub name: String,
    /// OpenStack compatibility service type (e.g. "identity", "image", "network", "compute", "placement", "volumev3").
    pub service_type: String,
    /// Canonical domain namespace.
    pub namespace: ServiceNamespace,
    /// Ownership mode.
    pub ownership: ServiceOwnership,
    /// Whether the service is enabled in the active profile.
    pub enabled: bool,
    /// Resource types owned by this service.
    pub resource_types: Vec<ResourceType>,
    /// Action IDs owned by this service.
    pub actions: Vec<ActionId>,
    /// API surfaces exposed by this service.
    pub api_surfaces: Vec<ApiSurface>,
    /// Endpoint templates for catalog projection.
    pub endpoints: Vec<EndpointTemplate>,
}

/// Projected service record for Keystone `/v3/auth/tokens` catalog response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeystoneCatalogService {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: String,
    pub endpoints: Vec<KeystoneCatalogEndpoint>,
}

/// Projected endpoint record for Keystone `/v3/auth/tokens` catalog response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeystoneCatalogEndpoint {
    pub id: String,
    pub interface: String,
    pub region: String,
    pub region_id: String,
    pub url: String,
}

/// Canonical, immutable static Cloud Kernel Service Registry.
#[derive(Debug, Clone)]
pub struct KernelRegistry {
    services: Vec<ServiceDescriptor>,
    service_by_id: HashMap<ServiceId, usize>,
    service_by_namespace: HashMap<ServiceNamespace, usize>,
}

impl KernelRegistry {
    /// Creates a registry from a list of `ServiceDescriptor`s.
    #[must_use]
    pub fn new(services: Vec<ServiceDescriptor>) -> Self {
        let mut service_by_id = HashMap::new();
        let mut service_by_namespace = HashMap::new();

        for (idx, svc) in services.iter().enumerate() {
            service_by_id.insert(svc.id.clone(), idx);
            service_by_namespace.insert(svc.namespace.clone(), idx);
        }

        Self {
            services,
            service_by_id,
            service_by_namespace,
        }
    }

    /// Builds the standard O3K Cloud Kernel registry for the current runtime.
    #[must_use]
    pub fn standard(base_url: &str, cinder_url: Option<&str>) -> Self {
        Self::for_profile("native-rust-testlab", base_url, cinder_url)
    }

    /// Builds the registry configured for a specific product profile.
    #[must_use]
    pub fn for_profile(profile: &str, base_url: &str, cinder_url: Option<&str>) -> Self {
        let base = base_url.trim_end_matches('/');
        let mut services = Vec::new();

        // Helper macro/closures to register cleanly
        let mk_svc_id =
            |s: &str| ServiceId::new(s).unwrap_or_else(|_| ServiceId::new_unchecked(s.to_owned()));
        let mk_ns =
            |s: &str| ServiceNamespace::new(s).unwrap_or_else(|_| ServiceNamespace(s.to_owned()));
        let mk_res = |ns: &str, n: &str| {
            ResourceType::new(ns, n)
                .unwrap_or_else(|_| ResourceType::new_unchecked(ns.to_owned(), n.to_owned()))
        };
        let mk_act = |ns: &str, a: &str| {
            ActionId::new(ns, a)
                .unwrap_or_else(|_| ActionId::new_unchecked(ns.to_owned(), a.to_owned()))
        };

        // 1. Identity
        services.push(ServiceDescriptor {
            id: mk_svc_id("identity"),
            name: "identity".to_owned(),
            service_type: "identity".to_owned(),
            namespace: mk_ns("identity"),
            ownership: ServiceOwnership::O3kImplemented,
            enabled: true,
            resource_types: vec![
                mk_res("identity", "token"),
                mk_res("identity", "project"),
                mk_res("identity", "user"),
                mk_res("identity", "role"),
            ],
            actions: vec![
                mk_act("identity", "IssueToken"),
                mk_act("identity", "ValidateToken"),
                mk_act("identity", "RevokeToken"),
            ],
            api_surfaces: vec![ApiSurface {
                name: "OpenStack Identity API".to_owned(),
                prefix: "/v3".to_owned(),
                version: "3.14".to_owned(),
                enabled: true,
            }],
            endpoints: vec![
                EndpointTemplate {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v3"),
                },
                EndpointTemplate {
                    interface: "internal".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v3"),
                },
                EndpointTemplate {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v3"),
                },
            ],
        });

        // 2. Image
        services.push(ServiceDescriptor {
            id: mk_svc_id("image"),
            name: "image".to_owned(),
            service_type: "image".to_owned(),
            namespace: mk_ns("image"),
            ownership: ServiceOwnership::O3kImplemented,
            enabled: true,
            resource_types: vec![mk_res("image", "image")],
            actions: vec![
                mk_act("image", "ListImages"),
                mk_act("image", "CreateImage"),
                mk_act("image", "ReadImage"),
                mk_act("image", "DeleteImage"),
                mk_act("image", "UploadImage"),
                mk_act("image", "DownloadImage"),
            ],
            api_surfaces: vec![ApiSurface {
                name: "OpenStack Image API".to_owned(),
                prefix: "/v2".to_owned(),
                version: "2.8".to_owned(),
                enabled: true,
            }],
            endpoints: vec![
                EndpointTemplate {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2"),
                },
                EndpointTemplate {
                    interface: "internal".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2"),
                },
                EndpointTemplate {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2"),
                },
            ],
        });

        // 3. Network
        services.push(ServiceDescriptor {
            id: mk_svc_id("network"),
            name: "network".to_owned(),
            service_type: "network".to_owned(),
            namespace: mk_ns("network"),
            ownership: ServiceOwnership::O3kImplemented,
            enabled: true,
            resource_types: vec![
                mk_res("network", "network"),
                mk_res("network", "subnet"),
                mk_res("network", "port"),
                mk_res("network", "extension"),
            ],
            actions: vec![
                mk_act("network", "ListNetworks"),
                mk_act("network", "CreateNetwork"),
                mk_act("network", "ReadNetwork"),
                mk_act("network", "DeleteNetwork"),
                mk_act("network", "ListSubnets"),
                mk_act("network", "CreateSubnet"),
                mk_act("network", "ReadSubnet"),
                mk_act("network", "DeleteSubnet"),
                mk_act("network", "ListPorts"),
                mk_act("network", "CreatePort"),
                mk_act("network", "ReadPort"),
                mk_act("network", "DeletePort"),
                mk_act("network", "ListExtensions"),
            ],
            api_surfaces: vec![ApiSurface {
                name: "OpenStack Network API".to_owned(),
                prefix: "/v2.0".to_owned(),
                version: "2.0".to_owned(),
                enabled: true,
            }],
            endpoints: vec![
                EndpointTemplate {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.0"),
                },
                EndpointTemplate {
                    interface: "internal".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.0"),
                },
                EndpointTemplate {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.0"),
                },
            ],
        });

        // 4. Compute
        services.push(ServiceDescriptor {
            id: mk_svc_id("compute"),
            name: "compute".to_owned(),
            service_type: "compute".to_owned(),
            namespace: mk_ns("compute"),
            ownership: ServiceOwnership::O3kImplemented,
            enabled: true,
            resource_types: vec![
                mk_res("compute", "flavor"),
                mk_res("compute", "keypair"),
                mk_res("compute", "server"),
                mk_res("volume", "volume_attachment"),
            ],
            actions: vec![
                mk_act("compute", "ListFlavors"),
                mk_act("compute", "CreateFlavor"),
                mk_act("compute", "ReadFlavor"),
                mk_act("compute", "DeleteFlavor"),
                mk_act("compute", "ListKeypairs"),
                mk_act("compute", "ImportKeypair"),
                mk_act("compute", "ReadKeypair"),
                mk_act("compute", "DeleteKeypair"),
                mk_act("compute", "ListServers"),
                mk_act("compute", "CreateServer"),
                mk_act("compute", "ReadServer"),
                mk_act("compute", "DeleteServer"),
                mk_act("compute", "StopServer"),
                mk_act("compute", "StartServer"),
                mk_act("compute", "RebootServer"),
                mk_act("compute", "ReadConsole"),
                mk_act("volume", "ListVolumeAttachments"),
                mk_act("volume", "AttachVolume"),
                mk_act("volume", "ReadVolumeAttachment"),
                mk_act("volume", "DetachVolume"),
            ],
            api_surfaces: vec![ApiSurface {
                name: "OpenStack Compute API".to_owned(),
                prefix: "/v2.1".to_owned(),
                version: "2.1".to_owned(),
                enabled: true,
            }],
            endpoints: vec![
                EndpointTemplate {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.1/{{project_id}}"),
                },
                EndpointTemplate {
                    interface: "internal".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.1/{{project_id}}"),
                },
                EndpointTemplate {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/v2.1/{{project_id}}"),
                },
            ],
        });

        // 5. Placement / Capacity
        services.push(ServiceDescriptor {
            id: mk_svc_id("placement"),
            name: "placement".to_owned(),
            service_type: "placement".to_owned(),
            namespace: mk_ns("placement"),
            ownership: ServiceOwnership::O3kImplemented,
            enabled: true,
            resource_types: vec![
                mk_res("placement", "resource_provider"),
                mk_res("placement", "allocation"),
            ],
            actions: vec![],
            api_surfaces: vec![ApiSurface {
                name: "OpenStack Placement API".to_owned(),
                prefix: "/placement".to_owned(),
                version: "1.0".to_owned(),
                enabled: true,
            }],
            endpoints: vec![
                EndpointTemplate {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/placement"),
                },
                EndpointTemplate {
                    interface: "internal".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/placement"),
                },
                EndpointTemplate {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: format!("{base}/placement"),
                },
            ],
        });

        // 6. External-Hosted Cinder (if endpoint configured or profile demands)
        if let Some(cinder) = cinder_url {
            let cinder_base = cinder.trim_end_matches('/');
            services.push(ServiceDescriptor {
                id: mk_svc_id("cinder"),
                name: "cinder".to_owned(),
                service_type: "volumev3".to_owned(),
                namespace: mk_ns("volume"),
                ownership: ServiceOwnership::ExternalHosted,
                enabled: true,
                resource_types: vec![mk_res("volume", "volume")],
                actions: vec![],
                api_surfaces: vec![ApiSurface {
                    name: "OpenStack Block Storage API v3".to_owned(),
                    prefix: "/v3".to_owned(),
                    version: "3.0".to_owned(),
                    enabled: true,
                }],
                endpoints: vec![
                    EndpointTemplate {
                        interface: "public".to_owned(),
                        region: "RegionOne".to_owned(),
                        url_template: format!("{cinder_base}/v3/{{project_id}}"),
                    },
                    EndpointTemplate {
                        interface: "internal".to_owned(),
                        region: "RegionOne".to_owned(),
                        url_template: format!("{cinder_base}/v3/{{project_id}}"),
                    },
                    EndpointTemplate {
                        interface: "admin".to_owned(),
                        region: "RegionOne".to_owned(),
                        url_template: format!("{cinder_base}/v3/{{project_id}}"),
                    },
                ],
            });
        }

        let _ = profile;
        Self::new(services)
    }

    /// Returns the list of registered services.
    #[must_use]
    pub fn services(&self) -> &[ServiceDescriptor] {
        &self.services
    }

    /// Looks up a service descriptor by ID.
    #[must_use]
    pub fn service_by_id(&self, id: &ServiceId) -> Option<&ServiceDescriptor> {
        self.service_by_id.get(id).map(|&idx| &self.services[idx])
    }

    /// Looks up a service descriptor by namespace.
    #[must_use]
    pub fn service_by_namespace(&self, ns: &ServiceNamespace) -> Option<&ServiceDescriptor> {
        self.service_by_namespace
            .get(ns)
            .map(|&idx| &self.services[idx])
    }

    /// Checks if an action is registered by any active service in the registry.
    #[must_use]
    pub fn has_action(&self, action: &ActionId) -> bool {
        self.services
            .iter()
            .filter(|s| s.enabled)
            .any(|s| s.actions.iter().any(|a| a == action))
    }

    /// Checks if a resource type is registered by any active service in the registry.
    #[must_use]
    pub fn has_resource_type(&self, res: &ResourceType) -> bool {
        self.services
            .iter()
            .filter(|s| s.enabled)
            .any(|s| s.resource_types.iter().any(|r| r == res))
    }

    /// Projects the static registry into the Keystone `/v3/auth/tokens` catalog format.
    #[must_use]
    pub fn project_keystone_catalog(&self, project_id: &str) -> Vec<KeystoneCatalogService> {
        let mut catalog: Vec<KeystoneCatalogService> = self
            .services
            .iter()
            .filter(|svc| svc.enabled)
            .map(|svc| {
                let endpoints: Vec<KeystoneCatalogEndpoint> = svc
                    .endpoints
                    .iter()
                    .enumerate()
                    .map(|(idx, ep)| KeystoneCatalogEndpoint {
                        id: format!("endpoint-{}-{idx}", svc.id),
                        interface: ep.interface.clone(),
                        region: ep.region.clone(),
                        region_id: ep.region.clone(),
                        url: ep.url_template.replace("{project_id}", project_id),
                    })
                    .collect();

                KeystoneCatalogService {
                    id: svc.id.to_string(),
                    name: svc.name.clone(),
                    service_type: svc.service_type.clone(),
                    endpoints,
                }
            })
            .collect();

        catalog.sort_by(|a, b| a.service_type.cmp(&b.service_type));
        catalog.retain(|svc| !svc.endpoints.is_empty());
        catalog
    }
}
