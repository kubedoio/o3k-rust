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
///
/// These are contract-level values — removing or renaming an entry is a
/// compatibility change requiring review.
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

    /// Stable machine code as SCREAMING_SNAKE_CASE contract value.
    #[must_use]
    pub fn as_code_str(&self) -> &'static str {
        match self {
            Self::BadRequest => "BAD_REQUEST",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::ResourceNotFound => "RESOURCE_NOT_FOUND",
            Self::Conflict => "CONFLICT",
            Self::RequestTooLarge => "REQUEST_TOO_LARGE",
            Self::InvalidCursor => "INVALID_CURSOR",
            Self::UnsupportedMediaType => "UNSUPPORTED_MEDIA_TYPE",
            Self::NotAvailable => "NOT_AVAILABLE",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

/// RFC 9457-compatible Problem Details response body.
///
/// Only public-okay fields (machine code, request_id, resource_id) are
/// included. Internal/store/provider errors are logged, not sent.
#[derive(Debug, Clone, Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub typ: String,
    pub title: String,
    pub status: u16,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
}

impl ProblemDetails {
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            typ: format!(
                "https://o3k.io/problems/{}",
                code.title().to_lowercase().replace(' ', "-")
            ),
            title: code.title().to_owned(),
            status: code.status().as_u16(),
            code: code.as_code_str().to_owned(),
            request_id: None,
            detail: None,
            resource_id: None,
        }
    }

    #[must_use]
    pub fn with_detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        let mut pd = Self::new(code);
        pd.detail = Some(detail.into());
        pd
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    #[must_use]
    pub fn with_resource_id(mut self, resource_id: impl Into<String>) -> Self {
        self.resource_id = Some(resource_id.into());
        self
    }

    // ── Common error shortcuts ────────────────────────────────────────────

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(ErrorCode::Unauthorized)
    }

    #[must_use]
    pub fn not_found(resource_id: Option<&str>) -> Self {
        let mut pd = Self::new(ErrorCode::ResourceNotFound);
        pd.resource_id = resource_id.map(|id| id.to_owned());
        pd
    }

    #[must_use]
    pub fn forbidden(detail: Option<&str>) -> Self {
        match detail {
            Some(d) => Self::with_detail(ErrorCode::Forbidden, d),
            None => Self::new(ErrorCode::Forbidden),
        }
    }

    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::with_detail(ErrorCode::BadRequest, detail)
    }

    /// INTERNAL_ERROR with no detail exposed to the client.
    /// The actual error is logged separately via tracing.
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
                (StatusCode::INTERNAL_SERVER_ERROR, axum::body::Body::empty()).into_response()
            })
    }
}

/// Convenience trait to convert common axum rejection/errors into
/// `ProblemDetails` responses with the correct Content-Type.
pub trait IntoProblemResponse {
    fn into_problem_response(self) -> Response;
}

impl IntoProblemResponse for ProblemDetails {
    fn into_problem_response(self) -> Response {
        self.into_response()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn error_code_status_mapping() {
        assert_eq!(ErrorCode::BadRequest.status(), StatusCode::BAD_REQUEST);
        assert_eq!(ErrorCode::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ErrorCode::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(ErrorCode::ResourceNotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(ErrorCode::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            ErrorCode::InternalError.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn error_code_strings_are_explicit_contract_values() {
        assert_eq!(ErrorCode::BadRequest.as_code_str(), "BAD_REQUEST");
        assert_eq!(
            ErrorCode::ResourceNotFound.as_code_str(),
            "RESOURCE_NOT_FOUND"
        );
        assert_eq!(ErrorCode::Unauthorized.as_code_str(), "UNAUTHORIZED");
        assert_eq!(ErrorCode::Forbidden.as_code_str(), "FORBIDDEN");
        assert_eq!(ErrorCode::InternalError.as_code_str(), "INTERNAL_ERROR");
        assert_eq!(ErrorCode::InvalidCursor.as_code_str(), "INVALID_CURSOR");
        assert_eq!(ErrorCode::Conflict.as_code_str(), "CONFLICT");
    }

    #[test]
    fn problem_details_unauthorized() {
        let pd = ProblemDetails::unauthorized();
        assert_eq!(pd.status, 401);
        assert_eq!(pd.code, "UNAUTHORIZED");
        assert!(pd.detail.is_none());
        assert!(pd.request_id.is_none());
        assert!(pd.resource_id.is_none());
    }

    #[test]
    fn problem_details_not_found() {
        let pd = ProblemDetails::not_found(Some("srv-abc"));
        assert_eq!(pd.status, 404);
        assert_eq!(pd.code, "RESOURCE_NOT_FOUND");
        assert_eq!(pd.resource_id.as_deref(), Some("srv-abc"));
    }

    #[test]
    fn problem_details_internal_has_no_detail() {
        let pd = ProblemDetails::internal();
        assert_eq!(pd.status, 500);
        assert_eq!(pd.code, "INTERNAL_ERROR");
        assert!(pd.detail.is_none());
        assert!(pd.request_id.is_none());
    }

    #[test]
    fn problem_details_serialization_matches_rfc9457() {
        let pd = ProblemDetails::unauthorized().with_request_id("req-abc");
        let json = serde_json::to_value(&pd).unwrap();
        assert_eq!(json["type"], "https://o3k.io/problems/unauthorized");
        assert_eq!(json["title"], "Unauthorized");
        assert_eq!(json["status"], 401);
        assert_eq!(json["code"], "UNAUTHORIZED");
        assert_eq!(json["request_id"], "req-abc");
        assert!(json.get("detail").is_none());
        assert!(json.get("resource_id").is_none());
    }

    #[test]
    fn problem_details_into_response_content_type() {
        let pd = ProblemDetails::unauthorized();
        let resp: Response = pd.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get("Content-Type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/problem+json"
        );
    }
}
