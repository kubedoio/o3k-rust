//! Opaque cursor pagination for native O3K list endpoints.
//!
//! Architecture (SPEC-0030 §11):
//! - Cursors are opaque to clients (HMAC-authenticated base64).
//! - Each cursor binds to the owner scope + resource type so a cursor
//!   from one tenant/collection cannot be reused for another.
//! - Tampered cursors fail with INVALID_CURSOR.
//! - Bounded page size enforced by the server.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Default page size for native list endpoints.
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// Maximum page size that the server will accept.
pub const MAX_PAGE_SIZE: usize = 200;

/// Internal cursor payload — never exposed directly to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPayload {
    pub last_id: String,
    pub scope_id: String,
    pub resource_type: String,
    pub version: u8,
}

/// Cursor configuration held by the native API state.
#[derive(Clone, Default)]
pub struct CursorConfig {
    hmac_key: Vec<u8>,
}

impl CursorConfig {
    #[must_use]
    pub fn new(key: Vec<u8>) -> Self {
        Self { hmac_key: key }
    }

    /// Load the persistent server-held cursor secret. Production startup must
    /// fail closed when pagination is enabled without this value.
    pub fn from_env() -> Result<Self, &'static str> {
        let key = if let Ok(value) = std::env::var("O3K_NATIVE_CURSOR_HMAC_KEY") {
            value.into_bytes()
        } else {
            let token_key = std::env::var("O3K_TOKEN_SIGNING_KEY")
                .map_err(|_| "native cursor signing key is not configured")?;
            let mut hasher = Sha256::new();
            hasher.update(b"o3k/native-cursor/v1/");
            hasher.update(token_key.as_bytes());
            hasher.finalize().to_vec()
        };
        if key.len() < 32 {
            return Err("native cursor signing key is too short");
        }
        Ok(Self::new(key))
    }

    fn compute_hmac(&self, payload_json: &[u8]) -> Vec<u8> {
        // new_from_slice only fails when key is empty or > 64 bytes;
        // we guarantee non-empty via construction, so unwrap is safe.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.hmac_key) else {
            return Vec::new();
        };
        mac.update(payload_json);
        mac.finalize().into_bytes().to_vec()
    }

    /// Encodes an opaque cursor string with HMAC authenticity tag.
    pub fn encode_cursor(&self, payload: &CursorPayload) -> String {
        let json = serde_json::to_string(payload).unwrap_or_default();
        let hmac_bytes = self.compute_hmac(json.as_bytes());
        let hmac_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&hmac_bytes);
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
        format!("{}.{}", payload_b64, hmac_b64)
    }

    /// Decodes and authenticates a cursor.
    pub fn decode_cursor(
        &self,
        cursor: &str,
        expected_scope_id: &str,
        expected_type: &str,
    ) -> Result<CursorPayload, &'static str> {
        let (payload_b64, hmac_b64) = cursor.split_once('.').ok_or("invalid cursor format")?;

        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| "invalid cursor encoding")?;

        let payload: CursorPayload =
            serde_json::from_slice(&payload_bytes).map_err(|_| "malformed cursor payload")?;

        // Verify HMAC
        let provided_hmac = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(hmac_b64)
            .map_err(|_| "invalid cursor hmac")?;

        // HMAC verification — digest forgery is infeasible without the key
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.hmac_key) else {
            return Err("cursor HMAC unavailable");
        };
        mac.update(&payload_bytes);
        mac.verify_slice(&provided_hmac)
            .map_err(|_| "cursor HMAC mismatch")?;

        if payload.version != 1 {
            return Err("unsupported cursor version");
        }
        if payload.scope_id != expected_scope_id {
            return Err("cursor scope mismatch");
        }
        if payload.resource_type != expected_type {
            return Err("cursor resource type mismatch");
        }
        Ok(payload)
    }
}

