//! Wire-to-application conversion for compute-agent events.
//!
//! The transport adapter (`o3k-compute-agent`) is the only place protobuf
//! messages become application-level `o3k_provider` event types. Every
//! conversion here is the transport boundary's validation: an unrepresentable
//! state or identity value is rejected before it can reach application logic
//! or durable state, and the event is dropped with a warning. For the
//! strict conversions this is stricter than the historical consumers, which
//! durably projected some invalid inputs (for example an acceptance with an
//! `Unspecified` state); rejecting protocol-violating input before any
//! durable write is the intended fail-closed behavior and is unreachable from
//! agents implementing the protocol.
//!
//! State and transfer-state conversions are strict (the wire `Unspecified`
//! sentinel and unknown values are errors). Error-category conversions are
//! lenient in the absence direction: the agent legitimately sends
//! `Unspecified` on non-failed updates, and application consumers historically
//! treated that as "no category" (`None`), so the conversion preserves it as
//! `None` instead of dropping the event. `ProtocolError` conversion is lenient
//! in both directions (category and operation identity), because the
//! historical projection treated both as absence and still projected the
//! error state.

use std::fmt;

use o3k_provider::{
    AgentArtifactAck, AgentArtifactStatus, AgentCommandAccepted, AgentErrorCategory,
    AgentObservation, AgentOperationState, AgentOperationUpdate, AgentProtocolError,
    ArtifactTransferState, BlockDeviceObservation, InstanceState,
};
use uuid::Uuid;

use crate::proto;

/// Why a wire message could not be represented as an application-level event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEventConversionError {
    /// A state, category, or transfer-state value has no application
    /// representation (including the wire `Unspecified` sentinel where the
    /// application vocabulary is strict).
    UnknownEnumValue(&'static str),
    /// An identity field that application logic requires as a UUID is not one.
    InvalidIdentity(&'static str),
}

impl fmt::Display for AgentEventConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEnumValue(what) => {
                write!(
                    formatter,
                    "agent event field {what} has no application representation"
                )
            }
            Self::InvalidIdentity(what) => {
                write!(formatter, "agent event identity {what} is not a valid UUID")
            }
        }
    }
}

impl std::error::Error for AgentEventConversionError {}

fn operation_state(
    value: proto::OperationState,
) -> Result<AgentOperationState, AgentEventConversionError> {
    use proto::OperationState as Wire;
    match value {
        Wire::Accepted => Ok(AgentOperationState::Accepted),
        Wire::Running => Ok(AgentOperationState::Running),
        Wire::Succeeded => Ok(AgentOperationState::Succeeded),
        Wire::Failed => Ok(AgentOperationState::Failed),
        Wire::UnknownOutcome => Ok(AgentOperationState::UnknownOutcome),
        Wire::Unspecified => Err(AgentEventConversionError::UnknownEnumValue(
            "operation_state",
        )),
    }
}

fn error_category(value: proto::ErrorCategory) -> Option<AgentErrorCategory> {
    use proto::ErrorCategory as Wire;
    match value {
        Wire::Unspecified => None,
        Wire::InvalidRequest => Some(AgentErrorCategory::InvalidRequest),
        Wire::Unauthenticated => Some(AgentErrorCategory::Unauthenticated),
        Wire::Unauthorized => Some(AgentErrorCategory::Unauthorized),
        Wire::Conflict => Some(AgentErrorCategory::Conflict),
        Wire::Capacity => Some(AgentErrorCategory::Capacity),
        Wire::NotFound => Some(AgentErrorCategory::NotFound),
        Wire::Retryable => Some(AgentErrorCategory::Retryable),
        Wire::UnknownOutcome => Some(AgentErrorCategory::UnknownOutcome),
        Wire::Terminal => Some(AgentErrorCategory::Terminal),
    }
}

fn resource_state(value: proto::ResourceState) -> Result<InstanceState, AgentEventConversionError> {
    use proto::ResourceState as Wire;
    match value {
        Wire::Creating => Ok(InstanceState::Creating),
        Wire::Running => Ok(InstanceState::Running),
        Wire::Stopped => Ok(InstanceState::Stopped),
        Wire::Deleting => Ok(InstanceState::Deleting),
        Wire::Deleted => Ok(InstanceState::Deleted),
        Wire::Error => Ok(InstanceState::Error),
        Wire::Unspecified => Err(AgentEventConversionError::UnknownEnumValue(
            "resource_state",
        )),
    }
}

