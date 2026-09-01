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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::action::ActionId;
use crate::controller::{
    ControllerHealth, ControllerRegistration, ControllerSession, ControllerState,
};
use crate::error::KernelError;
use crate::resource::ResourceType;

/// Validates an identifier against a character-class predicate and length bounds.
///
/// `char_ok` returns true for characters that may appear AFTER the first position.
/// The first character must satisfy `char_ok(first_char)` (not just `is_ok`).
/// Empty strings are rejected.
fn valid_identifier(s: &str, max_len: usize, char_ok: impl Fn(char) -> bool) -> bool {
    if s.is_empty() || s.len() > max_len {
        return false;
    }
    let mut chars = s.chars();
    match chars.next() {
        None => return false,
        Some(first) => {
            if !char_ok(first) {
                return false;
            }
        }
    }
    for c in chars {
        if !char_ok(c) {
            return false;
        }
    }
    true
}

fn is_lower_digit(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}
fn is_service_id_char(c: char) -> bool {
    is_lower_digit(c) || c == '.' || c == '_' || c == '-'
}
fn is_namespace_char(c: char) -> bool {
    is_lower_digit(c) || c == '_' || c == '-'
}
fn is_capability_char(c: char) -> bool {
    is_lower_digit(c) || c == '.' || c == '_' || c == ':' || c == '-'
}
fn is_action_name_char(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
}

/// Service ownership mode in O3K Cloud OS.
///
/// Re-exported from registry for convenience; the semantic is the same as the
/// legacy enum but extended for P12-native external-controller mode.
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

/// A registered resource type in a service manifest — preserves all accepted
/// descriptor fields from service-manifest-v1.schema.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredResourceType {
    /// Canonical resource type (e.g. `"compute:server"`).
    pub resource_type: ResourceType,
    /// Schema version string (e.g. `"v1"`).
    pub schema_version: String,
    /// Optional collection URL name override.
    pub collection: Option<String>,
    /// Scope kind: tenant, system, or mixed.
    pub scope: ResourceScope,
    /// Explicit generic lifecycle operation to canonical ActionId mapping.
    /// An omitted operation is a supported capability omission, not invalid
    /// registration (read-only resources are valid).
    #[serde(default)]
    pub operations: std::collections::HashMap<String, ActionId>,
}

/// Resource ownership scope classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    Tenant,
    System,
    Mixed,
}

impl ResourceScope {
    /// Parses a scope string from the wire format.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "system" => Some(Self::System),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }
}

impl std::fmt::Display for ResourceScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tenant => write!(f, "tenant"),
            Self::System => write!(f, "system"),
            Self::Mixed => write!(f, "mixed"),
        }
    }
}

/// A dependency declared by a service manifest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceDependency {
    /// Dependency kind: service, resource_type, action, or capability.
    pub kind: DependencyKind,
    /// Dependency name/identifier.
    pub name: String,
    /// Whether the dependency is mandatory.
    pub required: bool,
}

/// Dependency kind classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Service,
    ResourceType,
    Action,
    Capability,
}

/// Controller declaration from a service manifest — manifest policy only,
/// separate from runtime session state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManifestController {
    /// Controller mode: `"in-process"` or `"external"`.
    pub mode: String,
    /// Controller protocol: `"in-process"` or `"grpc"`.
    pub protocol: String,
    /// Protocol version string.
    pub protocol_version: String,
    /// Service principal identity (required for external controllers).
    pub service_principal: Option<String>,
}

/// Service Manifest v1 — canonical native O3K service identity and capability
/// contract.
///
/// This manifest describes what the service IS, not where it runs or which
/// OpenStack compatibility surfaces it exposes. The latter is handled by
/// [`OpenStackCompatibilityProjection`].
///
/// All accepted wire descriptor fields are preserved in typed form — no lossy
/// normalization to strings/singletons.
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
    /// Ownership mode: `o3k-implemented` or `external-controller`.
    pub ownership: ServiceOwnership,
    /// Resource types with full descriptor metadata.
    pub resource_types: Vec<RegisteredResourceType>,
    /// Action IDs owned by this service (e.g. `"database:CreateInstance"`).
    pub actions: Vec<String>,
    /// Optional capability labels.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional service dependencies (preserves kind/name/required).
    #[serde(default)]
    pub dependencies: Vec<ServiceDependency>,
    /// Optional quota dimensions.
    #[serde(default)]
    pub quota_dimensions: Vec<QuotaDimension>,
    /// Optional region scopes (preserves all regions).
    #[serde(default)]
    pub regions: Vec<String>,
    /// Optional availability domain scopes (preserves all).
    #[serde(default)]
    pub availability_domains: Vec<String>,
    /// Controller manifest declaration (separate from runtime session).
    #[serde(default)]
    pub controller: Option<ManifestController>,
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
        // manifest_version
        if self.manifest_version != 1 {
            return Err(ManifestError::UnsupportedVersion(self.manifest_version));
        }

        // service_id: 1..128, pattern ^[a-z0-9][a-z0-9._-]*$
        if !valid_identifier(&self.service_id, 128, is_service_id_char) {
            return Err(ManifestError::InvalidField(
                "service_id (1..128, lower-case/digit/./_-)",
            ));
        }

        // namespace: 1..64, pattern ^[a-z0-9][a-z0-9_-]*$
        if !valid_identifier(&self.namespace, 64, is_namespace_char) {
            return Err(ManifestError::InvalidField(
                "namespace (1..64, lower-case/digit/_-)",
            ));
        }

        // service_version: 1..64
        if self.service_version.trim().is_empty() || self.service_version.len() > 64 {
            return Err(ManifestError::InvalidField("service_version (1..64)"));
        }

        // Ownership: ExternalHosted is not a valid P12 manifest ownership mode
        if self.ownership == ServiceOwnership::ExternalHosted {
            return Err(ManifestError::InvalidField(
                "ownership: ExternalHosted is not valid for service manifest v1",
            ));
        }

        // controller — REQUIRED for every P12 ServiceManifest v1
        let Some(ref ctrl) = self.controller else {
            return Err(ManifestError::InvalidField("controller is required"));
        };
        // Strict ownership/controller matrix:
        // O3kImplemented -> mode=in-process, protocol=in-process
        // ExternalController -> mode=external, protocol=grpc, service_principal non-empty
        // Everything else is rejected.
        match (ctrl.mode.as_str(), ctrl.protocol.as_str(), self.ownership) {
            ("in-process", "in-process", ServiceOwnership::O3kImplemented) => {}
            ("external", "grpc", ServiceOwnership::ExternalController) => {
                if ctrl
                    .service_principal
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    return Err(ManifestError::InvalidField(
                        "controller.service_principal required for external mode",
                    ));
                }
            }
            _ => {
                return Err(ManifestError::InvalidField(
                    "invalid controller mode/protocol/ownership combination",
                ));
            }
        }
        if ctrl.protocol_version.trim().is_empty() || ctrl.protocol_version.len() > 64 {
            return Err(ManifestError::InvalidField(
                "controller.protocol_version (1..64)",
            ));
        }
        if let Some(ref sp) = ctrl.service_principal
            && sp.len() > 255
        {
            return Err(ManifestError::InvalidField(
                "controller.service_principal max 255",
            ));
        }

        // resource_types: 1..128, unique, namespace match, schema_version 1..64
        if self.resource_types.is_empty() || self.resource_types.len() > 128 {
            return Err(ManifestError::InvalidField("resource_types (1..128)"));
        }
        for (i, rt) in self.resource_types.iter().enumerate() {
            let rt_str = rt.resource_type.to_string();
            if self.resource_types[..i].contains(rt) {
                return Err(ManifestError::DuplicateResourceType(rt_str));
            }
            if rt.resource_type.namespace() != self.namespace {
                return Err(ManifestError::NamespaceMismatch {
                    identifier: rt_str,
                    expected: self.namespace.clone(),
                });
            }
            if rt.schema_version.trim().is_empty() || rt.schema_version.len() > 64 {
                return Err(ManifestError::InvalidField(
                    "resource_types[].schema_version (1..64)",
                ));
            }
            if let Some(ref coll) = rt.collection
                && (coll.trim().is_empty()
                    || coll.len() > 128
                    || !valid_identifier(coll, 128, is_namespace_char))
            {
                return Err(ManifestError::InvalidField(
                    "resource_types[].collection (1..128)",
                ));
            }
            let collection = rt.collection.as_deref().unwrap_or(rt.resource_type.name());
            if ["services", "resource-types", "identity", "operations"].contains(&collection) {
                return Err(ManifestError::InvalidField(
                    "resource_types[].collection collides with native control endpoint",
                ));
            }
            if self.resource_types[..i].iter().any(|previous| {
                previous
                    .collection
                    .as_deref()
                    .unwrap_or(previous.resource_type.name())
                    == collection
            }) {
                return Err(ManifestError::InvalidField(
                    "resource_types[].collection is ambiguous",
                ));
            }
        }

        // actions: 1..256, unique, accepted syntax
        if self.actions.is_empty() || self.actions.len() > 256 {
            return Err(ManifestError::InvalidField("actions (1..256)"));
        }
        for (i, act) in self.actions.iter().enumerate() {
            if self.actions[..i].contains(act) {
                return Err(ManifestError::DuplicateAction(act.clone()));
            }
            let Some((ns, act_name)) = act.split_once(':') else {
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
            if act.len() > 128 {
                return Err(ManifestError::InvalidField("action (max 128)"));
            }
            // Action name must be PascalCase: uppercase first char, followed by
            // alphanumeric/underscore.
            if act_name.is_empty()
                || !act_name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                || !act_name.chars().all(is_action_name_char)
            {
                return Err(ManifestError::InvalidField(
                    "action name must be PascalCase",
                ));
            }
        }

        let declared_actions: HashSet<ActionId> = self
            .actions
            .iter()
            .filter_map(|action| ActionId::parse(action).ok())
            .collect();
        for resource in &self.resource_types {
            for (operation, action) in &resource.operations {
                if !["list", "show", "create", "update", "delete"].contains(&operation.as_str()) {
                    return Err(ManifestError::InvalidField(
                        "resource_types[].operations key",
                    ));
                }
                if action.namespace() != self.namespace || !declared_actions.contains(action) {
                    return Err(ManifestError::InvalidField(
                        "resource_types[].operations action must be declared by service",
                    ));
                }
            }
        }

        // capabilities: max 128, unique, each 1..128
        if self.capabilities.len() > 128 {
            return Err(ManifestError::InvalidField("capabilities (max 128)"));
        }
        for (i, cap) in self.capabilities.iter().enumerate() {
            if self.capabilities[..i].contains(cap) {
                return Err(ManifestError::InvalidField("duplicate capability"));
            }
            if cap.trim().is_empty()
                || cap.len() > 128
                || !valid_identifier(cap, 128, is_capability_char)
            {
                return Err(ManifestError::InvalidField("capability (1..128)"));
            }
        }

        // dependencies: max 128, name 1..128
        if self.dependencies.len() > 128 {
            return Err(ManifestError::InvalidField("dependencies (max 128)"));
        }
        for dep in &self.dependencies {
            if dep.name.trim().is_empty() || dep.name.len() > 128 {
                return Err(ManifestError::InvalidField("dependency name (1..128)"));
            }
        }

        // quota_dimensions: max 64, key 1..128, unit 1..64, scope tenant/system
        if self.quota_dimensions.len() > 64 {
            return Err(ManifestError::InvalidField("quota_dimensions (max 64)"));
        }
        for (i, qd) in self.quota_dimensions.iter().enumerate() {
            if self.quota_dimensions[..i].iter().any(|o| o.key == qd.key) {
                return Err(ManifestError::DuplicateQuotaDimension(qd.key.clone()));
            }
            if qd.key.trim().is_empty()
                || qd.key.len() > 128
                || !valid_identifier(&qd.key, 128, is_capability_char)
            {
                return Err(ManifestError::InvalidField(
                    "quota_dimensions[].key (1..128)",
                ));
            }
            if qd.unit.trim().is_empty() || qd.unit.len() > 64 {
                return Err(ManifestError::InvalidField(
                    "quota_dimensions[].unit (1..64)",
                ));
            }
            match qd.scope.as_str() {
                "tenant" | "system" => {}
                _ => {
                    return Err(ManifestError::InvalidField(
                        "quota_dimensions[].scope: expected 'tenant' or 'system'",
                    ));
                }
            }
        }

        // regions: max 128, unique, entries 1..128
        if self.regions.len() > 128 {
            return Err(ManifestError::InvalidField("regions (max 128)"));
        }
        for (i, region) in self.regions.iter().enumerate() {
            if self.regions[..i].contains(region) {
                return Err(ManifestError::InvalidField("duplicate region"));
            }
            if region.trim().is_empty() || region.len() > 128 {
                return Err(ManifestError::InvalidField("regions[] (1..128)"));
            }
        }

        // availability_domains: max 256, unique, entries 1..128
        if self.availability_domains.len() > 256 {
            return Err(ManifestError::InvalidField(
                "availability_domains (max 256)",
            ));
        }
        for (i, az) in self.availability_domains.iter().enumerate() {
            if self.availability_domains[..i].contains(az) {
                return Err(ManifestError::InvalidField("duplicate availability_domain"));
            }
            if az.trim().is_empty() || az.len() > 128 {
                return Err(ManifestError::InvalidField(
                    "availability_domains[] (1..128)",
                ));
            }
        }

        Ok(())
    }

    /// Returns the canonical `ResourceType` values for all registered resource types.
    pub fn parsed_resource_types(&self) -> Result<Vec<ResourceType>, KernelError> {
        Ok(self
            .resource_types
            .iter()
            .map(|rt| rt.resource_type.clone())
            .collect())
    }

    /// Returns the parsed actions as canonical `ActionId` values.
    pub fn parsed_actions(&self) -> Result<Vec<ActionId>, KernelError> {
        self.actions.iter().map(|a| ActionId::parse(a)).collect()
    }
}

