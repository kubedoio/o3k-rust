use o3k_domain::ServerState;

use crate::StoreError;

/// Canonical storage encoding of a server lifecycle state. This is the only
/// sanctioned way to persist the canonical state into the resource ledger's
/// `observed_state` column; the Nova status projection in `o3k-api` is a
/// separate projection and must not be used here.
#[must_use]
pub fn server_state_to_storage(state: ServerState) -> &'static str {
    match state {
        ServerState::Requested => "REQUESTED",
        ServerState::Building => "BUILD",
        ServerState::Active => "ACTIVE",
        ServerState::Stopping => "STOPPING",
        ServerState::Stopped => "SHUTOFF",
        ServerState::Starting => "STARTING",
        ServerState::Rebooting => "REBOOTING",
        ServerState::Deleting => "DELETING",
        ServerState::Deleted => "DELETED",
        ServerState::Error => "ERROR",
    }
}

/// Fail-closed decode of a persisted server lifecycle state. Legacy values
/// written before the canonical model (lowercase spellings and provider-ish
/// aliases) are accepted so existing databases keep working; anything else is
/// corrupt and must not be misclassified as a valid lifecycle state.
pub fn server_state_from_storage(value: &str) -> Result<ServerState, StoreError> {
    match value.to_ascii_uppercase().as_str() {
        "REQUESTED" => Ok(ServerState::Requested),
        "BUILD" | "CREATING" => Ok(ServerState::Building),
        "ACTIVE" | "RUNNING" => Ok(ServerState::Active),
        "STOPPING" => Ok(ServerState::Stopping),
        "SHUTOFF" | "STOPPED" => Ok(ServerState::Stopped),
        "STARTING" => Ok(ServerState::Starting),
        "REBOOTING" => Ok(ServerState::Rebooting),
        "DELETING" => Ok(ServerState::Deleting),
        "DELETED" => Ok(ServerState::Deleted),
        "ERROR" => Ok(ServerState::Error),
        _ => Err(StoreError::Corrupt(format!(
            "unknown server state `{value}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{server_state_from_storage, server_state_to_storage};
    use crate::StoreError;
    use o3k_domain::ServerState;

    #[test]
    fn every_canonical_state_round_trips_its_storage_encoding() {
        let states = [
            ServerState::Requested,
            ServerState::Building,
            ServerState::Active,
            ServerState::Stopping,
            ServerState::Stopped,
            ServerState::Starting,
            ServerState::Rebooting,
            ServerState::Deleting,
            ServerState::Deleted,
            ServerState::Error,
        ];
        for state in states {
            let encoded = server_state_to_storage(state);
            assert!(matches!(
                server_state_from_storage(encoded),
                Ok(decoded) if decoded == state
            ));
        }
    }

    #[test]
    fn legacy_storage_spellings_are_accepted() {
        let legacy = [
            ("requested", ServerState::Requested),
            ("active", ServerState::Active),
            ("BUILD", ServerState::Building),
            ("SHUTOFF", ServerState::Stopped),
            ("DELETING", ServerState::Deleting),
            ("DELETED", ServerState::Deleted),
            ("ERROR", ServerState::Error),
            // Provider-ish aliases written by early agent adopters.
            ("RUNNING", ServerState::Active),
            ("STOPPED", ServerState::Stopped),
            ("CREATING", ServerState::Building),
            ("Active", ServerState::Active),
        ];
        for (stored, expected) in legacy {
            assert!(
                matches!(server_state_from_storage(stored), Ok(decoded) if decoded == expected),
                "legacy value `{stored}`"
            );
        }
    }

    #[test]
    fn corrupt_or_unknown_stored_state_fails_closed() {
        for corrupt in [
            "",
            "unknown",
            "garbage",
            "DELETEDX",
            "REQUESTING",
            "running_",
            "  ACTIVE",
            "ACTIVE ",
        ] {
            assert!(
                matches!(
                    server_state_from_storage(corrupt),
                    Err(StoreError::Corrupt(message)) if message == format!("unknown server state `{corrupt}`")
                ),
                "corrupt value `{corrupt}` must be rejected"
            );
        }
    }
}
