//! Canonical O3K location topology (regions, availability domains, and
//! failure domains) plus topology-to-target bindings.
//!
//! This module owns the single authoritative representation of deployment
//! location topology. A [`LocationRegistry`] is seeded by the deployment
//! composition root from configuration — never derived from compute providers,
//! hosts, hypervisors, clusters, storage backends, or network controllers —
//! and can afterwards be mutated only through its durable mutation methods,
//! each of which persists through [`TopologyStore`] before applying to memory.
//!
//! Public location IDs are provider-neutral and stable: replacing an
//! implementation provider must not change a region's public identity.
//!
//! Failure domains model the physical/convergence hierarchy *inside* one
//! availability domain (site → building → room → row → rack → chassis, plus
//! shared power/network/storage domains). [`TopologyBinding`] records which
//! failure domain a resource provider, host, fabric domain, or storage domain
//! is bound to. This is topology authority only: no scheduling, filtering, or
//! placement logic lives here.
//!
//! See ADR-0181, SPEC-0038, and P15.1 (issue #931).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::audit::AuditEvent;
use crate::error::KernelError;
use crate::manifest::ServiceManifest;

/// Maximum length of a failure-domain display name.
pub const MAX_FAILURE_DOMAIN_NAME_LEN: usize = 256;

/// Maximum number of metadata entries on one failure domain.
pub const MAX_METADATA_ENTRIES: usize = 32;

/// Maximum length of a failure-domain metadata key.
pub const MAX_METADATA_KEY_LEN: usize = 64;

/// Maximum length of a failure-domain metadata value.
pub const MAX_METADATA_VALUE_LEN: usize = 256;

/// Hard cap on failure-domain nesting depth (chain length including the
/// domain itself). Root domains sit at depth 1.
pub const MAX_FAILURE_DOMAIN_DEPTH: usize = 64;

/// Identifier characters permitted in a canonical location ID.
///
/// Location IDs are restricted to a stable, human-safe, provider-neutral
/// alphabet: lower-case letters, digits, `_` and `-`. IDs must not contain
/// upper-case, whitespace, or punctuation that could collide with host,
/// provider, or backend identifiers.
fn is_location_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}

/// Returns true when `id` is a valid canonical location identifier.
///
/// The accepted alphabet matches the public contract pattern
/// `^[a-z0-9][a-z0-9_-]*$` (1..=128): the first character must be a lower-case
/// letter or digit; the remainder may add `_` and `-`.
fn valid_location_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        None => false,
        Some(first) => {
            let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
            first_ok && id.len() <= 128 && chars.all(is_location_id_char)
        }
    }
}

/// Identifier characters permitted in a metadata key after the first one:
/// lower-case letters, digits, `_`, `.`, and `-`.
fn is_metadata_key_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-'
}

/// Returns true when `key` is a valid failure-domain metadata key.
///
/// The accepted alphabet matches `^[a-z0-9][a-z0-9_.-]*$` (1..=64).
fn valid_metadata_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        None => false,
        Some(first) => {
            let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
            first_ok && key.len() <= MAX_METADATA_KEY_LEN && chars.all(is_metadata_key_char)
        }
    }
}

/// A canonical availability domain within a region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityDomain {
    /// Stable provider-neutral availability domain identifier.
    pub id: String,
}

/// A canonical region and the availability domains it contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionDeclaration {
    /// Stable provider-neutral region identifier.
    pub id: String,
    /// Availability domains that belong to this region.
    #[serde(default)]
    pub availability_domains: Vec<AvailabilityDomain>,
}

/// Coarseness ordering of a [`FailureDomain`].
///
/// Ranks express nesting semantics: a domain may be nested under a parent of
/// equal or *coarser* class (`child.rank() >= parent.rank()`), never under a
/// finer one. Shared infrastructure classes (power, network, storage) share
/// one rank so they may sit at the same level of the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureDomainClass {
    Site,
    Building,
    Room,
    Row,
    Rack,
    Chassis,
    PowerDomain,
    NetworkDomain,
    StorageDomain,
}

impl FailureDomainClass {
    /// Coarseness rank used by nesting validation: coarser classes rank lower.
    #[must_use]
    pub fn rank(self) -> u16 {
        match self {
            Self::Site => 10,
            Self::Building => 20,
            Self::Room => 30,
            Self::Row => 40,
            Self::Rack => 50,
            Self::Chassis => 60,
            Self::PowerDomain | Self::NetworkDomain | Self::StorageDomain => 70,
        }
    }

    /// Stable kebab-case class name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Building => "building",
            Self::Room => "room",
            Self::Row => "row",
            Self::Rack => "rack",
            Self::Chassis => "chassis",
            Self::PowerDomain => "power-domain",
            Self::NetworkDomain => "network-domain",
            Self::StorageDomain => "storage-domain",
        }
    }
}

impl fmt::Display for FailureDomainClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A canonical failure domain inside one availability domain.
///
/// A failure domain is a node of the physical/convergence hierarchy that can
/// fail as one unit (site, building, room, row, rack, chassis, or a shared
/// power/network/storage domain). Every failure domain is owned by exactly one
/// availability domain and may declare at most one parent failure domain,
/// forming a DAG rooted at parentless domains.
///
/// `class`, `availability_domain`, `parent`, and `id` are immutable after
/// creation; only `name` and `metadata` may change, and each successful update
/// bumps `generation` (starting at 1) so callers get optimistic-concurrency
/// control against the durable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureDomain {
    /// Stable provider-neutral failure-domain identifier.
    pub id: String,
    /// Coarseness class; immutable after creation.
    pub class: FailureDomainClass,
    /// Human-readable display name; non-empty, at most 256 characters.
    pub name: String,
    /// Owning availability domain; immutable after creation.
    pub availability_domain: String,
    /// Optional parent failure domain; immutable after creation.
    #[serde(default)]
    pub parent: Option<String>,
    /// Optimistic-concurrency generation; starts at 1 and increments on every
    /// successful update.
    pub generation: u64,
    /// Arbitrary bounded key/value annotations.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl FailureDomain {
    /// Cheap shape validation for a failure domain read off the durable store.
    ///
    /// This is intentionally much lighter than full hierarchy validation
    /// ([`LocationRegistry::from_snapshot`]): it checks that a single row's
    /// scalar fields are structurally well-formed (ID alphabet/length, non-empty
    /// bounded name, metadata bounds, availability-domain/parent ID alphabet,
    /// and class parse consistency) so the API read path can fail closed rather
    /// than serve a malformed authoritative row. It does not traverse the
    /// hierarchy.
    pub fn validate_shape(&self) -> Result<(), LocationError> {
        validate_failure_domain_id(&self.id)?;
        validate_failure_domain_name(&self.name)?;
        validate_metadata(&self.metadata)?;
        if !valid_location_id(&self.availability_domain) {
            return Err(LocationError::MalformedAvailabilityDomainId(
                self.availability_domain.clone(),
            ));
        }
        if let Some(parent) = &self.parent
            && !valid_location_id(parent)
        {
            return Err(LocationError::MalformedFailureDomainId(parent.clone()));
        }
        // Class parse consistency: the typed variant must round-trip through
        // its stable wire name (defence in depth against a stored value that
        // is not one of the known kebab-case class names).
        let parsed = serde_json::from_value::<FailureDomainClass>(serde_json::Value::String(
            self.class.as_str().to_owned(),
        ))
        .map_err(|_| LocationError::MalformedFailureDomainId(self.id.clone()))?;
        if parsed != self.class {
            return Err(LocationError::MalformedFailureDomainId(self.id.clone()));
        }
        Ok(())
    }
}

/// Kind of a [`BindingTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingTargetKind {
    ResourceProvider,
    Host,
    FabricDomain,
    StorageDomain,
}

impl BindingTargetKind {
    /// Stable kebab-case kind name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResourceProvider => "resource-provider",
            Self::Host => "host",
            Self::FabricDomain => "fabric-domain",
            Self::StorageDomain => "storage-domain",
        }
    }
}

impl fmt::Display for BindingTargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A bindable topology target: a resource provider, host, fabric domain, or
/// storage domain identified by its durable public ID.
///
/// This struct deliberately carries *only* kind and ID. Whether such a target
/// currently exists is owned by the respective domain; topology records the
/// binding intent, it does not validate foreign lifecycles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingTarget {
    pub kind: BindingTargetKind,
    /// Durable public ID of the target, constrained to the canonical location
    /// ID alphabet.
    pub id: String,
}

/// A binding of one failure domain to one target.
///
/// Bindings are unique per `(failure_domain, target.kind, target.id)` and are
/// the only topology edges from the failure-domain hierarchy to the rest of
/// the system. Deleting a failure domain that still has bindings is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyBinding {
    pub failure_domain: String,
    pub target: BindingTarget,
}

impl TopologyBinding {
    /// Cheap shape validation for a binding row read off the durable store.
    ///
    /// Checks the failure-domain and target IDs against the canonical location
    /// alphabet and that the target kind is a known value (round-tripped
    /// through its wire name), so the API read path can fail closed instead of
    /// serving a malformed authoritative binding.
    pub fn validate_shape(&self) -> Result<(), LocationError> {
        if !valid_location_id(&self.failure_domain) {
            return Err(LocationError::MalformedFailureDomainId(
                self.failure_domain.clone(),
            ));
        }
        if !valid_location_id(&self.target.id) {
            return Err(LocationError::InvalidBindingTargetId(
                self.target.id.clone(),
            ));
        }
        let parsed = serde_json::from_value::<BindingTargetKind>(serde_json::Value::String(
            self.target.kind.as_str().to_owned(),
        ))
        .map_err(|_| LocationError::InvalidBindingTargetId(self.target.id.clone()))?;
        if parsed != self.target.kind {
            return Err(LocationError::InvalidBindingTargetId(
                self.target.id.clone(),
            ));
        }
        Ok(())
    }
}

/// The complete, durable picture of canonical topology.
///
/// A snapshot is what a [`TopologyStore`] persists and what
/// [`LocationRegistry::from_snapshot`] validates on restart reconstruction.
/// All vectors are kept deterministically sorted (regions, failure domains and
/// bindings by id; availability domains by id within their region).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    #[serde(default)]
    pub regions: Vec<RegionDeclaration>,
    #[serde(default)]
    pub failure_domains: Vec<FailureDomain>,
    #[serde(default)]
    pub bindings: Vec<TopologyBinding>,
}

/// Keyset cursor for bounded binding enumeration.
///
/// Bindings sort by `(failure_domain, target.kind, target.id)`; the cursor
/// carries the exclusive lower bound of the next page in exactly that order.
/// `target_kind` uses the stable kebab-case wire vocabulary
/// ([`BindingTargetKind::as_str`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BindingListCursor {
    pub failure_domain: String,
    pub target_kind: String,
    pub target_id: String,
}

impl BindingListCursor {
    /// Builds the cursor pointing immediately after `binding`.
    #[must_use]
    pub fn after(binding: &TopologyBinding) -> Self {
        Self {
            failure_domain: binding.failure_domain.clone(),
            target_kind: binding.target.kind.as_str().to_owned(),
            target_id: binding.target.id.clone(),
        }
    }
}