// ── Wire DTO: ServiceManifestV1 ─────────────────────────────────────────
//
// Exact wire representation matching contracts/service-manifest-v1.schema.json
// (x-o3k-status: accepted). Converted to the normalized ServiceManifest for
// registry storage.

/// Wire DTO for `service-manifest-v1.schema.json` — exact schema conformance.
///
/// This type exists only at the API/protocol boundary. The registry stores
/// the normalized [`ServiceManifest`] after validation and conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifestV1 {
    /// Constant: `"o3k.io/service-manifest/v1"`
    pub manifest_version: String,
    /// Stable O3K service identifier (e.g. `"database-example"`).
    pub service_id: String,
    /// Canonical service namespace (e.g. `"database"`).
    pub namespace: String,
    /// Human-readable service version (e.g. `"0.1.0"`).
    pub service_version: String,
    /// Ownership mode: `"o3k-implemented"` or `"external-controller"`.
    pub ownership_mode: String,
    /// Resource type descriptors.
    pub resource_types: Vec<ResourceTypeDescriptor>,
    /// Action identifiers (e.g. `"database:CreateInstance"`).
    pub actions: Vec<String>,
    /// Optional capability labels.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional dependency descriptors.
    #[serde(default)]
    pub dependencies: Vec<DependencyDescriptor>,
    /// Optional quota dimension descriptors.
    #[serde(default)]
    pub quota_dimensions: Vec<QuotaDimensionDescriptor>,
    /// Optional region list.
    #[serde(default)]
    pub regions: Vec<String>,
    /// Optional availability domain list.
    #[serde(default)]
    pub availability_domains: Vec<String>,
    /// Controller binding descriptor.
    pub controller: ControllerDescriptor,
}

/// Resource type descriptor in a service manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceTypeDescriptor {
    /// Canonical resource type (e.g. `"database:instance"`).
    #[serde(rename = "type")]
    pub type_: String,
    /// Schema version string for this resource type.
    pub schema_version: String,
    /// Optional collection URL name override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Scope kind: `"tenant"`, `"system"`, or `"mixed"`.
    #[serde(default = "default_tenant_scope")]
    pub scope: String,
    /// Explicit lifecycle operation to canonical ActionId mapping.
    #[serde(default)]
    pub operations: std::collections::HashMap<String, String>,
}

fn default_tenant_scope() -> String {
    "tenant".to_owned()
}

/// Dependency descriptor in a service manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyDescriptor {
    /// Dependency kind: `"service"`, `"resource_type"`, `"action"`, `"capability"`.
    pub kind: String,
    /// Dependency name.
    pub name: String,
    /// Whether the dependency is mandatory.
    pub required: bool,
}

/// Quota dimension descriptor in a service manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaDimensionDescriptor {
    /// Dimension key (e.g. `"instances"`).
    pub key: String,
    /// Human-readable unit (e.g. `"count"`, `"bytes"`).
    pub unit: String,
    /// Scope kind: `"tenant"` or `"system"`.
    pub scope: String,
}

/// Controller binding descriptor in a service manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerDescriptor {
    /// Controller mode: `"in-process"` or `"external"`.
    pub mode: String,
    /// Controller protocol: `"in-process"` or `"grpc"`.
    pub protocol: String,
    /// Protocol version string.
    pub protocol_version: String,
    /// Service principal identity (required for external controllers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_principal: Option<String>,
}

impl TryFrom<ServiceManifestV1> for ServiceManifest {
    type Error = ManifestError;