fn transfer_state(
    value: proto::ArtifactTransferState,
) -> Result<ArtifactTransferState, AgentEventConversionError> {
    use proto::ArtifactTransferState as Wire;
    match value {
        Wire::Offered => Ok(ArtifactTransferState::Offered),
        Wire::Receiving => Ok(ArtifactTransferState::Receiving),
        Wire::Committed => Ok(ArtifactTransferState::Committed),
        Wire::Rejected => Ok(ArtifactTransferState::Rejected),
        Wire::Expired => Ok(ArtifactTransferState::Expired),
        Wire::Unspecified => Err(AgentEventConversionError::UnknownEnumValue(
            "transfer_state",
        )),
    }
}

fn uuid(value: &str, what: &'static str) -> Result<Uuid, AgentEventConversionError> {
    Uuid::parse_str(value).map_err(|_| AgentEventConversionError::InvalidIdentity(what))
}

fn block_device_observation(value: proto::BlockDeviceObservation) -> BlockDeviceObservation {
    BlockDeviceObservation {
        volume_id: value.volume_id,
        attachment_id: value.attachment_id,
        driver_volume_type: value.driver_volume_type,
        device_path: (!value.device_path.is_empty()).then_some(value.device_path),
        host_path: (!value.host_path.is_empty()).then_some(value.host_path),
        attached: value.attached,
        found: value.found,
        initiator: (!value.initiator.is_empty()).then_some(value.initiator),
        host_name: (!value.host_name.is_empty()).then_some(value.host_name),
        ip_address: (!value.ip_address.is_empty()).then_some(value.ip_address),
        iscsi_logged_in: value.iscsi_logged_in,
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

pub fn command_accepted(
    value: proto::CommandAccepted,
) -> Result<AgentCommandAccepted, AgentEventConversionError> {
    Ok(AgentCommandAccepted {
        command_id: value.command_id,
        operation_id: uuid(&value.operation_id, "command_accepted.operation_id")?,
        state: operation_state(
            proto::OperationState::try_from(value.state).map_err(|_| {
                AgentEventConversionError::UnknownEnumValue("command_accepted.state")
            })?,
        )?,
        operation_sequence: value.operation_sequence,
        agent_id: value.agent_id,
        agent_epoch: value.agent_epoch,
    })
}

pub fn operation_update(
    value: proto::OperationUpdate,
) -> Result<AgentOperationUpdate, AgentEventConversionError> {
    let state = proto::OperationState::try_from(value.state)
        .map_err(|_| AgentEventConversionError::UnknownEnumValue("operation_update.state"))?;
    // The category is only classified evidence for terminal failures; an
    // unclassified or unknown value is preserved as absence (None) exactly
    // as the durable journal historically treated it, so a non-failed
    // update with an unclassified category still applies.
    let error_category = proto::ErrorCategory::try_from(value.error_category)
        .ok()
        .and_then(error_category);
    Ok(AgentOperationUpdate {
        operation_id: uuid(&value.operation_id, "operation_update.operation_id")?,
        resource_id: uuid(&value.resource_id, "operation_update.resource_id")?,
        state: operation_state(state)?,
        error_category,
        redacted_message: non_empty(value.redacted_message),
        operation_sequence: value.operation_sequence,
        provider_resource_id: non_empty(value.provider_resource_id),
        agent_id: value.agent_id,
        agent_epoch: value.agent_epoch,
    })
}

pub fn observation(
    value: proto::Observation,
) -> Result<AgentObservation, AgentEventConversionError> {
    let state = proto::ResourceState::try_from(value.state)
        .map_err(|_| AgentEventConversionError::UnknownEnumValue("observation.state"))?;
    let observed_operation_state = proto::OperationState::try_from(value.operation_state)
        .map_err(|_| AgentEventConversionError::UnknownEnumValue("observation.operation_state"))?;
    Ok(AgentObservation {
        agent_id: value.agent_id,
        agent_epoch: value.agent_epoch,
        resource_id: uuid(&value.resource_id, "observation.resource_id")?,
        provider_resource_id: non_empty(value.provider_resource_id),
        state: resource_state(state)?,
        operation_id: uuid(&value.operation_id, "observation.operation_id")?,
        operation_state: operation_state(observed_operation_state)?,
        observation_sequence: value.observation_sequence,
        observed_at_unix_ms: value.observed_at_unix_ms,
        redacted_message: non_empty(value.redacted_message),
        console_log_bytes: value.console_log_bytes,
        console_log_offset: value.console_log_offset,
        console_log_complete: value.console_log_complete,
        console_log_truncated: value.console_log_truncated,
        block_device: value.block_device.map(block_device_observation),
    })
}

pub fn protocol_error(
    value: proto::ProtocolError,
) -> Result<AgentProtocolError, AgentEventConversionError> {
    // Lenient in both directions, matching the historical projection: an
    // unclassified or unknown category and an unparseable operation identity
    // were both treated as absence (None) and the error was still projected
    // against any matching operation.
    let category = proto::ErrorCategory::try_from(value.category)
        .ok()
        .and_then(error_category);
    let operation_id = if value.operation_id.is_empty() {
        None
    } else {
        Uuid::parse_str(&value.operation_id).ok()
    };
    Ok(AgentProtocolError {
        category,
        code: value.code,
        redacted_message: non_empty(value.redacted_message),
        operation_id,
        retryable: value.retryable,
        command_id: non_empty(value.command_id),
    })
}

pub fn artifact_ack(
    value: proto::ArtifactAck,
) -> Result<AgentArtifactAck, AgentEventConversionError> {
    let state = proto::ArtifactTransferState::try_from(value.state)
        .map_err(|_| AgentEventConversionError::UnknownEnumValue("artifact_ack.state"))?;
    Ok(AgentArtifactAck {
        transfer_id: value.transfer_id,
        command_id: value.command_id,
        operation_id: uuid(&value.operation_id, "artifact_ack.operation_id")?,
        resource_id: uuid(&value.resource_id, "artifact_ack.resource_id")?,
        agent_id: value.agent_id,
        agent_epoch: value.agent_epoch,
        contiguous_bytes: value.contiguous_bytes,
        next_chunk_index: value.next_chunk_index,
        state: transfer_state(state)?,
        redacted_message: non_empty(value.redacted_message),
    })
}

pub fn artifact_status(
    value: proto::ArtifactStatus,
) -> Result<AgentArtifactStatus, AgentEventConversionError> {
    let state = proto::ArtifactTransferState::try_from(value.state)
        .map_err(|_| AgentEventConversionError::UnknownEnumValue("artifact_status.state"))?;
    Ok(AgentArtifactStatus {
        transfer_id: value.transfer_id,
        command_id: value.command_id,
        operation_id: uuid(&value.operation_id, "artifact_status.operation_id")?,
        resource_id: uuid(&value.resource_id, "artifact_status.resource_id")?,
        agent_id: value.agent_id,
        agent_epoch: value.agent_epoch,
        contiguous_bytes: value.contiguous_bytes,
        next_chunk_index: value.next_chunk_index,
        state: transfer_state(state)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto;

    fn wire_observation(
        value: proto::Observation,
    ) -> Result<AgentObservation, AgentEventConversionError> {
        observation(value)
    }

    fn wire_operation_update(state: proto::OperationState) -> proto::OperationUpdate {
        proto::OperationUpdate {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            operation_sequence: 1,
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            resource_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            state: state as i32,
            error_category: proto::ErrorCategory::Unspecified as i32,
            redacted_message: String::new(),
            provider_resource_id: String::new(),
        }
    }

    #[test]
    fn unspecified_operation_state_is_rejected_at_the_boundary() {
        let error = wire_operation_update(proto::OperationState::Unspecified);
        assert!(matches!(
            operation_update(error),
            Err(AgentEventConversionError::UnknownEnumValue(_))
        ));
    }

    #[test]
    fn invalid_identity_is_rejected_at_the_boundary() {
        let mut update = wire_operation_update(proto::OperationState::Succeeded);
        update.operation_id = "not-a-uuid".to_owned();
        assert!(matches!(
            operation_update(update),
            Err(AgentEventConversionError::InvalidIdentity(_))
        ));
    }

    #[test]
    fn unspecified_error_category_is_preserved_as_absence() -> Result<(), AgentEventConversionError>
    {
        // The agent legitimately sends UNSPECIFIED on non-failed updates; the
        // conversion keeps that as None instead of dropping the event.
        let update = wire_operation_update(proto::OperationState::Running);
        let converted = operation_update(update)?;
        assert_eq!(converted.error_category, None);
        assert_eq!(converted.state, AgentOperationState::Running);
        Ok(())
    }

    #[test]
    fn failed_update_preserves_the_classified_category_and_reason()
    -> Result<(), AgentEventConversionError> {
        let mut update = wire_operation_update(proto::OperationState::Failed);
        update.error_category = proto::ErrorCategory::Terminal as i32;
        update.redacted_message = "bounded reason".to_owned();
        let converted = operation_update(update)?;
        assert_eq!(converted.error_category, Some(AgentErrorCategory::Terminal));
        assert_eq!(
            converted.redacted_message.as_deref(),
            Some("bounded reason")
        );
        Ok(())
    }

    #[test]
    fn observation_requires_succeeded_operation_state_and_valid_identities()
    -> Result<(), AgentEventConversionError> {
        let mut observation = proto::Observation {
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            resource_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            provider_resource_id: "domain-1".to_owned(),
            state: proto::ResourceState::Running as i32,
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            operation_state: proto::OperationState::Succeeded as i32,
            observation_sequence: 1,
            ..Default::default()
        };
        let converted = wire_observation(observation.clone())?;
        assert_eq!(converted.state, InstanceState::Running);
        assert_eq!(converted.provider_resource_id.as_deref(), Some("domain-1"));

        observation.state = proto::ResourceState::Unspecified as i32;
        assert!(matches!(
            wire_observation(observation.clone()),
            Err(AgentEventConversionError::UnknownEnumValue(_))
        ));

        observation.state = proto::ResourceState::Running as i32;
        observation.operation_state = proto::OperationState::Running as i32;
        // A non-succeeded operation state is still representable; the durable
        // journal rejects it. Conversion only rejects unrepresentable values.
        assert!(wire_observation(observation).is_ok());
        Ok(())
    }

    #[test]
    fn protocol_error_preserves_optional_identity_and_category()
    -> Result<(), AgentEventConversionError> {
        let error = proto::ProtocolError {
            category: proto::ErrorCategory::Unspecified as i32,
            code: "code".to_owned(),
            redacted_message: "reason".to_owned(),
            operation_id: String::new(),
            retryable: true,
            command_id: String::new(),
        };
        let converted = protocol_error(error)?;
        assert_eq!(converted.category, None);
        assert_eq!(converted.operation_id, None);
        assert!(converted.retryable);

        let mut bound = proto::ProtocolError {
            category: proto::ErrorCategory::UnknownOutcome as i32,
            code: "code".to_owned(),
            redacted_message: String::new(),
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            retryable: false,
            command_id: "command-1".to_owned(),
        };
        let converted = protocol_error(bound.clone())?;
        assert_eq!(converted.category, Some(AgentErrorCategory::UnknownOutcome));
        assert_eq!(
            converted.operation_id.map(|id| id.to_string()).as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(converted.command_id.as_deref(), Some("command-1"));

        bound.operation_id = "not-a-uuid".to_owned();
        // Lenient identity handling matches the historical projection: an
        // unparseable operation identity is preserved as absence instead of
        // dropping the error event.
        let converted = protocol_error(bound)?;
        assert_eq!(converted.operation_id, None);
        Ok(())
    }

    #[test]
    fn artifact_status_and_ack_convert_identity_and_state() -> Result<(), AgentEventConversionError>
    {
        let status = proto::ArtifactStatus {
            transfer_id: "transfer-1".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            resource_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            contiguous_bytes: 4,
            next_chunk_index: 1,
            state: proto::ArtifactTransferState::Receiving as i32,
        };
        let converted = artifact_status(status)?;
        assert_eq!(converted.state, ArtifactTransferState::Receiving);
        assert_eq!(converted.contiguous_bytes, 4);

        let ack = proto::ArtifactAck {
            transfer_id: "transfer-1".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            resource_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            contiguous_bytes: 3,
            next_chunk_index: 1,
            state: proto::ArtifactTransferState::Committed as i32,
            redacted_message: String::new(),
        };
        let converted = artifact_ack(ack)?;
        assert_eq!(converted.state, ArtifactTransferState::Committed);

        let mut unspecified = proto::ArtifactAck {
            ..converted_ack_base()
        };
        unspecified.state = proto::ArtifactTransferState::Unspecified as i32;
        assert!(matches!(
            artifact_ack(unspecified),
            Err(AgentEventConversionError::UnknownEnumValue(_))
        ));
        Ok(())
    }

    fn converted_ack_base() -> proto::ArtifactAck {
        proto::ArtifactAck {
            transfer_id: "transfer-1".to_owned(),
            command_id: "command-1".to_owned(),
            operation_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            resource_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            agent_id: "agent-1".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
            contiguous_bytes: 3,
            next_chunk_index: 1,
            state: proto::ArtifactTransferState::Committed as i32,
            redacted_message: String::new(),
        }
    }
}
