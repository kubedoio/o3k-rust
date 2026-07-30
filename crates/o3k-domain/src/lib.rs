use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerId(Uuid);

impl ServerId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ServerId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Requested,
    Building,
    Active,
    Stopping,
    Stopped,
    Starting,
    Rebooting,
    Deleting,
    Deleted,
    Error,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("invalid server transition from {from:?} to {to:?}")]
    Invalid { from: ServerState, to: ServerState },
}

impl ServerState {
    pub fn transition(self, to: Self) -> Result<Self, TransitionError> {
        let valid = matches!(
            (self, to),
            (Self::Requested, Self::Building)
                | (Self::Building, Self::Active)
                | (Self::Building, Self::Error)
                | (Self::Active, Self::Stopping)
                | (Self::Stopped, Self::Starting)
                | (Self::Starting, Self::Active)
                | (Self::Stopping, Self::Stopped)
                | (Self::Active, Self::Rebooting)
                | (Self::Rebooting, Self::Active)
                | (Self::Requested, Self::Deleting)
                | (Self::Building, Self::Deleting)
                | (Self::Active, Self::Deleting)
                | (Self::Stopped, Self::Deleting)
                | (Self::Error, Self::Deleting)
                | (Self::Deleting, Self::Deleted)
        );

        valid
            .then_some(to)
            .ok_or(TransitionError::Invalid { from: self, to })
    }
}

#[cfg(test)]
mod tests {
    use super::{ServerState, TransitionError};

    #[test]
    fn valid_server_transition_is_accepted() {
        assert_eq!(
            ServerState::Requested.transition(ServerState::Building),
            Ok(ServerState::Building)
        );
    }

    #[test]
    fn invalid_server_transition_is_rejected() {
        assert_eq!(
            ServerState::Requested.transition(ServerState::Active),
            Err(TransitionError::Invalid {
                from: ServerState::Requested,
                to: ServerState::Active,
            })
        );
    }
}
