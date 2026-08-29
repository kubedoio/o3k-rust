use super::AgentError;
use o3k_compute_agent::ArtifactStore;

pub(super) fn cleanup_config_drive_artifact(
    root: &std::path::Path,
    agent_id: &str,
    resource_id: &str,
) -> Result<(), AgentError> {
    let store = ArtifactStore::open(root, agent_id)
        .map_err(|_| AgentError::Protocol("artifact store is unavailable".to_owned()))?;
    store
        .cleanup_config_drive_for_resource(resource_id)
        .map(|_| ())
        .map_err(|_| AgentError::Protocol("owned config-drive cleanup failed".to_owned()))
}

/// Best-effort reaping of the resource's owned config-drive artifacts after
/// the delete's host mutation cleanup. A failed cleanup is logged and never
/// changes the already-successful delete outcome: the leak verifier catches
/// residue separately, so a cleanup error must not turn a successful delete
/// into a failed or unknown command outcome.
pub(super) fn reap_config_drive_artifacts(
    artifact_root: &std::path::Path,
    agent_id: &str,
    resource_id: &str,
) {
    if let Err(error) = cleanup_config_drive_artifact(artifact_root, agent_id, resource_id) {
        tracing::warn!(
            resource_id = %resource_id,
            error = %error,
            "owned config-drive artifact cleanup failed; the delete outcome is unaffected"
        );
    }
}

/// Best-effort reaping of incomplete-transfer `.part` files that the
/// protocol can never resume (issue #88 S5 supplementary): a part with no
/// manifest or an expired incomplete transfer is an orphan (the control
/// plane expires the abandoned transfer row and never resumes it; re-drives
/// mint fresh transfer ids), while a non-expired incomplete transfer is
/// resumed with the SAME transfer id after reconnect and its part is kept.
/// The `resource_id` filter scopes the reap to one deleted resource; `None`
/// reaps globally at startup. A failed cleanup is logged and never crashes
/// startup or changes a delete outcome: the leak verifier catches residue
/// separately.
pub(super) fn reap_orphaned_transfer_parts(
    root: &std::path::Path,
    agent_id: &str,
    resource_id: Option<&str>,
) {
    let store = match ArtifactStore::open(root, agent_id) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "artifact store is unavailable; transfer-part reap skipped"
            );
            return;
        }
    };
    let result = match resource_id {
        Some(resource_id) => store.reap_orphaned_parts_for_resource(resource_id),
        None => store.reap_orphaned_parts(),
    };
    match result {
        Ok(removed) => {
            tracing::debug!(
                resource_id = ?resource_id,
                removed,
                "orphaned transfer-part reap completed"
            );
        }
        Err(error) => {
            tracing::warn!(
                resource_id = ?resource_id,
                error = %error,
                "owned transfer-part cleanup failed; the outcome is unaffected"
            );
        }
    }
}

pub(super) fn cleanup_console_log(
    artifact_root: &std::path::Path,
    instance_id: &str,
) -> Result<(), AgentError> {
    let domain_name = o3k_libvirt::stable_domain_name(instance_id);
    let path = artifact_root
        .parent()
        .ok_or_else(|| AgentError::Protocol("agent artifact root has no service root".to_owned()))?
        .join("console")
        .join(format!("{domain_name}.log"));
    match std::fs::remove_file(path) {
        Ok(()) => {
            let _ = std::fs::remove_dir(
                artifact_root
                    .parent()
                    .ok_or_else(|| {
                        AgentError::Protocol("agent artifact root has no service root".to_owned())
                    })?
                    .join("console"),
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AgentError::Protocol(
            "console log cleanup failed".to_owned(),
        )),
    }
}
