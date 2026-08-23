//! Validated conversion between untrusted protobuf values and kernel types.

use crate::proto;
use o3k_kernel::{
    ActionId, OwnershipScope, ResourceId, ResourceReference, ResourceType, ScopeId, ScopeKind,
};
use std::convert::TryFrom;
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConversionError {
    #[error("missing required field: {0}")]
    Missing(&'static str),
    #[error("invalid field {0}: {1}")]
    Invalid(&'static str, String),
}

fn required(value: String, name: &'static str) -> Result<String, ConversionError> {
    if value.trim().is_empty() {
        Err(ConversionError::Missing(name))
    } else {
        Ok(value)
    }
}

impl TryFrom<proto::Scope> for OwnershipScope {
    type Error = ConversionError;
    fn try_from(value: proto::Scope) -> Result<Self, Self::Error> {
        let id = ScopeId::new(required(value.id, "scope.id")?)
            .map_err(|e| ConversionError::Invalid("scope.id", e.to_string()))?;
        let kind = match proto::scope::Kind::try_from(value.kind)
            .map_err(|_| ConversionError::Invalid("scope.kind", "unknown enum".into()))?
        {
            proto::scope::Kind::Project => ScopeKind::Project,
            proto::scope::Kind::Domain => ScopeKind::Domain,
            proto::scope::Kind::System => ScopeKind::System,
            proto::scope::Kind::Unspecified => {
                return Err(ConversionError::Invalid("scope.kind", "unspecified".into()));
            }
        };
        Ok(OwnershipScope::new(
            id,
            kind,
            (!value.name.is_empty()).then_some(value.name),
            (!value.domain_id.is_empty()).then_some(value.domain_id),
        ))
    }
}

impl TryFrom<proto::ResourceRef> for ResourceReference {
    type Error = ConversionError;
    fn try_from(value: proto::ResourceRef) -> Result<Self, Self::Error> {
        let namespace = required(value.namespace, "resource.namespace")?;
        let name = required(value.r#type, "resource.type")?;
        let resource_type = ResourceType::new(namespace, name)
            .map_err(|e| ConversionError::Invalid("resource.type", e.to_string()))?;
        let resource_id = ResourceId::new(required(value.id, "resource.id")?)
            .map_err(|e| ConversionError::Invalid("resource.id", e.to_string()))?;
        if value.generation < 0 {
            return Err(ConversionError::Invalid(
                "resource.generation",
                "negative".into(),
            ));
        }
        Ok(Self {
            resource_type,
            resource_id,
            generation: value.generation,
        })
    }
}

impl TryFrom<proto::Context> for (Uuid, Uuid, ActionId, String, Uuid, u64, String, String) {
    type Error = ConversionError;
    fn try_from(value: proto::Context) -> Result<Self, Self::Error> {
        let parse = |v: String, n: &'static str| {
            Uuid::parse_str(&required(v, n)?)
                .map_err(|e| ConversionError::Invalid(n, e.to_string()))
        };
        let request = parse(value.request_id, "request_id")?;
        let operation = parse(value.operation_id, "operation_id")?;
        let action = ActionId::parse(&required(value.action, "action")?)
            .map_err(|e| ConversionError::Invalid("action", e.to_string()))?;
        let service = required(value.service_id, "service_id")?;
        let session = parse(value.session_id, "session_id")?;
        let replay = required(value.replay_identity, "replay_identity")?;
        Ok((
            request,
            operation,
            action,
            service,
            session,
            value.session_generation,
            replay,
            value.audit_correlation,
        ))
    }
}

pub fn failure_category(value: i32) -> Result<o3k_kernel::FailureCategory, ConversionError> {
    use o3k_kernel::FailureCategory as C;
    Ok(
        match proto::failure::Category::try_from(value)
            .map_err(|_| ConversionError::Invalid("failure.category", "unknown enum".into()))?
        {
            proto::failure::Category::InvalidRequest => C::InvalidRequest,
            proto::failure::Category::Unauthorized => C::Unauthorized,
            proto::failure::Category::Forbidden => C::Forbidden,
            proto::failure::Category::Conflict => C::Conflict,
            proto::failure::Category::StaleGeneration => C::StaleGeneration,
            proto::failure::Category::NotFound => C::NotFound,
            proto::failure::Category::NotReady => C::NotReady,
            proto::failure::Category::Retryable => C::Retryable,
            proto::failure::Category::NonRetryable => C::NonRetryable,
            proto::failure::Category::UnknownOutcome => C::UnknownOutcome,
            proto::failure::Category::Incompatible => C::Incompatible,
            proto::failure::Category::StaleSession => C::StaleSession,
            proto::failure::Category::ReplayConflict => C::ReplayConflict,
            proto::failure::Category::DelegationInvalid => C::DelegationInvalid,
            proto::failure::Category::ResourceExhausted => C::ResourceExhausted,
            proto::failure::Category::DeadlineExceeded => C::DeadlineExceeded,
            proto::failure::Category::Unspecified => {
                return Err(ConversionError::Invalid(
                    "failure.category",
                    "unspecified".into(),
                ));
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_wire_values_fail_closed() {
        assert!(ResourceReference::try_from(proto::ResourceRef::default()).is_err());
        assert!(OwnershipScope::try_from(proto::Scope::default()).is_err());
        assert!(failure_category(0).is_err());
    }
}
