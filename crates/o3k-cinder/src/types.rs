//! Cinder client domain types: errors, config, attachment, connection.

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub enum CinderError {
    #[error("cinder request was rejected: {0}")]
    InvalidRequest(String),
    #[error("cinder authentication failed")]
    Unauthorized,
    #[error("cinder resource was not found: {0}")]
    NotFound(String),
    #[error("cinder operation conflicts: {0}")]
    Conflict(String),
    #[error("cinder service is unavailable")]
    ServiceUnavailable,
    #[error("cinder response was malformed: {0}")]
    Protocol(String),
    #[error("cinder transport failure: {0}")]
    UnknownOutcome(String),
    #[error("keystone token acquisition failed: {0}")]
    Auth(String),
}

impl CinderError {
    /// Whether the error means the Cinder-side outcome is unknown (timeout,
    /// transport failure, or unclassified status). The caller must observe the
    /// attachment before retrying or compensating; it must never treat an
    /// unknown outcome as a confirmed failure.
    pub fn is_unknown_outcome(&self) -> bool {
        matches!(
            self,
            CinderError::UnknownOutcome(_)
                | CinderError::ServiceUnavailable
                | CinderError::Protocol(_)
        )
    }
}

/// Configuration for the outbound Cinder client. Credentials authenticate the
/// configured service identity; the scoped project is the project that owns
/// the volumes and attachments.
#[derive(Debug, Clone)]
pub struct CinderClientConfig {
    pub keystone_endpoint: String,
    pub cinder_endpoint: String,
    pub username: String,
    pub password: Secret,
    pub domain_name: String,
}

/// Bounded connector description matching the os-brick connector shape. The
/// application-level port type is reused so the outbound client speaks the
/// same bounded vocabulary as the orchestrator.
pub use o3k_provider::ComputeConnector;

/// Classification of the `connection_info` field of a Cinder attachment
/// response. The application-level vocabulary is reused so the adapter and
/// the orchestrator cannot drift apart.
pub use o3k_provider::ConnectionInfoPresence;

/// A Cinder attachment as returned by the API. `connection_info` is present
/// only after the connector update flow has completed.
#[derive(Debug, Clone)]
pub struct CinderAttachment {
    pub id: String,
    pub status: String,
    pub volume_id: String,
    pub connection_info: Option<ConnectionInfo>,
    presence: ConnectionInfoPresence,
}

impl CinderAttachment {
    pub fn parse(value: &serde_json::Value) -> Result<Self, CinderError> {
        // Single-object responses are wrapped (`{"attachment": {...}}`) while
        // list items are flat (`{"id": ..., "status": ...}`); both forms occur
        // in the Cinder 28 API.
        let attachment = value.get("attachment").unwrap_or(value);
        let id = attachment["id"]
            .as_str()
            .ok_or_else(|| CinderError::Protocol("attachment id is missing".to_owned()))?
            .to_owned();
        let status = attachment["status"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        let volume_id = attachment["volume_id"].as_str().unwrap_or("").to_owned();
        let (connection_info, presence) = match attachment.get("connection_info") {
            None => (None, ConnectionInfoPresence::Missing),
            Some(serde_json::Value::Null) => (None, ConnectionInfoPresence::Null),
            Some(serde_json::Value::Object(_)) => (
                Some(ConnectionInfo::new(&attachment["connection_info"])),
                ConnectionInfoPresence::Present,
            ),
            Some(_) => (None, ConnectionInfoPresence::Malformed),
        };
        Ok(Self {
            id,
            status,
            volume_id,
            connection_info,
            presence,
        })
    }

    /// How the `connection_info` field appeared in the response. Used for
    /// bounded redacted diagnostics and to decide between observation and
    /// compensation after an uncertain update outcome.
    pub fn connection_info_presence(&self) -> ConnectionInfoPresence {
        self.presence
    }
}

/// Bounded connection information extracted from a Cinder attachment. The raw
/// value is secret-bearing and is never formatted with its contents.
#[derive(Clone)]
pub struct ConnectionInfo {
    raw: serde_json::Value,
}

impl ConnectionInfo {
    fn new(raw: &serde_json::Value) -> Self {
        Self { raw: raw.clone() }
    }