/// Errors produced while validating canonical location topology, service
/// location references, or failure-domain topology mutations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    #[error("empty region id")]
    EmptyRegionId,
    #[error("empty availability domain id")]
    EmptyAvailabilityDomainId,
    #[error("malformed region id '{0}'")]
    MalformedRegionId(String),
    #[error("malformed availability domain id '{0}'")]
    MalformedAvailabilityDomainId(String),
    #[error("duplicate region: {0}")]
    DuplicateRegion(String),
    #[error("duplicate availability domain '{0}' in region '{1}'")]
    DuplicateAvailabilityDomainInRegion(String, String),
    #[error("ambiguous availability domain '{0}' declared in multiple regions")]
    AmbiguousAvailabilityDomain(String),
    #[error("service '{service}' declares unknown region '{region}'")]
    UnknownRegion { service: String, region: String },
    #[error("service '{service}' declares unknown availability domain '{availability_domain}'")]
    UnknownAvailabilityDomain {
        service: String,
        availability_domain: String,
    },
    #[error(
        "service '{service}' references availability domain '{availability_domain}' of region '{region}' it does not declare"
    )]
    AvailabilityDomainOutsideDeclaredRegions {
        service: String,
        availability_domain: String,
        region: String,
    },
    #[error("topology references unknown region '{0}'")]
    UnknownTopologyRegion(String),
    #[error("malformed failure domain id '{0}'")]
    MalformedFailureDomainId(String),
    #[error("failure domain '{0}' already exists")]
    DuplicateFailureDomain(String),
    #[error("unknown failure domain '{0}'")]
    UnknownFailureDomain(String),
    #[error("empty failure domain name")]
    EmptyFailureDomainName,
    #[error("failure domain name '{0}' exceeds {MAX_FAILURE_DOMAIN_NAME_LEN} characters")]
    FailureDomainNameTooLong(String),
    #[error("failure domain '{0}' references unknown availability domain '{1}'")]
    UnknownFailureDomainAvailabilityDomain(String, String),
    #[error("failure domain '{0}' declares missing parent '{1}'")]
    FailureDomainParentMissing(String, String),
    #[error("failure domain '{0}' cannot be its own parent")]
    FailureDomainSelfParent(String),
    #[error("failure domain '{0}' would create a parent cycle")]
    FailureDomainCycle(String),
    #[error(
        "failure domain '{0}' would exceed the maximum hierarchy depth of {MAX_FAILURE_DOMAIN_DEPTH}"
    )]
    FailureDomainHierarchyTooDeep(String),
    #[error(
        "failure domain '{failure_domain}' (class '{class}') cannot be nested under finer parent '{parent}' (class '{parent_class}')"
    )]
    InvalidFailureDomainNesting {
        failure_domain: String,
        class: FailureDomainClass,
        parent: String,
        parent_class: FailureDomainClass,
    },
    #[error(
        "failure domain '{failure_domain}' in availability domain '{availability_domain}' cannot be nested under parent '{parent}' in availability domain '{parent_availability_domain}'"
    )]
    FailureDomainOutsideAvailabilityDomain {
        failure_domain: String,
        parent: String,
        availability_domain: String,
        parent_availability_domain: String,
    },
    #[error("failure domain '{0}' still has child failure domains")]
    FailureDomainHasChildren(String),
    #[error("failure domain '{0}' still has bindings")]
    FailureDomainHasBindings(String),
    #[error("availability domain '{0}' still has failure domains")]
    AvailabilityDomainHasFailureDomains(String),
    #[error("region '{0}' still has availability domains")]
    RegionHasAvailabilityDomains(String),
    #[error(
        "duplicate binding of failure domain '{failure_domain}' to {target_kind} '{target_id}'"
    )]
    DuplicateBinding {
        failure_domain: String,
        target_kind: BindingTargetKind,
        target_id: String,
    },
    #[error("failure domain metadata has {0} entries (maximum {MAX_METADATA_ENTRIES})")]
    TooManyMetadataEntries(usize),
    #[error("invalid failure domain metadata key '{0}'")]
    InvalidMetadataKey(String),
    #[error(
        "failure domain metadata value for key '{0}' exceeds {MAX_METADATA_VALUE_LEN} characters"
    )]
    MetadataValueTooLong(String),
    #[error("invalid binding target id '{0}'")]
    InvalidBindingTargetId(String),
    #[error(
        "failure domain '{failure_domain}' has stale generation: expected {expected}, found {actual}"
    )]
    StaleFailureDomainGeneration {
        failure_domain: String,
        expected: u64,
        actual: u64,
    },
}

