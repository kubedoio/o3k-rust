//! Application-level port for external volume-attachment service operations
//! used by Nova-compatible compute.
//!
//! The durable attachment orchestrator in `o3k-compute` drives the Cinder
//! attachment lifecycle through this bounded port. External request/response
//! models stay in the adapter (`o3k-cinder`); the adapter converts them into
//! these values at its boundary. CHAP credentials are carried only through
//! this port into the bounded compute attachment description and are never
//! logged or persisted (redacted `Debug`).

use serde::Serialize;
use thiserror::Error;

/// Classification of the `connection_info` of an attachment response. The
/// orchestrator must distinguish these cases: missing and null are
/// deterministic service-side outcomes, while a non-object value or an object
/// without an extractable target is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionInfoPresence {
    Missing,
    Null,
    Malformed,
    Present,
}

/// Typed non-secret target data plus transient CHAP credentials needed to
/// attach through the compute execution boundary. Callers must never persist
/// or log the credential fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTarget {
    pub driver_volume_type: String,
    pub target_iqn: Option<String>,
    pub target_portal: Option<String>,
    pub target_lun: Option<u64>,
    pub local_path: Option<String>,
    pub auth_method: Option<String>,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
}

impl std::fmt::Display for AttachmentTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentTarget")
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

/// Bounded representation of an attachment response's `connection_info`.
/// The adapter computes the presence classification, the digest, the top-level
/// key names, and the extractable target from its raw model; application logic
/// only consumes these precomputed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub presence: ConnectionInfoPresence,
    /// SHA-256 digest of the canonical serialization, persisted instead of
    /// the raw connection information.
    pub digest: String,
    /// Top-level field names only, for bounded redacted diagnostics. Never
    /// the values.
    pub top_level_keys: Vec<String>,
    pub target: Option<AttachmentTarget>,
}

impl ConnectionInfo {
    #[must_use]
    pub const fn presence(&self) -> ConnectionInfoPresence {
        self.presence
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn top_level_keys(&self) -> &[String] {
        &self.top_level_keys
    }

    /// Whether the connection information carries a usable target for the
    /// compute boundary. An unusable value never justifies deleting a
    /// possibly-successful attachment without observation.
    #[must_use]
    pub fn has_usable_target(&self) -> bool {
        match self.target.as_ref() {
            Some(target) => match target.driver_volume_type.as_str() {
                "iscsi" => target.target_iqn.is_some() && target.target_portal.is_some(),
                "local" => target.local_path.is_some(),
                _ => false,
            },
            None => false,
        }
    }

    #[must_use]
    pub const fn attach_target(&self) -> Option<&AttachmentTarget> {
        self.target.as_ref()
    }
}

/// One attachment as returned by the external volume service. `connection_info`
/// is present only after the connector update flow has completed; the
/// `presence` classification covers every response shape, including missing,
/// null, and malformed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentObservation {
    pub id: String,
    pub status: String,
    pub volume_id: String,
    pub presence: ConnectionInfoPresence,
    pub connection_info: Option<ConnectionInfo>,
}

impl AttachmentObservation {
    #[must_use]
    pub const fn connection_info_presence(&self) -> ConnectionInfoPresence {
        self.presence
    }
}

/// Bounded connector description matching the os-brick connector shape sent to
/// the volume service during the connector update flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComputeConnector {
    pub host: String,
    pub ip: String,
    pub platform: String,
    pub os_type: String,
    pub multipath: bool,
    pub initiator: Option<String>,
}

/// Failure classification from the external volume attachment service.
/// Timeouts and transport failures are unknown outcomes: the caller must
/// observe the attachment before retrying or compensating.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AttachmentError {
    #[error("attachment service rejected the request: {0}")]
    InvalidRequest(String),
    #[error("attachment resource was not found: {0}")]
    NotFound(String),
    #[error("attachment operation conflicts: {0}")]
    Conflict(String),
    #[error("attachment service authentication failed")]
    Unauthorized,
    #[error("attachment service is unavailable")]
    Unavailable,
    #[error("attachment response was malformed: {0}")]
    Protocol(String),
    #[error("attachment outcome is unknown: {0}")]
    UnknownOutcome(String),
}

impl AttachmentError {
    /// Whether the error means the service-side outcome is unknown (timeout,
    /// transport failure, or unclassified status). The caller must observe the
    /// attachment before retrying or compensating.
    #[must_use]
    pub fn is_unknown_outcome(&self) -> bool {
        matches!(
            self,
            AttachmentError::UnknownOutcome(_)
                | AttachmentError::Unavailable
                | AttachmentError::Protocol(_)
        )
    }
}

/// Bounded application port for the external volume-attachment lifecycle used
/// by Nova-compatible compute. Implemented by the adapter crate that owns the
/// external service client.
#[async_trait::async_trait]
pub trait VolumeAttachmentProvider: Send + Sync {
    /// Creates or reserves an attachment for a volume on a server.
    async fn create_attachment(
        &self,
        project_id: &str,
        volume_id: &str,
        server_id: Option<&str>,
    ) -> Result<AttachmentObservation, AttachmentError>;

    /// Provides the compute connector through the attachment update flow.
    async fn update_attachment_connector(
        &self,
        project_id: &str,
        attachment_id: &str,
        connector: &ComputeConnector,
    ) -> Result<AttachmentObservation, AttachmentError>;

    /// Completes the attachment after the compute device is attached.
    async fn complete_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<(), AttachmentError>;

    /// Terminates the attachment during detach or compensation.
    async fn terminate_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<(), AttachmentError>;

    /// Shows one attachment, used for observation before any compensation.
    async fn show_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<AttachmentObservation, AttachmentError>;

    /// Lists attachments for the project, used to observe a volume's
    /// attachments when the attachment identity is unknown.
    async fn list_attachments(
        &self,
        project_id: &str,
    ) -> Result<Vec<AttachmentObservation>, AttachmentError>;
}