    pub fn driver_volume_type(&self) -> Option<&str> {
        self.raw
            .get("driver_volume_type")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                self.raw
                    .get("data")
                    .and_then(|d| d.get("driver_volume_type"))
                    .and_then(serde_json::Value::as_str)
            })
    }

    /// Extracts the typed non-secret target data required to attach through
    /// the compute boundary. Returns `None` when the value is not a JSON object
    /// from which a target could be extracted.
    pub(crate) fn attach_target(&self) -> Option<AttachTarget> {
        if !self.raw.is_object() {
            return None;
        }
        let data = self.raw.get("data").unwrap_or(&self.raw);
        let auth_username = data
            .get("auth_username")
            .and_then(serde_json::Value::as_str)
            .map(|value| Secret::new(value.to_owned()));
        let auth_password = data
            .get("auth_password")
            .and_then(serde_json::Value::as_str)
            .map(|value| Secret::new(value.to_owned()));
        Some(AttachTarget {
            driver_volume_type: self.driver_volume_type().unwrap_or("unknown").to_owned(),
            target_iqn: data
                .get("target_iqn")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    data.get("target_iqns")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|arr| arr.first())
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }),
            target_portal: data
                .get("target_portal")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    data.get("target_portals")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|arr| arr.first())
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }),
            target_lun: data
                .get("target_lun")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .or_else(|| {
                    data.get("target_luns")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|arr| arr.first())
                        .and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        })
                }),
            local_path: data
                .get("device_path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    data.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }),
            auth_method: data
                .get("auth_method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            auth_username,
            auth_password,
        })
    }

    /// SHA-256 digest of the canonical serialization. Persisted instead of the
    /// raw connection information.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(&self.raw).unwrap_or_default();
        let digest = Sha256::digest(&canonical);
        URL_SAFE_NO_PAD.encode(digest)
    }

    /// Top-level field names only, for bounded redacted diagnostics. Never the
    /// values: secrets such as `auth_password` are named but their contents are
    /// not exposed.
    pub fn top_level_keys(&self) -> Vec<String> {
        self.raw
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the connection information carries a usable target for the
    /// compute boundary. Used to distinguish a real, driver-populated
    /// connection (e.g. an iSCSI target with IQN and portal, or a local path)
    /// from an empty `{}` or otherwise unusable value. An unusable value never
    /// justifies deleting a possibly-successful Cinder attachment without
    /// observation, but a usable one means the connector update completed
    /// server-side and must not be compensated.
    pub fn has_usable_target(&self) -> bool {
        match self.attach_target() {
            Some(target) => match target.driver_volume_type.as_str() {
                "iscsi" => target.target_iqn.is_some() && target.target_portal.is_some(),
                "local" => target.local_path.is_some(),
                _ => false,
            },
            None => false,
        }
    }

    /// Consumes the raw value; the caller owns the secret-safe extraction.
    pub fn into_raw(self) -> serde_json::Value {
        self.raw
    }
}

impl fmt::Debug for ConnectionInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ConnectionInfo(driver={:?}, sha256={})",
            self.driver_volume_type(),
            self.digest()
        )
    }
}

/// Typed non-secret target data plus transient CHAP credentials. Callers must
/// never persist or log the credential fields.
#[derive(Clone)]
pub(crate) struct AttachTarget {
    pub driver_volume_type: String,
    pub target_iqn: Option<String>,
    pub target_portal: Option<String>,
    pub target_lun: Option<u64>,
    pub local_path: Option<String>,
    pub auth_method: Option<String>,
    pub auth_username: Option<Secret>,
    pub auth_password: Option<Secret>,
}

impl fmt::Debug for AttachTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachTarget")
            .field("driver_volume_type", &self.driver_volume_type)
            .field("target_iqn", &self.target_iqn)
            .field("target_portal", &self.target_portal)
            .field("target_lun", &self.target_lun)
            .field("local_path", &self.local_path)
            .field("auth_method", &self.auth_method)
            .field("auth_username", &"<redacted>")
            .field("auth_password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TokenResponse {
    token: TokenFields,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TokenFields {
    expires_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedToken {
    token: Secret,
    expires_at: u64,
}

/// Typed, bounded Cinder v3 attachment client.
#[derive(Clone)]