    /// Converts a wire `ServiceManifestV1` into the normalized internal
    /// [`ServiceManifest`], preserving all accepted descriptor fields.
    fn try_from(wire: ServiceManifestV1) -> Result<Self, Self::Error> {
        // Validate manifest_version constant
        if wire.manifest_version != "o3k.io/service-manifest/v1" {
            return Err(ManifestError::InvalidField("manifest_version"));
        }

        // Validate ownership_mode — map exactly to accepted vocabulary
        let ownership = match wire.ownership_mode.as_str() {
            "o3k-implemented" => ServiceOwnership::O3kImplemented,
            "external-controller" => ServiceOwnership::ExternalController,
            _ => {
                return Err(ManifestError::InvalidField(
                    "ownership_mode: expected 'o3k-implemented' or 'external-controller'",
                ));
            }
        };

        // Convert resource_type descriptors — preserve ALL fields
        let resource_types: Vec<RegisteredResourceType> = wire
            .resource_types
            .into_iter()
            .map(|rt| {
                let (ns, name) = rt.type_.split_once(':').ok_or_else(|| {
                    ManifestError::InvalidIdentifier(
                        rt.type_.clone(),
                        "resource type must be namespace:name".to_owned(),
                    )
                })?;
                let resource_type = ResourceType::new(ns, name).map_err(|e| {
                    ManifestError::InvalidIdentifier(rt.type_.clone(), e.to_string())
                })?;
                // Fail closed: resource scope must be a valid accepted value
                let scope = ResourceScope::parse(&rt.scope)
                    .ok_or(ManifestError::InvalidField("resource_types[].scope"))?;
                // schema_version must be non-empty
                if rt.schema_version.trim().is_empty() {
                    return Err(ManifestError::InvalidField(
                        "resource_types[].schema_version",
                    ));
                }
                if rt.schema_version.len() > 64 {
                    return Err(ManifestError::InvalidField(
                        "resource_types[].schema_version (max 64)",
                    ));
                }
                Ok(RegisteredResourceType {
                    resource_type,
                    schema_version: rt.schema_version,
                    collection: rt.collection,
                    scope,
                    operations: rt
                        .operations
                        .into_iter()
                        .map(|(operation, action)| {
                            let action = ActionId::parse(&action).map_err(|e| {
                                ManifestError::InvalidIdentifier(action.clone(), e.to_string())
                            })?;
                            Ok((operation, action))
                        })
                        .collect::<Result<_, ManifestError>>()?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Convert controller declaration — validate mode/protocol values
        let mode = wire.controller.mode;
        let protocol = wire.controller.protocol;
        match mode.as_str() {
            "in-process" | "external" => {}
            _ => {
                return Err(ManifestError::InvalidField(
                    "controller.mode: expected 'in-process' or 'external'",
                ));
            }
        }
        match protocol.as_str() {
            "in-process" | "grpc" => {}
            _ => {
                return Err(ManifestError::InvalidField(
                    "controller.protocol: expected 'in-process' or 'grpc'",
                ));
            }
        }
        // External controller requires explicit service_principal
        if mode == "external"
            && wire
                .controller
                .service_principal
                .as_deref()
                .unwrap_or("")
                .is_empty()
        {
            return Err(ManifestError::InvalidField(
                "controller.service_principal required for external mode",
            ));
        }
        let controller = Some(ManifestController {
            mode,
            protocol,
            protocol_version: wire.controller.protocol_version,
            service_principal: wire.controller.service_principal,
        });

        // Convert dependencies — fail closed on unknown kind
        let dependencies: Vec<ServiceDependency> = wire
            .dependencies
            .into_iter()
            .map(|d| {
                let kind = match d.kind.as_str() {
                    "service" => DependencyKind::Service,
                    "resource_type" => DependencyKind::ResourceType,
                    "action" => DependencyKind::Action,
                    "capability" => DependencyKind::Capability,
                    _ => {
                        return Err(ManifestError::InvalidField("dependencies[].kind"));
                    }
                };
                Ok(ServiceDependency {
                    kind,
                    name: d.name,
                    required: d.required,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Convert quota_dimensions — validate scope is tenant/system
        let quota_dimensions: Vec<QuotaDimension> = wire
            .quota_dimensions
            .into_iter()
            .map(|qd| {
                if qd.key.trim().is_empty() || qd.key.len() > 128 {
                    return Err(ManifestError::InvalidField("quota_dimensions[].key"));
                }
                match qd.scope.as_str() {
                    "tenant" | "system" => {}
                    _ => {
                        return Err(ManifestError::InvalidField(
                            "quota_dimensions[].scope: expected 'tenant' or 'system'",
                        ));
                    }
                }
                Ok(QuotaDimension {
                    key: qd.key,
                    unit: qd.unit,
                    scope: qd.scope,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Validate regions/AZ entries
        for region in &wire.regions {
            if region.trim().is_empty() || region.len() > 128 {
                return Err(ManifestError::InvalidField("regions[]"));
            }
        }
        for az in &wire.availability_domains {
            if az.trim().is_empty() || az.len() > 128 {
                return Err(ManifestError::InvalidField("availability_domains[]"));
            }
        }

        let manifest = ServiceManifest {
            manifest_version: 1,
            service_id: wire.service_id,
            namespace: wire.namespace,
            service_version: wire.service_version,
            ownership,
            resource_types,
            actions: wire.actions,
            capabilities: wire.capabilities,
            dependencies,
            quota_dimensions,
            regions: wire.regions,
            availability_domains: wire.availability_domains,
            controller,
            health: None,
        };
        // Normalized validate() is the final structural authority.
        manifest.validate()?;
        Ok(manifest)
    }
}

// ── Wire DTO: NativeResourceV1 ──────────────────────────────────────────
//
// Exact wire representation matching contracts/native-resource-envelope-v1.schema.json
// (x-o3k-status: accepted).

/// Wire DTO for `native-resource-envelope-v1.schema.json` — exact schema conformance.
///
/// The normalized internal form is [`ResourceEnvelope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeResourceV1 {
    /// Constant: `"o3k.io/v1"`
    pub api_version: String,
    /// Canonical resource type string (e.g. `"compute:server"`).
    pub kind: String,
    /// Common resource metadata.
    pub metadata: NativeResourceMetaV1,
    /// Service-owned desired-state payload.
    pub spec: serde_json::Value,
    /// Service-owned observed/status payload (required by schema).
    pub status: serde_json::Value,
}

/// Wire metadata for [`NativeResourceV1`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeResourceMetaV1 {
    /// Opaque canonical resource ID.
    pub id: String,
    /// Durable owner/security scope string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_scope: Option<String>,
    /// Monotonic desired-state generation.
    #[serde(default)]
    pub generation: i64,
    /// Optional region identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Optional availability domain identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_domain: Option<String>,
    /// RFC3339 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC3339 last-update timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Optional free-form labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::HashMap<String, String>>,
    /// Optional free-form annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::HashMap<String, String>>,
}

// ── Wire DTO: OpenStackProjectionV1 ─────────────────────────────────────
//
// Exact wire representation matching contracts/openstack-compatibility-projection-v1.schema.json
// (x-o3k-status: accepted).

/// Wire DTO for `openstack-compatibility-projection-v1.schema.json` — exact schema conformance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenStackProjectionV1 {
    /// Constant: `"o3k.io/openstack-projection/v1"`
    pub projection_version: String,
    /// O3K service_id this projection maps FROM.
    pub service_id: String,
    /// OpenStack service_type (e.g. `"compute"`, `"volumev3"`).
    pub service_type: String,
    /// Optional service name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Whether this projection is currently enabled/advertised.
    pub enabled: bool,
    /// Exposed API surfaces.
    #[serde(default)]
    pub api_surfaces: Vec<OpenStackApiSurfaceV1>,
    /// Catalog endpoint templates.
    #[serde(default)]
    pub endpoints: Vec<OpenStackEndpointV1>,
    /// Optional capability tags.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional evidence profile reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_profile: Option<String>,
}

/// API surface in an OpenStack compatibility projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenStackApiSurfaceV1 {
    /// Human-readable name.
    pub name: String,
    /// URL prefix/mount point (must start with `/`).
    pub prefix: String,
    /// Version string.
    pub version: String,
    /// Minimum microversion, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_microversion: Option<String>,
    /// Maximum microversion, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_microversion: Option<String>,
    /// Whether this surface is enabled.
    pub enabled: bool,
}

/// Catalog endpoint in an OpenStack compatibility projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenStackEndpointV1 {
    /// Interface label: `"public"`, `"internal"`, or `"admin"`.
    pub interface: String,
    /// Region identifier.
    pub region: String,
    /// URL template.
    pub url_template: String,
    /// Whether this endpoint is enabled.
    pub enabled: bool,
}

impl TryFrom<OpenStackProjectionV1> for OpenStackCompatibilityProjection {
    type Error = ManifestError;

    fn try_from(wire: OpenStackProjectionV1) -> Result<Self, Self::Error> {
        // Validate projection_version constant
        if wire.projection_version != "o3k.io/openstack-projection/v1" {
            return Err(ManifestError::InvalidField("projection_version"));
        }
        // service_id 1..128
        if wire.service_id.trim().is_empty() || wire.service_id.len() > 128 {
            return Err(ManifestError::InvalidField("service_id (1..128)"));
        }
        // service_type 1..128
        if wire.service_type.trim().is_empty() || wire.service_type.len() > 128 {
            return Err(ManifestError::InvalidField("service_type (1..128)"));
        }
        // service_name <=128
        if let Some(ref name) = wire.service_name
            && name.len() > 128
        {
            return Err(ManifestError::InvalidField("service_name (max 128)"));
        }
        // api_surfaces <=32
        if wire.api_surfaces.len() > 32 {
            return Err(ManifestError::InvalidField("api_surfaces (max 32)"));
        }
        for api in &wire.api_surfaces {
            // name 1..160
            if api.name.trim().is_empty() || api.name.len() > 160 {
                return Err(ManifestError::InvalidField("api_surfaces[].name (1..160)"));
            }
            // prefix starts "/" and <=255
            if !api.prefix.starts_with('/') || api.prefix.len() > 255 {
                return Err(ManifestError::InvalidField(
                    "api_surfaces[].prefix must start with / (max 255)",
                ));
            }
            // version 1..64
            if api.version.trim().is_empty() || api.version.len() > 64 {
                return Err(ManifestError::InvalidField(
                    "api_surfaces[].version (1..64)",
                ));
            }
            // min/max microversion <=32
            if let Some(ref mv) = api.min_microversion
                && mv.len() > 32
            {
                return Err(ManifestError::InvalidField(
                    "api_surfaces[].min_microversion (max 32)",
                ));
            }
            if let Some(ref mv) = api.max_microversion
                && mv.len() > 32
            {
                return Err(ManifestError::InvalidField(
                    "api_surfaces[].max_microversion (max 32)",
                ));
            }
        }
        // endpoints <=384
        if wire.endpoints.len() > 384 {
            return Err(ManifestError::InvalidField("endpoints (max 384)"));
        }
        for ep in &wire.endpoints {
            // interface public/internal/admin
            match ep.interface.as_str() {
                "public" | "internal" | "admin" => {}
                _ => {
                    return Err(ManifestError::InvalidField(
                        "endpoints[].interface: expected 'public', 'internal', or 'admin'",
                    ));
                }
            }
            // region 1..128
            if ep.region.trim().is_empty() || ep.region.len() > 128 {
                return Err(ManifestError::InvalidField("endpoints[].region (1..128)"));
            }
            // url_template 1..2048 (no prefix restriction per accepted schema)
            if ep.url_template.trim().is_empty() || ep.url_template.len() > 2048 {
                return Err(ManifestError::InvalidField(
                    "endpoints[].url_template (1..2048)",
                ));
            }
        }
        // capabilities <=256, unique, each 1..128
        if wire.capabilities.len() > 256 {
            return Err(ManifestError::InvalidField("capabilities (max 256)"));
        }
        for (i, cap) in wire.capabilities.iter().enumerate() {
            if wire.capabilities[..i].contains(cap) {
                return Err(ManifestError::InvalidField("duplicate capability"));
            }
            if cap.trim().is_empty() || cap.len() > 128 {
                return Err(ManifestError::InvalidField("capability (1..128)"));
            }
        }
        // evidence_profile <=128
        if let Some(ref ep) = wire.evidence_profile
            && ep.len() > 128
        {
            return Err(ManifestError::InvalidField("evidence_profile (max 128)"));
        }
        Ok(OpenStackCompatibilityProjection {
            service_id: wire.service_id,
            service_type: wire.service_type,
            service_name: wire.service_name,
            enabled: wire.enabled,
            api_surfaces: wire
                .api_surfaces
                .into_iter()
                .map(|s| OpenStackApiSurface {
                    name: s.name,
                    prefix: s.prefix,
                    version: s.version,
                    min_microversion: s.min_microversion,
                    max_microversion: s.max_microversion,
                    enabled: s.enabled,
                })
                .collect(),
            endpoints: wire
                .endpoints
                .into_iter()
                .map(|e| OpenStackEndpointTemplate {
                    interface: e.interface,
                    region: e.region,
                    url_template: e.url_template,
                    enabled: e.enabled,
                })
                .collect(),
            capabilities: wire.capabilities,
            evidence_profile: wire.evidence_profile,
        })
    }
}

/// OpenStack Compatibility Projection v1 — expanded for accepted wire contract.
///
/// Preserves all fields from openstack-compatibility-projection-v1.schema.json.
/// A native-only service (e.g. `database`) has no projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackCompatibilityProjection {
    /// O3K service_id this projection maps FROM.
    pub service_id: String,
    /// OpenStack service_type (e.g. `"compute"`, `"volumev3"`).
    pub service_type: String,
    /// Optional OpenStack service name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Whether this projection is currently enabled/advertised.
    pub enabled: bool,
    /// Exposed API surfaces (with microversion and enabled state).
    #[serde(default)]
    pub api_surfaces: Vec<OpenStackApiSurface>,
    /// Catalog endpoint templates (with enabled state).
    #[serde(default)]
    pub endpoints: Vec<OpenStackEndpointTemplate>,
    /// Optional capability tags.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional evidence profile reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_profile: Option<String>,
}

/// OpenStack API surface description (expanded for projection v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackApiSurface {
    /// Human-readable name.
    pub name: String,
    /// URL prefix/mount point.
    pub prefix: String,
    /// Version string.
    pub version: String,
    /// Minimum microversion, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_microversion: Option<String>,
    /// Maximum microversion, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_microversion: Option<String>,
    /// Whether this surface is enabled.
    pub enabled: bool,
}

/// OpenStack catalog endpoint template (expanded for projection v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStackEndpointTemplate {
    /// Interface label.
    pub interface: String,
    /// Region identifier.
    pub region: String,
    /// URL template.
    pub url_template: String,
    /// Whether this endpoint is enabled.
    pub enabled: bool,
}

/// Errors produced during manifest validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(u32),
    #[error("unsupported wire manifest version: {0}")]
    UnsupportedWireVersion(String),
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

    /// Load and register one accepted v1 manifest from a JSON artifact.
    /// Runtime callers use this generic path for every external service; the
    /// registry never dispatches on a service-specific namespace.
    pub fn register_json_file(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), ManifestError> {
        let bytes =
            std::fs::read(path).map_err(|_| ManifestError::InvalidField("manifest file"))?;
        let wire: ServiceManifestV1 = serde_json::from_slice(&bytes)
            .map_err(|_| ManifestError::InvalidField("manifest JSON"))?;
        self.register(wire.try_into()?)
    }

    /// Load all JSON files in a configured manifest directory. Files are
    /// sorted to make registration and conflict diagnostics deterministic.
    pub fn register_json_directory(
        &mut self,
        directory: impl AsRef<std::path::Path>,
    ) -> Result<usize, ManifestError> {
        let mut paths = std::fs::read_dir(directory)
            .map_err(|_| ManifestError::InvalidField("manifest directory"))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|_| ManifestError::InvalidField("manifest directory entry"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in &paths {
            self.register_json_file(path)?;
        }
        Ok(paths.len())
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
        if let Some(existing) = self.controllers.get(service_id)
            && let Some(ref existing_session) = existing.session
            && session.session_generation <= existing_session.session_generation
        {
            return Err(ManifestError::InvalidField(
                "session_generation must increase",
            ));
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

    /// Registers the runtime readiness of a first-party in-process service.
    ///
    /// In-process services have no transport session to register, but they
    /// still participate in the same lifecycle state used by native
    /// discovery and mutation dispatch.  The composition root supplies the
    /// result of the service's real dependency checks; an unavailable
    /// dependency therefore remains fail-closed as `NotReady`.
    pub fn register_in_process_controller(
        &mut self,
        service_id: &str,
        ready: bool,
        detail: Option<String>,
    ) -> Result<(), ManifestError> {
        let manifest = self
            .manifests
            .get(service_id)
            .ok_or(ManifestError::InvalidField("service_id"))?;
        let controller = manifest
            .controller
            .as_ref()
            .ok_or(ManifestError::InvalidField("controller"))?;
        if manifest.ownership != ServiceOwnership::O3kImplemented
            || controller.mode != "in-process"
            || controller.protocol != "in-process"
        {
            return Err(ManifestError::InvalidField(
                "service is not an in-process first-party service",
            ));
        }
        if self
            .controllers
            .get(service_id)
            .is_some_and(|registration| registration.session.is_some())
        {
            return Err(ManifestError::InvalidField(
                "external controller session already registered",
            ));
        }
        let (major, minor) = controller
            .protocol_version
            .split_once('.')
            .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
            .ok_or(ManifestError::InvalidField("controller.protocol_version"))?;

        self.controllers.insert(
            service_id.to_owned(),
            ControllerRegistration {
                service_id: service_id.to_owned(),
                namespace: manifest.namespace.clone(),
                session: None,
                state: if ready {
                    ControllerState::Ready
                } else {
                    ControllerState::NotReady
                },
                health: Some(ControllerHealth {
                    healthy: ready,
                    detail,
                    protocol_version: crate::controller::ProtocolVersion::new(major, minor),
                }),
            },
        );
        Ok(())
    }

    /// Transitions a controller from `Declared` to `Ready` after health checks
    /// pass. This is a separate step because SPEC-0031 requires protocol
    /// negotiation, manifest verification and health confirmation before Ready.
    pub fn activate_controller(&mut self, service_id: &str) -> Result<(), ManifestError> {
        let reg = self
            .controllers
            .get_mut(service_id)
            .ok_or(ManifestError::InvalidField("service_id"))?;
        if reg.state != ControllerState::Declared {
            return Err(ManifestError::InvalidField(
                "controller must be Declared to activate",
            ));
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
        // 1. Validate manifest structure (all schema-compliant field bounds)
        manifest.validate()?;

        // 2. Reject duplicate service ID
        if self.manifests.contains_key(&manifest.service_id) {
            return Err(ManifestError::DuplicateServiceId(
                manifest.service_id.clone(),
            ));
        }

        // 3. Reject duplicate namespace
        if self.by_namespace.contains_key(&manifest.namespace) {
            return Err(ManifestError::DuplicateNamespace(
                manifest.namespace.clone(),
            ));
        }

        // 4. Check duplicate resource types against existing registrations
        let parsed_rts: Vec<ResourceType> = manifest
            .resource_types
            .iter()
            .map(|rt| rt.resource_type.clone())
            .collect();
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

        // 5. Check duplicate actions against existing registrations
        let parsed_actions: Vec<ActionId> = manifest
            .actions
            .iter()
            .map(|a| ActionId::parse(a))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ManifestError::InvalidIdentifier("action".to_owned(), e.to_string()))?;
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

        // 6. ALL CHECKS PASSED — atomically apply (HashMap inserts are infallible)
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

    /// Returns a mutable reference to a registered manifest by service ID.
    #[must_use]
    pub fn get_mut(&mut self, service_id: &str) -> Option<&mut ServiceManifest> {
        self.manifests.get_mut(service_id)
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

    /// Refuse to forget a registered service authority.
    ///
    /// P12 has no ownership/in-flight-operation retirement protocol. A
    /// manifest therefore cannot be removed while the registry is the source
    /// of service authority; controller disconnects use `remove_controller`
    /// and retain the manifest for reconciliation.
    pub fn remove(&mut self, service_id: &str) -> Result<(), ManifestError> {
        if self.manifests.contains_key(service_id) {
            return Err(ManifestError::InvalidField(
                "service authority removal is not supported",
            ));
        }
        Ok(())
    }

    /// Checks whether a resource type is registered by any active service.
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn has_resource_type(&self, resource_type: &ResourceType) -> bool {
        self.manifests.values().any(|m| {
            m.parsed_resource_types()
                .expect("registered manifest must have valid resource types")
                .contains(resource_type)
        })
    }

    /// Checks whether an action is registered by any active service.
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn has_action(&self, action: &ActionId) -> bool {
        self.manifests.values().any(|m| {
            m.parsed_actions()
                .expect("registered manifest must have valid actions")
                .contains(action)
        })
    }

    /// Seeds the registry with core P0-P11 services for the native TestLab
    /// profile. Only services whose canonical actions exist in the accepted
    /// `contracts/cloud-kernel-actions.yaml` are included.
    ///
    /// Placement remains deferred; native Network and Volume reads are
    /// advertised because their canonical actions are now implemented.
    ///
    /// Returns an error if a built-in core manifest fails registration — such
    /// a failure is an invariant violation that must propagate through daemon
    /// startup.
    pub fn seed_core(&mut self) -> Result<(), ManifestError> {
        let core_manifests: Vec<ServiceManifest> = vec![
            ServiceManifest {
                manifest_version: 1,
                service_id: "identity".to_owned(),
                namespace: "identity".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("identity", "token"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::new(),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("identity", "project"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::new(),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("identity", "user"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::System,
                        operations: std::collections::HashMap::new(),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("identity", "role"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::System,
                        operations: std::collections::HashMap::new(),
                    },
                ],
                // Accepted in contracts/cloud-kernel-actions.yaml
                actions: vec![
                    "identity:IssueToken".to_owned(),
                    "identity:ValidateToken".to_owned(),
                    "identity:RevokeToken".to_owned(),
                ],
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: Some(ManifestController {
                    mode: "in-process".to_owned(),
                    protocol: "in-process".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    service_principal: None,
                }),
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "image".to_owned(),
                namespace: "image".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![RegisteredResourceType {
                    resource_type: ResourceType::new_unchecked("image", "image"),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: ResourceScope::Tenant,
                    operations: std::collections::HashMap::new(),
                }],
                // Accepted in contracts/cloud-kernel-actions.yaml
                actions: vec![
                    "image:ListImages".to_owned(),
                    "image:CreateImage".to_owned(),
                    "image:ReadImage".to_owned(),
                    "image:DeleteImage".to_owned(),
                    "image:UploadImage".to_owned(),
                    "image:DownloadImage".to_owned(),
                ],
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: Some(ManifestController {
                    mode: "in-process".to_owned(),
                    protocol: "in-process".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    service_principal: None,
                }),
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "compute".to_owned(),
                namespace: "compute".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("compute", "server"),
                        schema_version: "v1".to_owned(),
                        collection: Some("servers".to_owned()),
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::from([
                            (
                                "list".to_owned(),
                                ActionId::new_unchecked("compute", "ListServers"),
                            ),
                            (
                                "show".to_owned(),
                                ActionId::new_unchecked("compute", "ReadServer"),
                            ),
                            (
                                "create".to_owned(),
                                ActionId::new_unchecked("compute", "CreateServer"),
                            ),
                            (
                                "delete".to_owned(),
                                ActionId::new_unchecked("compute", "DeleteServer"),
                            ),
                        ]),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("compute", "flavor"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::new(),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("compute", "keypair"),
                        schema_version: "v1".to_owned(),
                        collection: None,
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::new(),
                    },
                ],
                // Accepted in contracts/cloud-kernel-actions.yaml
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
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: Some(ManifestController {
                    mode: "in-process".to_owned(),
                    protocol: "in-process".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    service_principal: None,
                }),
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "network".to_owned(),
                namespace: "network".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("network", "address_realm"),
                        schema_version: "v1".to_owned(),
                        collection: Some("address-realms".to_owned()),
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::from([
                            (
                                "list".to_owned(),
                                ActionId::new_unchecked("network", "ListAddressRealms"),
                            ),
                            (
                                "show".to_owned(),
                                ActionId::new_unchecked("network", "ReadAddressRealm"),
                            ),
                        ]),
                    },
                    RegisteredResourceType {
                        resource_type: ResourceType::new_unchecked("network", "network"),
                        schema_version: "v1".to_owned(),
                        collection: Some("networks".to_owned()),
                        scope: ResourceScope::Tenant,
                        operations: std::collections::HashMap::from([
                            (
                                "list".to_owned(),
                                ActionId::new_unchecked("network", "ListNetworks"),
                            ),
                            (
                                "show".to_owned(),
                                ActionId::new_unchecked("network", "ReadNetwork"),
                            ),
                            (
                                "update".to_owned(),
                                ActionId::new_unchecked("network", "UpdateNetwork"),
                            ),
                            (
                                "create".to_owned(),
                                ActionId::new_unchecked("network", "CreateNetwork"),
                            ),
                            (
                                "delete".to_owned(),
                                ActionId::new_unchecked("network", "DeleteNetwork"),
                            ),
                        ]),
                    },
                ],
                actions: vec![
                    "network:ListAddressRealms".to_owned(),
                    "network:ReadAddressRealm".to_owned(),
                    "network:ListNetworks".to_owned(),
                    "network:CreateNetwork".to_owned(),
                    "network:ReadNetwork".to_owned(),
                    "network:UpdateNetwork".to_owned(),
                    "network:DeleteNetwork".to_owned(),
                ],
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: Some(ManifestController {
                    mode: "in-process".to_owned(),
                    protocol: "in-process".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    service_principal: None,
                }),
                health: None,
            },
            ServiceManifest {
                manifest_version: 1,
                service_id: "volume".to_owned(),
                namespace: "volume".to_owned(),
                service_version: "0.4.0".to_owned(),
                ownership: ServiceOwnership::O3kImplemented,
                resource_types: vec![RegisteredResourceType {
                    resource_type: ResourceType::new_unchecked("volume", "volume"),
                    schema_version: "v1".to_owned(),
                    collection: Some("volumes".to_owned()),
                    scope: ResourceScope::Tenant,
                    operations: std::collections::HashMap::from([
                        (
                            "list".to_owned(),
                            ActionId::new_unchecked("volume", "ListVolumes"),
                        ),
                        (
                            "show".to_owned(),
                            ActionId::new_unchecked("volume", "ReadVolume"),
                        ),
                        (
                            "create".to_owned(),
                            ActionId::new_unchecked("volume", "CreateVolume"),
                        ),
                        (
                            "delete".to_owned(),
                            ActionId::new_unchecked("volume", "DeleteVolume"),
                        ),
                    ]),
                }],
                actions: vec![
                    "volume:ListVolumes".to_owned(),
                    "volume:CreateVolume".to_owned(),
                    "volume:ReadVolume".to_owned(),
                    "volume:DeleteVolume".to_owned(),
                ],
                capabilities: vec![],
                dependencies: vec![],
                quota_dimensions: vec![],
                regions: vec![],
                availability_domains: vec![],
                controller: Some(ManifestController {
                    mode: "in-process".to_owned(),
                    protocol: "in-process".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    service_principal: None,
                }),
                health: None,
            },
            // placement: no actions currently accepted in
            // contracts/cloud-kernel-actions.yaml.
        ];

        for m in core_manifests {
            // Idempotency check: exact equivalent already registered → skip.
            if let Some(existing) = self.manifests.get(&m.service_id) {
                if *existing == m {
                    // Same manifest content — idempotent skip.
                    continue;
                }
                // Same service ID with semantically different content → fail.
                return Err(ManifestError::DuplicateServiceId(m.service_id.clone()));
            }
            // Same namespace owned by a different service → fail.
            if let Some(owner_id) = self.by_namespace.get(&m.namespace)
                && owner_id != &m.service_id
            {
                return Err(ManifestError::DuplicateNamespace(m.namespace.clone()));
            }
            // Fail closed: a built-in core manifest that cannot register
            // is an invariant violation that must propagate through startup.
            self.register(m)?;
        }
        Ok(())
    }

    /// Returns all unique resource types across registered services.
    #[allow(clippy::expect_used)]
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
mod external_loader_tests {
    use super::*;

    #[test]
    fn external_manifest_loader_accepts_synthetic_service_without_dispatch_code() {
        let path =
            std::env::temp_dir().join(format!("o3k-synthetic-{}.json", uuid::Uuid::new_v4()));
        let manifest = r#"{
          "manifest_version":"o3k.io/service-manifest/v1",
          "service_id":"synthetic-example",
          "namespace":"synthetic",
          "service_version":"1.0.0",
          "ownership_mode":"external-controller",
          "resource_types":[{"type":"synthetic:item","schema_version":"v1","scope":"tenant","operations":{"show":"synthetic:ReadItem","create":"synthetic:CreateItem","delete":"synthetic:DeleteItem"}}],
          "actions":["synthetic:ReadItem","synthetic:CreateItem","synthetic:DeleteItem"],
          "controller":{"mode":"external","protocol":"grpc","protocol_version":"1.0","service_principal":"synthetic-controller"}
        }"#;
        std::fs::write(&path, manifest).unwrap();
        let mut registry = ManifestRegistry::new();
        registry.register_json_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(registry.has_resource_type(&ResourceType::new("synthetic", "item").unwrap()));
        assert!(registry.has_action(&ActionId::new("synthetic", "CreateItem").unwrap()));
    }

    #[test]
    fn external_manifest_directory_loader_fails_closed_on_bad_json() {
        let directory =
            std::env::temp_dir().join(format!("o3k-manifests-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("bad.json"), b"not-json").unwrap();
        let mut registry = ManifestRegistry::new();
        assert!(registry.register_json_directory(&directory).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn valid_database_manifest() -> ServiceManifest {
        ServiceManifest {
            manifest_version: 1,
            service_id: "database-example".to_owned(),
            namespace: "database".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new("database", "instance").unwrap(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec![
                "database:CreateInstance".to_owned(),
                "database:ReadInstance".to_owned(),
                "database:DeleteInstance".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: Some(ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
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
        let err = m.validate().unwrap_err();
        assert!(
            err.to_string().contains("service_id"),
            "expected service_id error, got {err}"
        );
    }

    #[test]
    fn manifest_rejects_resource_type_outside_namespace() {
        let mut m = valid_database_manifest();
        m.resource_types = vec![RegisteredResourceType {
            resource_type: ResourceType::new("compute", "server").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        }];
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
        assert!(
            err.to_string().contains("resource_types"),
            "expected resource_types error, got {err}"
        );
    }

    #[test]
    fn register_rejects_empty_actions() {
        let mut m = valid_database_manifest();
        m.actions = vec![];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("actions"),
            "expected actions error, got {err}"
        );
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
        let rt = RegisteredResourceType {
            resource_type: ResourceType::new("database", "instance").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        };
        m.resource_types = vec![rt.clone(), rt];
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
        m.resource_types = vec![RegisteredResourceType {
            resource_type: ResourceType::new("network", "port").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        }];
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
        m2.resource_types = vec![RegisteredResourceType {
            resource_type: ResourceType::new("database-2", "instance").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        }];
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
        bad.resource_types = vec![RegisteredResourceType {
            resource_type: ResourceType::new("database", "instance").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        }]; // clashes
        let _ = reg.register(bad);

        // State must remain unchanged: only 1 service
        assert_eq!(reg.len(), 1);
        assert!(reg.get("database-example").is_some());
        assert!(reg.get("database-attempt").is_none());
    }

    #[test]
    fn register_rejects_malformed_resource_type_format() {
        let mut m = valid_database_manifest();
        m.resource_types = vec![RegisteredResourceType {
            resource_type: ResourceType::new("other", "resource").unwrap(),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::new(),
        }];
        let mut reg = ManifestRegistry::new();
        let err = reg.register(m).unwrap_err();
        assert!(
            matches!(err, ManifestError::NamespaceMismatch { .. }),
            "expected NamespaceMismatch for mismatched resource type namespace, got {err}"
        );
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
    fn seed_core_registers_accepted_services() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        // Native Network and Volume reads are accepted canonical services.
        assert_eq!(reg.len(), 5);
        assert!(reg.get("identity").is_some());
        assert!(reg.get("image").is_some());
        assert!(reg.get("compute").is_some());
        assert!(reg.get("network").is_some());
        assert!(reg.get("volume").is_some());
        assert!(reg.get("placement").is_none());
    }

    #[test]
    fn in_process_controller_readiness_uses_shared_lifecycle_state() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();

        reg.register_in_process_controller("network", true, Some("network store ready".into()))
            .unwrap();
        assert_eq!(
            reg.controller("network").unwrap().state,
            ControllerState::Ready
        );
        assert!(reg.controller("network").unwrap().session.is_none());

        reg.register_in_process_controller("volume", false, Some("storage provider absent".into()))
            .unwrap();
        assert_eq!(
            reg.controller("volume").unwrap().state,
            ControllerState::NotReady
        );
        assert!(
            !reg.controller("volume")
                .unwrap()
                .health
                .as_ref()
                .unwrap()
                .healthy
        );
    }

    #[test]
    fn in_process_controller_registration_rejects_external_service() {
        let mut reg = ManifestRegistry::new();
        let mut manifest = valid_database_manifest();
        manifest.ownership = ServiceOwnership::ExternalController;
        manifest.controller = Some(ManifestController {
            mode: "external".to_owned(),
            protocol: "grpc".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: Some("database-controller".to_owned()),
        });
        reg.register(manifest).unwrap();
        assert!(
            reg.register_in_process_controller("database-example", true, None)
                .is_err()
        );
    }

    #[test]
    fn in_process_controller_health_reports_manifest_protocol_version() {
        let mut reg = ManifestRegistry::new();
        let mut manifest = valid_database_manifest();
        manifest.controller.as_mut().unwrap().protocol_version = "1.5".to_owned();
        reg.register(manifest).unwrap();
        reg.register_in_process_controller("database-example", true, None)
            .unwrap();
        assert_eq!(
            reg.controller("database-example")
                .unwrap()
                .health
                .as_ref()
                .unwrap()
                .protocol_version,
            crate::controller::ProtocolVersion::new(1, 5)
        );
    }

    #[test]
    fn seed_core_by_namespace_index_consistent() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        // get_by_namespace must work for all seeded services
        for ns in &["identity", "image", "compute", "network", "volume"] {
            let svc = reg.get_by_namespace(ns);
            assert!(svc.is_some(), "get_by_namespace({ns}) returned None");
            assert_eq!(svc.unwrap().namespace, *ns);
        }
        // secondary index must exactly match manifest count
        assert_eq!(reg.len(), 5);
        assert_eq!(reg.all().len(), 5);
    }

    #[test]
    fn seed_core_duplicate_namespace_fails() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        let m = ServiceManifest {
            manifest_version: 1,
            service_id: "compute-dup".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new("compute", "server").unwrap(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec!["compute:CreateServer".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: Some(ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
            health: None,
        };
        let err = reg.register(m).unwrap_err();
        assert!(
            matches!(err, ManifestError::DuplicateNamespace(_)),
            "expected DuplicateNamespace, got {err}"
        );
    }

    #[test]
    fn seed_core_no_openstack_capabilities() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        for m in reg.all() {
            for cap in &m.capabilities {
                assert!(
                    !cap.starts_with("openstack-"),
                    "service {} must not have OpenStack capabilities in native manifest: {cap}",
                    m.service_id
                );
            }
        }
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
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new("compute", "custom_resource").unwrap(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: HashMap::new(),
            }],
            actions: vec!["compute:CustomAction".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: Some(ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
            health: None,
        };
        reg.register(m).unwrap();
        // seed_core must fail: compute namespace is already owned by
        // a different service (custom-compute), and the built-in compute
        // manifest is incompatible with the registered one.
        let err = reg.seed_core().unwrap_err();
        assert!(
            matches!(err, ManifestError::DuplicateNamespace(_)),
            "expected DuplicateNamespace since custom-compute owns 'compute' namespace, got {err}"
        );
        // custom-compute must still remain registered
        assert!(reg.get("custom-compute").is_some());
    }

    #[test]
    fn seed_core_invalid_manifest_fails_closed() {
        // Built-in core manifests must be valid; if invalid, seed fails.
        // This test only verifies the API shape: seed_core returns Result.
        let mut reg = ManifestRegistry::new();
        assert!(reg.seed_core().is_ok());
    }

    // ── Schema conformance tests ────────────────────────────────────────

    /// Embed the service-manifest-v1 schema.
    const SERVICE_MANIFEST_SCHEMA: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/service-manifest-v1.schema.json"
    ));

    /// Embed the native-resource-envelope-v1 schema.
    const RESOURCE_ENVELOPE_SCHEMA: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/native-resource-envelope-v1.schema.json"
    ));

    /// Embed the openstack-compatibility-projection-v1 schema.
    const PROJECTION_SCHEMA: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/openstack-compatibility-projection-v1.schema.json"
    ));

    #[test]
    fn wire_manifest_v1_conforms_to_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(SERVICE_MANIFEST_SCHEMA).expect("valid schema JSON");
        let compiled = jsonschema::validator_for(&schema).expect("valid compiled schema");

        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "database-example".to_owned(),
            namespace: "database".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "database:instance".to_owned(),
                schema_version: "v1".to_owned(),
                collection: Some("instances".to_owned()),
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec![
                "database:CreateInstance".to_owned(),
                "database:ReadInstance".to_owned(),
                "database:DeleteInstance".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let json = serde_json::to_value(&wire).expect("serialization");
        if let Err(errors) = compiled.validate(&json) {
            panic!("ServiceManifestV1 schema validation failed:\n{}", errors);
        }
    }

    #[test]
    fn wire_manifest_v1_rejects_invalid_ownership_mode() {
        let schema: serde_json::Value =
            serde_json::from_str(SERVICE_MANIFEST_SCHEMA).expect("valid schema JSON");
        let compiled = jsonschema::validator_for(&schema).expect("valid compiled schema");

        // ownership_mode must be a specific enum value
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "bad-service".to_owned(),
            namespace: "bad".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "invalid-mode".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "bad:thing".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["bad:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let json = serde_json::to_value(&wire).expect("serialization");
        let validation = compiled.validate(&json);
        assert!(
            validation.is_err(),
            "expected schema validation failure for invalid ownership_mode"
        );
    }

    #[test]
    fn wire_manifest_v1_rejects_missing_version() {
        let schema: serde_json::Value =
            serde_json::from_str(SERVICE_MANIFEST_SCHEMA).expect("valid schema JSON");
        let compiled = jsonschema::validator_for(&schema).expect("valid compiled schema");

        let mut json = serde_json::json!({
            "service_id": "no-version",
            "namespace": "test",
            "service_version": "0.1.0",
            "ownership_mode": "o3k-implemented",
            "resource_types": [{"type": "test:resource", "schema_version": "v1"}],
            "actions": ["test:DoSomething"],
            "controller": {
                "mode": "in-process",
                "protocol": "in-process",
                "protocol_version": "1.0"
            }
        });
        // manifest_version is required by the schema as a const
        let validation = compiled.validate(&json);
        assert!(
            validation.is_err(),
            "expected schema validation failure for missing manifest_version"
        );

        // Also test invalid manifest_version value
        json["manifest_version"] = serde_json::json!("wrong-value");
        let validation2 = compiled.validate(&json);
        assert!(
            validation2.is_err(),
            "expected schema validation failure for wrong manifest_version"
        );
    }

    #[test]
    fn wire_resource_envelope_v1_conforms_to_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(RESOURCE_ENVELOPE_SCHEMA).expect("valid schema JSON");
        let compiled = jsonschema::validator_for(&schema).expect("valid compiled schema");

        let wire = NativeResourceV1 {
            api_version: "o3k.io/v1".to_owned(),
            kind: "compute:server".to_owned(),
            metadata: NativeResourceMetaV1 {
                id: "srv-abc-123".to_owned(),
                owner_scope: Some("proj-xyz".to_owned()),
                generation: 1,
                region: None,
                availability_domain: None,
                created_at: Some("2026-08-21T12:00:00Z".to_owned()),
                updated_at: None,
                labels: None,
                annotations: None,
            },
            spec: serde_json::json!({"flavor": "m1.small"}),
            status: serde_json::json!({"state": "ACTIVE"}),
        };

        let json = serde_json::to_value(&wire).expect("serialization");
        if let Err(errors) = compiled.validate(&json) {
            panic!("NativeResourceV1 schema validation failed:\n{}", errors);
        }

        // Verify api_version is exactly "o3k.io/v1"
        assert_eq!(json["api_version"], "o3k.io/v1");
        // Verify kind follows namespace:type pattern
        assert!(json["kind"].as_str().unwrap().contains(':'));
    }

    #[test]
    fn wire_projection_v1_conforms_to_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(PROJECTION_SCHEMA).expect("valid schema JSON");
        let compiled = jsonschema::validator_for(&schema).expect("valid compiled schema");

        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "compute".to_owned(),
            service_type: "compute".to_owned(),
            service_name: Some("OpenStack Compute".to_owned()),
            enabled: true,
            api_surfaces: vec![OpenStackApiSurfaceV1 {
                name: "Nova API".to_owned(),
                prefix: "/v2.1".to_owned(),
                version: "2.1".to_owned(),
                min_microversion: Some("2.1".to_owned()),
                max_microversion: Some("2.99".to_owned()),
                enabled: true,
            }],
            endpoints: vec![OpenStackEndpointV1 {
                interface: "public".to_owned(),
                region: "RegionOne".to_owned(),
                url_template: "http://localhost:18080/v2.1/{project_id}".to_owned(),
                enabled: true,
            }],
            capabilities: vec!["compute:servers".to_owned()],
            evidence_profile: Some("native-rust-testlab".to_owned()),
        };

        let json = serde_json::to_value(&wire).expect("serialization");
        if let Err(errors) = compiled.validate(&json) {
            panic!(
                "OpenStackProjectionV1 schema validation failed:\n{}",
                errors
            );
        }
    }

    #[test]
    fn wire_manifest_v1_converts_to_internal_service_manifest() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "database-example".to_owned(),
            namespace: "database".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "database:instance".to_owned(),
                schema_version: "v1".to_owned(),
                collection: Some("instances".to_owned()),
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["database:CreateInstance".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion succeeds");
        assert_eq!(internal.service_id, "database-example");
        assert_eq!(internal.namespace, "database");
        assert_eq!(internal.manifest_version, 1);
        assert!(
            internal
                .resource_types
                .iter()
                .any(|rt| rt.resource_type.to_string() == "database:instance")
        );
        assert!(
            internal
                .actions
                .contains(&"database:CreateInstance".to_owned())
        );
    }

    #[test]
    fn wire_manifest_v1_rejects_wrong_version_string() {
        let wire = ServiceManifestV1 {
            manifest_version: "wrong-version".to_owned(),
            service_id: "test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "test:res".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected conversion failure for wrong manifest_version"
        );
    }

    // ── Semantic-preservation tests ─────────────────────────────────────

    #[test]
    fn conversion_preserves_resource_schema_version() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "db-test".to_owned(),
            namespace: "database".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "database:instance".to_owned(),
                schema_version: "v2".to_owned(),
                collection: Some("myinstances".to_owned()),
                scope: "system".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["database:CreateInstance".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        let rt = &internal.resource_types[0];
        assert_eq!(rt.schema_version, "v2");
        assert_eq!(rt.collection.as_deref(), Some("myinstances"));
        assert_eq!(rt.scope, ResourceScope::System);
    }

    #[test]
    fn conversion_preserves_mixed_resource_scope() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "scope-test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![
                ResourceTypeDescriptor {
                    type_: "test:tenant-res".to_owned(),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: "tenant".to_owned(),
                    operations: HashMap::new(),
                },
                ResourceTypeDescriptor {
                    type_: "test:sys-res".to_owned(),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: "system".to_owned(),
                    operations: HashMap::new(),
                },
                ResourceTypeDescriptor {
                    type_: "test:mixed-res".to_owned(),
                    schema_version: "v1".to_owned(),
                    collection: None,
                    scope: "mixed".to_owned(),
                    operations: HashMap::new(),
                },
            ],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        assert_eq!(internal.resource_types.len(), 3);
        assert_eq!(internal.resource_types[0].scope, ResourceScope::Tenant);
        assert_eq!(internal.resource_types[1].scope, ResourceScope::System);
        assert_eq!(internal.resource_types[2].scope, ResourceScope::Mixed);
    }

    #[test]
    fn conversion_preserves_dependency_semantics() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "dep-test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "test:resource".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![
                DependencyDescriptor {
                    kind: "service".to_owned(),
                    name: "compute".to_owned(),
                    required: true,
                },
                DependencyDescriptor {
                    kind: "resource_type".to_owned(),
                    name: "compute:server".to_owned(),
                    required: false,
                },
                DependencyDescriptor {
                    kind: "capability".to_owned(),
                    name: "snapshots".to_owned(),
                    required: true,
                },
            ],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        assert_eq!(internal.dependencies.len(), 3);
        assert_eq!(internal.dependencies[0].kind, DependencyKind::Service);
        assert_eq!(internal.dependencies[0].name, "compute");
        assert!(internal.dependencies[0].required);
        assert_eq!(internal.dependencies[1].kind, DependencyKind::ResourceType);
        assert_eq!(internal.dependencies[1].name, "compute:server");
        assert!(!internal.dependencies[1].required);
        assert_eq!(internal.dependencies[2].kind, DependencyKind::Capability);
        assert!(internal.dependencies[2].required);
    }

    #[test]
    fn conversion_preserves_multiple_regions_and_availability_domains() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "region-test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "test:resource".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned(), "region-b".to_owned()],
            availability_domains: vec![
                "zone-1".to_owned(),
                "zone-2".to_owned(),
                "zone-3".to_owned(),
            ],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        assert_eq!(internal.regions.len(), 2);
        assert!(internal.regions.contains(&"region-a".to_owned()));
        assert!(internal.regions.contains(&"region-b".to_owned()));
        assert_eq!(internal.availability_domains.len(), 3);
        assert!(internal.availability_domains.contains(&"zone-3".to_owned()));
    }

