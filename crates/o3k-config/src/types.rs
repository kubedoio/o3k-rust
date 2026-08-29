//! Configuration domain types: deployment profile, database backend, config, errors.

use std::{fmt, net::SocketAddr, path::PathBuf};

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
pub const DEFAULT_DATA_DIR: &str = "./data";
pub const DEFAULT_LOG_FILTER: &str = "info";
pub const DEFAULT_COMPUTE_CONTROL_ADDR: &str = "127.0.0.1:50051";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DatabaseBackend {
    #[default]
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeploymentProfile {
    #[default]
    Standalone,
    Kubernetes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Fake,
    CellHv,
    Agent,
    Libvirt,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub(crate) String);

impl Secret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub config_path: Option<PathBuf>,
    pub listen_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub database_backend: DatabaseBackend,
    pub deployment_profile: DeploymentProfile,
    pub log_format: LogFormat,
    pub log_filter: String,
    pub provider: Provider,
    pub cellhv_endpoint: Option<String>,
    pub cellhv_expected_version: Option<String>,
    pub cellhv_ca_certificate: Option<PathBuf>,
    pub cellhv_client_certificate: Option<PathBuf>,
    pub cellhv_client_key: Option<PathBuf>,
    pub compute_control_addr: SocketAddr,
    pub compute_server_certificate: Option<PathBuf>,
    pub compute_server_private_key: Option<PathBuf>,
    pub compute_client_ca: Option<PathBuf>,
    pub compute_authorized_agents: Option<String>,
    pub(crate) database_url: Option<Secret>,
    pub(crate) bootstrap_secret: Option<Secret>,
    pub(crate) bootstrap_password: Option<Secret>,
    pub(crate) cinder_password: Option<Secret>,
    pub(crate) token_signing_key: Option<Secret>,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("config_path", &self.config_path)
            .field("listen_addr", &self.listen_addr)
            .field("data_dir", &self.data_dir)
            .field("database_backend", &self.database_backend)
            .field("deployment_profile", &self.deployment_profile)
            .field("database_url", &self.database_url)
            .field("log_format", &self.log_format)
            .field("log_filter", &self.log_filter)
            .field("provider", &self.provider)
            .field("cellhv_endpoint", &self.cellhv_endpoint)
            .field("cellhv_expected_version", &self.cellhv_expected_version)
            .field("cellhv_ca_certificate", &self.cellhv_ca_certificate)
            .field("cellhv_client_certificate", &self.cellhv_client_certificate)
            .field("cellhv_client_key", &"<redacted>")
            .field("compute_control_addr", &self.compute_control_addr)
            .field(
                "compute_server_certificate",
                &self.compute_server_certificate,
            )
            .field("compute_server_private_key", &"<redacted>")
            .field("compute_client_ca", &self.compute_client_ca)
            .field(
                "compute_authorized_agents",
                &self
                    .compute_authorized_agents
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field("bootstrap_secret", &self.bootstrap_secret)
            .field("bootstrap_password", &self.bootstrap_password)
            .field("cinder_password", &self.cinder_password)
            .field("token_signing_key", &self.token_signing_key)
            .finish()
    }
}
