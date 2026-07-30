use std::{
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_DATA_DIR: &str = "./data";
const DEFAULT_LOG_FILTER: &str = "info";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Fake,
    CellHv,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

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
    pub log_format: LogFormat,
    pub log_filter: String,
    pub provider: Provider,
    bootstrap_secret: Option<Secret>,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("config_path", &self.config_path)
            .field("listen_addr", &self.listen_addr)
            .field("data_dir", &self.data_dir)
            .field("log_format", &self.log_format)
            .field("log_filter", &self.log_filter)
            .field("provider", &self.provider)
            .field("bootstrap_secret", &self.bootstrap_secret)
            .finish()
    }
}

impl Config {
    /// Loads defaults, then a TOML file, environment variables, and CLI flags.
    /// Later sources override earlier sources.
    pub fn from_sources<I, E>(args: I, environment: E) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = String>,
        E: IntoIterator<Item = (String, String)>,
    {
        let args: Vec<String> = args.into_iter().collect();
        let cli = PartialConfig::from_cli(&args)?;
        let environment: Vec<(String, String)> = environment.into_iter().collect();
        let config_path = cli
            .config_path
            .clone()
            .or_else(|| value_from_env(&environment, "O3K_CONFIG").map(PathBuf::from));

        let mut values = PartialConfig::default();
        if let Some(path) = &config_path {
            let contents = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
                path: path.clone(),
                source,
            })?;
            let file: FileConfig = toml::from_str(&contents)
                .map_err(|_| ConfigError::ParseFile { path: path.clone() })?;
            values.merge(file.into());
        }
        values.merge(PartialConfig::from_environment(&environment));
        values.merge(cli);
        values.into_config(config_path)
    }

    #[must_use]
    pub fn bootstrap_secret(&self) -> Option<&Secret> {
        self.bootstrap_secret.as_ref()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unknown command-line option `{0}`")]
    UnknownOption(String),
    #[error("missing value for command-line option `{0}`")]
    MissingValue(String),
    #[error("cannot read configuration file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse configuration file {path}")]
    ParseFile { path: PathBuf },
    #[error("listen address is invalid")]
    InvalidListenAddress,
    #[error("data directory must be non-empty and must not be the filesystem root")]
    InvalidDataDirectory,
    #[error("log filter must be non-empty")]
    EmptyLogFilter,
    #[error("log format must be `json` or `pretty`")]
    InvalidLogFormat,
    #[error("provider must be `fake` or `cellhv`")]
    InvalidProvider,
    #[error("bootstrap secret must not contain a newline")]
    InvalidSecret,
}

#[derive(Debug, Default, Clone)]
struct PartialConfig {
    config_path: Option<PathBuf>,
    listen_addr: Option<String>,
    data_dir: Option<String>,
    log_format: Option<String>,
    log_filter: Option<String>,
    provider: Option<String>,
    bootstrap_secret: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    listen_addr: Option<String>,
    data_dir: Option<String>,
    log_format: Option<String>,
    log_filter: Option<String>,
    provider: Option<String>,
    bootstrap_secret: Option<String>,
}

impl From<FileConfig> for PartialConfig {
    fn from(file: FileConfig) -> Self {
        Self {
            listen_addr: file.listen_addr,
            data_dir: file.data_dir,
            log_format: file.log_format,
            log_filter: file.log_filter,
            provider: file.provider,
            bootstrap_secret: file.bootstrap_secret,
            ..Self::default()
        }
    }
}

impl PartialConfig {
    fn merge(&mut self, other: Self) {
        if other.config_path.is_some() {
            self.config_path = other.config_path;
        }
        if other.listen_addr.is_some() {
            self.listen_addr = other.listen_addr;
        }
        if other.data_dir.is_some() {
            self.data_dir = other.data_dir;
        }
        if other.log_format.is_some() {
            self.log_format = other.log_format;
        }
        if other.log_filter.is_some() {
            self.log_filter = other.log_filter;
        }
        if other.provider.is_some() {
            self.provider = other.provider;
        }
        if other.bootstrap_secret.is_some() {
            self.bootstrap_secret = other.bootstrap_secret;
        }
    }

    fn from_environment(environment: &[(String, String)]) -> Self {
        Self {
            listen_addr: value_from_env(environment, "O3K_LISTEN_ADDR"),
            data_dir: value_from_env(environment, "O3K_DATA_DIR"),
            log_format: value_from_env(environment, "O3K_LOG_FORMAT"),
            log_filter: value_from_env(environment, "O3K_LOG_FILTER"),
            provider: value_from_env(environment, "O3K_PROVIDER"),
            bootstrap_secret: value_from_env(environment, "O3K_BOOTSTRAP_SECRET"),
            ..Self::default()
        }
    }