/// Errors from durable topology mutations of [`LocationRegistry`].
///
/// Every mutation validates against in-memory state first ([`Self::Location`]),
/// then persists through [`TopologyStore`], then applies to memory. A store
/// failure ([`Self::Store`]) propagates with in-memory state untouched: the
/// mutation either durably applied (and then also applied in memory) or not at
/// all. Note a store timeout is an *unknown* outcome — the caller must observe
/// durable state (reload the snapshot) before retrying instead of assuming the
/// mutation did not land.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    /// In-memory topology validation rejected the mutation.
    #[error(transparent)]
    Location(#[from] LocationError),
    /// The durable store rejected or failed to persist the mutation; memory is
    /// unchanged.
    #[error(transparent)]
    Store(#[from] KernelError),
}

/// Durable topology authority port.
///
/// Implementations persist [`TopologySnapshot`] state incrementally. Beyond
/// mapping calls to SQL (or another backend), implementations MUST meet two
/// contractual duties, because kernel-level validation alone cannot make
/// concurrent mutation atomic:
///
/// 1. **Serialization.** Concurrent mutation calls (inserts, updates, deletes)
///    MUST be serialized — for example with SQLite `BEGIN IMMEDIATE` or a
///    PostgreSQL transaction-level advisory lock — so a consistent sequence of
///    durable states exists even when multiple writers race.
/// 2. **Referential integrity.** Primary and foreign keys MUST be enforced so
///    writers that bypass kernel validation cannot create duplicate regions,
///    availability domains, failure domains or bindings, cannot store
///    hierarchy cycles or dangling parents, and cannot move a failure domain
///    across generations (`update_failure_domain` / `delete_failure_domain`
///    MUST compare `expected_generation` against the stored row).
/// 3. **Atomic required audit.** When a mutation carries `Some(audit)`, the
///    audit row MUST be written in the SAME store transaction as the mutation
///    using the canonical `audit_events` insert semantics (see the store
///    adapters). If either the mutation or the audit insert fails, neither is
///    visible. This makes topology audit durability structural rather than a
///    separately-published side effect.
///
/// Exactly one [`LocationRegistry`] in-memory instance may drive a given store;
/// restart reconstruction goes through [`TopologyStore::load_snapshot`] +
/// [`LocationRegistry::from_snapshot`], never through parallel mutation paths.
///
/// A store timeout or transport failure is an *unknown outcome*: the caller
/// must observe durable state before retrying rather than assuming the
/// mutation did not land.
///
/// Multi-daemon limitation: O3K serializes topology mutation through one
/// in-process [`LocationRegistry`] behind the native API. Running more than one
/// `o3kd` against the same store is not a supported deployment: the in-memory
/// "single-item" read surface (`LocationRegistry::region`, `failure_domain`, …)
/// reflects only what the local process mutated, while store-backed collection
/// reads (`list_failure_domains`, `list_bindings`) observe the full durable
/// sequence. Use exactly one writer per store and reconstruct each process from
/// `load_snapshot`, as the composition does at startup.
#[async_trait]
pub trait TopologyStore: Send + Sync {
    /// Loads the full durable topology snapshot.
    async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError>;

    /// Persists one region (idempotent: re-inserting an existing region is a
    /// no-op success, mirroring kernel replay semantics).
    async fn insert_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Removes one region. Callers validate the region has no availability
    /// domains first; the store enforces that invariant (FK) regardless.
    async fn delete_region(
        &self,
        region_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Persists one availability domain inside `region_id`.
    async fn insert_availability_domain(
        &self,
        region_id: &str,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Removes one availability domain. Callers validate no failure domain
    /// references it first; the store enforces that invariant (FK) regardless.
    async fn delete_availability_domain(
        &self,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Persists one failure domain.
    async fn insert_failure_domain(
        &self,
        domain: &FailureDomain,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Persists a name/metadata update, failing when the stored generation
    /// differs from `expected_generation`.
    async fn update_failure_domain(
        &self,
        domain: &FailureDomain,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Removes one failure domain, failing when the stored generation differs
    /// from `expected_generation`.
    async fn delete_failure_domain(
        &self,
        domain_id: &str,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Persists one binding (idempotent: re-inserting an existing binding is a
    /// no-op success, mirroring kernel replay semantics).
    async fn insert_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Removes one binding (idempotent: removing an absent binding is a no-op
    /// success, mirroring kernel replay semantics).
    async fn delete_binding(
        &self,
        binding: &TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), KernelError>;

    /// Lists failure domains in ascending id order, keyset-paged.
    ///
    /// `after_id` is the exclusive lower id bound ([`None`] starts at the
    /// beginning). Implementations return at most `limit + 1` rows: the extra
    /// row, when present, signals to the caller that a further page exists.
    async fn list_failure_domains(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FailureDomain>, KernelError>;

    /// Lists bindings in ascending `(failure_domain, target.kind, target.id)`
    /// order, keyset-paged (see [`Self::list_failure_domains`] for the
    /// `limit + 1` row convention and [`BindingListCursor`] for the key).
    async fn list_bindings(
        &self,
        after: Option<&BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError>;

    /// Lists the bindings of one failure domain in ascending
    /// `(target.kind, target.id)` order, keyset-paged (same conventions as
    /// [`Self::list_bindings`]).
    async fn list_bindings_of(
        &self,
        failure_domain: &str,
        after: Option<&BindingListCursor>,
        limit: usize,
    ) -> Result<Vec<TopologyBinding>, KernelError>;

    /// Records one audit event as a standalone durable write.
    ///
    /// Used only by idempotent no-op replay paths where a successful (2xx)
    /// mutation request produced no store mutation (so there is no mutation
    /// transaction to fold the audit into) yet the request is still an audited
    /// action — a replayed request is still an audited action. Normal
    /// mutations fold the audit into their own transaction via the mutation
    /// methods' `audit` parameter; this is the no-op-replay complement.
    async fn record_audit(&self, audit: &AuditEvent) -> Result<(), KernelError>;
}

/// The mutable failure-domain topology behind [`LocationRegistry`].
///
/// Both vectors are kept sorted by their identity key so every accessor and
/// [`TopologySnapshot`] is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TopologyState {
    failure_domains: Vec<FailureDomain>,
    bindings: Vec<TopologyBinding>,
}

/// The single canonical authority for O3K location topology.
///
/// A [`LocationRegistry`] is built via [`LocationRegistry::from_declarations`]
/// or [`LocationRegistry::from_snapshot`], which apply deterministic
/// validation, and afterwards mutated only through its async mutation methods
/// ([`LocationRegistry::declare_region`], [`LocationRegistry::bind`], …).
///
/// Failure domains and bindings are interior-mutable behind an `RwLock`: their
/// mutations take `&self`, validate under a read lock, persist through
/// [`TopologyStore`], then apply under a write lock; a store error leaves
/// memory untouched, and locks are never held across `.await` points. Regions
/// and availability domains stay in a directly borrowable plain field so the
/// original reference-returning read API is preserved exactly; their mutations
/// take `&mut self` (which proves exclusivity, so they need no lock). Both
/// halves follow the same validate → persist → apply order.
///
/// Because the store serializes mutations and enforces referential integrity,
/// kernel validation plus the store give atomicity: exactly one durable
/// mutation sequence exists, and memory replays it. This requires exactly one
/// in-memory registry per deployment driving a given store — all topology
/// mutation must flow through these methods; there is no other mutation path.
///
/// This is the one authority for "which regions, availability domains, and
/// failure domains exist and what is bound to them". Service manifests
/// reference (filter) canonical IDs; they never invent location identity.
///
/// Note: `LocationRegistry` intentionally does not implement `Deserialize` so
/// its sorted/validated invariants cannot be bypassed by deserializing an
/// arbitrary value. Use [`LocationRegistry::from_snapshot`].
pub struct LocationRegistry {
    /// Regions and their availability domains. Plain (not locked) so the
    /// reference-returning read API (`regions()`, `region()`,
    /// `availability_domains_of()`) keeps its original signatures; mutated
    /// only through `&mut self` methods.
    regions: Vec<RegionDeclaration>,
    /// Interior-mutable failure-domain hierarchy and bindings.
    topology: RwLock<TopologyState>,
}

impl fmt::Debug for LocationRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocationRegistry")
            .field("regions", &self.regions)
            .field("topology", &*self.topology_read())
            .finish()
    }
}

impl Clone for LocationRegistry {
    fn clone(&self) -> Self {
        Self {
            regions: self.regions.clone(),
            topology: RwLock::new(self.topology_read().clone()),
        }
    }
}

impl PartialEq for LocationRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.regions == other.regions && *self.topology_read() == *other.topology_read()
    }
}

impl Eq for LocationRegistry {}

impl Default for LocationRegistry {
    fn default() -> Self {
        Self {
            regions: Vec::new(),
            topology: RwLock::new(TopologyState::default()),
        }
    }
}

// The registry's public discovery projection remains the regions document it
// always was; failure domains and bindings are full topology state, exposed
// through `snapshot()`, not through this compatibility shape.
impl Serialize for LocationRegistry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("LocationRegistry", 1)?;
        state.serialize_field("regions", &self.regions)?;
        state.end()
    }
}

impl LocationRegistry {
    /// Builds a validated, deterministically ordered registry from declarations
    /// with empty failure-domain topology.
    ///
    /// This is [`LocationRegistry::from_snapshot`] restricted to regions and
    /// availability domains; validation rejects empty/malformed IDs, duplicate
    /// region IDs, duplicate availability-domain IDs within a region, and the
    /// same availability domain appearing in more than one region (an
    /// ambiguous mapping).
    pub fn from_declarations(declarations: Vec<RegionDeclaration>) -> Result<Self, LocationError> {
        Self::from_snapshot(TopologySnapshot {
            regions: declarations,
            failure_domains: Vec::new(),
            bindings: Vec::new(),
        })
    }

    /// Builds a validated, deterministically ordered registry from a full
    /// [`TopologySnapshot`].
    ///
    /// This is the restart-reconstruction entry point: it re-validates the
    /// entire durable state (regions, availability domains, failure-domain
    /// hierarchy, and bindings) so corrupt or divergent store content fails
    /// closed instead of entering memory. Cycle detection happens here, since
    /// arbitrary snapshots (unlike mutation-path creates, where `parent` is
    /// immutable) can contain cycles.
    pub fn from_snapshot(snapshot: TopologySnapshot) -> Result<Self, LocationError> {
        let TopologySnapshot {
            regions,
            failure_domains,
            bindings,
        } = snapshot;

        validate_region_declarations(&regions)?;
        let az_index: HashMap<&str, &str> = availability_domain_index(&regions);

        // Failure domains must have unique IDs before any structural check,
        // because hierarchy validation resolves parents by ID.
        let mut seen_domains = HashSet::new();
        for domain in &failure_domains {
            if !seen_domains.insert(domain.id.as_str()) {
                return Err(LocationError::DuplicateFailureDomain(domain.id.clone()));
            }
        }
        let domain_context: HashMap<String, FailureDomain> = failure_domains
            .iter()
            .map(|domain| (domain.id.clone(), domain.clone()))
            .collect();
        for domain in &failure_domains {
            validate_failure_domain_structure(domain, &domain_context, &az_index)?;
        }

        let mut seen_bindings = HashSet::new();
        for binding in &bindings {
            if !domain_context.contains_key(binding.failure_domain.as_str()) {
                return Err(LocationError::UnknownFailureDomain(
                    binding.failure_domain.clone(),
                ));
            }
            if !valid_location_id(&binding.target.id) {
                return Err(LocationError::InvalidBindingTargetId(
                    binding.target.id.clone(),
                ));
            }
            let key = binding_key(binding);
            if !seen_bindings.insert(key) {
                return Err(LocationError::DuplicateBinding {
                    failure_domain: binding.failure_domain.clone(),
                    target_kind: binding.target.kind,
                    target_id: binding.target.id.clone(),
                });
            }
        }

        Ok(Self {
            regions: sorted_regions(regions),
            topology: RwLock::new(TopologyState {
                failure_domains: sorted_failure_domains(failure_domains),
                bindings: sorted_bindings(bindings),
            }),
        })
    }

    /// Returns the regions in deterministic (sorted by id) order.
    #[must_use]
    pub fn regions(&self) -> &[RegionDeclaration] {
        &self.regions
    }

    /// Returns true if no regions are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Returns the number of configured regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Returns true when the region identifier is a configured canonical region.
    #[must_use]
    pub fn contains_region(&self, id: &str) -> bool {
        self.regions.iter().any(|region| region.id == id)
    }

    /// Returns the canonical region with the given id, if configured.
    #[must_use]
    pub fn region(&self, id: &str) -> Option<&RegionDeclaration> {
        self.regions.iter().find(|region| region.id == id)
    }

    /// Returns the availability-domain identifiers declared for `region`.
    #[must_use]
    pub fn availability_domains_of(&self, region: &str) -> &[AvailabilityDomain] {
        self.region(region)
            .map(|region| region.availability_domains.as_slice())
            .unwrap_or(&[])
    }

    /// Returns all failure domains in deterministic (sorted by id) order.
    #[must_use]
    pub fn failure_domains(&self) -> Vec<FailureDomain> {
        self.topology_read().failure_domains.clone()
    }

    /// Returns a clone of the failure domain with the given id, if configured.
    #[must_use]
    pub fn failure_domain(&self, id: &str) -> Option<FailureDomain> {
        self.topology_read()
            .failure_domains
            .iter()
            .find(|domain| domain.id == id)
            .cloned()
    }

    /// Returns true when the failure-domain identifier is configured.
    #[must_use]
    pub fn contains_failure_domain(&self, id: &str) -> bool {
        self.topology_read()
            .failure_domains
            .iter()
            .any(|domain| domain.id == id)
    }

    /// Returns the direct children of `parent_id`, sorted by id (empty when
    /// the parent does not exist).
    #[must_use]
    pub fn child_failure_domains(&self, parent_id: &str) -> Vec<FailureDomain> {
        self.topology_read()
            .failure_domains
            .iter()
            .filter(|domain| domain.parent.as_deref() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// Returns all failure domains owned by `az_id`, sorted by id.
    #[must_use]
    pub fn failure_domains_of_az(&self, az_id: &str) -> Vec<FailureDomain> {
        self.topology_read()
            .failure_domains
            .iter()
            .filter(|domain| domain.availability_domain == az_id)
            .cloned()
            .collect()
    }

    /// Returns all bindings of one failure domain, sorted by target.
    #[must_use]
    pub fn bindings_of(&self, failure_domain_id: &str) -> Vec<TopologyBinding> {
        self.topology_read()
            .bindings
            .iter()
            .filter(|binding| binding.failure_domain == failure_domain_id)
            .cloned()
            .collect()
    }

    /// Returns all bindings pointing at one target, sorted by failure domain.
    #[must_use]
    pub fn bindings_of_target(&self, kind: BindingTargetKind, id: &str) -> Vec<TopologyBinding> {
        self.topology_read()
            .bindings
            .iter()
            .filter(|binding| binding.target.kind == kind && binding.target.id == id)
            .cloned()
            .collect()
    }

    /// Returns the complete validated topology snapshot (all vectors sorted).
    #[must_use]
    pub fn snapshot(&self) -> TopologySnapshot {
        let topology = self.topology_read();
        TopologySnapshot {
            regions: self.regions.clone(),
            failure_domains: topology.failure_domains.clone(),
            bindings: topology.bindings.clone(),
        }
    }

    /// Validates that a service manifest references only canonical locations.
    ///
    /// A manifest's `regions` and `availability_domains` are *filters* over the
    /// canonical registry: every referenced id must already exist as canonical
    /// O3K location identity, and (when the manifest declares regional scope)
    /// every declared availability domain must belong to one of the regions the
    /// manifest itself advertises. This fails closed so a manifest can never
    /// invent location truth, drift from the registry, or silently depend on an
    /// availability domain whose region it does not advertise. Failure domains
    /// do not participate in manifest placement filtering.
    pub fn validate_manifest_locations(
        &self,
        manifest: &ServiceManifest,
    ) -> Result<(), LocationError> {
        let az_index: HashMap<&str, &str> = availability_domain_index(&self.regions);

        // The set of canonical regions this manifest explicitly advertises.
        // Empty means the manifest places globally (no regional restriction),
        // in which case any canonical AZ is acceptable.
        let declared_regions: BTreeSet<&str> =
            manifest.regions.iter().map(String::as_str).collect();

        for region in &manifest.regions {
            if !self.contains_region(region) {
                return Err(LocationError::UnknownRegion {
                    service: manifest.service_id.clone(),
                    region: region.clone(),
                });
            }
        }
        for az in &manifest.availability_domains {
            match az_index.get(az.as_str()) {
                None => {
                    return Err(LocationError::UnknownAvailabilityDomain {
                        service: manifest.service_id.clone(),
                        availability_domain: az.clone(),
                    });
                }
                Some(owning_region)
                    if !declared_regions.is_empty()
                        && !declared_regions.contains(owning_region) =>
                {
                    return Err(LocationError::AvailabilityDomainOutsideDeclaredRegions {
                        service: manifest.service_id.clone(),
                        availability_domain: az.clone(),
                        region: owning_region.to_string(),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Validates every manifest in `manifests` against the canonical registry.
    ///
    /// Used by the composition root to fail closed at startup when any
    /// registered service references an unknown location.
    pub fn validate_manifest_registry(
        &self,
        registry: &crate::ManifestRegistry,
    ) -> Result<(), LocationError> {
        for manifest in registry.all() {
            self.validate_manifest_locations(manifest)?;
        }
        Ok(())
    }

    /// Declares one canonical region durably.
    ///
    /// Re-declaring an existing region is an idempotent no-op success (replay
    /// convergence) and does not touch the store.
    ///
    /// Takes `&mut self`: regions live in a directly borrowable plain field (to
    /// preserve the reference-returning read API), so mutating them requires
    /// the caller to prove exclusivity. Composition roots declare topology
    /// before publishing the registry into shared state.
    pub async fn declare_region(
        &mut self,
        store: &dyn TopologyStore,
        id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        validate_region_id_str(id)?;
        if self.contains_region(id) {
            if let Some(audit) = audit {
                store.record_audit(audit).await?;
            }
            return Ok(());
        }
        store.insert_region(id, audit).await?;
        self.regions.push(RegionDeclaration {
            id: id.to_owned(),
            availability_domains: Vec::new(),
        });
        self.regions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(())
    }

    /// Removes one canonical region durably.
    ///
    /// Removing an absent region is an idempotent no-op success. A region that
    /// still has availability domains is rejected ([`LocationError::RegionHasAvailabilityDomains`]);
    /// the store enforces the same invariant.
    pub async fn remove_region(
        &mut self,
        store: &dyn TopologyStore,
        id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        validate_region_id_str(id)?;
        if !self.contains_region(id) {
            if let Some(audit) = audit {
                store.record_audit(audit).await?;
            }
            return Ok(());
        }
        if self
            .region(id)
            .is_some_and(|region| !region.availability_domains.is_empty())
        {
            return Err(LocationError::RegionHasAvailabilityDomains(id.to_owned()).into());
        }
        store.delete_region(id, audit).await?;
        self.regions.retain(|region| region.id != id);
        Ok(())
    }

    /// Declares one canonical availability domain inside `region_id` durably.
    ///
    /// Re-declaring an AZ that already belongs to `region_id` is an idempotent
    /// no-op success. Availability-domain IDs are globally unique: declaring an
    /// ID that already belongs to another region is rejected with
    /// [`LocationError::AmbiguousAvailabilityDomain`].
    pub async fn declare_availability_domain(
        &mut self,
        store: &dyn TopologyStore,
        region_id: &str,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        validate_region_id_str(region_id)?;
        validate_az_id_str(az_id)?;
        if !self.contains_region(region_id) {
            return Err(LocationError::UnknownTopologyRegion(region_id.to_owned()).into());
        }
        for region in &self.regions {
            if region.availability_domains.iter().any(|az| az.id == az_id) {
                if region.id == region_id {
                    // Same region: idempotent replay no-op. Still audited.
                    if let Some(audit) = audit {
                        store.record_audit(audit).await?;
                    }
                    return Ok(());
                }
                return Err(LocationError::AmbiguousAvailabilityDomain(az_id.to_owned()).into());
            }
        }
        store
            .insert_availability_domain(region_id, az_id, audit)
            .await?;
        // Exclusive access guarantees the region is still present and the AZ
        // still absent, so the validated apply is unconditional.
        if let Some(region) = self
            .regions
            .iter_mut()
            .find(|region| region.id == region_id)
        {
            region.availability_domains.push(AvailabilityDomain {
                id: az_id.to_owned(),
            });
            region.availability_domains.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Ok(())
    }

    /// Removes one canonical availability domain durably.
    ///
    /// Removing an absent AZ is an idempotent no-op success. An AZ that still
    /// has failure domains referencing it is rejected with
    /// [`LocationError::AvailabilityDomainHasFailureDomains`]; the store
    /// enforces the same invariant.
    pub async fn remove_availability_domain(
        &mut self,
        store: &dyn TopologyStore,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        validate_az_id_str(az_id)?;
        let az_known = self
            .regions
            .iter()
            .flat_map(|region| region.availability_domains.iter())
            .any(|az| az.id == az_id);
        if !az_known {
            if let Some(audit) = audit {
                store.record_audit(audit).await?;
            }
            return Ok(());
        }
        if self
            .topology_read()
            .failure_domains
            .iter()
            .any(|domain| domain.availability_domain == az_id)
        {
            return Err(
                LocationError::AvailabilityDomainHasFailureDomains(az_id.to_owned()).into(),
            );
        }
        store.delete_availability_domain(az_id, audit).await?;
        for region in &mut self.regions {
            region.availability_domains.retain(|az| az.id != az_id);
        }
        Ok(())
    }

    /// Creates one failure domain durably.
    ///
    /// The registry owns generation identity: the passed `generation` is
    /// ignored and the stored domain starts at generation 1. Validation covers
    /// ID alphabet and uniqueness, name bounds, AZ existence, parent existence
    /// and same-AZ rule, rank ordering (`child.rank() >= parent.rank()`), and
    /// cycle/depth caps on the parent chain.
    pub async fn create_failure_domain(
        &self,
        store: &dyn TopologyStore,
        mut domain: FailureDomain,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        {
            let topology = self.topology_read();
            validate_failure_domain_id(&domain.id)?;
            if topology
                .failure_domains
                .iter()
                .any(|known| known.id == domain.id)
            {
                return Err(LocationError::DuplicateFailureDomain(domain.id.clone()).into());
            }
            let az_index = availability_domain_index(&self.regions);
            validate_failure_domain_structure(&domain, &failure_domain_map(&topology), &az_index)?;
        }
        // The registry owns generation identity: creations always start at 1.
        domain.generation = 1;
        store.insert_failure_domain(&domain, audit).await?;
        let mut topology = self.topology_write();
        if !topology
            .failure_domains
            .iter()
            .any(|known| known.id == domain.id)
        {
            topology.failure_domains.push(domain);
            topology.failure_domains.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Ok(())
    }

    /// Updates the display name and metadata of one failure domain durably.
    ///
    /// `class`, `availability_domain`, `parent`, and `id` are immutable after
    /// creation; this method cannot change them. `expected_generation` must
    /// match the current generation or the call fails with
    /// [`LocationError::StaleFailureDomainGeneration`] and nothing is persisted.
    /// On success the generation is bumped by one.
    pub async fn update_failure_domain(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        name: &str,
        metadata: BTreeMap<String, String>,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        let updated = {
            let topology = self.topology_read();
            let current = topology
                .failure_domains
                .iter()
                .find(|domain| domain.id == id)
                .ok_or_else(|| LocationError::UnknownFailureDomain(id.to_owned()))?;
            if current.generation != expected_generation {
                return Err(LocationError::StaleFailureDomainGeneration {
                    failure_domain: id.to_owned(),
                    expected: expected_generation,
                    actual: current.generation,
                }
                .into());
            }
            validate_failure_domain_name(name)?;
            validate_metadata(&metadata)?;
            FailureDomain {
                id: current.id.clone(),
                class: current.class,
                name: name.to_owned(),
                availability_domain: current.availability_domain.clone(),
                parent: current.parent.clone(),
                generation: expected_generation + 1,
                metadata,
            }
        };
        store
            .update_failure_domain(&updated, expected_generation, audit)
            .await?;
        let mut topology = self.topology_write();
        match topology
            .failure_domains
            .iter_mut()
            .find(|domain| domain.id == id)
        {
            Some(slot) if slot.generation == expected_generation => {
                *slot = updated;
                Ok(())
            }
            Some(slot) => Err(LocationError::StaleFailureDomainGeneration {
                failure_domain: id.to_owned(),
                expected: expected_generation,
                actual: slot.generation,
            }
            .into()),
            None => Err(LocationError::UnknownFailureDomain(id.to_owned()).into()),
        }
    }

    /// Deletes one failure domain durably.
    ///
    /// Deletion requires the expected generation (optimistic concurrency) and
    /// fails while the domain still has child domains
    /// ([`LocationError::FailureDomainHasChildren`]) or bindings
    /// ([`LocationError::FailureDomainHasBindings`]). Deleting an unknown
    /// domain fails with [`LocationError::UnknownFailureDomain`].
    pub async fn delete_failure_domain(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        {
            let topology = self.topology_read();
            let current = topology
                .failure_domains
                .iter()
                .find(|domain| domain.id == id)
                .ok_or_else(|| LocationError::UnknownFailureDomain(id.to_owned()))?;
            if current.generation != expected_generation {
                return Err(LocationError::StaleFailureDomainGeneration {
                    failure_domain: id.to_owned(),
                    expected: expected_generation,
                    actual: current.generation,
                }
                .into());
            }
            if topology
                .failure_domains
                .iter()
                .any(|domain| domain.parent.as_deref() == Some(id))
            {
                return Err(LocationError::FailureDomainHasChildren(id.to_owned()).into());
            }
            if topology
                .bindings
                .iter()
                .any(|binding| binding.failure_domain == id)
            {
                return Err(LocationError::FailureDomainHasBindings(id.to_owned()).into());
            }
        }
        store
            .delete_failure_domain(id, expected_generation, audit)
            .await?;
        let mut topology = self.topology_write();
        match topology
            .failure_domains
            .iter()
            .find(|domain| domain.id == id)
        {
            Some(current) if current.generation == expected_generation => {
                topology.failure_domains.retain(|domain| domain.id != id);
                Ok(())
            }
            Some(current) => Err(LocationError::StaleFailureDomainGeneration {
                failure_domain: id.to_owned(),
                expected: expected_generation,
                actual: current.generation,
            }
            .into()),
            None => Err(LocationError::UnknownFailureDomain(id.to_owned()).into()),
        }
    }

    /// Binds one failure domain to one target durably.
    ///
    /// The failure domain must exist and the target ID must use the canonical
    /// location ID alphabet. Re-inserting an existing binding is an idempotent
    /// no-op success (replay convergence) and does not touch the store.
    /// Whether the target itself currently exists is owned by the respective
    /// domain, not by topology.
    pub async fn bind(
        &self,
        store: &dyn TopologyStore,
        binding: TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        let replay = {
            let topology = self.topology_read();
            if !topology
                .failure_domains
                .iter()
                .any(|domain| domain.id == binding.failure_domain)
            {
                return Err(
                    LocationError::UnknownFailureDomain(binding.failure_domain.clone()).into(),
                );
            }
            if !valid_location_id(&binding.target.id) {
                return Err(
                    LocationError::InvalidBindingTargetId(binding.target.id.clone()).into(),
                );
            }
            // Idempotent replay no-op: we record the audit below, after the read
            // lock is dropped (locks are never held across `.await`).
            topology.bindings.iter().any(|known| known == &binding)
        };
        if replay {
            if let Some(audit) = audit {
                store.record_audit(audit).await?;
            }
            return Ok(());
        }
        store.insert_binding(&binding, audit).await?;
        let mut topology = self.topology_write();
        if !topology.bindings.iter().any(|known| known == &binding) {
            topology.bindings.push(binding);
            topology
                .bindings
                .sort_by(|a, b| binding_key(a).cmp(&binding_key(b)));
        }
        Ok(())
    }

    /// Removes one binding durably.
    ///
    /// Removing an absent binding is an idempotent no-op success (replay
    /// convergence) and does not touch the store.
    pub async fn unbind(
        &self,
        store: &dyn TopologyStore,
        binding: TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        let replay = {
            let topology = self.topology_read();
            // Idempotent replay no-op: we record the audit below, after the read
            // lock is dropped (locks are never held across `.await`).
            !topology.bindings.iter().any(|known| known == &binding)
        };
        if replay {
            if let Some(audit) = audit {
                store.record_audit(audit).await?;
            }
            return Ok(());
        }
        store.delete_binding(&binding, audit).await?;
        self.topology_write()
            .bindings
            .retain(|known| known != &binding);
        Ok(())
    }

    /// Acquires the topology read lock, recovering from poisoning so a
    /// panicking writer cannot wedge every subsequent topology read.
    fn topology_read(&self) -> RwLockReadGuard<'_, TopologyState> {
        self.topology
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquires the topology write lock, recovering from poisoning.
    fn topology_write(&self) -> RwLockWriteGuard<'_, TopologyState> {
        self.topology
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn validate_region_id_str(id: &str) -> Result<(), LocationError> {
    if id.is_empty() {
        return Err(LocationError::EmptyRegionId);
    }
    if !valid_location_id(id) {
        return Err(LocationError::MalformedRegionId(id.to_owned()));
    }
    Ok(())
}

fn validate_az_id_str(id: &str) -> Result<(), LocationError> {
    if id.is_empty() {
        return Err(LocationError::EmptyAvailabilityDomainId);
    }
    if !valid_location_id(id) {
        return Err(LocationError::MalformedAvailabilityDomainId(id.to_owned()));
    }
    Ok(())
}

fn validate_failure_domain_id(id: &str) -> Result<(), LocationError> {
    if !valid_location_id(id) {
        return Err(LocationError::MalformedFailureDomainId(id.to_owned()));
    }
    Ok(())
}

fn validate_failure_domain_name(name: &str) -> Result<(), LocationError> {
    if name.is_empty() {
        return Err(LocationError::EmptyFailureDomainName);
    }
    if name.len() > MAX_FAILURE_DOMAIN_NAME_LEN {
        return Err(LocationError::FailureDomainNameTooLong(name.to_owned()));
    }
    Ok(())
}

fn validate_metadata(metadata: &BTreeMap<String, String>) -> Result<(), LocationError> {
    if metadata.len() > MAX_METADATA_ENTRIES {
        return Err(LocationError::TooManyMetadataEntries(metadata.len()));
    }
    for (key, value) in metadata {
        if !valid_metadata_key(key) {
            return Err(LocationError::InvalidMetadataKey(key.clone()));
        }
        if value.len() > MAX_METADATA_VALUE_LEN {
            return Err(LocationError::MetadataValueTooLong(key.clone()));
        }
    }
    Ok(())
}

/// Validates the region declarations shared by [`LocationRegistry::from_declarations`]
/// and [`LocationRegistry::from_snapshot`].
fn validate_region_declarations(regions: &[RegionDeclaration]) -> Result<(), LocationError> {
    let mut seen_regions = HashSet::new();
    // Tracks availability-domain -> region for cross-region duplicates.
    let mut az_to_region: HashMap<&str, &str> = HashMap::new();

    for region in regions {
        validate_region_id_str(&region.id)?;
        if !seen_regions.insert(region.id.as_str()) {
            return Err(LocationError::DuplicateRegion(region.id.clone()));
        }
        let mut seen_az_in_region = HashSet::new();
        for az in &region.availability_domains {
            validate_az_id_str(&az.id)?;
            if !seen_az_in_region.insert(az.id.as_str()) {
                return Err(LocationError::DuplicateAvailabilityDomainInRegion(
                    az.id.clone(),
                    region.id.clone(),
                ));
            }
            if az_to_region
                .insert(az.id.as_str(), region.id.as_str())
                .is_some()
            {
                return Err(LocationError::AmbiguousAvailabilityDomain(az.id.clone()));
            }
        }
    }
    Ok(())
}

/// Maps every availability domain to its owning region.
fn availability_domain_index(regions: &[RegionDeclaration]) -> HashMap<&str, &str> {
    regions
        .iter()
        .flat_map(|region| {
            region
                .availability_domains
                .iter()
                .map(move |az| (az.id.as_str(), region.id.as_str()))
        })
        .collect()
}

/// Indexes failure domains by ID for hierarchy validation.
fn failure_domain_map(topology: &TopologyState) -> HashMap<String, FailureDomain> {
    topology
        .failure_domains
        .iter()
        .map(|domain| (domain.id.clone(), domain.clone()))
        .collect()
}

/// Validates one failure domain against the given topology context.
///
/// `context` maps failure-domain ID to domain and is used to resolve and walk
/// parent chains; `az_index` maps availability-domain ID to its owning region
/// and answers AZ existence. Callers validate ID uniqueness before invoking
/// this (duplicate IDs would make parent resolution ambiguous).
fn validate_failure_domain_structure(
    domain: &FailureDomain,
    context: &HashMap<String, FailureDomain>,
    az_index: &HashMap<&str, &str>,
) -> Result<(), LocationError> {
    validate_failure_domain_id(&domain.id)?;
    validate_failure_domain_name(&domain.name)?;
    validate_metadata(&domain.metadata)?;
    if !az_index.contains_key(domain.availability_domain.as_str()) {
        return Err(LocationError::UnknownFailureDomainAvailabilityDomain(
            domain.id.clone(),
            domain.availability_domain.clone(),
        ));
    }

    let Some(parent_id) = domain.parent.as_ref() else {
        return Ok(());
    };
    if parent_id == &domain.id {
        return Err(LocationError::FailureDomainSelfParent(domain.id.clone()));
    }
    let parent = context.get(parent_id).ok_or_else(|| {
        LocationError::FailureDomainParentMissing(domain.id.clone(), parent_id.clone())
    })?;
    if parent.availability_domain != domain.availability_domain {
        return Err(LocationError::FailureDomainOutsideAvailabilityDomain {
            failure_domain: domain.id.clone(),
            parent: parent_id.clone(),
            availability_domain: domain.availability_domain.clone(),
            parent_availability_domain: parent.availability_domain.clone(),
        });
    }
    if domain.class.rank() < parent.class.rank() {
        return Err(LocationError::InvalidFailureDomainNesting {
            failure_domain: domain.id.clone(),
            class: domain.class,
            parent: parent_id.clone(),
            parent_class: parent.class,
        });
    }

    // Cycle and depth: walk the parent chain from the immediate parent. On the
    // mutation path this walk is defence in depth (parent is immutable after a
    // validated create, so chains are acyclic by construction); snapshot
    // reconstruction validates arbitrary store content, where cycles are
    // possible and must be rejected here.
    let mut visited = HashSet::new();
    visited.insert(domain.id.as_str());
    let mut current = parent;
    let mut depth = 2; // the domain itself plus its parent
    loop {
        if !visited.insert(current.id.as_str()) {
            return Err(LocationError::FailureDomainCycle(domain.id.clone()));
        }
        if depth > MAX_FAILURE_DOMAIN_DEPTH {
            return Err(LocationError::FailureDomainHierarchyTooDeep(
                domain.id.clone(),
            ));
        }
        let Some(next_id) = current.parent.as_ref() else {
            break;
        };
        // A missing grandparent is reported as FailureDomainParentMissing when
        // that node itself is validated; stop walking here.
        let Some(next) = context.get(next_id) else {
            break;
        };
        current = next;
        depth += 1;
    }
    Ok(())
}

/// Deterministic ordering key for bindings.
fn binding_key(binding: &TopologyBinding) -> (&str, &str, &str) {
    (
        binding.failure_domain.as_str(),
        binding.target.kind.as_str(),
        binding.target.id.as_str(),
    )
}

fn sorted_regions(mut regions: Vec<RegionDeclaration>) -> Vec<RegionDeclaration> {
    for region in &mut regions {
        region.availability_domains.sort_by(|a, b| a.id.cmp(&b.id));
    }
    regions.sort_by(|a, b| a.id.cmp(&b.id));
    regions
}

fn sorted_failure_domains(mut domains: Vec<FailureDomain>) -> Vec<FailureDomain> {
    domains.sort_by(|a, b| a.id.cmp(&b.id));
    domains
}

fn sorted_bindings(mut bindings: Vec<TopologyBinding>) -> Vec<TopologyBinding> {
    bindings.sort_by(|a, b| binding_key(a).cmp(&binding_key(b)));
    bindings
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn declaration(id: &str, azs: &[&str]) -> RegionDeclaration {
        RegionDeclaration {
            id: id.to_owned(),
            availability_domains: azs
                .iter()
                .map(|az| AvailabilityDomain { id: az.to_string() })
                .collect(),
        }
    }

    fn registry(declarations: Vec<RegionDeclaration>) -> LocationRegistry {
        LocationRegistry::from_declarations(declarations).expect("valid declarations")
    }

    fn failure_domain(
        id: &str,
        class: FailureDomainClass,
        az: &str,
        parent: Option<&str>,
    ) -> FailureDomain {
        FailureDomain {
            id: id.to_owned(),
            class,
            name: id.to_owned(),
            availability_domain: az.to_owned(),
            parent: parent.map(str::to_owned),
            generation: 1,
            metadata: BTreeMap::new(),
        }
    }

    fn binding(failure_domain: &str, kind: BindingTargetKind, id: &str) -> TopologyBinding {
        TopologyBinding {
            failure_domain: failure_domain.to_owned(),
            target: BindingTarget {
                kind,
                id: id.to_owned(),
            },
        }
    }

    /// Builds a registry and a consistent in-memory store by declaring the
    /// regions/AZs through the durable mutation path, so store and memory
    /// agree the way a real composition root arranges them.
    async fn registry_with_store(
        declarations: Vec<RegionDeclaration>,
    ) -> (LocationRegistry, MemoryTopologyStore) {
        let store = MemoryTopologyStore::default();
        let mut registry = LocationRegistry::default();
        for region in declarations {
            registry
                .declare_region(&store, &region.id, None)
                .await
                .unwrap();
            for az in region.availability_domains {
                registry
                    .declare_availability_domain(&store, &region.id, &az.id, None)
                    .await
                    .unwrap();
            }
        }
        (registry, store)
    }

    /// In-memory [`TopologyStore`] double enforcing the same uniqueness,
    /// referential-integrity, and generation rules a real store must enforce.
    #[derive(Debug, Default)]
    struct MemoryTopologyStore {
        state: Mutex<MemoryStoreState>,
    }

    #[derive(Debug, Default)]
    struct MemoryStoreState {
        regions: BTreeSet<String>,
        az_to_region: BTreeMap<String, String>,
        failure_domains: BTreeMap<String, FailureDomain>,
        bindings: BTreeSet<(String, BindingTargetKind, String)>,
        audit_events: Vec<AuditEvent>,
    }

    impl MemoryTopologyStore {
        fn state(&self) -> std::sync::MutexGuard<'_, MemoryStoreState> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        /// Pushes the caller's audit event onto an already-held store-state guard.
        /// The mutation methods already hold the lock, so we must never
        /// re-acquire it here (a `std::sync::Mutex` is not reentrant).
        fn push_audit(state: &mut MemoryStoreState, audit: Option<&AuditEvent>) {
            if let Some(audit) = audit {
                state.audit_events.push(audit.clone());
            }
        }
    }

    #[async_trait]
    impl TopologyStore for MemoryTopologyStore {
        async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
            let state = self.state();
            let mut regions: BTreeMap<String, RegionDeclaration> = BTreeMap::new();
            for region_id in &state.regions {
                regions.insert(
                    region_id.clone(),
                    RegionDeclaration {
                        id: region_id.clone(),
                        availability_domains: Vec::new(),
                    },
                );
            }
            for (az, region) in &state.az_to_region {
                regions
                    .entry(region.clone())
                    .or_insert_with(|| RegionDeclaration {
                        id: region.clone(),
                        availability_domains: Vec::new(),
                    })
                    .availability_domains
                    .push(AvailabilityDomain { id: az.clone() });
            }
            Ok(TopologySnapshot {
                regions: sorted_regions(regions.into_values().collect()),
                failure_domains: state.failure_domains.values().cloned().collect(),
                bindings: sorted_bindings(
                    state
                        .bindings
                        .iter()
                        .map(|(fd, kind, id)| binding(fd, *kind, id))
                        .collect(),
                ),
            })
        }

        async fn insert_region(
            &self,
            region_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.regions.insert(region_id.to_owned()) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate region '{region_id}'"
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_region(
            &self,
            region_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if state
                .az_to_region
                .values()
                .any(|region| region == region_id)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "region '{region_id}' still has availability domains"
                )));
            }
            state.regions.remove(region_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_availability_domain(
            &self,
            region_id: &str,
            az_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.regions.contains(region_id) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "unknown region '{region_id}'"
                )));
            }
            if state
                .az_to_region
                .insert(az_id.to_owned(), region_id.to_owned())
                .is_some()
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate availability domain '{az_id}'"
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_availability_domain(
            &self,
            az_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if state
                .failure_domains
                .values()
                .any(|domain| domain.availability_domain == az_id)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "availability domain '{az_id}' still has failure domains"
                )));
            }
            state.az_to_region.remove(az_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_failure_domain(
            &self,
            domain: &FailureDomain,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.az_to_region.contains_key(&domain.availability_domain) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "failure domain '{}' references unknown availability domain '{}'",
                    domain.id, domain.availability_domain
                )));
            }
            if let Some(parent) = &domain.parent
                && !state.failure_domains.contains_key(parent)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "failure domain '{}' has dangling parent '{parent}'",
                    domain.id
                )));
            }
            if state
                .failure_domains
                .insert(domain.id.clone(), domain.clone())
                .is_some()
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate failure domain '{}'",
                    domain.id
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn update_failure_domain(
            &self,
            domain: &FailureDomain,
            expected_generation: u64,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            let stored = state.failure_domains.get(&domain.id).ok_or_else(|| {
                KernelError::TopologyCorrupt(format!("unknown failure domain '{}'", domain.id))
            })?;
            if stored.generation != expected_generation {
                return Err(KernelError::TopologyCorrupt(format!(
                    "stale generation for failure domain '{}' (expected {expected_generation})",
                    domain.id
                )));
            }
            state
                .failure_domains
                .insert(domain.id.clone(), domain.clone());
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_failure_domain(
            &self,
            domain_id: &str,
            expected_generation: u64,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            let stored = state.failure_domains.get(domain_id).ok_or_else(|| {
                KernelError::TopologyCorrupt(format!("unknown failure domain '{domain_id}'"))
            })?;
            if stored.generation != expected_generation {
                return Err(KernelError::TopologyCorrupt(format!(
                    "stale generation for failure domain '{domain_id}' (expected {expected_generation})"
                )));
            }
            state.failure_domains.remove(domain_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_binding(
            &self,
            binding: &TopologyBinding,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.failure_domains.contains_key(&binding.failure_domain) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "binding references unknown failure domain '{}'",
                    binding.failure_domain
                )));
            }
            let key = (
                binding.failure_domain.clone(),
                binding.target.kind,
                binding.target.id.clone(),
            );
            if !state.bindings.insert(key) {
                return Err(KernelError::TopologyCorrupt("duplicate binding".to_owned()));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_binding(
            &self,
            binding: &TopologyBinding,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            state.bindings.remove(&(
                binding.failure_domain.clone(),
                binding.target.kind,
                binding.target.id.clone(),
            ));
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn list_failure_domains(
            &self,
            after_id: Option<&str>,
            limit: usize,
        ) -> Result<Vec<FailureDomain>, KernelError> {
            let state = self.state();
            Ok(state
                .failure_domains
                .values()
                .filter(|domain| after_id.is_none_or(|after| domain.id.as_str() > after))
                .take(limit.saturating_add(1))
                .cloned()
                .collect())
        }

        async fn list_bindings(
            &self,
            after: Option<&BindingListCursor>,
            limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            let state = self.state();
            Ok(state
                .bindings
                .iter()
                .filter(|(fd, kind, id)| {
                    after.is_none_or(|cursor| {
                        (fd.as_str(), kind.as_str(), id.as_str())
                            > (
                                cursor.failure_domain.as_str(),
                                cursor.target_kind.as_str(),
                                cursor.target_id.as_str(),
                            )
                    })
                })
                .take(limit.saturating_add(1))
                .map(|(fd, kind, id)| binding(fd, *kind, id))
                .collect())
        }

        async fn list_bindings_of(
            &self,
            failure_domain: &str,
            after: Option<&BindingListCursor>,
            limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            let state = self.state();
            Ok(state
                .bindings
                .iter()
                .filter(|(fd, _, _)| fd == failure_domain)
                .filter(|(_, kind, id)| {
                    after.is_none_or(|cursor| {
                        (kind.as_str(), id.as_str())
                            > (cursor.target_kind.as_str(), cursor.target_id.as_str())
                    })
                })
                .take(limit.saturating_add(1))
                .map(|(fd, kind, id)| binding(fd, *kind, id))
                .collect())
        }

        async fn record_audit(&self, audit: &AuditEvent) -> Result<(), KernelError> {
            Self::push_audit(&mut self.state(), Some(audit));
            Ok(())
        }
    }

    /// Store double that fails every mutation, to prove memory is untouched on
    /// store errors.
    struct UnavailableTopologyStore;

    #[async_trait]
    impl TopologyStore for UnavailableTopologyStore {
        async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn insert_region(
            &self,
            _region_id: &str,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn delete_region(
            &self,
            _region_id: &str,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn insert_availability_domain(
            &self,
            _region_id: &str,
            _az_id: &str,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn delete_availability_domain(
            &self,
            _az_id: &str,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn insert_failure_domain(
            &self,
            _domain: &FailureDomain,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn update_failure_domain(
            &self,
            _domain: &FailureDomain,
            _expected_generation: u64,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn delete_failure_domain(
            &self,
            _domain_id: &str,
            _expected_generation: u64,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn insert_binding(
            &self,
            _binding: &TopologyBinding,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn delete_binding(
            &self,
            _binding: &TopologyBinding,
            _audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn list_failure_domains(
            &self,
            _after_id: Option<&str>,
            _limit: usize,
        ) -> Result<Vec<FailureDomain>, KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn list_bindings(
            &self,
            _after: Option<&BindingListCursor>,
            _limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn list_bindings_of(
            &self,
            _failure_domain: &str,
            _after: Option<&BindingListCursor>,
            _limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }

        async fn record_audit(&self, _audit: &AuditEvent) -> Result<(), KernelError> {
            Err(KernelError::TopologyUnavailable("injected".to_owned()))
        }
    }

    #[test]
    fn rejects_empty_region_id() {
        let err = LocationRegistry::from_declarations(vec![declaration("", &[])]).unwrap_err();
        assert_eq!(err, LocationError::EmptyRegionId);
    }

    #[test]
    fn rejects_empty_availability_domain_id() {
        let err =
            LocationRegistry::from_declarations(vec![declaration("region-a", &[""])]).unwrap_err();
        assert_eq!(err, LocationError::EmptyAvailabilityDomainId);
    }

    #[test]
    fn rejects_duplicate_region_ids() {
        let err = LocationRegistry::from_declarations(vec![
            declaration("region-a", &[]),
            declaration("region-a", &[]),
        ])
        .unwrap_err();
        assert_eq!(err, LocationError::DuplicateRegion("region-a".to_owned()));
    }

    #[test]
    fn rejects_duplicate_az_in_region() {
        let err =
            LocationRegistry::from_declarations(vec![declaration("region-a", &["az-1", "az-1"])])
                .unwrap_err();
        assert_eq!(
            err,
            LocationError::DuplicateAvailabilityDomainInRegion(
                "az-1".to_owned(),
                "region-a".to_owned()
            )
        );
    }

    #[test]
    fn rejects_az_across_two_regions() {
        let err = LocationRegistry::from_declarations(vec![
            declaration("region-a", &["az-1"]),
            declaration("region-b", &["az-1"]),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::AmbiguousAvailabilityDomain("az-1".to_owned())
        );
    }

    #[test]
    fn rejects_malformed_region_id() {
        let err = LocationRegistry::from_declarations(vec![declaration("Two Regions!", &[])])
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::MalformedRegionId("Two Regions!".to_owned())
        );
    }

    #[test]
    fn rejects_region_id_with_leading_punctuation() {
        // Matches the public contract pattern ^[a-z0-9]... : a leading `-` or
        // `_` is invalid even though those characters are legal in the rest.
        for id in ["-east", "_east"] {
            let err = LocationRegistry::from_declarations(vec![declaration(id, &[])]).unwrap_err();
            assert_eq!(err, LocationError::MalformedRegionId(id.to_owned()));
        }
    }

    #[test]
    fn rejects_malformed_az_id_with_uppercase() {
        let err = LocationRegistry::from_declarations(vec![declaration("region-a", &["AZ-1"])])
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::MalformedAvailabilityDomainId("AZ-1".to_owned())
        );
    }

    #[test]
    fn sorted_deterministically() {
        let registry = registry(vec![
            declaration("region-b", &["az-2", "az-1"]),
            declaration("region-a", &["az-3"]),
        ]);
        let regions = registry.regions();
        let ids: Vec<&str> = regions.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["region-a", "region-b"]);
        let region_b_azs = registry.availability_domains_of("region-b");
        let azs: Vec<&str> = region_b_azs.iter().map(|az| az.id.as_str()).collect();
        assert_eq!(azs, vec!["az-1", "az-2"]);
    }

    #[test]
    fn region_with_multiple_availability_domains() {
        let registry = registry(vec![declaration("region-a", &["az-1", "az-2", "az-3"])]);
        assert_eq!(registry.regions().len(), 1);
        assert_eq!(registry.availability_domains_of("region-a").len(), 3);
    }

    #[test]
    fn validate_manifest_rejects_unknown_region() {
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let mut manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["not-a-region".to_owned()],
            availability_domains: vec![],
            controller: None,
            health: None,
        };
        // A manifest must still carry at least the structural fields validate()
        // would require; here we only exercise location reference checking.
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownRegion {
                service: "compute".to_owned(),
                region: "not-a-region".to_owned()
            }
        );
        manifest.regions = vec!["region-a".to_owned()];
        assert!(registry.validate_manifest_locations(&manifest).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_unknown_availability_domain() {
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec!["missing-az".to_owned()],
            controller: None,
            health: None,
        };
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownAvailabilityDomain {
                service: "compute".to_owned(),
                availability_domain: "missing-az".to_owned()
            }
        );
    }

    #[test]
    fn validate_manifest_rejects_az_outside_declared_regions() {
        let registry = LocationRegistry::from_declarations(vec![
            declaration("region-a", &["az-a"]),
            declaration("region-b", &["az-b"]),
        ])
        .unwrap();
        // AZ `az-b` is canonical but belongs to region-b; a manifest that only
        // declares region-a must not silently depend on it.
        let manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-b".to_owned()],
            controller: None,
            health: None,
        };
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::AvailabilityDomainOutsideDeclaredRegions {
                service: "compute".to_owned(),
                availability_domain: "az-b".to_owned(),
                region: "region-b".to_owned()
            }
        );
        // The same AZ inside the declared region is valid.
        let ok_manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-a".to_owned()],
            controller: None,
            health: None,
        };
        assert!(registry.validate_manifest_locations(&ok_manifest).is_ok());
    }

    #[test]
    fn region_identity_is_provider_independent_and_structurally_closed() {
        // Public region identity is purely the declared canonical id: providers
        // do not participate in location identity at all. Two configurations
        // that share region/AZ ids expose identical public topology regardless
        // of declaration order (which is how a hypothetical provider change
        // would otherwise surface).
        let a = registry(vec![
            declaration("region-a", &["az-1"]),
            declaration("region-b", &[]),
        ]);
        let b = registry(vec![
            declaration("region-b", &[]),
            declaration("region-a", &["az-1"]),
        ]);
        assert_eq!(a.regions(), b.regions());
        let serialized_a: serde_json::Value = serde_json::to_value(&a).unwrap();
        let serialized_b: serde_json::Value = serde_json::to_value(&b).unwrap();
        assert_eq!(serialized_a, serialized_b);

        // Structural closure: the serialized public topology carries exactly the
        // location-identity keys and nothing else, so provider/host/backend
        // identity cannot leak into tenant-facing location data.
        for region in serialized_a
            .get("regions")
            .and_then(serde_json::Value::as_array)
            .unwrap()
        {
            let mut keys: Vec<&str> = region
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort();
            assert_eq!(keys, vec!["availability_domains", "id"]);
            for az in region
                .get("availability_domains")
                .and_then(serde_json::Value::as_array)
                .unwrap()
            {
                let mut az_keys: Vec<&str> =
                    az.as_object().unwrap().keys().map(String::as_str).collect();
                az_keys.sort();
                assert_eq!(az_keys, vec!["id"]);
            }
        }
    }

    #[test]
    fn validate_manifest_registry_rejects_unknown_region_across_manifests() {
        use crate::manifest::{ManifestController, RegisteredResourceType, ResourceScope};
        use crate::resource::ResourceType;
        use crate::{ManifestRegistry, ServiceOwnership};
        // Two manifests: one references only canonical locations, the other
        // references an unknown region. The registry-level validator must fail
        // closed on the bad one.
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let mut manifest_registry = ManifestRegistry::new();
        let controller = Some(ManifestController {
            mode: "in-process".to_owned(),
            protocol: "in-process".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: None,
        });
        let good = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new_unchecked("compute", "server"),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-1".to_owned()],
            controller: controller.clone(),
            health: None,
        };
        let bad = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "network".to_owned(),
            namespace: "network".to_owned(),
            service_version: "1".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new_unchecked("network", "address_realm"),
                schema_version: "v1".to_owned(),
                collection: Some("address-realms".to_owned()),
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec!["network:ListAddressRealms".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-b".to_owned()],
            availability_domains: vec![],
            controller,
            health: None,
        };
        manifest_registry.register(good).unwrap();
        manifest_registry.register(bad).unwrap();
        let err = registry
            .validate_manifest_registry(&manifest_registry)
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownRegion {
                service: "network".to_owned(),
                region: "region-b".to_owned()
            }
        );
    }

    #[test]
    fn failure_domain_class_ranks_and_wire_names() {
        assert_eq!(FailureDomainClass::Site.rank(), 10);
        assert_eq!(FailureDomainClass::Building.rank(), 20);
        assert_eq!(FailureDomainClass::Room.rank(), 30);
        assert_eq!(FailureDomainClass::Row.rank(), 40);
        assert_eq!(FailureDomainClass::Rack.rank(), 50);
        assert_eq!(FailureDomainClass::Chassis.rank(), 60);
        assert_eq!(FailureDomainClass::PowerDomain.rank(), 70);
        assert_eq!(FailureDomainClass::NetworkDomain.rank(), 70);
        assert_eq!(FailureDomainClass::StorageDomain.rank(), 70);
        // Kebab-case wire names for both topology enums.
        assert_eq!(
            serde_json::to_string(&FailureDomainClass::PowerDomain).unwrap(),
            "\"power-domain\""
        );
        assert_eq!(
            serde_json::to_string(&BindingTargetKind::ResourceProvider).unwrap(),
            "\"resource-provider\""
        );
        assert_eq!(
            serde_json::from_str::<FailureDomainClass>("\"storage-domain\"").unwrap(),
            FailureDomainClass::StorageDomain
        );
    }

    #[tokio::test]
    async fn failure_domain_create_lists_sorted_and_starts_at_generation_one() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        let mut second = failure_domain("rack-b", FailureDomainClass::Rack, "az-1", None);
        second.generation = 42; // registry owns generation identity
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-c", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(&store, second, None)
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-a", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let domains = registry.failure_domains();
        let ids: Vec<&str> = domains.iter().map(|domain| domain.id.as_str()).collect();
        assert_eq!(ids, vec!["rack-a", "rack-b", "rack-c"]);
        assert!(domains.iter().all(|domain| domain.generation == 1));
    }

    #[tokio::test]
    async fn failure_domain_create_rejects_malformed_and_duplicate_ids() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("Bad ID!", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::MalformedFailureDomainId(
                "Bad ID!".to_owned()
            ))
        );

        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::DuplicateFailureDomain("rack-1".to_owned()))
        );
        assert_eq!(registry.failure_domains().len(), 1);
    }

    #[tokio::test]
    async fn failure_domain_create_rejects_bad_names() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        let mut empty_name = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        empty_name.name = String::new();
        let err = registry
            .create_failure_domain(&store, empty_name, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::EmptyFailureDomainName)
        );

        let mut long_name = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        long_name.name = "x".repeat(MAX_FAILURE_DOMAIN_NAME_LEN + 1);
        let err = registry
            .create_failure_domain(&store, long_name, None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TopologyError::Location(LocationError::FailureDomainNameTooLong(_))
        ));

        let mut max_name = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        max_name.name = "x".repeat(MAX_FAILURE_DOMAIN_NAME_LEN);
        registry
            .create_failure_domain(&store, max_name, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failure_domain_create_rejects_unknown_availability_domain() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-missing", None),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::UnknownFailureDomainAvailabilityDomain(
                "rack-1".to_owned(),
                "az-missing".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn failure_domain_create_rejects_self_parent_and_missing_parent() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", Some("rack-1")),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainSelfParent("rack-1".to_owned()))
        );

        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", Some("ghost")),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainParentMissing(
                "rack-1".to_owned(),
                "ghost".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn failure_domain_create_rejects_cross_availability_domain_parent() {
        let (registry, store) =
            registry_with_store(vec![declaration("region-a", &["az-1", "az-2"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-2", FailureDomainClass::Rack, "az-2", Some("rack-1")),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainOutsideAvailabilityDomain {
                failure_domain: "rack-2".to_owned(),
                parent: "rack-1".to_owned(),
                availability_domain: "az-2".to_owned(),
                parent_availability_domain: "az-1".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn failure_domain_nesting_respects_class_rank() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        // Building under rack: coarser (20) under finer (50) is rejected.
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let err = registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "building-1",
                    FailureDomainClass::Building,
                    "az-1",
                    Some("rack-1"),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::InvalidFailureDomainNesting {
                failure_domain: "building-1".to_owned(),
                class: FailureDomainClass::Building,
                parent: "rack-1".to_owned(),
                parent_class: FailureDomainClass::Rack,
            })
        );

        // Rack under building: finer (50) under coarser (20) is fine, and so is
        // an equal-rank sibling chain (rack under rack).
        registry
            .create_failure_domain(
                &store,
                failure_domain("building-1", FailureDomainClass::Building, "az-1", None),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "rack-2",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("building-1"),
                ),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-3", FailureDomainClass::Rack, "az-1", Some("rack-2")),
                None,
            )
            .await
            .unwrap();

        // Equal rank across the shared-infrastructure classes is allowed.
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "power-1",
                    FailureDomainClass::PowerDomain,
                    "az-1",
                    Some("rack-3"),
                ),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "net-1",
                    FailureDomainClass::NetworkDomain,
                    "az-1",
                    Some("power-1"),
                ),
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failure_domain_hierarchy_depth_cap() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        // A 64-deep chain (all equal rank) is accepted.
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-001", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        for depth in 2..=MAX_FAILURE_DOMAIN_DEPTH {
            let id = format!("rack-{depth:03}");
            let parent = format!("rack-{:03}", depth - 1);
            registry
                .create_failure_domain(
                    &store,
                    failure_domain(&id, FailureDomainClass::Rack, "az-1", Some(&parent)),
                    None,
                )
                .await
                .unwrap_or_else(|err| panic!("depth {depth} must be accepted: {err}"));
        }
        assert_eq!(registry.failure_domains().len(), MAX_FAILURE_DOMAIN_DEPTH);

        // The 65th level is rejected.
        let err = registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "rack-065",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("rack-064"),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainHierarchyTooDeep(
                "rack-065".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn failure_domain_metadata_bounds() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        // Too many entries.
        let mut too_many = BTreeMap::new();
        for index in 0..=MAX_METADATA_ENTRIES {
            too_many.insert(format!("key-{index:02}"), "value".to_owned());
        }
        let err = registry
            .update_failure_domain(&store, "rack-1", "rack", too_many.clone(), 1, None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            TopologyError::Location(LocationError::UnknownFailureDomain(_))
        ));

        // Create first, then test metadata validation on update (creation uses
        // the same validator; exercise both paths).
        let mut domain = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        domain.metadata = too_many;
        let err = registry
            .create_failure_domain(&store, domain, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::TooManyMetadataEntries(
                MAX_METADATA_ENTRIES + 1
            ))
        );

        for bad_key in [
            "",
            "Bad Key",
            ".leading-dot",
            "_leading-underscore",
            &"k".repeat(MAX_METADATA_KEY_LEN + 1),
        ] {
            let mut domain = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
            domain.metadata = BTreeMap::from([(bad_key.to_owned(), "value".to_owned())]);
            let err = registry
                .create_failure_domain(&store, domain, None)
                .await
                .unwrap_err();
            assert_eq!(
                err,
                TopologyError::Location(LocationError::InvalidMetadataKey(bad_key.to_owned())),
                "key {bad_key:?}"
            );
        }

        // Values are length-capped (key is reported for diagnostics).
        let mut domain = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        domain.metadata =
            BTreeMap::from([("note".to_owned(), "x".repeat(MAX_METADATA_VALUE_LEN + 1))]);
        let err = registry
            .create_failure_domain(&store, domain, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::MetadataValueTooLong("note".to_owned()))
        );

        // Boundary-valid metadata is accepted.
        let mut domain = failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None);
        domain.metadata = BTreeMap::from([
            (
                "key.with-dots_and-dashes".to_owned(),
                "x".repeat(MAX_METADATA_VALUE_LEN),
            ),
            ("k".repeat(MAX_METADATA_KEY_LEN), "v".to_owned()),
        ]);
        for index in 2..MAX_METADATA_ENTRIES {
            domain
                .metadata
                .insert(format!("k{index:02}"), "v".to_owned());
        }
        assert_eq!(domain.metadata.len(), MAX_METADATA_ENTRIES);
        registry
            .create_failure_domain(&store, domain, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failure_domain_update_bumps_generation_and_rejects_stale() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let stale = registry
            .update_failure_domain(&store, "rack-1", "renamed", BTreeMap::new(), 0, None)
            .await
            .unwrap_err();
        assert_eq!(
            stale,
            TopologyError::Location(LocationError::StaleFailureDomainGeneration {
                failure_domain: "rack-1".to_owned(),
                expected: 0,
                actual: 1,
            })
        );

        registry
            .update_failure_domain(
                &store,
                "rack-1",
                "renamed rack",
                BTreeMap::from([("note".to_owned(), "hello".to_owned())]),
                1,
                None,
            )
            .await
            .unwrap();
        let updated = registry.failure_domain("rack-1").unwrap();
        assert_eq!(updated.name, "renamed rack");
        assert_eq!(updated.generation, 2);
        assert_eq!(
            updated.metadata.get("note").map(String::as_str),
            Some("hello")
        );

        // The old generation is now stale for further updates and deletes.
        let stale = registry
            .update_failure_domain(&store, "rack-1", "again", BTreeMap::new(), 1, None)
            .await
            .unwrap_err();
        assert!(matches!(
            stale,
            TopologyError::Location(LocationError::StaleFailureDomainGeneration { .. })
        ));
        let stale = registry
            .delete_failure_domain(&store, "rack-1", 1, None)
            .await
            .unwrap_err();
        assert!(matches!(
            stale,
            TopologyError::Location(LocationError::StaleFailureDomainGeneration { .. })
        ));

        // Unknown domains cannot be updated or deleted.
        let err = registry
            .update_failure_domain(&store, "ghost", "x", BTreeMap::new(), 1, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::UnknownFailureDomain("ghost".to_owned()))
        );
        let err = registry
            .delete_failure_domain(&store, "ghost", 1, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::UnknownFailureDomain("ghost".to_owned()))
        );
    }

    #[tokio::test]
    async fn failure_domain_update_rejects_invalid_name_and_metadata() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let err = registry
            .update_failure_domain(&store, "rack-1", "", BTreeMap::new(), 1, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::EmptyFailureDomainName)
        );
        let err = registry
            .update_failure_domain(
                &store,
                "rack-1",
                "ok",
                BTreeMap::from([("Bad Key".to_owned(), "v".to_owned())]),
                1,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::InvalidMetadataKey("Bad Key".to_owned()))
        );
        // Nothing was persisted for either rejection.
        let current = registry.failure_domain("rack-1").unwrap();
        assert_eq!(current.generation, 1);
        assert_eq!(current.name, "rack-1");
        assert!(current.metadata.is_empty());
    }

    #[tokio::test]
    async fn failure_domain_delete_protection() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("building-1", FailureDomainClass::Building, "az-1", None),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "rack-1",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("building-1"),
                ),
                None,
            )
            .await
            .unwrap();

        // A domain with children cannot be deleted.
        let err = registry
            .delete_failure_domain(&store, "building-1", 1, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainHasChildren(
                "building-1".to_owned()
            ))
        );

        // Nor can a domain that still has bindings.
        registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                None,
            )
            .await
            .unwrap();
        let err = registry
            .delete_failure_domain(&store, "rack-1", 1, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::FailureDomainHasBindings("rack-1".to_owned()))
        );

        // Unbind, then delete bottom-up works.
        registry
            .unbind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                None,
            )
            .await
            .unwrap();
        registry
            .delete_failure_domain(&store, "rack-1", 1, None)
            .await
            .unwrap();
        registry
            .delete_failure_domain(&store, "building-1", 1, None)
            .await
            .unwrap();
        assert!(registry.failure_domains().is_empty());
    }

    #[tokio::test]
    async fn binding_replay_is_idempotent() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let target = binding("rack-1", BindingTargetKind::Host, "hv-1");
        registry.bind(&store, target.clone(), None).await.unwrap();
        // Duplicate insert is a no-op success (replay convergence) and does
        // not touch the store.
        registry.bind(&store, target.clone(), None).await.unwrap();
        assert_eq!(registry.bindings_of("rack-1"), vec![target.clone()]);
        assert_eq!(
            registry.bindings_of_target(BindingTargetKind::Host, "hv-1"),
            vec![target.clone()]
        );

        // Unbind of an absent binding is a no-op success; unbind of a present
        // binding removes exactly it.
        registry
            .unbind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-2"),
                None,
            )
            .await
            .unwrap();
        registry.unbind(&store, target.clone(), None).await.unwrap();
        assert!(registry.bindings_of("rack-1").is_empty());
        registry.unbind(&store, target, None).await.unwrap();
    }

    #[tokio::test]
    async fn binding_validates_failure_domain_and_target_id() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();

        let err = registry
            .bind(
                &store,
                binding("ghost", BindingTargetKind::Host, "hv-1"),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::UnknownFailureDomain("ghost".to_owned()))
        );
        let err = registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "Bad ID"),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::InvalidBindingTargetId("Bad ID".to_owned()))
        );
        assert!(registry.bindings_of("rack-1").is_empty());
    }

    #[tokio::test]
    async fn region_and_az_declare_replay_and_uniqueness() {
        let mut registry = LocationRegistry::default();
        let store = MemoryTopologyStore::default();

        // Duplicate region declare is an idempotent no-op.
        registry
            .declare_region(&store, "region-a", None)
            .await
            .unwrap();
        registry
            .declare_region(&store, "region-a", None)
            .await
            .unwrap();
        assert_eq!(registry.len(), 1);

        // Duplicate AZ declare in the same region is an idempotent no-op.
        registry
            .declare_availability_domain(&store, "region-a", "az-1", None)
            .await
            .unwrap();
        registry
            .declare_availability_domain(&store, "region-a", "az-1", None)
            .await
            .unwrap();
        assert_eq!(registry.availability_domains_of("region-a").len(), 1);

        // The same AZ id in a different region is globally ambiguous.
        registry
            .declare_region(&store, "region-b", None)
            .await
            .unwrap();
        let err = registry
            .declare_availability_domain(&store, "region-b", "az-1", None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::AmbiguousAvailabilityDomain(
                "az-1".to_owned()
            ))
        );

        // Declaring into an unknown region fails.
        let err = registry
            .declare_availability_domain(&store, "region-x", "az-9", None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::UnknownTopologyRegion("region-x".to_owned()))
        );

        // Removing an absent region or AZ is an idempotent no-op.
        registry
            .remove_region(&store, "region-x", None)
            .await
            .unwrap();
        registry
            .remove_availability_domain(&store, "az-9", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn region_and_az_delete_protection() {
        let (mut registry, store) =
            registry_with_store(vec![declaration("region-a", &["az-1", "az-2"])]).await;

        // A region with AZs cannot be removed.
        let err = registry
            .remove_region(&store, "region-a", None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::RegionHasAvailabilityDomains(
                "region-a".to_owned()
            ))
        );

        // An AZ with failure domains cannot be removed.
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap();
        let err = registry
            .remove_availability_domain(&store, "az-1", None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Location(LocationError::AvailabilityDomainHasFailureDomains(
                "az-1".to_owned()
            ))
        );

        // Teardown in dependency order works.
        registry
            .delete_failure_domain(&store, "rack-1", 1, None)
            .await
            .unwrap();
        registry
            .remove_availability_domain(&store, "az-1", None)
            .await
            .unwrap();
        registry
            .remove_availability_domain(&store, "az-2", None)
            .await
            .unwrap();
        registry
            .remove_region(&store, "region-a", None)
            .await
            .unwrap();
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn store_failure_leaves_memory_untouched() {
        let mut registry = registry(vec![declaration("region-a", &["az-1"])]);
        let store = UnavailableTopologyStore;

        let err = registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TopologyError::Store(KernelError::TopologyUnavailable("injected".to_owned()))
        );
        assert!(registry.failure_domains().is_empty());
        // The store saw no durable mutation either: replaying through a healthy
        // store converges on the same empty topology.
        let healthy = MemoryTopologyStore::default();
        let snapshot = healthy.load_snapshot().await.unwrap();
        assert_eq!(snapshot.failure_domains, Vec::new());

        // The same holds for region declaration and binding.
        let err = registry
            .declare_region(&store, "region-x", None)
            .await
            .unwrap_err();
        assert!(matches!(err, TopologyError::Store(_)));
        assert!(!registry.contains_region("region-x"));
    }

    #[tokio::test]
    async fn restart_reconstruction_round_trips_through_snapshot() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        registry
            .create_failure_domain(
                &store,
                failure_domain("building-1", FailureDomainClass::Building, "az-1", None),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "rack-1",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("building-1"),
                ),
                None,
            )
            .await
            .unwrap();
        registry
            .create_failure_domain(
                &store,
                failure_domain(
                    "rack-2",
                    FailureDomainClass::Rack,
                    "az-1",
                    Some("building-1"),
                ),
                None,
            )
            .await
            .unwrap();
        registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                None,
            )
            .await
            .unwrap();
        registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::ResourceProvider, "agent-1"),
                None,
            )
            .await
            .unwrap();
        registry
            .update_failure_domain(
                &store,
                "rack-2",
                "second rack",
                BTreeMap::from([("aisle".to_owned(), "a1".to_owned())]),
                1,
                None,
            )
            .await
            .unwrap();

        let snapshot = registry.snapshot();
        // The durable store and in-memory state agree.
        let durable = store.load_snapshot().await.unwrap();
        assert_eq!(durable, snapshot);

        // Rebuild from the snapshot (restart path) and verify equality of the
        // full public surface.
        let rebuilt = LocationRegistry::from_snapshot(snapshot.clone()).unwrap();
        assert_eq!(rebuilt.snapshot(), snapshot);
        assert_eq!(rebuilt, registry);
        assert_eq!(rebuilt.failure_domains(), registry.failure_domains());
        assert_eq!(
            rebuilt.bindings_of("rack-1"),
            registry.bindings_of("rack-1")
        );
        assert_eq!(
            rebuilt.child_failure_domains("building-1"),
            registry.child_failure_domains("building-1")
        );
    }

    #[test]
    fn from_snapshot_rejects_corrupt_topology() {
        // Duplicate failure-domain IDs.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
            ],
            bindings: vec![],
        })
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::DuplicateFailureDomain("rack-1".to_owned())
        );

        // Hierarchy cycle (impossible through the mutation path, possible in
        // arbitrary durable content).
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![
                failure_domain("rack-a", FailureDomainClass::Rack, "az-1", Some("rack-b")),
                failure_domain("rack-b", FailureDomainClass::Rack, "az-1", Some("rack-a")),
            ],
            bindings: vec![],
        })
        .unwrap_err();
        assert!(matches!(err, LocationError::FailureDomainCycle(_)));

        // Unknown parent.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![failure_domain(
                "rack-1",
                FailureDomainClass::Rack,
                "az-1",
                Some("ghost"),
            )],
            bindings: vec![],
        })
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::FailureDomainParentMissing("rack-1".to_owned(), "ghost".to_owned())
        );

        // Unknown owning availability domain.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![failure_domain(
                "rack-1",
                FailureDomainClass::Rack,
                "az-missing",
                None,
            )],
            bindings: vec![],
        })
        .unwrap_err();
        assert!(matches!(
            err,
            LocationError::UnknownFailureDomainAvailabilityDomain(_, _)
        ));

        // Binding to an unknown failure domain.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![],
            bindings: vec![binding("ghost", BindingTargetKind::Host, "hv-1")],
        })
        .unwrap_err();
        assert_eq!(err, LocationError::UnknownFailureDomain("ghost".to_owned()));

        // Duplicate bindings.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![failure_domain(
                "rack-1",
                FailureDomainClass::Rack,
                "az-1",
                None,
            )],
            bindings: vec![
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
            ],
        })
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::DuplicateBinding {
                failure_domain: "rack-1".to_owned(),
                target_kind: BindingTargetKind::Host,
                target_id: "hv-1".to_owned(),
            }
        );

        // Invalid binding target id.
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: vec![failure_domain(
                "rack-1",
                FailureDomainClass::Rack,
                "az-1",
                None,
            )],
            bindings: vec![binding("rack-1", BindingTargetKind::Host, "Bad ID")],
        })
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::InvalidBindingTargetId("Bad ID".to_owned())
        );

        // A snapshot deeper than the hard cap is rejected.
        let mut deep = Vec::new();
        for depth in 1..=MAX_FAILURE_DOMAIN_DEPTH + 1 {
            let parent = (depth > 1).then(|| format!("rack-{:03}", depth - 1));
            deep.push(failure_domain(
                &format!("rack-{depth:03}"),
                FailureDomainClass::Rack,
                "az-1",
                parent.as_deref(),
            ));
        }
        let err = LocationRegistry::from_snapshot(TopologySnapshot {
            regions: vec![declaration("region-a", &["az-1"])],
            failure_domains: deep,
            bindings: vec![],
        })
        .unwrap_err();
        assert!(matches!(
            err,
            LocationError::FailureDomainHierarchyTooDeep(_)
        ));
    }

    #[tokio::test]
    async fn manifest_validation_passes_with_failure_domains_present() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;
        registry
            .create_failure_domain(
                &store,
                failure_domain("building-1", FailureDomainClass::Building, "az-1", None),
                None,
            )
            .await
            .unwrap();
        registry
            .bind(
                &store,
                binding("building-1", BindingTargetKind::FabricDomain, "fabric-1"),
                None,
            )
            .await
            .unwrap();

        let manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-1".to_owned()],
            controller: None,
            health: None,
        };
        assert!(registry.validate_manifest_locations(&manifest).is_ok());
    }

    fn audit_event(kind: &str, id: &str) -> AuditEvent {
        AuditEvent {
            event_id: crate::EventId::new(),
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            request_id: "req-1".to_owned(),
            audit_id: "audit-1".to_owned(),
            principal_id: crate::PrincipalId::new_unchecked("operator-1"),
            principal_kind: crate::PrincipalKind::User,
            effective_scope: crate::OwnershipScope::new(
                crate::ScopeId::new_unchecked("system"),
                crate::ScopeKind::System,
                None,
                None,
            ),
            service_namespace: crate::ServiceNamespace::new_unchecked("topology".to_owned()),
            action: crate::ActionId::new_unchecked("topology", "ManageTopology"),
            resource_type: Some(crate::ResourceType::new_unchecked("topology", kind)),
            resource_id: Some(crate::ResourceId::new_unchecked(id)),
            owner_scope: None,
            authorization_decision: None,
            operation_id: None,
            outcome: crate::AuditOutcome::Succeeded,
            reason_category: None,
            service_principal: None,
        }
    }

    #[tokio::test]
    async fn mutations_and_replays_thread_the_audit_event_through_the_store() {
        let (registry, store) = registry_with_store(vec![declaration("region-a", &["az-1"])]).await;

        // A real mutation folds the event into the mutation path.
        let create_event = audit_event("failure_domain", "rack-1");
        registry
            .create_failure_domain(
                &store,
                failure_domain("rack-1", FailureDomainClass::Rack, "az-1", None),
                Some(&create_event),
            )
            .await
            .unwrap();
        assert_eq!(store.state().audit_events.len(), 1, "create recorded once");
        assert_eq!(store.state().audit_events[0], create_event);

        // A create-replay records an audit event too (replayed action is audited)
        // — creation replay is an error here (duplicate), so the caller routes
        // the standalone write through `record_audit`; the store port accepts it.
        store
            .record_audit(&audit_event("failure_domain", "rack-1"))
            .await
            .unwrap();
        assert_eq!(store.state().audit_events.len(), 2);

        // A bind no-op replay (existing binding) still records through
        // `record_audit` via the registry.
        let bind_replay = audit_event("binding", "rack-1");
        registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                Some(&bind_replay),
            )
            .await
            .unwrap();
        // Binding hv-1 is new -> mutation path records it; replay the binding to
        // hit the no-op path.
        registry
            .bind(
                &store,
                binding("rack-1", BindingTargetKind::Host, "hv-1"),
                Some(&audit_event("binding", "rack-1")),
            )
            .await
            .unwrap();
        // Four audits total: the create, the explicit record_audit, the bind
        // insert, and the bind no-op replay. Every one is a manage action on the
        // system operator — including the no-op replays.
        let events = store.state().audit_events.clone();
        assert_eq!(
            events.len(),
            4,
            "exactly one durable audit per successful request"
        );
        assert!(
            events
                .iter()
                .all(|e| e.action.as_str() == "topology:ManageTopology"
                    && e.principal_id.as_str() == "operator-1"
                    && e.service_namespace.as_str() == "topology"),
            "every recorded audit event carries the manage action + operator principal: {events:?}"
        );
    }
}
