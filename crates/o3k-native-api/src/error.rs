//! RFC 9457-compatible HTTP Problem Details for the native O3K API.
//!
//! Every native API error returns `Content-Type: application/problem+json`
//! with a stable O3K machine `code` and optional `request_id`, `resource_id`,
//! and `detail` fields.
//!
//! Error responses MUST NOT expose SQL errors, secrets, credentials,
//! provider-private information, or cross-tenant resource metadata.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// O3K stable machine-readable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Generic bad request / validation error.
    BadRequest,
    /// Missing or invalid authentication credentials.
    Unauthorized,
    /// Authenticated caller lacks required authorization.
    Forbidden,
    /// The requested resource was not found.
    ResourceNotFound,
    /// The requested operation conflicts with current resource state.
    Conflict,
    /// Request too large or pagination limit exceeded.
    RequestTooLarge,
    /// Malformed pagination cursor.
    InvalidCursor,
    /// Unsupported media type or content type.
    UnsupportedMediaType,
    /// The requested resource type or service is not available.
    NotAvailable,
    /// An unexpected internal error occurred.
    InternalError,
}

impl ErrorCode {
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::ResourceNotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::RequestTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::InvalidCursor => StatusCode::BAD_REQUEST,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::NotAvailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::BadRequest => "Bad Request",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::ResourceNotFound => "Resource Not Found",
            Self::Conflict => "Conflict",
            Self::RequestTooLarge => "Request Too Large",
            Self::InvalidCursor => "Invalid Cursor",
            Self::UnsupportedMediaType => "Unsupported Media Type",
            Self::NotAvailable => "Not Available",
            Self::InternalError => "Internal Server Error",
        }
    }
}

/// RFC 9457-compatible Problem Details response body.
#[derive(Debug, Clone, Serialize)]
pub struct ProblemDetails {
    /// A URI reference identifying the problem type.
    #[serde(rename = "type")]
    pub typ: String,
    /// A short, human-readable summary.
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// O3K stable machine code.
    pub code: String,
    /// The request correlation ID, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Human-readable explanation specific to this occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Resource identifier relevant to the error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
}

impl ProblemDetails {
    /// Creates a new ProblemDetails for the given error code.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            typ: format!(
                "https://o3k.io/problems/{}",
                code.title().to_lowercase().replace(' ', "-")
            ),
            title: code.title().to_owned(),
            status: code.status().as_u16(),
            code: format!("{:?}", code).to_uppercase(),
            request_id: None,
            detail: None,
            resource_id: None,
        }
    }

    /// Creates a ProblemDetails with a custom detail message.
    #[must_use]
    pub fn with_detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        let mut pd = Self::new(code);
        pd.detail = Some(detail.into());
        pd
    }

    /// Sets the request correlation ID.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the resource ID.
    #[must_use]
    pub fn with_resource_id(mut self, resource_id: impl Into<String>) -> Self {
        self.resource_id = Some(resource_id.into());
        self
    }

    // ── Common error shortcuts ────────────────────────────────────────────

    /// Returns an unauthorized error.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(ErrorCode::Unauthorized)
    }

    /// Returns a not-found error with optional resource id.
    #[must_use]
    pub fn not_found(resource_id: Option<&str>) -> Self {
        let mut pd = Self::new(ErrorCode::ResourceNotFound);
        pd.resource_id = resource_id.map(|id| id.to_owned());
        pd
    }

    /// Returns a forbidden error with optional detail.
    #[must_use]
    pub fn forbidden(detail: Option<&str>) -> Self {
        match detail {
            Some(d) => Self::with_detail(ErrorCode::Forbidden, d),
            None => Self::new(ErrorCode::Forbidden),
        }
    }

    /// Returns a bad request error with the given detail.
    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::with_detail(ErrorCode::BadRequest, detail)
    }

    /// Returns an internal error with the given detail (for logging, not
    /// sent to the client).
    #[must_use]
    pub fn internal() -> Self {
        Self::new(ErrorCode::InternalError)
    }
}

impl IntoResponse for ProblemDetails {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_string(&self).unwrap_or_else(|_| {
            r#"{"type":"https://o3k.io/problems/internal-error","title":"Internal Server Error","status":500,"code":"INTERNAL_ERROR"}"#.to_owned()
        });
        Response::builder()
            .status(status)
            .header("Content-Type", "application/problem+json")
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| {
                // Status 500 with empty body — builder cannot fail on valid status.
                (StatusCode::INTERNAL_SERVER_ERROR, axum::body::Body::empty()).into_response()
            })
    }
}
