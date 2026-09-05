//! Trusted OIDC access-token validation.
//!
//! This module deliberately stops at authentication evidence. It does not
//! provision principals, discover scopes, or issue O3K tokens.

use std::{sync::Arc, time::Duration};

use jsonwebtoken::{Algorithm, DecodingKey, TokenData, Validation, decode, decode_header};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

const DEFAULT_MAX_TOKEN_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_DOCUMENT_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedIssuer {
    pub id: String,
    pub issuer: Url,
    pub audience: String,
    pub algorithms: Vec<Algorithm>,
    pub discovery_url: Url,
    pub allow_insecure_local: bool,
    pub timeout: Duration,
    pub max_token_bytes: usize,
    pub max_document_bytes: usize,
    pub clock_skew: Duration,
}

impl TrustedIssuer {
    pub fn validate(&self) -> Result<(), OidcError> {
        if self.id.trim().is_empty()
            || self.audience.trim().is_empty()
            || self.algorithms.is_empty()
            || self.timeout.is_zero()
            || self.max_token_bytes == 0
            || self.max_document_bytes == 0
        {
            return Err(OidcError::InvalidConfiguration);
        }
        if self.issuer.path() != "/" && self.issuer.path().ends_with('/') {
            return Err(OidcError::InvalidConfiguration);
        }
        if !self.allow_insecure_local
            && (self.issuer.scheme() != "https" || self.discovery_url.scheme() != "https")
        {
            return Err(OidcError::InsecureIssuer);
        }
        if self.allow_insecure_local
            && (!is_local_url(&self.issuer) || !is_local_url(&self.discovery_url))
        {
            return Err(OidcError::InvalidConfiguration);
        }
        Ok(())
    }

