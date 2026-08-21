//! Service Manifest v1 and OpenStack Compatibility Projection types.
//!
//! ADR-0174 / SPEC-0031 define these as the canonical service identity and
//! capability contract, separated from OpenStack compatibility metadata.
//!
//! Architectural invariants:
//! - The manifest describes O3K-native service identity and capabilities.
//! - OpenStack compatibility is a separate projection.
//! - A service does not need an OpenStack service_type or Keystone endpoint
//!   definition to be a valid first-class O3K service.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::action::ActionId;
use crate::controller::{
    ControllerHealth, ControllerRegistration, ControllerSession, ControllerState,
};
use crate::error::KernelError;
use crate::resource::ResourceType;

/// Service ownership mode in O3K Cloud OS.
///
/// Re-exported from registry for convenience; the semantic is the same.
pub use crate::registry::ServiceOwnership;

/// The lifecycle state of a registered service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleState {
    /// Manifest declared, not yet fully validated or ready.
    Declared,
    /// Service is fully registered, validated, and able to accept work.
    Ready,
    /// Service authority and resources remain, but the service cannot safely
    /// accept new work.
    NotReady,
    /// Operator-disabled; no new work flows to the service.
    Disabled,
    /// Protocol or manifest version cannot be safely used.
    Incompatible,
}

impl std::fmt::Display for ServiceLifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declared => write!(f, "declared"),
            Self::Ready => write!(f, "ready"),
            Self::NotReady => write!(f, "not_ready"),
            Self::Disabled => write!(f, "disabled"),
            Self::Incompatible => write!(f, "incompatible"),
        }
    }
}

/// Controller protocol binding in a service manifest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControllerBinding {
    /// Protocol identifier (e.g. `"grpc-v1"`).
    pub protocol: String,
    /// Target endpoint for the controller.
    pub endpoint: String,
    /// Optional controller identity reference for mTLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_ref: Option<String>,
}

/// A quota dimension declared by a service.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuotaDimension {
    /// Dimension key (e.g. `"instances"`).
    pub key: String,
    /// Human-readable unit (e.g. `"count"`, `"bytes"`, `"mb"`).
    pub unit: String,
    /// Scope kind for this dimension (e.g. `"project"`).
    pub scope: String,
}

/// Health/readiness metadata for a service.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceHealth {
    /// Whether the service reports as healthy.
    pub healthy: bool,
    /// Human-readable status detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Service Manifest v1 — canonical native O3K service identity and capability
/// contract.
///
/// This manifest describes what the service IS, not where it runs or which
/// OpenStack compatibility surfaces it exposes. The latter is handled by
/// [`OpenStackCompatibilityProjection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceManifest {
    /// Manifest schema version (v1 uses `1`).
    pub manifest_version: u32,
    /// Stable O3K service identifier (e.g. `"database-example"`).
    pub service_id: String,
    /// Canonical service namespace (e.g. `"database"`).
    pub namespace: String,
    /// Human-readable service version (e.g. `"0.1.0"`).
    pub service_version: String,
    /// Ownership mode: `o3k-implemented` or `external-hosted`.
    pub ownership: ServiceOwnership,
    /// Resource types owned by this service.
    #[serde(default)]
    pub resource_types: Vec<String>,
    /// Action IDs owned by this service (e.g. `"database:CreateInstance"`).
    #[serde(default)]
    pub actions: Vec<String>,
    /// Optional capability labels.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional service dependencies (namespace:type references).
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Optional quota dimensions.
    #[serde(default)]
    pub quota_dimensions: Vec<QuotaDimension>,
    /// Optional region scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Optional availability domain scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_domain: Option<String>,
    /// Controller binding for external services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<ControllerBinding>,
    /// Health/readiness metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<ServiceHealth>,
}