    #[test]
    fn conversion_preserves_external_controller_semantics() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "ext-ctrl".to_owned(),
            namespace: "ext".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "external-controller".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "ext:resource".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["ext:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "external".to_owned(),
                protocol: "grpc".to_owned(),
                protocol_version: "1.5".to_owned(),
                service_principal: Some("ext-ctrl-svc@o3k.io".to_owned()),
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        assert_eq!(internal.ownership, ServiceOwnership::ExternalController);
        let ctrl = internal.controller.expect("controller should be present");
        assert_eq!(ctrl.mode, "external");
        assert_eq!(ctrl.protocol, "grpc");
        assert_eq!(ctrl.protocol_version, "1.5");
        assert_eq!(
            ctrl.service_principal.as_deref(),
            Some("ext-ctrl-svc@o3k.io")
        );
    }

    #[test]
    fn conversion_preserves_quota_dimensions() {
        let wire = ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "quota-test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "test:resource".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![
                QuotaDimensionDescriptor {
                    key: "instances".to_owned(),
                    unit: "count".to_owned(),
                    scope: "tenant".to_owned(),
                },
                QuotaDimensionDescriptor {
                    key: "storage_gb".to_owned(),
                    unit: "gibibytes".to_owned(),
                    scope: "tenant".to_owned(),
                },
            ],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        };

        let internal: ServiceManifest = wire.try_into().expect("conversion");
        assert_eq!(internal.quota_dimensions.len(), 2);
        assert_eq!(internal.quota_dimensions[0].key, "instances");
        assert_eq!(internal.quota_dimensions[1].key, "storage_gb");
    }

    #[test]
    fn projection_preserves_all_wire_fields() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "compute".to_owned(),
            service_type: "compute".to_owned(),
            service_name: Some("OpenStack Compute Service".to_owned()),
            enabled: true,
            api_surfaces: vec![
                OpenStackApiSurfaceV1 {
                    name: "Nova API".to_owned(),
                    prefix: "/v2.1".to_owned(),
                    version: "2.1".to_owned(),
                    min_microversion: Some("2.1".to_owned()),
                    max_microversion: Some("2.99".to_owned()),
                    enabled: true,
                },
                OpenStackApiSurfaceV1 {
                    name: "Nova Legacy".to_owned(),
                    prefix: "/v2".to_owned(),
                    version: "2.0".to_owned(),
                    min_microversion: None,
                    max_microversion: None,
                    enabled: false,
                },
            ],
            endpoints: vec![
                OpenStackEndpointV1 {
                    interface: "public".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: "http://public:8774/v2.1/{project_id}".to_owned(),
                    enabled: true,
                },
                OpenStackEndpointV1 {
                    interface: "admin".to_owned(),
                    region: "RegionOne".to_owned(),
                    url_template: "http://admin:8774/v2.1/{project_id}".to_owned(),
                    enabled: false,
                },
            ],
            capabilities: vec!["compute:servers".to_owned(), "compute:flavors".to_owned()],
            evidence_profile: Some("native-rust-testlab".to_owned()),
        };

        let internal: OpenStackCompatibilityProjection =
            wire.try_into().expect("projection conversion");
        assert_eq!(internal.service_id, "compute");
        assert_eq!(internal.service_type, "compute");
        assert_eq!(
            internal.service_name.as_deref(),
            Some("OpenStack Compute Service")
        );
        assert!(internal.enabled);
        assert_eq!(internal.api_surfaces.len(), 2);
        assert_eq!(
            internal.api_surfaces[0].min_microversion.as_deref(),
            Some("2.1")
        );
        assert_eq!(
            internal.api_surfaces[0].max_microversion.as_deref(),
            Some("2.99")
        );
        assert!(internal.api_surfaces[0].enabled);
        assert!(!internal.api_surfaces[1].enabled);
        assert_eq!(internal.endpoints.len(), 2);
        assert!(internal.endpoints[0].enabled);
        assert!(!internal.endpoints[1].enabled);
        assert_eq!(internal.capabilities.len(), 2);
        assert_eq!(
            internal.evidence_profile.as_deref(),
            Some("native-rust-testlab")
        );
    }

    // ── Fail-closed conversion tests ────────────────────────────────────

    fn valid_wire_manifest() -> ServiceManifestV1 {
        ServiceManifestV1 {
            manifest_version: "o3k.io/service-manifest/v1".to_owned(),
            service_id: "test".to_owned(),
            namespace: "test".to_owned(),
            service_version: "0.1.0".to_owned(),
            ownership_mode: "o3k-implemented".to_owned(),
            resource_types: vec![ResourceTypeDescriptor {
                type_: "test:resource".to_owned(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: "tenant".to_owned(),
                operations: HashMap::new(),
            }],
            actions: vec!["test:Action".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: ControllerDescriptor {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            },
        }
    }

    #[test]
    fn conversion_rejects_invalid_resource_scope() {
        let mut wire = valid_wire_manifest();
        wire.resource_types[0].scope = "invalid-scope".to_owned();
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid resource scope"
        );
    }

    #[test]
    fn conversion_rejects_invalid_dependency_kind() {
        let mut wire = valid_wire_manifest();
        wire.dependencies = vec![DependencyDescriptor {
            kind: "bogus-kind".to_owned(),
            name: "something".to_owned(),
            required: true,
        }];
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid dependency kind"
        );
    }

    #[test]
    fn conversion_rejects_invalid_ownership_mode() {
        let mut wire = valid_wire_manifest();
        wire.ownership_mode = "invalid-mode".to_owned();
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid ownership mode"
        );
    }

    #[test]
    fn conversion_rejects_invalid_controller_mode() {
        let mut wire = valid_wire_manifest();
        wire.controller.mode = "invalid".to_owned();
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid controller mode"
        );
    }

    #[test]
    fn conversion_rejects_invalid_controller_protocol() {
        let mut wire = valid_wire_manifest();
        wire.controller.protocol = "invalid".to_owned();
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid controller protocol"
        );
    }

    #[test]
    fn conversion_rejects_external_controller_without_service_principal() {
        let mut wire = valid_wire_manifest();
        wire.ownership_mode = "external-controller".to_owned();
        wire.controller = ControllerDescriptor {
            mode: "external".to_owned(),
            protocol: "grpc".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: None,
        };
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of external controller without service_principal"
        );
    }

    #[test]
    fn conversion_rejects_invalid_quota_scope() {
        let mut wire = valid_wire_manifest();
        wire.quota_dimensions = vec![QuotaDimensionDescriptor {
            key: "instances".to_owned(),
            unit: "count".to_owned(),
            scope: "invalid-scope".to_owned(),
        }];
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(result.is_err(), "expected rejection of invalid quota scope");
    }

    #[test]
    fn seed_core_idempotent_on_exact_match() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        // Calling seed_core again with the same built-in manifests must succeed.
        assert!(reg.seed_core().is_ok());
        // State unchanged.
        assert_eq!(reg.len(), 5);
    }

    #[test]
    fn seed_core_fails_on_modified_ownership() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        // Modify the registered compute manifest's ownership
        if let Some(compute) = reg.get_mut("compute") {
            compute.ownership = ServiceOwnership::ExternalController;
        }
        // seed_core must fail: different ownership is not idempotent.
        let err = reg.seed_core().unwrap_err();
        assert!(
            matches!(err, ManifestError::DuplicateServiceId(_)),
            "expected DuplicateServiceId for modified ownership, got {err}"
        );
    }

    #[test]
    fn seed_core_fails_on_modified_capabilities() {
        let mut reg = ManifestRegistry::new();
        reg.seed_core().unwrap();
        // Add a capability to the registered manifest
        if let Some(compute) = reg.get_mut("compute") {
            compute.capabilities.push("some-capability".to_owned());
        }
        let err = reg.seed_core().unwrap_err();
        assert!(
            matches!(err, ManifestError::DuplicateServiceId(_)),
            "expected DuplicateServiceId for modified capabilities, got {err}"
        );
    }

    #[test]
    fn conversion_rejects_empty_schema_version() {
        let mut wire = valid_wire_manifest();
        wire.resource_types[0].schema_version = "".to_owned();
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of empty schema_version"
        );
    }

    #[test]
    fn conversion_rejects_empty_region_entry() {
        let mut wire = valid_wire_manifest();
        wire.regions = vec!["".to_owned()];
        let result: Result<ServiceManifest, ManifestError> = wire.try_into();
        assert!(result.is_err(), "expected rejection of empty region entry");
    }

    #[test]
    fn projection_conversion_rejects_invalid_endpoint_interface() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "compute".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![OpenStackEndpointV1 {
                interface: "bogus".to_owned(),
                region: "RegionOne".to_owned(),
                url_template: "http://localhost".to_owned(),
                enabled: true,
            }],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of invalid endpoint interface"
        );
    }

    #[test]
    fn projection_conversion_rejects_wrong_projection_version() {
        let wire = OpenStackProjectionV1 {
            projection_version: "wrong-version".to_owned(),
            service_id: "compute".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of wrong projection version"
        );
    }

    // ── Direct-Rust contract-parity tests ───────────────────────────────

    #[test]
    fn direct_rust_rejects_invalid_service_id() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.service_id = "Bad Service".to_owned();
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("service_id"),
            "expected service_id error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_invalid_namespace() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.namespace = "bad.namespace".to_owned();
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("namespace"),
            "expected namespace error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_invalid_collection() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.resource_types[0].collection = Some("bad collection".to_owned());
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("collection"),
            "expected collection error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_invalid_action_format() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.actions = vec!["database:123bad".to_owned()];
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("PascalCase"),
            "expected action PascalCase error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_invalid_capability() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.capabilities = vec!["Bad Capability".to_owned()];
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("capability"),
            "expected capability error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_invalid_quota_key() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.quota_dimensions = vec![QuotaDimension {
            key: "Bad Key".to_owned(),
            unit: "count".to_owned(),
            scope: "tenant".to_owned(),
        }];
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("key"),
            "expected quota key error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_129_resource_types() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.resource_types = (0..129)
            .map(|i| RegisteredResourceType {
                resource_type: ResourceType::new("database", format!("instance_{i}")).unwrap(),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: HashMap::new(),
            })
            .collect();
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("resource_types"),
            "expected resource_types error for 129, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_257_actions() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.actions = (0..257).map(|i| format!("database:Action_{i}")).collect();
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("actions"),
            "expected actions error for 257, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_missing_controller() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.controller = None;
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("controller"),
            "expected controller required error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_o3k_implemented_with_grpc() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.controller = Some(ManifestController {
            mode: "in-process".to_owned(),
            protocol: "grpc".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: None,
        });
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("controller"),
            "expected controller combination error for in-process+grpc, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_external_controller_with_in_process_protocol() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.ownership = ServiceOwnership::ExternalController;
        m.controller = Some(ManifestController {
            mode: "external".to_owned(),
            protocol: "in-process".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: Some("svc@o3k".to_owned()),
        });
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("controller"),
            "expected controller combination error for external+in-process, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_external_hosted_ownership() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.ownership = ServiceOwnership::ExternalHosted;
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("ownership"),
            "expected ExternalHosted rejection, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_duplicate_capability() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.capabilities = vec!["dup".to_owned(), "dup".to_owned()];
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "expected duplicate capability error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_duplicate_region() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.regions = vec!["region-a".to_owned(), "region-a".to_owned()];
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "expected duplicate region error, got {err}"
        );
    }

    #[test]
    fn direct_rust_rejects_empty_controller_protocol_version() {
        let mut reg = ManifestRegistry::new();
        let mut m = valid_database_manifest();
        m.controller = Some(ManifestController {
            mode: "in-process".to_owned(),
            protocol: "in-process".to_owned(),
            protocol_version: "".to_owned(),
            service_principal: None,
        });
        let err = reg.register(m).unwrap_err();
        assert!(
            err.to_string().contains("protocol_version"),
            "expected protocol_version error, got {err}"
        );
    }

    #[test]
    fn projection_rejects_oversized_service_type() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "x".repeat(129),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of oversized service_type"
        );
    }

    #[test]
    fn projection_rejects_33_surfaces() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: (0..33)
                .map(|i| OpenStackApiSurfaceV1 {
                    name: format!("surface_{i}"),
                    prefix: "/v2".to_owned(),
                    version: "1.0".to_owned(),
                    min_microversion: None,
                    max_microversion: None,
                    enabled: true,
                })
                .collect(),
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(result.is_err(), "expected rejection of 33 API surfaces");
    }

    #[test]
    fn projection_rejects_empty_surface_version() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![OpenStackApiSurfaceV1 {
                name: "Nova".to_owned(),
                prefix: "/v2.1".to_owned(),
                version: "".to_owned(),
                min_microversion: None,
                max_microversion: None,
                enabled: true,
            }],
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of empty surface version"
        );
    }

    #[test]
    fn projection_rejects_oversized_microversion() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![OpenStackApiSurfaceV1 {
                name: "Nova".to_owned(),
                prefix: "/v2.1".to_owned(),
                version: "2.1".to_owned(),
                min_microversion: Some("x".repeat(33)),
                max_microversion: None,
                enabled: true,
            }],
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of oversized microversion"
        );
    }

    #[test]
    fn projection_rejects_empty_region() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![OpenStackEndpointV1 {
                interface: "public".to_owned(),
                region: "".to_owned(),
                url_template: "http://localhost".to_owned(),
                enabled: true,
            }],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of empty endpoint region"
        );
    }

    #[test]
    fn projection_rejects_duplicate_capability() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![],
            capabilities: vec!["cap".to_owned(), "cap".to_owned()],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of duplicate projection capability"
        );
    }

    #[test]
    fn projection_rejects_oversized_evidence_profile() {
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![],
            capabilities: vec![],
            evidence_profile: Some("x".repeat(129)),
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_err(),
            "expected rejection of oversized evidence_profile"
        );
    }

    #[test]
    fn projection_accepts_non_http_url_template() {
        // The accepted schema does NOT require url_template to start with
        // "http" or "/". A valid non-HTTP URL must not be rejected.
        let wire = OpenStackProjectionV1 {
            projection_version: "o3k.io/openstack-projection/v1".to_owned(),
            service_id: "test".to_owned(),
            service_type: "compute".to_owned(),
            service_name: None,
            enabled: true,
            api_surfaces: vec![],
            endpoints: vec![OpenStackEndpointV1 {
                interface: "public".to_owned(),
                region: "RegionOne".to_owned(),
                url_template: "unix:///var/run/o3k.sock".to_owned(),
                enabled: true,
            }],
            capabilities: vec![],
            evidence_profile: None,
        };
        let result: Result<OpenStackCompatibilityProjection, ManifestError> = wire.try_into();
        assert!(
            result.is_ok(),
            "a valid non-http URL template must not be rejected by an undocumented prefix rule"
        );
    }

    #[test]
    fn service_authority_cannot_be_forgotten_by_registry_removal() {
        let mut registry = ManifestRegistry::new();
        registry.seed_core().expect("core manifests");
        let before = registry.len();
        assert!(registry.remove("compute").is_err());
        assert_eq!(registry.len(), before);
        assert!(registry.get("compute").is_some());

        registry.remove_controller("compute");
        assert!(registry.get("compute").is_some());
    }
}
