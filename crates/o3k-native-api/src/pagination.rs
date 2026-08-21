//! Opaque cursor pagination for native O3K list endpoints.
//!
//! Architecture (SPEC-0030 §11):
//! - Cursors are opaque to clients (base64-encoded JSON).
//! - Each cursor binds to the owner scope so a cursor from one tenant
//!   cannot iterate another tenant's resources.
//! - Tampered/malformed cursors fail safely.
//! - Bounded page size enforced by the server.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Default page size for native list endpoints.
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// Maximum page size that the server will accept.
pub const MAX_PAGE_SIZE: usize = 200;

/// Internal cursor payload — never exposed directly to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPayload {
    /// The last-seen resource ID (exclusive start for the next page).
    pub last_id: String,
    /// A stable scope identifier, bound so cross-tenant cursor use fails.
    pub scope_id: String,
    /// Cursor schema version for forward compatibility.
    pub version: u8,
}

/// Encodes an opaque cursor string from the internal payload.
///
/// The cursor is base64-encoded JSON. Clients MUST NOT inspect or reverse.
pub(crate) fn encode_cursor(payload: &CursorPayload) -> String {
    let json = serde_json::to_string(payload).unwrap_or_else(|_| String::new());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// Decodes and validates a cursor string, returning the internal payload.
///
/// Returns `None` if the cursor is malformed, tampered, or from a
/// different scope than the caller's.
pub(crate) fn decode_cursor(
    cursor: &str,
    expected_scope_id: &str,
) -> Result<CursorPayload, &'static str> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "invalid cursor encoding")?;
    let payload: CursorPayload =
        serde_json::from_slice(&bytes).map_err(|_| "malformed cursor payload")?;

    if payload.version != 1 {
        return Err("unsupported cursor version");
    }
    if payload.scope_id != expected_scope_id {
        return Err("cursor scope mismatch");
    }
    Ok(payload)
}

/// Helper to extract page size from query parameters with bounds enforcement.
pub(crate) fn parse_page_size(limit_param: Option<&str>) -> usize {
    match limit_param.and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n > 0 => n.min(MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}
