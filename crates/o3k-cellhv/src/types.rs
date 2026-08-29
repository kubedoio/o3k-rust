//! CellHV provider domain types: config, errors.

use std::{fmt, path::PathBuf};

use thiserror::Error;

#[derive(Clone)]
pub struct CellHvConfig {
    pub endpoint: String,
    pub expected_version: String,
    pub ca_certificate: Option<PathBuf>,
    pub client_certificate: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

impl fmt::Debug for CellHvConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CellHvConfig")
            .field("endpoint", &self.endpoint)
            .field("expected_version", &self.expected_version)
            .field("ca_certificate", &self.ca_certificate)
            .field("client_certificate", &self.client_certificate)
            .field("client_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CellHvError {
    #[error("CellHV configuration is invalid")]
    InvalidConfiguration,
    #[error("CellHV TLS material is unavailable")]
    TlsMaterial,
    #[error("CellHV transport is unavailable")]
    Transport(#[source] tonic::transport::Error),
    #[error("CellHV capability or operation response is incompatible")]
    Incompatible,
}
