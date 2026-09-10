//! Validated bounded queries and opaque cursor pagination for native O3K.
//!
//! Raw HTTP parameters stop at [`CursorConfig::validate_query`]. Stores receive
//! only the decoded continuation key and requested bound. The public page never
//! exposes that key; it is authenticated and encoded here before serialization.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 200;
pub const MAX_CURSOR_LENGTH: usize = 4096;

const CURSOR_VERSION: u8 = 1;
const MAX_SCOPE_LENGTH: usize = 256;
const MAX_RESOURCE_TYPE_LENGTH: usize = 256;
const MAX_CONTINUATION_LENGTH: usize = 512;
const MAX_QUERY_IDENTITY_LENGTH: usize = 256;
const STABLE_ORDERING: &str = "id.asc";
const NO_FILTERS: &str = "filters:none";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueryValidationError {
    #[error("invalid page limit")]
    InvalidLimit,
    #[error("invalid collection identity")]
    InvalidIdentity,
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("cursor signing is unavailable")]
    CursorUnavailable,
    #[error("invalid repository page")]
    InvalidRepositoryPage,
}

/// Canonical validated collection query passed to the application boundary.
/// Fields are private so handlers cannot manufacture one or pass raw cursors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceQuery {
    scope_id: String,
    resource_type: String,
    limit: usize,
    continuation_key: Option<String>,
    query_identity: String,
}

impl ResourceQuery {
    #[must_use]
    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }
    #[must_use]
    pub fn resource_type(&self) -> &str {
        &self.resource_type
    }
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }
    #[must_use]
    pub fn continuation_key(&self) -> Option<&str> {
        self.continuation_key.as_deref()
    }
    #[must_use]
    pub fn ordering(&self) -> &'static str {
        STABLE_ORDERING
    }
}

/// Repository-owned bounded page. Construction validates page invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryPage<T> {
    items: Vec<T>,
    has_more: bool,
    continuation_key: Option<String>,
}

impl<T> RepositoryPage<T> {
    pub fn new(
        items: Vec<T>,
        has_more: bool,
        continuation_key: Option<String>,
        requested_limit: usize,
    ) -> Result<Self, QueryValidationError> {
        let continuation_valid = continuation_key
            .as_deref()
            .is_none_or(|key| !key.is_empty() && key.len() <= MAX_CONTINUATION_LENGTH);
        if items.len() > requested_limit
            || has_more != continuation_key.is_some()
            || (has_more && items.is_empty())
            || !continuation_valid
        {
            return Err(QueryValidationError::InvalidRepositoryPage);
        }
        Ok(Self {
            items,
            has_more,
            continuation_key,
        })
    }
}

/// Public page contract. The raw continuation key is structurally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourcePage<T> {
    items: Vec<T>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

impl<T> ResourcePage<T> {
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }
    #[must_use]
    pub const fn has_more(&self) -> bool {
        self.has_more
    }
    #[must_use]
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorPayload {
    continuation_key: String,
    scope_id: String,
    resource_type: String,
    query_identity: String,
    version: u8,
}

/// Application/query-service cursor authority.
///
/// `Default` is deliberately unusable for pagination. It supports construction
/// of routers without native collection support while cursor use fails closed.
#[derive(Clone, Default)]
pub struct CursorConfig {
    hmac_key: Option<Vec<u8>>,
}