impl ServiceManifest {
    /// Validates the manifest against basic invariants.
    ///
    /// Returns an error if:
    /// - required fields are missing;
    /// - resource types or actions use identifiers outside the declared namespace;
    /// - the manifest version is unsupported.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != 1 {
            return Err(ManifestError::UnsupportedVersion(self.manifest_version));
        }
        if self.service_id.trim().is_empty() {
            return Err(ManifestError::InvalidField("service_id"));
        }
        if self.namespace.trim().is_empty() {
            return Err(ManifestError::InvalidField("namespace"));
        }
        if self.service_version.trim().is_empty() {
            return Err(ManifestError::InvalidField("service_version"));
        }

        for rt in &self.resource_types {
            let Some((ns, _)) = rt.split_once(':') else {
                return Err(ManifestError::InvalidIdentifier(
                    rt.clone(),
                    "resource type must be namespace:name".to_owned(),
                ));
            };
            if ns != self.namespace {
                return Err(ManifestError::NamespaceMismatch {
                    identifier: rt.clone(),
                    expected: self.namespace.clone(),
                });
            }
        }

        for act in &self.actions {
            let Some((ns, _)) = act.split_once(':') else {
                return Err(ManifestError::InvalidIdentifier(
                    act.clone(),
                    "action must be namespace:Action".to_owned(),
                ));
            };
            if ns != self.namespace {
                return Err(ManifestError::NamespaceMismatch {
                    identifier: act.clone(),
                    expected: self.namespace.clone(),
                });
            }
        }

        Ok(())
    }

    /// Returns the parsed resource types as canonical `ResourceType` values.
    pub fn parsed_resource_types(&self) -> Result<Vec<ResourceType>, KernelError> {
        self.resource_types
            .iter()
            .map(|rt| {
                let (ns, name) = rt.split_once(':').ok_or_else(|| {
                    KernelError::InvalidResourceType(format!("malformed resource type: {rt}"))
                })?;
                ResourceType::new(ns, name)
            })
            .collect()
    }

    /// Returns the parsed actions as canonical `ActionId` values.
    pub fn parsed_actions(&self) -> Result<Vec<ActionId>, KernelError> {
        self.actions.iter().map(|a| ActionId::parse(a)).collect()
    }
}

/// OpenStack Compatibility Projection v1.
///
/// This is the metadata that maps an O3K service to Keystone/OpenStack
/// compatibility concepts. A native-only service (e.g. `database`) has no
/// projection. An OpenStack-compatible service (e.g. `compute`, `network`,
/// `volume`) has one projection per verified compatibility surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackCompatibilityProjection {
    /// O3K service_id this projection maps FROM.
    pub service_id: String,
    /// OpenStack service_type (e.g. `"compute"`, `"volumev3"`).
    pub service_type: String,
    /// Exposed API surfaces.
    #[serde(default)]
    pub api_surfaces: Vec<OpenStackApiSurface>,
    /// Catalog endpoint templates.
    #[serde(default)]
    pub endpoints: Vec<OpenStackEndpointTemplate>,
    /// Whether this projection is currently enabled/advertised.
    pub enabled: bool,
}

/// OpenStack API surface description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackApiSurface {
    /// Human-readable name.
    pub name: String,
    /// URL prefix/mount point.
    pub prefix: String,
    /// Version string.
    pub version: String,
}

/// OpenStack catalog endpoint template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackEndpointTemplate {
    /// Interface label.
    pub interface: String,
    /// Region identifier.
    pub region: String,
    /// URL template.
    pub url_template: String,
}

/// Errors produced during manifest validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(u32),
    #[error("invalid or missing manifest field: {0}")]
    InvalidField(&'static str),
    #[error("invalid identifier '{0}': {1}")]
    InvalidIdentifier(String, String),
    #[error("identifier '{identifier}' uses namespace outside owned namespace '{expected}'")]
    NamespaceMismatch {
        identifier: String,
        expected: String,
    },
    #[error("duplicate service ID: {0}")]
    DuplicateServiceId(String),
    #[error("duplicate namespace: {0}")]
    DuplicateNamespace(String),
    #[error("duplicate resource type: {0}")]
    DuplicateResourceType(String),
    #[error("duplicate action: {0}")]
    DuplicateAction(String),
    #[error("duplicate quota dimension: {0}")]
    DuplicateQuotaDimension(String),
}

