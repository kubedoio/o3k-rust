# SPEC-0004 — Keystone v3 bootstrap token flow

Status: Implemented subset

## Decision

O3K supports only `POST /v3/auth/tokens` with exactly one `password` identity
method and a project scope. Bootstrap credentials are configured explicitly
with `O3K_BOOTSTRAP_PASSWORD` and `O3K_TOKEN_SIGNING_KEY`; the signing key is
separate and must be at least 32 bytes. The route is unavailable until both are
configured.

The token is an opaque URL-safe HMAC-SHA256 signed value containing the user,
project, issue time, expiry, and random token ID. It is not a general JWT
compatibility promise. Tokens expire after one hour in this alpha profile and
remain verifiable across restart when the same key is retained.

## Bootstrap records

- user: `bootstrap-user` / `admin`;
- project: `bootstrap-project` / `admin`;
- domain: `default` / `Default`;
- role: `member`;
- catalog: identity only (`/v3`).

The catalog does not advertise compute, image, or network services until those
routes exist. Authentication failures use a generic 401 message and never
echo credentials, names, tokens, or internal errors.

## Evidence

The in-process API tests cover successful issuance, `X-Subject-Token`, project
scope, invalid password redaction, and response shape. Identity unit tests cover
signing, verification, expiry, and invalid credentials. CLI/process-level smoke
evidence is deferred until the complete TestLab workflow issue.
