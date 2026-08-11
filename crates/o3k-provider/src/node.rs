//! Application-level representation of authenticated compute-agent nodes and
//! their artifact-transfer contract.
//!
//! These types are the bounded application surface for agent eligibility,
//! inventory publication, and resolved create inputs. The transport adapter
//! (`o3k-compute-agent`) owns the wire forms and converts them into these
//! values at its boundary.

use async_trait::async_trait;

use crate::{AgentEvent, CreateInstanceRequest, ProviderError};

/// Whether the agent's control connection is currently alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAvailability {
    Available,
    Unavailable,
}

/// The control plane's administrative intent for the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAdministrativeState {
    Enabled,
    Draining,
    Disabled,
}

/// A named, versioned capability flag negotiated by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCapabilityFlag {
    pub name: String,
    pub supported: bool,
}

/// Bounded capability facts needed for scheduling and compatibility
/// decisions. Capability flags and disk formats are never treated as
/// capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub agent_provider_name: String,
    pub agent_provider_version: String,
    pub max_vcpus: u64,
    pub max_memory_mib: u64,
    pub max_disk_gb: u64,
    pub lifecycle_actions: Vec<String>,
    pub console_log: bool,
    pub flags: Vec<AgentCapabilityFlag>,
}

/// Application-level snapshot of one registered agent node. The stable agent
/// ID doubles as the Placement provider ID, so reconnects update the same
/// provider and preserve durable allocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNodeSnapshot {
    pub agent_id: String,
    pub agent_epoch: String,
    pub availability: AgentAvailability,
    pub administrative_state: AgentAdministrativeState,
    pub capabilities: AgentCapabilities,
}

/// A read-side lease on one registry epoch. While this value is alive, the
/// registry implementation must not make a replacement epoch current for the
/// same agent. Evidence consumers hold the lease across their durable write,
/// making current-epoch validation and projection one linearizable action.
pub trait AgentEpochLease: Send {}

/// The bounded application port for authenticated agent nodes. Dispatch and
/// wire conversion stay in the transport adapter; application services only
/// read node snapshots and the application-level event stream.
#[async_trait]
pub trait AgentNodeRegistry: Send + Sync {
    async fn all(&self) -> Vec<AgentNodeSnapshot>;
    async fn snapshot(&self, agent_id: &str) -> Option<AgentNodeSnapshot>;
    async fn lease_current_epoch(
        &self,
        agent_id: &str,
        agent_epoch: &str,
    ) -> Option<Box<dyn AgentEpochLease>>;
    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<AgentEvent>;
}

/// Artifact kinds the agent realizes on a host. Wire `Unspecified` has no
/// application representation and is rejected at the transport boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    ImageBase,
    ConfigDriveIso,
}

/// One network attachment the agent must realize for a create command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAttachmentSpec {
    pub port_id: String,
    pub mac: String,
    pub fixed_ipv4: String,
    pub subnet_cidr: String,
    pub gateway_ipv4: String,
}

/// Fully resolved, immutable inputs required by the agent create command.
/// The control plane constructs this value from its image, network, and
/// config-drive services; the agent provider never guesses paths, checksums,
/// addresses, or flavor values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCreateInputs {
    pub flavor_id: String,
    pub image_artifact_id: String,
    pub image_sha256: String,
    pub image_format: String,
    pub disk_gib: u64,
    pub config_drive_artifact_id: String,
    pub config_drive_sha256: String,
    pub network_attachments: Vec<NetworkAttachmentSpec>,
}

/// Verified bytes that must be present on the agent before a create command
/// is dispatched. Implementations must source these bytes from managed,
/// digest-checked stores; paths are intentionally not part of this contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCreateArtifact {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub sha256: String,
    pub format: String,
    pub bytes: Vec<u8>,
}

/// Resolves control-plane-owned resources into the bounded protocol inputs
/// required by an agent. Implementations must return verified references and
/// digests; returning placeholder values is intentionally not supported.
#[async_trait]
pub trait ResolvedCreateResolver: Send + Sync {
    async fn resolve(
        &self,
        request: &CreateInstanceRequest,
        agent: &AgentNodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError>;
}

/// A resolver used by profiles that have not yet wired image/config-drive/
/// network realization. It fails closed, making the missing integration
/// explicit instead of sending fabricated protocol data to a host.
#[derive(Debug, Default)]
pub struct UnconfiguredResolvedCreateResolver;

#[async_trait]
impl ResolvedCreateResolver for UnconfiguredResolvedCreateResolver {
    async fn resolve(
        &self,
        _request: &CreateInstanceRequest,
        _agent: &AgentNodeSnapshot,
    ) -> Result<ResolvedCreateInputs, ProviderError> {
        Err(ProviderError::InvalidRequest)
    }
}

#[async_trait]
pub trait CreateArtifactResolver: Send + Sync {
    async fn resolve_artifacts(
        &self,
        request: &CreateInstanceRequest,
        agent: &AgentNodeSnapshot,
        inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError>;
}

#[derive(Debug, Default)]
pub struct UnconfiguredCreateArtifactResolver;

#[async_trait]
impl CreateArtifactResolver for UnconfiguredCreateArtifactResolver {
    async fn resolve_artifacts(
        &self,
        _request: &CreateInstanceRequest,
        _agent: &AgentNodeSnapshot,
        _inputs: &ResolvedCreateInputs,
    ) -> Result<Vec<ResolvedCreateArtifact>, ProviderError> {
        Err(ProviderError::InvalidRequest)
    }
}
