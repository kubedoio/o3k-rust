mod audit;
mod composition;
mod compute;
pub mod diagnostics;
mod governance;
mod helpers;
mod network;
mod operation;
mod quota;
pub(crate) mod resource;
mod token;
mod volume;

#[cfg(test)]
mod tests;

pub use audit::AuditReaderAdapter;
pub use composition::CompositionResourceHandler;
pub use compute::ServerReaderAdapter;
pub use diagnostics::DiagnosticsReaderAdapter;
pub use governance::GovernanceReaderAdapter;
pub use network::NetworkReaderAdapter;
pub use operation::OperationReaderAdapter;
pub use quota::QuotaReaderAdapter;
pub use resource::GenericResourceApplication;
pub use token::TokenIssuerAdapter;
pub use volume::VolumeReaderAdapter;
