//! Application-level representation of compute-agent events.
//!
//! These types are the bounded application contract for provider
//! observations: the durable journal (`o3k-reconciler`), the compute service
//! event projection, and the console event consumer depend on them, while the
//! transport adapter (`o3k-compute-agent`) converts wire messages into these
//! values at publish time. No protobuf type appears here.
//!
//! The state and category enums mirror the wire vocabulary minus its
//! `Unspecified` sentinel: an unrepresentable or unknown wire value is
//! rejected at the transport boundary instead of reaching application logic.

use uuid::Uuid;

use crate::{BlockDeviceObservation, InstanceState};

/// Operation-state vocabulary carried by authenticated agent evidence.
/// Deliberately distinct from `OperationState`: the agent protocol
/// distinguishes only these five states, while the provider-facing state also
/// carries `Retryable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentOperationState {
    Accepted,
    Running,
    Succeeded,
    Failed,
    UnknownOutcome,
}

/// Error-category vocabulary carried by authenticated agent evidence. This
/// includes the two authentication categories the agent may report; they are
/// rejected by the transport boundary before reaching application logic when
/// no application category exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentErrorCategory {
    InvalidRequest,
    Unauthenticated,
    Unauthorized,
    Conflict,
    Capacity,
    NotFound,
    Retryable,
    UnknownOutcome,
    Terminal,
}

/// Artifact transfer state as reported by an authenticated agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArtifactTransferState {
    Offered,
    Receiving,
    Committed,
    Rejected,
    Expired,
}

/// Authenticated agent acknowledgement that a command was durably accepted
/// before execution began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommandAccepted {
    pub command_id: String,
    pub operation_id: Uuid,
    pub state: AgentOperationState,
    pub operation_sequence: u64,
    pub agent_id: String,
    pub agent_epoch: String,
}

/// Authenticated agent progress update for one durable operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOperationUpdate {
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub state: AgentOperationState,
    pub error_category: Option<AgentErrorCategory>,
    /// Contract-redacted failure reason. Never a secret, raw command, or
    /// provider connection detail; still bounded and sanitized before durable
    /// storage by the journal.
    pub redacted_message: Option<String>,
    pub operation_sequence: u64,
    pub provider_resource_id: Option<String>,
    pub agent_id: String,
    pub agent_epoch: String,
}

/// Authenticated resource observation. The only live input that may change
/// the durable resource state; stale epochs, stale sequences, and identity
/// mismatches are rejected by the durable journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentObservation {
    pub agent_id: String,
    pub agent_epoch: String,
    pub resource_id: Uuid,
    pub provider_resource_id: Option<String>,
    pub state: InstanceState,
    pub operation_id: Uuid,
    pub operation_state: AgentOperationState,
    pub observation_sequence: u64,
    pub observed_at_unix_ms: i64,
    pub redacted_message: Option<String>,
    /// Bounded console bytes, separately authorized by the console service.
    pub console_log_bytes: Vec<u8>,
    pub console_log_offset: u64,
    pub console_log_complete: bool,
    pub console_log_truncated: bool,
    /// Non-secret block-device observation for collect-connector, attach,
    /// detach, and observe-disk commands.
    pub block_device: Option<BlockDeviceObservation>,
}

/// Authenticated agent protocol error bound to an operation or command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProtocolError {
    /// `None` when the agent did not supply a classified category; the
    /// transport boundary preserves that as absence rather than inventing one.
    pub category: Option<AgentErrorCategory>,
    pub code: String,
    pub redacted_message: Option<String>,
    pub operation_id: Option<Uuid>,
    pub retryable: bool,
    pub command_id: Option<String>,
}

/// Authenticated acknowledgement that an artifact transfer was durably
/// committed or rejected by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentArtifactAck {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub agent_id: String,
    pub agent_epoch: String,
    pub contiguous_bytes: u64,
    pub next_chunk_index: u32,
    pub state: ArtifactTransferState,
    pub redacted_message: Option<String>,
}

/// Authenticated artifact transfer status used for the durable recovery
/// projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentArtifactStatus {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: Uuid,
    pub resource_id: Uuid,
    pub agent_id: String,
    pub agent_epoch: String,
    pub contiguous_bytes: u64,
    pub next_chunk_index: u32,
    pub state: ArtifactTransferState,
}

/// Application-level stream event published by an agent connection.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    CommandAccepted(AgentCommandAccepted),
    Operation(AgentOperationUpdate),
    Observation(Box<AgentObservation>),
    ArtifactAck(AgentArtifactAck),
    ArtifactStatus(AgentArtifactStatus),
    Error(AgentProtocolError),
}