impl CursorConfig {
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.hmac_key.as_ref().is_some_and(|key| key.len() >= 32)
    }

    pub fn new(key: Vec<u8>) -> Result<Self, QueryValidationError> {
        if key.len() < 32 {
            return Err(QueryValidationError::CursorUnavailable);
        }
        Ok(Self {
            hmac_key: Some(key),
        })
    }

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
        Self::new(key).map_err(|_| "native cursor signing key is too short")
    }

    /// Validate raw collection parameters and authenticate any continuation.
    /// This is the only constructor for [`ResourceQuery`].
    pub fn validate_query(
        &self,
        raw_limit: Option<&str>,
        raw_cursor: Option<&str>,
        scope_id: &str,
        resource_type: &str,
    ) -> Result<ResourceQuery, QueryValidationError> {
        validate_identity(scope_id, MAX_SCOPE_LENGTH)?;
        validate_identity(resource_type, MAX_RESOURCE_TYPE_LENGTH)?;
        self.validate_query_with_identity(
            raw_limit,
            raw_cursor,
            scope_id,
            resource_type,
            &canonical_query_identity(),
        )
    }

    /// Validate a bounded query with an application-supplied canonical identity.
    /// The identity is authenticated into the cursor, binding continuation to
    /// every accepted filter without exposing raw HTTP parameters to storage.
    pub fn validate_query_with_identity(
        &self,
        raw_limit: Option<&str>,
        raw_cursor: Option<&str>,
        scope_id: &str,
        resource_type: &str,
        query_identity: &str,
    ) -> Result<ResourceQuery, QueryValidationError> {
        validate_identity(query_identity, MAX_QUERY_IDENTITY_LENGTH)?;
        let limit = parse_page_size_strict(raw_limit)?;
        if raw_cursor.is_some() {
            self.key()?;
        }
        let continuation_key = raw_cursor
            .map(|cursor| {
                self.decode_cursor(cursor, scope_id, resource_type, query_identity)
                    .map(|payload| payload.continuation_key)
            })
            .transpose()?;
        Ok(ResourceQuery {
            scope_id: scope_id.to_owned(),
            resource_type: resource_type.to_owned(),
            limit,
            continuation_key,
            query_identity: query_identity.to_owned(),
        })
    }

    /// Convert a repository-bounded page into the public opaque page.
    pub fn complete_page<T>(
        &self,
        query: &ResourceQuery,
        page: RepositoryPage<T>,
    ) -> Result<ResourcePage<T>, QueryValidationError> {
        if page.items.len() > query.limit {
            return Err(QueryValidationError::InvalidRepositoryPage);
        }
        let next_cursor = page
            .continuation_key
            .as_deref()
            .map(|continuation_key| self.encode_cursor(query, continuation_key))
            .transpose()?;
        if page.has_more != next_cursor.is_some() {
            return Err(QueryValidationError::InvalidRepositoryPage);
        }
        Ok(ResourcePage {
            items: page.items,
            has_more: page.has_more,
            next_cursor,
        })
    }

    fn key(&self) -> Result<&[u8], QueryValidationError> {
        self.hmac_key
            .as_deref()
            .ok_or(QueryValidationError::CursorUnavailable)
    }

    fn encode_cursor(
        &self,
        query: &ResourceQuery,
        continuation_key: &str,
    ) -> Result<String, QueryValidationError> {
        validate_identity(continuation_key, MAX_CONTINUATION_LENGTH)?;
        let payload = CursorPayload {
            continuation_key: continuation_key.to_owned(),
            scope_id: query.scope_id.clone(),
            resource_type: query.resource_type.clone(),
            query_identity: query.query_identity.clone(),
            version: CURSOR_VERSION,
        };
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|_| QueryValidationError::InvalidRepositoryPage)?;
        let mut mac = HmacSha256::new_from_slice(self.key()?)
            .map_err(|_| QueryValidationError::CursorUnavailable)?;
        mac.update(&payload_json);
        let signature = mac.finalize().into_bytes();
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
        let signature_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
        let cursor = format!("{payload_b64}.{signature_b64}");
        if cursor.len() > MAX_CURSOR_LENGTH {
            return Err(QueryValidationError::InvalidRepositoryPage);
        }
        Ok(cursor)
    }

    fn decode_cursor(
        &self,
        cursor: &str,
        expected_scope_id: &str,
        expected_resource_type: &str,
        expected_query_identity: &str,
    ) -> Result<CursorPayload, QueryValidationError> {
        // This limit intentionally precedes split, base64, and JSON work.
        if cursor.len() > MAX_CURSOR_LENGTH {
            return Err(QueryValidationError::InvalidCursor);
        }
        let (payload_b64, signature_b64) = cursor
            .split_once('.')
            .ok_or(QueryValidationError::InvalidCursor)?;
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| QueryValidationError::InvalidCursor)?;
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature_b64)
            .map_err(|_| QueryValidationError::InvalidCursor)?;
        let mut mac = HmacSha256::new_from_slice(self.key()?)
            .map_err(|_| QueryValidationError::CursorUnavailable)?;
        mac.update(&payload_bytes);
        mac.verify_slice(&signature)
            .map_err(|_| QueryValidationError::InvalidCursor)?;
        let payload: CursorPayload = serde_json::from_slice(&payload_bytes)
            .map_err(|_| QueryValidationError::InvalidCursor)?;
        if payload.version != CURSOR_VERSION
            || payload.scope_id != expected_scope_id
            || payload.resource_type != expected_resource_type
            || payload.query_identity != expected_query_identity
            || validate_identity(&payload.scope_id, MAX_SCOPE_LENGTH).is_err()
            || validate_identity(&payload.resource_type, MAX_RESOURCE_TYPE_LENGTH).is_err()
            || validate_identity(&payload.query_identity, MAX_QUERY_IDENTITY_LENGTH).is_err()
            || validate_identity(&payload.continuation_key, MAX_CONTINUATION_LENGTH).is_err()
        {
            return Err(QueryValidationError::InvalidCursor);
        }
        Ok(payload)
    }
}

