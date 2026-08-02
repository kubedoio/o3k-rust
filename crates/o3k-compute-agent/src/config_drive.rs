use thiserror::Error;

use o3k_provider_contract::compute_proto as proto;

use crate::{MAX_ARTIFACT_BYTES, deterministic_artifact_transfer_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDriveMaterializationRequest {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: String,
    pub resource_id: String,
    pub agent_id: String,
    pub artifact_id: String,
    pub sha256: String,
    pub format: String,
    pub size_bytes: u64,
    pub instance_id: String,
}

#[derive(Debug, Error)]
pub enum ConfigDriveMaterializationError {
    #[error("config-drive command identity is invalid")]
    Ownership,
}

/// Extracts the authenticated config-drive reference without performing host
/// I/O. Media generation and libvirt attachment remain separate boundaries.
pub fn config_drive_materialization_request(
    command: &proto::Command,
) -> Result<ConfigDriveMaterializationRequest, ConfigDriveMaterializationError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(ConfigDriveMaterializationError::Ownership);
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(ConfigDriveMaterializationError::Ownership);
    };
    let Some(reference) = resolved.config_drive_transfer.as_ref() else {
        return Err(ConfigDriveMaterializationError::Ownership);
    };
    let expected_transfer = deterministic_artifact_transfer_id(
        &command.command_id,
        proto::ArtifactKind::ConfigDriveIso,
        &resolved.config_drive_artifact_id,
    );
    if reference.transfer_id != expected_transfer
        || reference.expires_at_unix_ms <= crate::unix_ms()
        || reference.size_bytes > MAX_ARTIFACT_BYTES
        || resolved.config_drive_artifact_id.is_empty()
        || resolved.config_drive_sha256.len() != 64
    {
        return Err(ConfigDriveMaterializationError::Ownership);
    }
    Ok(ConfigDriveMaterializationRequest {
        transfer_id: reference.transfer_id.clone(),
        command_id: command.command_id.clone(),
        operation_id: command.operation_id.clone(),
        resource_id: command.resource_id.clone(),
        agent_id: command.agent_id.clone(),
        artifact_id: resolved.config_drive_artifact_id.clone(),
        sha256: resolved.config_drive_sha256.clone(),
        format: "iso".to_owned(),
        size_bytes: reference.size_bytes,
        instance_id: command.resource_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_requires_command_bound_iso_transfer() -> Result<(), ConfigDriveMaterializationError>
    {
        let command_id = "command-config";
        let artifact_id = "config-drive-1";
        let command = proto::Command {
            command_id: command_id.to_owned(),
            operation_id: "operation-1".to_owned(),
            agent_id: "agent-1".to_owned(),
            resource_id: "resource-1".to_owned(),
            action: Some(proto::command::Action::Create(proto::CreateCommand {
                resolved: Some(proto::ResolvedCreateInputs {
                    config_drive_artifact_id: artifact_id.to_owned(),
                    config_drive_sha256: "b".repeat(64),
                    config_drive_transfer: Some(proto::ArtifactReference {
                        transfer_id: deterministic_artifact_transfer_id(
                            command_id,
                            proto::ArtifactKind::ConfigDriveIso,
                            artifact_id,
                        ),
                        expires_at_unix_ms: crate::unix_ms().saturating_add(10_000),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        let request = config_drive_materialization_request(&command)?;
        assert_eq!(request.artifact_id, artifact_id);
        assert_eq!(request.format, "iso");
        Ok(())
    }
}