/// Helper to extract page size from query parameters with bounds enforcement.
pub(crate) fn parse_page_size(limit_param: Option<&str>) -> usize {
    match limit_param.and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n > 0 => n.min(MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// Resolve a continuation against a deterministic, already-authorized ID
/// ordering. A missing anchor is stale and must not silently produce an empty
/// page.
#[allow(dead_code)]
pub(crate) fn continuation_index(ids: &[String], last_id: &str) -> Result<usize, &'static str> {
    ids.iter()
        .position(|id| id == last_id)
        .map(|index| index + 1)
        .ok_or("stale cursor anchor")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_config() -> CursorConfig {
        CursorConfig::new(b"test-key-16-bytes!".to_vec())
    }

    fn test_payload() -> CursorPayload {
        CursorPayload {
            last_id: "srv-abc-123".to_owned(),
            scope_id: "proj-1".to_owned(),
            resource_type: "compute:server".to_owned(),
            version: 1,
        }
    }

    #[test]
    fn encode_decode_round_trip() {
        let cfg = test_config();
        let payload = test_payload();
        let encoded = cfg.encode_cursor(&payload);
        assert!(!encoded.is_empty());
        assert!(encoded.contains('.'));

        let decoded = cfg
            .decode_cursor(&encoded, "proj-1", "compute:server")
            .unwrap();
        assert_eq!(decoded.last_id, "srv-abc-123");
        assert_eq!(decoded.scope_id, "proj-1");
        assert_eq!(decoded.resource_type, "compute:server");
        assert_eq!(decoded.version, 1);
    }

    #[test]
    fn decode_rejects_wrong_scope() {
        let cfg = test_config();
        let encoded = cfg.encode_cursor(&test_payload());
        let result = cfg.decode_cursor(&encoded, "proj-2", "compute:server");
        assert!(result.is_err());
    }

    #[test]
    fn decode_rejects_wrong_resource_type() {
        let cfg = test_config();
        let encoded = cfg.encode_cursor(&test_payload());
        let result = cfg.decode_cursor(&encoded, "proj-1", "volume:volume");
        assert!(result.is_err());
    }

    #[test]
    fn decode_rejects_tampered_last_id() {
        let cfg = test_config();
        let encoded = cfg.encode_cursor(&test_payload());

        // Replace last_id in the payload portion
        let (payload_b64, _) = encoded.split_once('.').unwrap();
        let mut payload: CursorPayload = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload_b64)
                .unwrap(),
        )
        .unwrap();
        payload.last_id = "tampered-id".to_owned();

        // Re-encode with WRONG hmac (tampered)
        let tampered_json = serde_json::to_string(&payload).unwrap();
        let tampered_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tampered_json.as_bytes());
        let wrong_hmac = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("fakehmac");
        let tampered = format!("{}.{}", tampered_b64, wrong_hmac);

        let result = cfg.decode_cursor(&tampered, "proj-1", "compute:server");
        assert!(result.is_err(), "tampered cursor must be rejected");
    }

    #[test]
    fn decode_rejects_malformed_cursor() {
        let cfg = test_config();
        let result = cfg.decode_cursor("not-valid!!", "proj-1", "compute:server");
        assert!(result.is_err());
    }

    #[test]
    fn decode_rejects_different_key() {
        let cfg1 = test_config();
        let cfg2 = CursorConfig::new(b"different-key-here!!".to_vec());
        let encoded = cfg1.encode_cursor(&test_payload());
        let result = cfg2.decode_cursor(&encoded, "proj-1", "compute:server");
        assert!(result.is_err(), "different key must reject");
    }

    #[test]
    fn parse_page_size_default() {
        assert_eq!(parse_page_size(None), DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_page_size_clamps() {
        assert_eq!(parse_page_size(Some("9999")), MAX_PAGE_SIZE);
        assert_eq!(parse_page_size(Some("0")), DEFAULT_PAGE_SIZE);
        assert_eq!(parse_page_size(Some("abc")), DEFAULT_PAGE_SIZE);
    }
}
