use thiserror::Error;

/// Errors produced by kernel contracts, identity validation, or authorization.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum KernelError {
    #[error("invalid principal identifier: {0}")]
    InvalidPrincipalId(String),

    #[error("invalid scope identifier: {0}")]
    InvalidScopeId(String),

    #[error("invalid resource identifier: {0}")]
    InvalidResourceId(String),

    #[error("invalid resource type: {0}")]
    InvalidResourceType(String),

    #[error("invalid action identifier: {0}")]
    InvalidActionId(String),

    #[error("invalid service identifier: {0}")]
    InvalidServiceId(String),

    #[error("invalid namespace: {0}")]
    InvalidNamespace(String),

    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),
}