/// A registry of active service manifests.
///
/// This is the v2 registry concept from ADR-0174. It coexists with the
/// existing static `KernelRegistry` and is not the runtime authority until
/// the P12 migration is proven.
#[derive(Debug, Clone, Default)]
pub struct ManifestRegistry {
    manifests: HashMap<String, ServiceManifest>,
    by_namespace: HashMap<String, String>, // namespace -> service_id
    controllers: HashMap<String, ControllerRegistration>, // service_id -> registration
}

impl ManifestRegistry {
    /// Creates a new empty manifest registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or updates a controller session for a registered service.
    ///
    /// The controller starts in `Declared` state. Call `update_controller_health`
    /// after health/readiness verification transitions it to `Ready`.
    ///
    /// Returns an error if the service is not registered.
    pub fn register_controller(
        &mut self,
        service_id: &str,
        session: ControllerSession,
    ) -> Result<(), ManifestError> {
        if !self.manifests.contains_key(service_id) {
            return Err(ManifestError::InvalidField("service_id"));
        }
        let namespace = self.manifests[service_id].namespace.clone();

        // Check session generation: a newer session supersedes.
        if let Some(existing) = self.controllers.get(service_id) {
            if let Some(ref existing_session) = existing.session {
                if session.session_generation <= existing_session.session_generation {
                    return Err(ManifestError::InvalidField(
                        "session_generation must increase",
                    ));
                }
            }
        }

        let registration = ControllerRegistration {
            service_id: service_id.to_owned(),
            namespace,
            session: Some(session),
            state: ControllerState::Declared,
            health: None,
        };
        self.controllers.insert(service_id.to_owned(), registration);
        Ok(())
    }

    /// Transitions a controller from `Declared` to `Ready` after health checks
    /// pass. This is a separate step because SPEC-0031 requires protocol
    /// negotiation, manifest verification and health confirmation before Ready.
    pub fn activate_controller(
        &mut self,
        service_id: &str,
    ) -> Result<(), ManifestError> {
        let reg = self.controllers.get_mut(service_id).ok_or(ManifestError::InvalidField("service_id"))?;
        if reg.state != ControllerState::Declared {
            return Err(ManifestError::InvalidField("controller must be Declared to activate"));
        }
        reg.state = ControllerState::Ready;
        Ok(())
    }

    /// Updates the health of a registered controller.
    pub fn update_controller_health(
        &mut self,
        service_id: &str,
        health: ControllerHealth,
    ) -> Result<(), ManifestError> {
        let reg = self
            .controllers
            .get_mut(service_id)
            .ok_or(ManifestError::InvalidField("service_id"))?;
        let state = if health.healthy {
            ControllerState::Ready
        } else {
            ControllerState::NotReady
        };
        reg.health = Some(health);
        reg.state = state;
        Ok(())
    }

    /// Returns the controller registration for a service, if any.
    #[must_use]
    pub fn controller(&self, service_id: &str) -> Option<&ControllerRegistration> {
        self.controllers.get(service_id)
    }

    /// Returns all controller registrations.
    #[must_use]
    pub fn all_controllers(&self) -> Vec<&ControllerRegistration> {
        self.controllers.values().collect()
    }

    /// Removes a controller registration (does not unregister the manifest).
    pub fn remove_controller(&mut self, service_id: &str) {
        self.controllers.remove(service_id);
    }

