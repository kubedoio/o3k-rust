use uuid::Uuid;

use super::error::StoreError;

/// Maps the durable lifecycle to the provider-neutral kernel vocabulary.
/// Provider-only fields remain on `OperationRecord` and never cross this
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Running,
    Succeeded,
    Retryable,
    UnknownOutcome,
    Failed,
}

impl From<OperationState> for o3k_kernel::OperationState {
    fn from(state: OperationState) -> Self {
        match state {
            OperationState::Pending => Self::Pending,
            OperationState::Running => Self::Running,
            OperationState::Succeeded => Self::Succeeded,
            OperationState::Retryable => Self::Retryable,
            OperationState::UnknownOutcome => Self::UnknownOutcome,
            OperationState::Failed => Self::Failed,
        }
    }
}

impl From<o3k_kernel::OperationState> for OperationState {
    fn from(state: o3k_kernel::OperationState) -> Self {
        match state {
            o3k_kernel::OperationState::Pending => Self::Pending,
            o3k_kernel::OperationState::Running => Self::Running,
            o3k_kernel::OperationState::Succeeded => Self::Succeeded,
            o3k_kernel::OperationState::Retryable => Self::Retryable,
            o3k_kernel::OperationState::UnknownOutcome => Self::UnknownOutcome,
            o3k_kernel::OperationState::Failed => Self::Failed,
        }
    }
}

impl OperationState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Retryable => "retryable",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "retryable" => Ok(Self::Retryable),
            "unknown_outcome" => Ok(Self::UnknownOutcome),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown operation state `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOverlayState {
    Pending,
    Materializing,
    Ready,
    Deleting,
    Deleted,
    Failed,
}

impl ImageOverlayState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Materializing => "materializing",
            Self::Ready => "ready",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "materializing" => Ok(Self::Materializing),
            "ready" => Ok(Self::Ready),
            "deleting" => Ok(Self::Deleting),
            "deleted" => Ok(Self::Deleted),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown image overlay state `{value}`"
            ))),
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Deleted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCommandState {
    Pending,
    Accepted,
    Running,
    Succeeded,
    Retryable,
    UnknownOutcome,
    Failed,
}

impl AgentCommandState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Retryable => "retryable",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "retryable" => Ok(Self::Retryable),
            "unknown_outcome" => Ok(Self::UnknownOutcome),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::Corrupt(format!(
                "unknown agent command state `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalCheckpointMode {
    Passive,
    Full,
    Restart,
    Truncate,
}

impl WalCheckpointMode {
    #[must_use]
    pub fn as_pragma_str(&self) -> &'static str {
        match self {
            Self::Passive => "PASSIVE",
            Self::Full => "FULL",
            Self::Restart => "RESTART",
            Self::Truncate => "TRUNCATE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyReservation {
    Created(Uuid),
    ExistingEquivalent(Uuid),
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalAcceptanceOutcome {
    Created {
        operation_id: Uuid,
        resource_id: Uuid,
    },
    ExistingEquivalent {
        operation_id: Uuid,
        resource_id: Uuid,
    },
    Conflict,
}
