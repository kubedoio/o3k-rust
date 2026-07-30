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
                | (Self::Requested, Self::Error)
                | (Self::Building, Self::Error)
                | (Self::Active, Self::Error)
                | (Self::Stopping, Self::Error)
                | (Self::Stopped, Self::Error)
                | (Self::Starting, Self::Error)
                | (Self::Rebooting, Self::Error)
                | (Self::Deleting, Self::Error)
                | (Self::Active, Self::Stopping)
                | (Self::Stopped, Self::Starting)
                | (Self::Starting, Self::Active)
                | (Self::Stopping, Self::Stopped)
                | (Self::Active, Self::Rebooting)
                | (Self::Rebooting, Self::Active)
                | (Self::Requested, Self::Deleting)
                | (Self::Building, Self::Deleting)
                | (Self::Active, Self::Deleting)
                | (Self::Stopping, Self::Deleting)
                | (Self::Stopped, Self::Deleting)
                | (Self::Starting, Self::Deleting)
                | (Self::Rebooting, Self::Deleting)
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
    fn valid_server_transitions_are_accepted() {
        let transitions = [
            (ServerState::Requested, ServerState::Building),
            (ServerState::Building, ServerState::Active),
            (ServerState::Active, ServerState::Stopping),
            (ServerState::Stopping, ServerState::Stopped),
            (ServerState::Stopped, ServerState::Starting),
            (ServerState::Starting, ServerState::Active),
            (ServerState::Active, ServerState::Rebooting),
            (ServerState::Rebooting, ServerState::Active),
            (ServerState::Deleting, ServerState::Deleted),
        ];

        for (from, to) in transitions {
            assert_eq!(from.transition(to), Ok(to), "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn every_non_deleted_state_can_fail_or_be_deleted() {
        let non_deleted_states = [
            ServerState::Requested,
            ServerState::Building,
            ServerState::Active,
            ServerState::Stopping,
            ServerState::Stopped,
            ServerState::Starting,
            ServerState::Rebooting,
            ServerState::Deleting,
            ServerState::Error,
        ];

        for state in non_deleted_states {
            if state != ServerState::Error {
                assert_eq!(state.transition(ServerState::Error), Ok(ServerState::Error));
            }
            if state != ServerState::Deleting {
                assert_eq!(
                    state.transition(ServerState::Deleting),
                    Ok(ServerState::Deleting)
                );
            }
        }
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