    /// Attempts to register a service manifest.
    ///
    /// Returns an error if the manifest fails validation or conflicts with
    /// an already-registered namespace/resource type/action.
    ///
    /// Registration is atomic: all invariants are checked before any mutation.
    /// On failure, the registry state is left unchanged.
    pub fn register(&mut self, manifest: ServiceManifest) -> Result<(), ManifestError> {
        use crate::error::KernelError;

        // 1. Validate manifest structure
        manifest.validate()?;

        // 2. Reject duplicate / blank service ID
        if manifest.service_id.trim().is_empty() {
            return Err(ManifestError::InvalidField("service_id"));
        }
        if manifest.service_id.len() > 128 {
            return Err(ManifestError::InvalidField("service_id (max 128 chars)"));
        }
        if self.manifests.contains_key(&manifest.service_id) {
            return Err(ManifestError::DuplicateServiceId(
                manifest.service_id.clone(),
            ));
        }

        // 3. Validate namespace
        if manifest.namespace.trim().is_empty() {
            return Err(ManifestError::InvalidField("namespace"));
        }
        if manifest.namespace.len() > 64 {
            return Err(ManifestError::InvalidField("namespace (max 64 chars)"));
        }
        if self.by_namespace.contains_key(&manifest.namespace) {
            return Err(ManifestError::DuplicateNamespace(
                manifest.namespace.clone(),
            ));
        }

        // 4. Parse and validate resource types eagerly (fail-closed)
        if manifest.resource_types.is_empty() {
            return Err(ManifestError::InvalidField("resource_types (must declare at least one)"));
        }
        if manifest.resource_types.len() > 256 {
            return Err(ManifestError::InvalidField("resource_types (max 256)"));
        }
        let parsed_rts: Vec<ResourceType> = manifest
            .resource_types
            .iter()
            .map(|rt| {
                let (ns, name) = rt.split_once(':').ok_or_else(|| {
                    ManifestError::InvalidIdentifier(rt.clone(), "missing ':' separator".to_owned())
                })?;
                if ns != manifest.namespace {
                    return Err(ManifestError::NamespaceMismatch {
                        identifier: rt.clone(),
                        expected: manifest.namespace.clone(),
                    });
                }
                ResourceType::new(ns, name).map_err(|e| {
                    ManifestError::InvalidIdentifier(rt.clone(), e.to_string())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // 5. Check duplicate resource types within this manifest
        for (i, rt) in parsed_rts.iter().enumerate() {
            if parsed_rts[..i].contains(rt) {
                return Err(ManifestError::DuplicateResourceType(rt.to_string()));
            }
        }

        // 6. Parse and validate actions eagerly (fail-closed)
        if manifest.actions.is_empty() {
            return Err(ManifestError::InvalidField("actions (must declare at least one)"));
        }
        if manifest.actions.len() > 512 {
            return Err(ManifestError::InvalidField("actions (max 512)"));
        }
        let parsed_actions: Vec<ActionId> = manifest
            .actions
            .iter()
            .map(|a| {
                let (ns, act) = a.split_once(':').ok_or_else(|| {
                    ManifestError::InvalidIdentifier(
                        a.clone(),
                        "action must be namespace:Action".to_owned(),
                    )
                })?;
                if ns != manifest.namespace {
                    return Err(ManifestError::NamespaceMismatch {
                        identifier: a.clone(),
                        expected: manifest.namespace.clone(),
                    });
                }
                ActionId::new(ns, act).map_err(|e| {
                    ManifestError::InvalidIdentifier(a.clone(), e.to_string())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // 7. Check duplicate actions within this manifest
        for (i, act) in parsed_actions.iter().enumerate() {
            if parsed_actions[..i].contains(act) {
                return Err(ManifestError::DuplicateAction(act.to_string()));
            }
        }

        // 8. Check duplicate resource types against existing registrations
        for rt in &parsed_rts {
            for existing in self.manifests.values() {
                let existing_rts = existing.parsed_resource_types().map_err(|e: KernelError| {
                    ManifestError::InvalidIdentifier("existing".to_owned(), e.to_string())
                })?;
                if existing_rts.contains(rt) {
                    return Err(ManifestError::DuplicateResourceType(rt.to_string()));
                }
            }
        }

        // 9. Check duplicate actions against existing registrations
        for act in &parsed_actions {
            for existing in self.manifests.values() {
                let existing_acts = existing.parsed_actions().map_err(|e: KernelError| {
                    ManifestError::InvalidIdentifier("existing".to_owned(), e.to_string())
                })?;
                if existing_acts.contains(act) {
                    return Err(ManifestError::DuplicateAction(act.to_string()));
                }
            }
        }

        // 10. Validate quota dimensions (service-owned namespace)
        for (i, qd) in manifest.quota_dimensions.iter().enumerate() {
            if qd.key.trim().is_empty() || qd.key.len() > 64 {
                return Err(ManifestError::InvalidField("quota_dimensions[].key"));
            }
            if manifest.quota_dimensions[..i].iter().any(|o| o.key == qd.key) {
                return Err(ManifestError::DuplicateQuotaDimension(qd.key.clone()));
            }
        }

        // 11. ALL CHECKS PASSED — atomically apply (HashMap inserts are infallible)
        let service_id = manifest.service_id.clone();
        self.by_namespace
            .insert(manifest.namespace.clone(), service_id.clone());
        self.manifests.insert(service_id, manifest);
        Ok(())
    }

    /// Returns a reference to a registered manifest by service ID.
    #[must_use]
    pub fn get(&self, service_id: &str) -> Option<&ServiceManifest> {
        self.manifests.get(service_id)
    }

    /// Returns a reference to a registered manifest by namespace.
    #[must_use]
    pub fn get_by_namespace(&self, namespace: &str) -> Option<&ServiceManifest> {
        self.by_namespace
            .get(namespace)
            .and_then(|id| self.manifests.get(id))
    }

    /// Returns all registered manifests.
    #[must_use]
    pub fn all(&self) -> Vec<&ServiceManifest> {
        self.manifests.values().collect()
    }

    /// Returns the number of registered services.
    #[must_use]
    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    /// Returns true if no services are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    /// Remove a service by ID.
    pub fn remove(&mut self, service_id: &str) {
        if let Some(manifest) = self.manifests.remove(service_id) {
            self.by_namespace.remove(&manifest.namespace);
        }
    }

    /// Checks whether a resource type is registered by any active service.
    ///
    /// # Panics
    /// Panics if any registered manifest has malformed resource types — this
    /// indicates a registry invariant violation.
    #[must_use]
    pub fn has_resource_type(&self, resource_type: &ResourceType) -> bool {
        self.manifests.values().any(|m| {
            m.parsed_resource_types()
                .expect("registered manifest must have valid resource types")
                .contains(resource_type)
        })
    }

    /// Checks whether an action is registered by any active service.
    ///
    /// # Panics
    /// Panics if any registered manifest has malformed actions — this
    /// indicates a registry invariant violation.
    #[must_use]
    pub fn has_action(&self, action: &ActionId) -> bool {
        self.manifests.values().any(|m| {
            m.parsed_actions()
                .expect("registered manifest must have valid actions")
                .contains(action)
        })
    }

    /// Seeds the registry with core P0-P11 services for the native TestLab
    /// profile. This adapts the static `KernelRegistry` services into the
    /// `ManifestRegistry` format so that native discovery has a coherent
    /// source of truth during migration.
    ///
    /// Existing core services are registered with `Declared` lifecycle state.
    /// Controller binding and health/readiness are set by the runtime after
    /// this seeding step.
    ///
    /// This is a migration adapter and will be replaced as core services
    /// adopt native manifest registration directly.
    pub fn seed_core(&mut self) {
        let core_manifests = vec![
            ServiceManifest {
                manifest_version: 1,
                service_id: "identity".to_owned(),
                namespace: "identity".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    "identity:token".to_owned(),
                    "identity:project".to_owned(),
                    "identity:user".to_owned(),
                    "identity:role".to_owned(),
                ],
                actions: vec![
                    "identity:IssueToken".to_owned(),
                    "identity:ValidateToken".to_owned(),
                    "identity:RevokeToken".to_owned(),
                ],
                capabilities: vec!["openstack-identity-v3".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "image".to_owned(),
                namespace: "image".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec!["image:image".to_owned()],
                actions: vec![
                    "image:ListImages".to_owned(),
                    "image:CreateImage".to_owned(),
                    "image:ReadImage".to_owned(),
                    "image:DeleteImage".to_owned(),
                    "image:UploadImage".to_owned(),
                    "image:DownloadImage".to_owned(),
                ],
                capabilities: vec!["openstack-glance-v2".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "network".to_owned(),
                namespace: "network".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    "network:network".to_owned(),
                    "network:subnet".to_owned(),
                    "network:port".to_owned(),
                ],
                actions: vec![
                    "network:ListNetworks".to_owned(),
                    "network:CreateNetwork".to_owned(),
                    "network:ReadNetwork".to_owned(),
                    "network:DeleteNetwork".to_owned(),
                    "network:ListSubnets".to_owned(),
                    "network:CreateSubnet".to_owned(),
                    "network:ReadSubnet".to_owned(),
                    "network:DeleteSubnet".to_owned(),
                    "network:ListPorts".to_owned(),
                    "network:CreatePort".to_owned(),
                    "network:ReadPort".to_owned(),
                    "network:DeletePort".to_owned(),
                ],
                capabilities: vec!["openstack-neutron-v2".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "compute".to_owned(),
                namespace: "compute".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    "compute:server".to_owned(),
                    "compute:flavor".to_owned(),
                    "compute:keypair".to_owned(),
                ],
                actions: vec![
                    "compute:ListFlavors".to_owned(),
                    "compute:CreateFlavor".to_owned(),
                    "compute:ReadFlavor".to_owned(),
                    "compute:DeleteFlavor".to_owned(),
                    "compute:ListKeypairs".to_owned(),
                    "compute:ImportKeypair".to_owned(),
                    "compute:ReadKeypair".to_owned(),
                    "compute:DeleteKeypair".to_owned(),
                    "compute:ListServers".to_owned(),
                    "compute:CreateServer".to_owned(),
                    "compute:ReadServer".to_owned(),
                    "compute:DeleteServer".to_owned(),
                    "compute:StopServer".to_owned(),
                    "compute:StartServer".to_owned(),
                    "compute:RebootServer".to_owned(),
                    "compute:ReadConsole".to_owned(),
                ],
                capabilities: vec!["openstack-nova-v2.1".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "placement".to_owned(),
                namespace: "placement".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    "placement:resource_provider".to_owned(),
                    "placement:allocation".to_owned(),
                ],
                actions: vec![],
                capabilities: vec!["openstack-placement-v1".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "volume".to_owned(),
                namespace: "volume".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    "volume:volume".to_owned(),
                    "volume:volume_attachment".to_owned(),
                ],
                actions: vec![
                    "volume:ListVolumeAttachments".to_owned(),
                    "volume:AttachVolume".to_owned(),
                    "volume:ReadVolumeAttachment".to_owned(),
                    "volume:DetachVolume".to_owned(),
                ],
                capabilities: vec!["o3k-native-storage-v1".to_owned()],
                dependencies: vec![],
                quota_dimensions: vec![],
                region: None,
                availability_domain: None,
                controller: None,
                health: None,
            },
        ];

        for m in core_manifests {
            // Silently skip services that are already registered
            // (e.g. if a controller registered them explicitly)
            if self.manifests.contains_key(&m.service_id) {
                continue;
            }
            if self.by_namespace.contains_key(&m.namespace) {
                continue;
            }
            let service_id = m.service_id.clone();
            self.manifests.insert(service_id, m);
        }
    }

    /// Returns all unique resource types across registered services.
    ///
    /// # Panics
    /// Panics if any registered manifest has malformed resource types — this
    /// indicates a registry invariant violation.
    #[must_use]
    pub fn all_resource_types(&self) -> Vec<ResourceType> {
        let mut types: Vec<ResourceType> = Vec::new();
        for m in self.manifests.values() {
            let rts = m
                .parsed_resource_types()
                .expect("registered manifest must have valid resource types");
            for rt in rts {
                if !types.contains(&rt) {
                    types.push(rt);
                }
            }
        }
        types
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn valid_database_manifest() -> ServiceManifest {
        ServiceManifest {
            manifest_version: 1,
            service_id: "database-example".to_owned(),
            namespace: "database".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec!["database:instance".to_owned()],
            actions: vec![
                "database:CreateInstance".to_owned(),
                "database:ReadInstance".to_owned(),
                "database:DeleteInstance".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            region: None,
            availability_domain: None,
            controller: None,
            health: None,
        }
    }

    #[test]
    fn valid_manifest_passes_validation() {
        let m = valid_database_manifest();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn manifest_rejects_missing_service_id() {
        let mut m = valid_database_manifest();
        m.service_id = "".to_owned();
        assert_eq!(
            m.validate().unwrap_err(),
            ManifestError::InvalidField("service_id")
        );
    }

    #[test]
    fn manifest_rejects_resource_type_outside_namespace() {
        let mut m = valid_database_manifest();
        m.resource_types = vec!["compute:server".to_owned()];
        assert_eq!(
            m.validate().unwrap_err(),
            ManifestError::NamespaceMismatch {
                identifier: "compute:server".to_owned(),
                expected: "database".to_owned(),
            }
        );
    }

    #[test]
    fn manifest_rejects_action_outside_namespace() {
        let mut m = valid_database_manifest();
        m.actions = vec!["volume:CreateVolume".to_owned()];
        assert_eq!(
            m.validate().unwrap_err(),
            ManifestError::NamespaceMismatch {
                identifier: "volume:CreateVolume".to_owned(),
                expected: "database".to_owned(),
            }
        );
    }

    #[test]
    fn manifest_registry_rejects_duplicate_namespace() {
        let mut reg = ManifestRegistry::new();
        let m1 = valid_database_manifest();
        let mut m2 = valid_database_manifest();
        m2.service_id = "database-example-2".to_owned();

        assert!(reg.register(m1).is_ok());
        assert_eq!(
            reg.register(m2).unwrap_err(),
            ManifestError::DuplicateNamespace("database".to_owned())
        );
    }

    #[test]
    fn manifest_registry_accepts_distinct_services() {
        let mut reg = ManifestRegistry::new();
        let mut db = valid_database_manifest();
        db.service_id = "database-example".to_owned();
        db.namespace = "database".to_owned();

        assert!(reg.register(db).is_ok());
        assert_eq!(reg.len(), 1);
        assert!(reg.get("database-example").is_some());
        assert!(reg.get_by_namespace("database").is_some());
    }

    #[test]
    fn manifest_registry_has_resource_type_and_action() -> Result<(), KernelError> {
        let mut reg = ManifestRegistry::new();
        reg.register(valid_database_manifest())
            .map_err(|_| KernelError::InvalidServiceId("registration failed".to_owned()))?;

        assert!(reg.has_resource_type(&ResourceType::new("database", "instance")?));
        assert!(reg.has_action(&ActionId::new("database", "CreateInstance")?));
        assert!(!reg.has_action(&ActionId::new("database", "NonExistent")?));
        Ok(())
    }

    // ── Hardened registration tests ──────────────────────────────────────

    #[test]
    fn register_rejects_empty_resource_types() {
        let mut m = valid_database_manifest();
        m.resource_types = vec![];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(err.to_string().contains("resource_types"), "expected resource_types error, got {err}");
    }

    #[test]
    fn register_rejects_empty_actions() {
        let mut m = valid_database_manifest();
        m.actions = vec![];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(err.to_string().contains("actions"), "expected actions error, got {err}");
    }

    #[test]
    fn register_rejects_duplicate_service_id() {
        let mut reg = ManifestRegistry::new();
        reg.register(valid_database_manifest()).unwrap();
        let err = reg.register(valid_database_manifest()).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateServiceId(_)));
    }

    #[test]
    fn register_rejects_duplicate_resource_type_in_one_manifest() {
        let mut m = valid_database_manifest();
        m.resource_types = vec!["database:instance".to_owned(), "database:instance".to_owned()];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateResourceType(_)));
    }

    #[test]
    fn register_rejects_duplicate_action_in_one_manifest() {
        let mut m = valid_database_manifest();
        m.actions = vec![
            "database:CreateInstance".to_owned(),
            "database:CreateInstance".to_owned(),
        ];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateAction(_)));
    }

    #[test]
    fn register_rejects_resource_type_outside_manifest_namespace() {
        let mut m = valid_database_manifest();
        m.resource_types = vec!["network:port".to_owned()];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::NamespaceMismatch { .. }));
    }

    #[test]
    fn register_rejects_action_outside_manifest_namespace() {
        let mut m = valid_database_manifest();
        m.actions = vec!["compute:CreateServer".to_owned()];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::NamespaceMismatch { .. }));
    }

    #[test]
    fn register_rejects_resource_type_claiming_owned_namespace() {
        // A service cannot claim a resource type in a namespace it does not own.
        let mut reg = ManifestRegistry::new();
        reg.register(valid_database_manifest()).unwrap();
        let mut m2 = valid_database_manifest();
        m2.service_id = "database-example-2".to_owned();
        m2.namespace = "database-2".to_owned();
        m2.resource_types = vec!["database-2:instance".to_owned()];
        // Try to claim database:CreateInstance which is owned by database-example
        m2.actions = vec!["database:CreateInstance".to_owned()];
        let err = reg.register(m2).unwrap_err();
        assert!(
            matches!(err, ManifestError::NamespaceMismatch { .. }),
            "expected NamespaceMismatch for cross-namespace action, got {err}"
        );
    }

    #[test]
    fn register_rejects_same_namespace_second_service() {
        let mut reg = ManifestRegistry::new();
        reg.register(valid_database_manifest()).unwrap();
        let mut m2 = valid_database_manifest();
        m2.service_id = "database-example-2".to_owned();
        // Keep same namespace — should be rejected
        let err = reg.register(m2).unwrap_err();
        assert!(
            matches!(err, ManifestError::DuplicateNamespace(_)),
            "expected DuplicateNamespace, got {err}"
        );
    }

    #[test]
    fn register_validation_atomic_no_partial_state() {
        // Prove that a failing registration does not leave stale indexes.
        let mut reg = ManifestRegistry::new();
        reg.register(valid_database_manifest()).unwrap();
        assert_eq!(reg.len(), 1);

        // Attempt to register a service with invalid resource type that clashes
        let mut bad = valid_database_manifest();
        bad.service_id = "database-attempt".to_owned();
        bad.namespace = "database-attempt".to_owned();
        bad.resource_types = vec!["database:instance".to_owned()]; // clashes
        let _ = reg.register(bad);

        // State must remain unchanged: only 1 service
        assert_eq!(reg.len(), 1);
        assert!(reg.get("database-example").is_some());
        assert!(reg.get("database-attempt").is_none());
    }

    #[test]
    fn register_rejects_malformed_resource_type_format() {
        let mut m = valid_database_manifest();
        m.resource_types = vec!["not-namespaced".to_owned()];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidIdentifier(_, _)));
    }

    #[test]
    fn register_rejects_malformed_action_format() {
        let mut m = valid_database_manifest();
        m.actions = vec!["NoNamespace".to_owned()];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidIdentifier(_, _)));
    }

    #[test]
    fn register_rejects_empty_namespace() {
        let mut m = valid_database_manifest();
        m.namespace = "".to_owned();
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(err.to_string().contains("namespace"));
    }

    #[test]
    fn register_rejects_overly_long_namespace() {
        let mut m = valid_database_manifest();
        m.namespace = "a".repeat(65);
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(err.to_string().contains("namespace"));
    }

    #[test]
    fn seed_core_registers_all_services() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core();
        assert_eq!(reg.len(), 6);
        assert!(reg.get("identity").is_some());
        assert!(reg.get("image").is_some());
        assert!(reg.get("network").is_some());
        assert!(reg.get("compute").is_some());
        assert!(reg.get("placement").is_some());
        assert!(reg.get("volume").is_some());
    }

    #[test]
    fn seed_core_does_not_overwrite_explicit_registrations() {
        let mut reg = ManifestRegistry::new();
        let m = ServiceManifest {
            manifest_version: 1,
            service_id: "custom-compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1.0.0".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec!["compute:custom_resource".to_owned()],
            actions: vec!["compute:CustomAction".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            region: None,
            availability_domain: None,
            controller: None,
            health: None,
        };
        reg.register(m).unwrap();
        reg.seed_core();
        // compute namespace should still be held by custom-compute, not overridden
        assert!(reg.get("custom-compute").is_some());
        // core "compute" service should not be registered since namespace taken
        assert!(reg.get("compute").is_none());
    }
}
