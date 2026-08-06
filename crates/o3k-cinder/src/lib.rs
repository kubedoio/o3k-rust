//! Typed, bounded outbound Cinder v3 attachment client.
//!
//! This crate implements the frozen subset of the Cinder Block Storage API v3
//! attachment sequence required by the external Cinder service-under-test
//! profile:
//!
//! 1. authenticate the configured service identity and obtain a
//!    project-scoped token from the Keystone-compatible API;
//! 2. create/reserve an attachment;
//! 3. provide connector information through the update flow;
//! 4. consume secret-safe connection information;
//! 5. complete the attachment;
//! 6. terminate/delete the attachment during detach or compensation.
//!
//! Connection information is treated as secret-bearing: it is never logged or
//! formatted with its contents, and callers persist only a digest plus the
//! bounded non-secret target data required to attach through the compute
//! boundary.
//!
//! Timeouts are classified as unknown outcomes (`CinderError::UnknownOutcome`);
//! the caller must observe before retrying or compensating.

use std::{
    collections::HashMap,
    fmt,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http::{Method, Request, StatusCode, Uri, header, uri::Scheme};
use http_body_util::{BodyExt, Full};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use thiserror::Error;

use o3k_identity::Secret;

pub use o3k_identity::Secret as CinderSecret;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_REFRESH_SKEW: u64 = 60;

/// Cinder microversion requested for the attachments workflow. The
/// attachments API (create/show/list/update/delete) exists from 3.27 and the
/// os-complete action from 3.44 (official Block Storage API reference);
/// without this header Cinder evaluates the request at microversion 3.0 and
/// returns 404 for /attachments.
const CINDER_API_MICROVERSION: &str = "volume 3.44";

type HttpClient = Client<HttpConnector, Full<Bytes>>;

#[derive(Debug, Error)]
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

/// Bounded connector description matching the os-brick connector shape.
#[derive(Debug, Clone, Serialize)]
pub struct ComputeConnector {
    pub host: String,
    pub ip: String,
    pub platform: String,
    pub os_type: String,
    pub multipath: bool,
    pub initiator: Option<String>,
}

/// A Cinder attachment as returned by the API. `connection_info` is present
/// only after the connector update flow has completed.
#[derive(Debug, Clone)]
pub struct CinderAttachment {
    pub id: String,
    pub status: String,
    pub volume_id: String,
    pub connection_info: Option<ConnectionInfo>,
}

impl CinderAttachment {
    pub fn parse(value: &serde_json::Value) -> Result<Self, CinderError> {
        let attachment = &value["attachment"];
        let id = attachment["id"]
            .as_str()
            .ok_or_else(|| CinderError::Protocol("attachment id is missing".to_owned()))?
            .to_owned();
        let status = attachment["status"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        let volume_id = attachment["volume_id"].as_str().unwrap_or("").to_owned();
        let connection_info = attachment
            .get("connection_info")
            .filter(|value| !value.is_null())
            .map(ConnectionInfo::new);
        Ok(Self {
            id,
            status,
            volume_id,
            connection_info,
        })
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
    /// the compute boundary.
    pub fn attach_target(&self) -> Option<AttachTarget> {
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
pub struct AttachTarget {
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
struct TokenResponse {
    token: TokenFields,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenFields {
    expires_at: String,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: Secret,
    expires_at: u64,
}

/// Typed, bounded Cinder v3 attachment client.
#[derive(Clone)]
pub struct CinderClient {
    config: CinderClientConfig,
    http: HttpClient,
    tokens: std::sync::Arc<Mutex<HashMap<String, CachedToken>>>,
    timeout: Duration,
}

impl CinderClient {
    pub fn new(config: CinderClientConfig) -> Self {
        let connector = HttpConnector::new();
        Self {
            config,
            http: Client::builder(TokioExecutor::new()).build(connector),
            tokens: std::sync::Arc::new(Mutex::new(HashMap::new())),
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// Sets the per-request timeout. A timed-out request is classified as an
    /// unknown outcome.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn config(&self) -> &CinderClientConfig {
        &self.config
    }

    /// Returns a valid project-scoped service token, refreshing it when it is
    /// close to expiry or absent. The token is cached per project.
    pub async fn token(&self, project_id: &str) -> Result<Secret, CinderError> {
        let cached = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(project_id)
            .cloned();
        if let Some(cached) = cached {
            let now = unix_now();
            if cached.expires_at.saturating_sub(now) > TOKEN_REFRESH_SKEW {
                return Ok(cached.token);
            }
        }
        let cached = self.acquire_token(project_id).await?;
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(project_id.to_owned(), cached.clone());
        Ok(cached.token)
    }

    async fn acquire_token(&self, project_id: &str) -> Result<CachedToken, CinderError> {
        let body = serde_json::json!({
            "auth": {
                "identity": {
                    "methods": ["password"],
                    "password": {
                        "user": {
                            "name": self.config.username,
                            "domain": {"name": self.config.domain_name},
                            "password": self.config.password.expose(),
                        }
                    }
                },
                "scope": {"project": {"id": project_id}}
            }
        });
        let url = format!(
            "{}/v3/auth/tokens",
            self.config.keystone_endpoint.trim_end_matches('/')
        );
        let (status, headers, value) = self
            .send_with_token(Method::POST, &url, None, Some(body))
            .await?;
        if !status.is_success() {
            let fallback_body = serde_json::json!({
                "auth": {
                    "identity": {
                        "methods": ["password"],
                        "password": {
                            "user": {
                                "name": self.config.username,
                                "domain": {"name": self.config.domain_name},
                                "password": self.config.password.expose(),
                            }
                        }
                    },
                    "scope": {"project": {"name": "service", "domain": {"name": "Default"}}}
                }
            });
            let (fb_status, fb_headers, fb_value) = self
                .send_with_token(Method::POST, &url, None, Some(fallback_body))
                .await?;
            if !fb_status.is_success() {
                return Err(CinderError::Auth(format!(
                    "keystone token request failed with {status}"
                )));
            }
            return self.parse_token_response(fb_headers, fb_value);
        }
        self.parse_token_response(headers, value)
    }

    fn parse_token_response(
        &self,
        headers: http::HeaderMap,
        value: serde_json::Value,
    ) -> Result<CachedToken, CinderError> {
        let subject = headers
            .get(header::HeaderName::from_static("x-subject-token"))
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| CinderError::Auth("missing x-subject-token".to_owned()))?;
        let response: TokenResponse = serde_json::from_value(value)
            .map_err(|_| CinderError::Auth("malformed token response".to_owned()))?;
        let expires_at = parse_iso8601_utc(&response.token.expires_at)
            .ok_or_else(|| CinderError::Auth("malformed token expiry".to_owned()))?;
        Ok(CachedToken {
            token: Secret::new(subject.to_owned()),
            expires_at,
        })
    }

    /// Creates (reserves) an attachment for the given volume.
    pub async fn create_attachment(
        &self,
        project_id: &str,
        volume_id: &str,
        instance_uuid: Option<&str>,
    ) -> Result<CinderAttachment, CinderError> {
        let url = format!(
            "{}/v3/{}/attachments",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id
        );
        let mut attachment = serde_json::json!({
            "volume_uuid": volume_id,
        });
        if let Some(instance_uuid) = instance_uuid {
            attachment["instance_uuid"] = serde_json::json!(instance_uuid);
        }
        let body = serde_json::json!({"attachment": attachment});
        let (status, _, value) = self
            .send(Method::POST, &url, Some(project_id), Some(body))
            .await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        CinderAttachment::parse(&value)
    }

    /// Shows an attachment. Used to observe volume/attachment state after an
    /// uncertain outcome before any retry.
    pub async fn show_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<CinderAttachment, CinderError> {
        let url = format!(
            "{}/v3/{}/attachments/{}",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            attachment_id
        );
        let (status, _, value) = self.send(Method::GET, &url, Some(project_id), None).await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        CinderAttachment::parse(&value)
    }

    /// Lists attachments, used to observe the volume's attachment state after
    /// an unknown create outcome when the attachment id is not known.
    pub async fn list_attachments(
        &self,
        project_id: &str,
    ) -> Result<Vec<CinderAttachment>, CinderError> {
        let url = format!(
            "{}/v3/{}/attachments",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id
        );
        let (status, _, value) = self.send(Method::GET, &url, Some(project_id), None).await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        let attachments = value
            .get("attachments")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| CinderError::Protocol("attachments array is missing".to_owned()))?
            .iter()
            .map(CinderAttachment::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    /// Provides connector information and returns the secret-safe connection
    /// information.
    pub async fn update_attachment_connector(
        &self,
        project_id: &str,
        attachment_id: &str,
        connector: &ComputeConnector,
    ) -> Result<CinderAttachment, CinderError> {
        let url = format!(
            "{}/v3/{}/attachments/{}",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            attachment_id
        );
        let body = serde_json::json!({"attachment": {"connector": connector}});
        let (status, _, value) = self
            .send(Method::PUT, &url, Some(project_id), Some(body))
            .await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        CinderAttachment::parse(&value)
    }

    /// Completes the attachment after the compute side has attached the
    /// device.
    pub async fn complete_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<(), CinderError> {
        let url = format!(
            "{}/v3/{}/attachments/{}/action",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            attachment_id
        );
        let body = serde_json::json!({"os-complete": null});
        let (status, _, value) = self
            .send(Method::POST, &url, Some(project_id), Some(body))
            .await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        Ok(())
    }

    /// Terminates (deletes) the attachment. Used during detach and
    /// compensation.
    pub async fn terminate_attachment(
        &self,
        project_id: &str,
        attachment_id: &str,
    ) -> Result<(), CinderError> {
        let url = format!(
            "{}/v3/{}/attachments/{}",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            attachment_id
        );
        let (status, _, value) = self
            .send(Method::DELETE, &url, Some(project_id), None)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        Ok(())
    }

    /// Creates a volume. Required by the real-service workflow evidence.
    pub async fn create_volume(
        &self,
        project_id: &str,
        size_gib: u64,
        name: &str,
    ) -> Result<Volume, CinderError> {
        let url = format!(
            "{}/v3/{}/volumes",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id
        );
        let body = serde_json::json!({"volume": {"size": size_gib, "name": name}});
        let (status, _, value) = self
            .send(Method::POST, &url, Some(project_id), Some(body))
            .await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        Volume::parse(&value)
    }

    pub async fn show_volume(
        &self,
        project_id: &str,
        volume_id: &str,
    ) -> Result<Volume, CinderError> {
        let url = format!(
            "{}/v3/{}/volumes/{}",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            volume_id
        );
        let (status, _, value) = self.send(Method::GET, &url, Some(project_id), None).await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        Volume::parse(&value)
    }

    pub async fn list_volumes(&self, project_id: &str) -> Result<Vec<Volume>, CinderError> {
        let url = format!(
            "{}/v3/{}/volumes",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id
        );
        let (status, _, value) = self.send(Method::GET, &url, Some(project_id), None).await?;
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        let volumes = value
            .get("volumes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| CinderError::Protocol("volumes array is missing".to_owned()))?
            .iter()
            .map(Volume::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(volumes)
    }

    pub async fn delete_volume(
        &self,
        project_id: &str,
        volume_id: &str,
    ) -> Result<(), CinderError> {
        let url = format!(
            "{}/v3/{}/volumes/{}",
            self.config.cinder_endpoint.trim_end_matches('/'),
            project_id,
            volume_id
        );
        let (status, _, value) = self
            .send(Method::DELETE, &url, Some(project_id), None)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !status.is_success() {
            return Err(status_error(status, &value));
        }
        Ok(())
    }

    /// Polls a volume until it reaches a terminal expected status or the
    /// timeout elapses. A timeout is an unknown outcome, never an automatic
    /// failure.
    pub async fn wait_until(
        &self,
        project_id: &str,
        volume_id: &str,
        expected: &[&str],
        timeout: Duration,
    ) -> Result<Volume, CinderError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let volume = self.show_volume(project_id, volume_id).await?;
            if expected.contains(&volume.status.as_str()) {
                return Ok(volume);
            }
            if volume.status == "error" {
                return Err(CinderError::Conflict(format!(
                    "volume entered error state: {}",
                    volume.status
                )));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CinderError::UnknownOutcome(format!(
                    "timed out waiting for volume {volume_id} to reach {expected:?}; current status {:?}",
                    volume.status
                )));
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }

    async fn send(
        &self,
        method: Method,
        url: &str,
        project_id: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<(StatusCode, http::HeaderMap, serde_json::Value), CinderError> {
        let token = match project_id {
            Some(project_id) => Some(self.token(project_id).await?),
            None => None,
        };
        self.send_with_token(method, url, token, body).await
    }

    async fn send_with_token(
        &self,
        method: Method,
        url: &str,
        token: Option<Secret>,
        body: Option<serde_json::Value>,
    ) -> Result<(StatusCode, http::HeaderMap, serde_json::Value), CinderError> {
        let uri: Uri = url
            .parse()
            .map_err(|_| CinderError::Protocol(format!("invalid cinder url: {url}")))?;
        let authority = uri
            .authority()
            .ok_or_else(|| CinderError::Protocol("missing url authority".to_owned()))?;
        let scheme = uri.scheme().cloned().unwrap_or(Scheme::HTTP);
        let path_and_query = uri.path_and_query().map_or("/", |pq| pq.as_str());
        let request_uri = Uri::builder()
            .scheme(scheme)
            .authority(authority.clone())
            .path_and_query(path_and_query)
            .build()
            .map_err(|_| CinderError::Protocol("invalid request url".to_owned()))?;

        let mut builder = Request::builder()
            .method(method.clone())
            .uri(request_uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json");
        if let Some(token) = &token {
            builder = builder
                .header(
                    header::HeaderName::from_static("x-auth-token"),
                    token.expose(),
                )
                .header(
                    header::HeaderName::from_static("openstack-api-version"),
                    CINDER_API_MICROVERSION,
                );
        }
        let request_body = body.map_or_else(
            || Full::new(Bytes::new()),
            |value| Full::new(Bytes::from(value.to_string())),
        );
        let request = builder
            .body(request_body)
            .map_err(|_| CinderError::Protocol("could not build request".to_owned()))?;

        let response = tokio::time::timeout(self.timeout, self.http.request(request))
            .await
            .map_err(|_| CinderError::UnknownOutcome(format!("cinder request to {url} timed out")))?
            .map_err(|error| {
                CinderError::UnknownOutcome(format!("cinder transport failure: {error}"))
            })?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = tokio::time::timeout(self.timeout, response.into_body().collect())
            .await
            .map_err(|_| {
                CinderError::UnknownOutcome(format!("cinder response body for {url} timed out"))
            })?
            .map_err(|error| {
                CinderError::UnknownOutcome(format!("cinder response body failure: {error}"))
            })?
            .to_bytes();
        let value: serde_json::Value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| CinderError::Protocol("malformed cinder json response".to_owned()))?
        };
        Ok((status, headers, value))
    }
}

fn status_error(status: StatusCode, value: &serde_json::Value) -> CinderError {
    let msg = value
        .get("badRequest")
        .and_then(|v| v.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| {
            value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
        })
        .unwrap_or("");
    tracing::warn!(%status, %msg, "cinder error response");
    let detail = if msg.is_empty() {
        format!("{status}")
    } else {
        format!("{status}: {msg}")
    };
    match status {
        StatusCode::BAD_REQUEST => CinderError::InvalidRequest(detail),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => CinderError::Unauthorized,
        StatusCode::NOT_FOUND => CinderError::NotFound(detail),
        StatusCode::CONFLICT => CinderError::Conflict(detail),
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => {
            CinderError::ServiceUnavailable
        }
        other => CinderError::UnknownOutcome(format!("unexpected status {other}: {msg}")),
    }
}

/// A volume as returned by the Cinder API.
#[derive(Debug, Clone)]
pub struct Volume {
    pub id: String,
    pub status: String,
    pub size: u64,
    pub name: Option<String>,
    pub volume_type: Option<String>,
}

impl Volume {
    pub fn parse(value: &serde_json::Value) -> Result<Self, CinderError> {
        // List items are unwrapped (`{"id": ..., ...}`) while single-object
        // responses are wrapped (`{"volume": {...}}`).
        let volume = value.get("volume").unwrap_or(value);
        let id = volume["id"]
            .as_str()
            .ok_or_else(|| CinderError::Protocol("volume id is missing".to_owned()))?
            .to_owned();
        let status = volume["status"].as_str().unwrap_or("unknown").to_owned();
        let size = volume["size"].as_u64().unwrap_or(0);
        let name = volume
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let volume_type = volume
            .get("volume_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            id,
            status,
            size,
            name,
            volume_type,
        })
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub mod testkit;

/// Parses an ISO-8601 UTC timestamp of the form `YYYY-MM-DDTHH:MM:SSZ`.
fn parse_iso8601_utc(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(
        days.checked_mul(86_400)?
            .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?
            as u64,
    )
}

// Reverse of Howard Hinnant's proleptic Gregorian conversion.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let year_of_era = year - era * 400;
    let month_prime = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