    #[must_use]
    pub fn test_local(id: &str, issuer: Url, audience: &str, discovery_url: Url) -> Self {
        Self {
            id: id.to_owned(),
            issuer,
            audience: audience.to_owned(),
            algorithms: vec![Algorithm::RS256],
            discovery_url,
            allow_insecure_local: true,
            timeout: Duration::from_secs(2),
            max_token_bytes: DEFAULT_MAX_TOKEN_BYTES,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            clock_skew: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedExternalIdentity {
    pub trusted_issuer_id: String,
    pub issuer: String,
    pub subject: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OidcError {
    #[error("OIDC authentication failed")]
    AuthenticationFailed,
    #[error("OIDC issuer configuration is invalid")]
    InvalidConfiguration,
    #[error("OIDC issuer must use HTTPS")]
    InsecureIssuer,
    #[error("OIDC discovery or JWKS document is unavailable")]
    ProviderUnavailable,
    #[error("OIDC discovery or JWKS document exceeds its configured limit")]
    DocumentTooLarge,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
}

#[derive(Clone)]
pub struct OidcValidator {
    client: reqwest::Client,
    issuer: TrustedIssuer,
    keys: Arc<RwLock<Option<jsonwebtoken::jwk::JwkSet>>>,
}

impl OidcValidator {
    pub fn new(issuer: TrustedIssuer) -> Result<Self, OidcError> {
        issuer.validate()?;
        let client = reqwest::Client::builder()
            .connect_timeout(issuer.timeout)
            .timeout(issuer.timeout)
            .build()
            .map_err(|_| OidcError::InvalidConfiguration)?;
        Ok(Self {
            client,
            issuer,
            keys: Arc::new(RwLock::new(None)),
        })
    }

    #[must_use]
    pub fn issuer(&self) -> &TrustedIssuer {
        &self.issuer
    }

    /// Validate an access token using the cached JWKS, refreshing once when
    /// the token names a key that is not cached. No token bytes enter errors.
    pub async fn validate(&self, token: &str) -> Result<ValidatedExternalIdentity, OidcError> {
        if token.len() > self.issuer.max_token_bytes {
            return Err(OidcError::AuthenticationFailed);
        }
        let header = decode_header(token).map_err(|_| OidcError::AuthenticationFailed)?;
        let kid = header
            .kid
            .as_deref()
            .ok_or(OidcError::AuthenticationFailed)?;
        let keys = self.get_keys(kid).await?;
        self.validate_with_jwks(token, &keys)
    }

    pub fn validate_with_jwks(
        &self,
        token: &str,
        keys: &jsonwebtoken::jwk::JwkSet,
    ) -> Result<ValidatedExternalIdentity, OidcError> {
        if token.len() > self.issuer.max_token_bytes {
            return Err(OidcError::AuthenticationFailed);
        }
        let kid = decode_header(token)
            .map_err(|_| OidcError::AuthenticationFailed)?
            .kid
            .ok_or(OidcError::AuthenticationFailed)?;
        self.decode(token, &kid, keys)
    }

    async fn get_keys(&self, kid: &str) -> Result<jsonwebtoken::jwk::JwkSet, OidcError> {
        if let Some(keys) = self.keys.read().await.clone()
            && keys.find(kid).is_some()
        {
            return Ok(keys);
        }
        let fresh = self.fetch_jwks().await?;
        if fresh.find(kid).is_none() {
            return Err(OidcError::AuthenticationFailed);
        }
        *self.keys.write().await = Some(fresh.clone());
        Ok(fresh)
    }

    fn decode(
        &self,
        token: &str,
        kid: &str,
        keys: &jsonwebtoken::jwk::JwkSet,
    ) -> Result<ValidatedExternalIdentity, OidcError> {
        let jwk = keys.find(kid).ok_or(OidcError::AuthenticationFailed)?;
        let algorithm = decode_header(token)
            .map_err(|_| OidcError::AuthenticationFailed)?
            .alg;
        if !self.issuer.algorithms.contains(&algorithm) {
            return Err(OidcError::AuthenticationFailed);
        }
        let key = DecodingKey::from_jwk(jwk).map_err(|_| OidcError::AuthenticationFailed)?;
        let mut validation = Validation::new(algorithm);
        validation.leeway = self.issuer.clock_skew.as_secs();
        validation.validate_nbf = true;
        validation.set_issuer(&[self.issuer.issuer.as_str()]);
        validation.set_audience(std::slice::from_ref(&self.issuer.audience));
        validation
            .required_spec_claims
            .extend(["iss", "aud", "sub"].into_iter().map(str::to_owned));
        let TokenData { claims, .. }: TokenData<Claims> =
            decode(token, &key, &validation).map_err(|_| OidcError::AuthenticationFailed)?;
        if claims.sub.is_empty() || claims.sub.len() > 512 {
            return Err(OidcError::AuthenticationFailed);
        }
        Ok(ValidatedExternalIdentity {
            trusted_issuer_id: self.issuer.id.clone(),
            issuer: claims.iss,
            subject: claims.sub,
        })
    }

    async fn fetch_jwks(&self) -> Result<jsonwebtoken::jwk::JwkSet, OidcError> {
        let discovery = self
            .client
            .get(self.issuer.discovery_url.clone())
            .send()
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        let body = bounded_body(discovery, self.issuer.max_document_bytes).await?;
        let document: DiscoveryDocument =
            serde_json::from_slice(&body).map_err(|_| OidcError::ProviderUnavailable)?;
        let discovered_issuer =
            Url::parse(&document.issuer).map_err(|_| OidcError::ProviderUnavailable)?;
        if discovered_issuer != self.issuer.issuer {
            return Err(OidcError::ProviderUnavailable);
        }
        let jwks_url =
            Url::parse(&document.jwks_uri).map_err(|_| OidcError::ProviderUnavailable)?;
        if !self.issuer.allow_insecure_local && jwks_url.scheme() != "https" {
            return Err(OidcError::InsecureIssuer);
        }
        if self.issuer.allow_insecure_local && !is_local_url(&jwks_url) {
            return Err(OidcError::InvalidConfiguration);
        }
        let response = self
            .client
            .get(jwks_url)
            .send()
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        let body = bounded_body(response, self.issuer.max_document_bytes).await?;
        serde_json::from_slice(&body).map_err(|_| OidcError::ProviderUnavailable)
    }
}

async fn bounded_body(response: reqwest::Response, max: usize) -> Result<Vec<u8>, OidcError> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(OidcError::DocumentTooLarge);
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| OidcError::ProviderUnavailable)?;
    if body.len() > max {
        return Err(OidcError::DocumentTooLarge);
    }
    Ok(body.to_vec())
}

fn is_local_url(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn production_configuration_rejects_http() -> Result<(), OidcError> {
        let issuer_url =
            Url::parse("http://idp.example.test").map_err(|_| OidcError::InvalidConfiguration)?;
        let discovery_url = Url::parse("http://idp.example.test/.well-known/openid-configuration")
            .map_err(|_| OidcError::InvalidConfiguration)?;
        let issuer = TrustedIssuer {
            id: "test".to_owned(),
            issuer: issuer_url,
            audience: "o3k".to_owned(),
            algorithms: vec![Algorithm::RS256],
            discovery_url,
            allow_insecure_local: false,
            timeout: Duration::from_secs(1),
            max_token_bytes: DEFAULT_MAX_TOKEN_BYTES,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            clock_skew: Duration::from_secs(30),
        };
        assert_eq!(issuer.validate(), Err(OidcError::InsecureIssuer));
        Ok(())
    }

    #[test]
    fn local_test_configuration_is_explicit() -> Result<(), OidcError> {
        let issuer_url =
            Url::parse("http://127.0.0.1:9000").map_err(|_| OidcError::InvalidConfiguration)?;
        let discovery_url = Url::parse("http://127.0.0.1:9000/.well-known/openid-configuration")
            .map_err(|_| OidcError::InvalidConfiguration)?;
        let issuer = TrustedIssuer::test_local("local", issuer_url, "o3k", discovery_url);
        assert!(issuer.validate().is_ok());
        Ok(())
    }

    #[test]
    fn errors_are_not_token_bearing() {
        let error = OidcError::AuthenticationFailed;
        assert_eq!(error.to_string(), "OIDC authentication failed");
    }

    #[test]
    fn validates_claims_and_signature_against_explicit_jwks() -> Result<(), OidcError> {
        let issuer_url =
            Url::parse("http://127.0.0.1:9000").map_err(|_| OidcError::InvalidConfiguration)?;
        let discovery_url = Url::parse("http://127.0.0.1:9000/.well-known/openid-configuration")
            .map_err(|_| OidcError::InvalidConfiguration)?;
        let mut trusted = TrustedIssuer::test_local("local", issuer_url, "o3k", discovery_url);
        trusted.algorithms = vec![Algorithm::HS256];
        let validator = OidcValidator::new(trusted)?;
        let secret = b"a-test-secret-that-is-long-enough";
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("key-1".to_owned());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| OidcError::InvalidConfiguration)?
            .as_secs();
        let token = encode(
            &header,
            &json!({
                "iss": "http://127.0.0.1:9000/",
                "sub": "external-subject",
                "aud": "o3k",
                "exp": now + 300,
                "nbf": now - 1,
            }),
            &EncodingKey::from_secret(secret),
        )
        .map_err(|_| OidcError::AuthenticationFailed)?;
        let mut jwk = jsonwebtoken::jwk::Jwk::from_decoding_key(
            &DecodingKey::from_secret(secret),
            Some(Algorithm::HS256),
        )
        .map_err(|_| OidcError::AuthenticationFailed)?;
        jwk.common.key_id = Some("key-1".to_owned());
        let identity_result =
            validator.validate_with_jwks(&token, &jsonwebtoken::jwk::JwkSet { keys: vec![jwk] });
        let identity = identity_result?;
        assert_eq!(identity.trusted_issuer_id, "local");
        assert_eq!(identity.subject, "external-subject");
        Ok(())
    }
}
