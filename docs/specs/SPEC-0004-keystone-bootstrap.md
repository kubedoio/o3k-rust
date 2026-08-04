# SPEC-0004 — Keystone v3 bootstrap token flow

Status: Implemented subset

Normative trust model: [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)

## Decision

O3K supports `POST /v3/auth/tokens` with exactly one `password` identity method
and a project scope in the TestLab bootstrap profile. Bootstrap credentials are
configured explicitly with `O3K_BOOTSTRAP_PASSWORD` and
`O3K_TOKEN_SIGNING_KEY`; the signing key is separate and must be at least 32
bytes. The route is unavailable until both are configured.

The token is an opaque URL-safe HMAC-SHA256 authenticated value containing the
user, project, issue time, expiry, and random token ID. It is not a general JWT
compatibility promise. Tokens expire after one hour in this alpha profile and
remain verifiable across restart when the same key is retained.

This spec defines only the implemented bootstrap subset. Durable identity
resources, service identity, authorization context, catalog policy, and future
expansion are governed by SPEC-0020 and ADR-0161.

## Bootstrap records

```text
user ID:      bootstrap-user
user name:    admin
project ID:   bootstrap-project
project name: admin
domain ID:    default
domain name:  Default
role ID/name: member
```

Project and user names are not internal IDs. In particular, the project name
`admin` must never be used where the durable project ID `bootstrap-project` is
required by stores, authorization checks, operations, provider commands, or
runner probes.

## Catalog

The bootstrap catalog advertises only implemented and enabled service
profiles:

- identity (`/v3`);
- image (`/v2`);
- network (`/v2.0`);
- compute (`/v2.1`);
- Placement only when its public profile is implemented and enabled.

The catalog must not advertise `volumev3` until the Cinder-compatible baseline,
policy, and contract tests pass. Planned services are omitted rather than
represented by placeholder endpoints.

Catalog URLs are derived from validated configuration. Authentication failures
use a generic 401 message and never echo credentials, names, raw tokens,
signing material, or internal errors.

## Authorization context

A successful bootstrap authentication produces the same normalized internal
`AuthContext` required by SPEC-0020. Service modules consume the durable user
and project IDs, roles, expiry, audit identity, and request identity from that
context. They do not parse or reinterpret the raw token independently.

## Evidence

The in-process API tests cover:

- successful issuance;
- `X-Subject-Token`;
- project scope;
- project ID/name distinction;
- invalid password redaction;
- response shape;
- catalog omission for unsupported services.

Identity unit tests cover signing, verification, expiry, invalid credentials,
and restart behavior. Process-level and OpenStack CLI evidence remains part of
the declared TestLab compatibility and full-cloud gates.