    fn from_cli(args: &[String]) -> Result<Self, ConfigError> {
        let mut result = Self::default();
        let mut args = args.iter().skip(1);
        while let Some(argument) = args.next() {
            let (option, inline_value) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(option, value)| {
                    (option, Some(value))
                });
            let mut value = |name: &str| {
                inline_value
                    .map(str::to_owned)
                    .or_else(|| args.next().cloned())
                    .ok_or_else(|| ConfigError::MissingValue(name.to_owned()))
            };
            match option {
                "--config" => result.config_path = Some(PathBuf::from(value("--config")?)),
                "--listen-addr" => result.listen_addr = Some(value("--listen-addr")?),
                "--data-dir" => result.data_dir = Some(value("--data-dir")?),
                "--log-format" => result.log_format = Some(value("--log-format")?),
                "--log-filter" => result.log_filter = Some(value("--log-filter")?),
                "--provider" => result.provider = Some(value("--provider")?),
                "--bootstrap-secret" => {
                    result.bootstrap_secret = Some(value("--bootstrap-secret")?)
                }
                option if option == "--help" || option == "-h" => {
                    return Err(ConfigError::UnknownOption(option.to_owned()));
                }
                option if option.starts_with('-') => {
                    return Err(ConfigError::UnknownOption(option.to_owned()));
                }
                value => return Err(ConfigError::UnknownOption(value.to_owned())),
            }
        }
        Ok(result)
    }

    fn into_config(self, config_path: Option<PathBuf>) -> Result<Config, ConfigError> {
        let listen_addr = self
            .listen_addr
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_owned())
            .parse()
            .map_err(|_| ConfigError::InvalidListenAddress)?;
        let data_dir = PathBuf::from(self.data_dir.unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned()));
        if data_dir.as_os_str().is_empty() || data_dir == Path::new("/") {
            return Err(ConfigError::InvalidDataDirectory);
        }
        let log_filter = self
            .log_filter
            .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());
        if log_filter.trim().is_empty() {
            return Err(ConfigError::EmptyLogFilter);
        }
        let log_format = match self
            .log_format
            .as_deref()
            .unwrap_or("json")
            .to_ascii_lowercase()
            .as_str()
        {
            "json" => LogFormat::Json,
            "pretty" => LogFormat::Pretty,
            _ => return Err(ConfigError::InvalidLogFormat),
        };
        let provider = match self
            .provider
            .as_deref()
            .unwrap_or("fake")
            .to_ascii_lowercase()
            .as_str()
        {
            "fake" => Provider::Fake,
            "cellhv" => Provider::CellHv,
            _ => return Err(ConfigError::InvalidProvider),
        };
        if self
            .bootstrap_secret
            .as_deref()
            .is_some_and(|secret| secret.contains(['\n', '\r']))
        {
            return Err(ConfigError::InvalidSecret);
        }

        Ok(Config {
            config_path,
            listen_addr,
            data_dir,
            log_format,
            log_filter,
            provider,
            bootstrap_secret: self.bootstrap_secret.map(Secret),
        })
    }
}

fn value_from_env(environment: &[(String, String)], name: &str) -> Option<String> {
    environment
        .iter()
        .find_map(|(key, value)| (key == name).then(|| value.clone()))
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, LogFormat, Provider};
    use std::{fs, path::PathBuf};

    fn env(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn precedence_is_defaults_then_file_then_environment_then_cli()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = PathBuf::from(format!(
            "/tmp/o3k-config-test-{}-{}.toml",
            std::process::id(),
            1
        ));
        fs::write(
            &path,
            "listen_addr = '127.0.0.1:9000'\ndata_dir = '/var/lib/o3k'\nprovider = 'cellhv'\n",
        )?;
        let args = vec![
            "o3kd".to_owned(),
            "--config".to_owned(),
            path.display().to_string(),
            "--listen-addr=127.0.0.1:9100".to_owned(),
        ];
        let config = Config::from_sources(
            args,
            env(&[("O3K_CONFIG", "ignored.toml"), ("O3K_DATA_DIR", "/tmp/o3k")]),
        )?;

        assert_eq!(config.listen_addr.to_string(), "127.0.0.1:9100");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/o3k"));
        assert_eq!(config.provider, Provider::CellHv);
        assert_eq!(config.log_format, LogFormat::Json);
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn secret_is_redacted_from_debug_and_displayed_errors() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = Config::from_sources(
            [
                "o3kd".to_owned(),
                "--bootstrap-secret".to_owned(),
                "top-secret".to_owned(),
            ],
            Vec::new(),
        )?;
        let secret = config
            .bootstrap_secret()
            .ok_or_else(|| std::io::Error::other("secret missing"))?;
        assert_eq!(secret.expose(), "top-secret");
        assert!(!format!("{config:?}").contains("top-secret"));
        assert_eq!(secret.to_string(), "<redacted>");
        Ok(())
    }

    #[test]
    fn invalid_configuration_fails_before_runtime_use() {
        let result = Config::from_sources(
            [
                "o3kd".to_owned(),
                "--listen-addr".to_owned(),
                "not-an-address".to_owned(),
            ],
            Vec::new(),
        );
        assert!(matches!(result, Err(ConfigError::InvalidListenAddress)));
    }
}