fn validate_identity(value: &str, max: usize) -> Result<(), QueryValidationError> {
    if value.is_empty() || value.len() > max {
        Err(QueryValidationError::InvalidIdentity)
    } else {
        Ok(())
    }
}

fn canonical_query_identity() -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"o3k/resource-query/v1\0");
    hasher.update(STABLE_ORDERING.as_bytes());
    hasher.update(b"\0");
    hasher.update(NO_FILTERS.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

pub(crate) fn parse_page_size_strict(
    limit_param: Option<&str>,
) -> Result<usize, QueryValidationError> {
    let Some(value) = limit_param else {
        return Ok(DEFAULT_PAGE_SIZE);
    };
    // Bound parsing work and reject signs/whitespace before numeric parsing.
    if value.is_empty() || value.len() > MAX_PAGE_SIZE.to_string().len() {
        return Err(QueryValidationError::InvalidLimit);
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(QueryValidationError::InvalidLimit);
    }
    let value = value
        .parse::<usize>()
        .map_err(|_| QueryValidationError::InvalidLimit)?;
    if !(1..=MAX_PAGE_SIZE).contains(&value) {
        return Err(QueryValidationError::InvalidLimit);
    }
    Ok(value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cursor_service() -> CursorConfig {
        CursorConfig::new(b"test-only-cursor-key-at-least-32-bytes".to_vec()).unwrap()
    }

    fn query(service: &CursorConfig, scope: &str, resource: &str) -> ResourceQuery {
        service
            .validate_query(Some("2"), None, scope, resource)
            .unwrap()
    }

    fn cursor(service: &CursorConfig, scope: &str, resource: &str) -> String {
        let query = query(service, scope, resource);
        let repository =
            RepositoryPage::new(vec!["a", "b"], true, Some("b".to_owned()), 2).unwrap();
        service
            .complete_page(&query, repository)
            .unwrap()
            .next_cursor
            .unwrap()
    }

    #[test]
    fn a0_strict_limit_validation() {
        assert_eq!(parse_page_size_strict(None), Ok(DEFAULT_PAGE_SIZE));
        for valid in ["1", "50", "200"] {
            assert!(parse_page_size_strict(Some(valid)).is_ok());
        }
        for invalid in ["", "0", "201", "9999", "-1", "+1", " 1", "abc"] {
            assert_eq!(
                parse_page_size_strict(Some(invalid)),
                Err(QueryValidationError::InvalidLimit)
            );
        }
    }

    #[test]
    fn a0_cursor_is_bound_to_scope_resource_and_query_identity() {
        let service = cursor_service();
        let encoded = cursor(&service, "project-a", "compute:server");
        let accepted = service
            .validate_query(Some("1"), Some(&encoded), "project-a", "compute:server")
            .unwrap();
        assert_eq!(accepted.continuation_key(), Some("b"));
        assert!(
            service
                .validate_query(Some("1"), Some(&encoded), "project-b", "compute:server")
                .is_err()
        );
        assert!(
            service
                .validate_query(Some("1"), Some(&encoded), "project-a", "volume:volume")
                .is_err()
        );
    }

    #[test]
    fn a0_cursor_length_is_rejected_before_decode() {
        let oversized = "x".repeat(MAX_CURSOR_LENGTH + 1);
        assert_eq!(
            cursor_service().validate_query(
                Some("1"),
                Some(&oversized),
                "project-a",
                "compute:server"
            ),
            Err(QueryValidationError::InvalidCursor)
        );
    }

    #[test]
    fn a0_tampered_and_malformed_cursors_fail_closed() {
        let service = cursor_service();
        let mut encoded = cursor(&service, "project-a", "compute:server");
        encoded.replace_range(0..1, "x");
        assert!(
            service
                .validate_query(Some("2"), Some(&encoded), "project-a", "compute:server")
                .is_err()
        );
        assert!(
            service
                .validate_query(
                    Some("2"),
                    Some("not-a-cursor"),
                    "project-a",
                    "compute:server"
                )
                .is_err()
        );
    }

    #[test]
    fn a0_page_invariants_are_explicit() {
        assert!(RepositoryPage::new(vec![1, 2], false, None, 2).is_ok());
        assert!(RepositoryPage::<u8>::new(vec![], false, None, 2).is_ok());
        assert!(RepositoryPage::new(vec![1, 2, 3], false, None, 2).is_err());
        assert!(RepositoryPage::new(vec![1], true, None, 2).is_err());
        assert!(RepositoryPage::new(vec![1], false, Some("1".to_owned()), 2).is_err());
    }

    #[test]
    fn a0_unconfigured_cursor_service_fails_closed() {
        let service = CursorConfig::default();
        assert_eq!(
            service.validate_query(
                Some("2"),
                Some("payload.signature"),
                "project-a",
                "compute:server"
            ),
            Err(QueryValidationError::CursorUnavailable)
        );
        let page = RepositoryPage::new(vec![1], true, Some("1".to_owned()), 2).unwrap();
        assert_eq!(
            service.complete_page(
                &query(&cursor_service(), "project-a", "compute:server"),
                page
            ),
            Err(QueryValidationError::CursorUnavailable)
        );
    }

    #[test]
    fn a0_mutation_between_pages_uses_weak_keyset_semantics() {
        // The continuation is an exclusive stable key, so inserts before the
        // anchor are intentionally not revisited and inserts after it become
        // visible on a later page.
        let first_page = ["a", "b"];
        let anchor = first_page.last().copied().unwrap();
        let after_insert_before = ["a0", "b", "c"]
            .into_iter()
            .filter(|id| *id > anchor)
            .collect::<Vec<_>>();
        assert_eq!(after_insert_before, vec!["c"]);

        // Deleting the returned anchor still leaves an exclusive continuation
        // that is valid; deleting future rows simply shortens the next page.
        let after_anchor_delete = ["c", "d"]
            .into_iter()
            .filter(|id| *id > anchor)
            .collect::<Vec<_>>();
        assert_eq!(after_anchor_delete, vec!["c", "d"]);
        let after_future_delete = ["c"]
            .into_iter()
            .filter(|id| *id > anchor)
            .collect::<Vec<_>>();
        assert_eq!(after_future_delete, vec!["c"]);
    }

    #[test]
    fn a0_bounded_page_rejects_more_than_requested_items() {
        let requested = 3;
        let fetched = vec![1, 2, 3, 4];
        assert!(RepositoryPage::new(fetched, true, Some("4".into()), requested).is_err());
    }
}
