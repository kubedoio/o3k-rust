# P12-IAM.1 — Trusted OIDC Issuer and Access-Token Validation

## Goal

Implement the standards-based validation boundary selected in P12-IAM.0 without yet issuing native O3K scoped tokens.

## Preconditions

- P12-IAM.0 accepted and merged.
- Start from newly merged protected `main`.

## Required implementation

Introduce the smallest generic IAM federation abstraction needed to validate external OIDC access tokens against configured trusted issuers.

At minimum support:

- issuer configuration with stable trust identity;
- OIDC discovery or explicitly configured metadata according to the accepted ADR;
- HTTPS-only remote metadata/JWKS in production except explicit test-only local profiles;
- JWKS retrieval with bounded timeout/size;
- key selection by `kid` where applicable;
- signature verification using an explicit accepted algorithm allowlist;
- exact issuer validation;
- required audience validation for O3K;
- `exp` and `nbf` validation with bounded clock skew;
- bounded token/header/claim sizes;
- safe cache and key-rotation behavior;
- one bounded refresh on unknown `kid` if selected by the accepted design;
- fail-closed behavior for unavailable/untrusted issuers;
- secret-safe errors and logs.

Return a typed validated external identity containing only safe, required claims, including at minimum canonicalized issuer and subject. Do not expose raw tokens beyond the validation boundary.

## Do not

- do not implement JWT signature verification manually;
- do not accept `alg=none`;
- do not trust token-provided JWKS/JWK URLs;
- do not dynamically trust arbitrary issuers;
- do not accept an ID token as an O3K API credential unless a later accepted profile explicitly selects that behavior;
- do not bind by email, preferred_username or display name;
- do not issue O3K tokens in this slice;
- do not add Araf-specific code.

## Configuration

Trusted issuers must be explicit operator configuration or durable accepted O3K IAM configuration. Define startup validation and safe reload/rotation behavior according to the ADR. Configuration errors fail closed and must be diagnosable without exposing secrets.

## Required tests

Positive:

- valid issuer/audience/signature/time window;
- accepted key rotation;
- cached key reuse;
- bounded refresh on new `kid` if supported.

Negative:

- wrong issuer;
- wrong audience;
- expired token;
- not-yet-valid token;
- invalid signature;
- unsupported algorithm;
- missing subject;
- missing/invalid `kid` as applicable;
- malformed JWT;
- oversized JWT/claims/JWKS;
- discovery/JWKS timeout;
- HTTPS downgrade/untrusted metadata;
- issuer configuration mismatch.

Ensure logs/errors never contain the raw access token.

## Validation

Run full Rust gates plus targeted IAM tests. Include dependency/security review for any new OIDC/JWT/HTTP client crates.

## Completion evidence

Document exact issuer profile used in tests, accepted algorithms, audience contract, cache/rotation behavior and negative-test matrix.

Verdict:

`P12-IAM.1 trusted OIDC validation: PASS|BLOCKED`
