# SPEC-0020 — Keystone trust, catalog, and authorization context

Status: Proposed

Primary reference profile: OpenStack 2026.1 Gazpacho

Backward reference profile: OpenStack 2025.2 Flamingo where declared

Related decisions:

- [ADR-0161](../adr/ADR-0161-keystone-trust-and-service-identity.md)
- [SPEC-0004](SPEC-0004-keystone-bootstrap.md)

## Purpose

This specification defines the identity and trust contract consumed by every
O3K service. It extends the existing bootstrap-token subset without claiming
complete Keystone v3 parity.

The implementation must provide one authoritative answer for authentication,
project scope, roles, service identity, catalog discovery, expiry, and audit
context. Service modules must not reinterpret token claims independently.

## Normative identity resources

The domain model distinguishes immutable IDs from mutable or display names.
The following resource types are required by the declared profile:

- domain;
- project;
- user;
- group;
- role;
- role assignment;
- service;
- endpoint;
- region;
- token record or verifiable token identity;
- revocation record when revocation is enabled.

At minimum, every resource has:

- a stable typed ID;
- a bounded display name where applicable;
- enabled state where applicable;
- creation and update metadata;
- ownership or domain relationship;
- uniqueness constraints defined by resource type.

A name is never accepted as an internal ID. Name resolution is explicit,
scoped, ambiguity-checked, and limited to public operations that advertise it.

## Bootstrap profile

The TestLab bootstrap profile creates deterministic records:

```text
domain ID:  default
domain name: Default
user ID:    bootstrap-user
user name:  admin
project ID: bootstrap-project
project name: admin
role ID:    member
role name:  member
```

The duplicate user/project display name is intentional compatibility behavior
and proves why name and ID types must remain distinct.

The bootstrap profile must not expose the password or signing key. It must be
possible to replace bootstrap records with durable identity administration
without changing the authorization-context contract.

## Authentication request subset

The alpha profile supports:

- `POST /v3/auth/tokens`;
- exactly one `password` identity method;
- project scope by advertised ID or unambiguous supported name form;
- `X-Subject-Token` response header;
- explicit issue and expiry timestamps;
- generic authentication failure responses.

Unsupported authentication methods fail with the documented Keystone-compatible
error envelope and are not silently ignored.

## Token requirements

A token or its server-side record binds:

- token ID or safe token digest;
- user ID;
- project ID and project-domain ID;
- effective role IDs and names required by the public response;
- authentication method;
- issued-at and expires-at timestamps;
- audit ID;
- optional parent audit ID;
- optional service identity;
- token format version.

The alpha token may remain an opaque authenticated value rather than a JWT.
Its signature or lookup key is independent from the bootstrap password.

Validation fails closed for:

- invalid signature or missing record;
- expiry;
- revoked token where revocation is enabled;
- malformed version;
- missing required scope;
- disabled user or project;
- impossible role assignment;
- unacceptable clock skew.

Error responses do not disclose whether the user, project, token, password, or
role exists.

## Internal `AuthContext`

All protocol adapters convert a validated token into one immutable internal
context equivalent to:

```text
AuthContext
  token_id_or_digest
  user_id
  project_id
  project_domain_id
  role_ids
  role_names
  system_scope?
  authentication_methods
  issued_at
  expires_at
  audit_id
  parent_audit_id?
  service_identity?
  request_id
```

Requirements:

- IDs use typed domain values, not arbitrary strings in application services;
- secrets and raw token values are excluded from `Debug`, logs, metrics, and
  evidence;
- policy decisions receive the same normalized context across all services;
- request ID and audit ID propagate through operations and agent commands;
- a project path parameter must match the effective scoped project where the
  OpenStack operation requires project scoping;
- project name/ID confusion fails before store lookup or provider dispatch.

## Policy contract

The first profile supports a small explicit policy table rather than a generic
policy language. Each operation declares:

- required token scope;
- accepted roles;
- whether a service identity is required;
- whether the target resource must belong to the scoped project;
- whether system or administrative access is supported;
- whether the operation is public discovery.

Default behavior is deny. An endpoint without a policy declaration cannot be
registered.

## Service identity and service tokens

A service identity contains:

- service user ID;
- service project ID;
- service role ID;
- calling service type;
- authenticated channel or token identity;
- issue, expiry, and audit information.

For cross-service work, effective authorization contains both:

1. the original user/project context; and
2. the authenticated service identity.

Examples:

- Nova resolving an image through Glance;
- Nova binding a port through Neutron;
- Nova reserving resources in Placement;
- Nova creating or completing a Cinder attachment.

The modular-monolith profile may pass the dual context in process. A future
network boundary must use mTLS or another accepted service-authentication
mechanism and must preserve the original user audit context.

A service identity never expands the user's project scope unless an explicitly
reviewed administrative policy allows it.

## Service catalog contract

The catalog advertises only implemented and enabled profiles.

Required catalog fields:

- stable service ID;
- service type;
- service name;
- endpoint ID;
- interface;
- region ID;
- enabled state;
- URL;
- discoverable version root.

Expected service types by milestone:

- `identity` — mandatory;
- `image` — mandatory for the first guest;
- `compute` — mandatory for the first guest;
- `network` — mandatory for the first guest;
- `placement` — mandatory for the first guest;
- `volumev3` — advertised only after the Cinder baseline passes its contract
  and authorization tests.

Catalog URLs must be derived from validated configuration, not request headers
that permit host-header injection. Disabled or unsupported services are omitted
rather than advertised with failing placeholder endpoints.

## Discovery behavior

The declared profile provides consistent discovery for:

- the identity root;
- `/v3`;
- advertised service roots;
- supported API versions or microversion ranges;
- endpoint URLs in the catalog.

OpenStack client warnings caused by contradictory discovery and catalog
responses are compatibility failures.

## Persistence and restart

Identity state and signing configuration must support:

- atomic migrations;
- uniqueness and foreign-key constraints;
- restart without changing stable IDs;
- token verification across restart when keys and records are retained;
- safe key rotation design before rotation is enabled;
- corrupted-state failure without silently rebuilding a different identity
  universe.

## Audit and redaction

Audit events include:

- request ID;
- audit ID;
- user ID;
- project ID;
- service identity where present;
- operation name;
- allow/deny result;
- bounded reason code;
- target resource ID where authorized to record it.

Audit events exclude:

- password;
- raw token;
- signing key;
- private certificate material;
- unbounded request bodies;
- user-data or image credentials.

## Required tests

### Domain and store

- ID/name distinction;
- uniqueness and ambiguity;
- role assignment validity;
- disabled resources;
- migration and restart;
- concurrent updates;
- corrupted token or identity state.

### Authentication

- success with project ID;
- supported project-name resolution;
- invalid password redaction;
- expired token;
- invalid signature;
- disabled user/project;
- clock-skew boundaries;
- restart verification.

### Authorization

- cross-project denial before provider dispatch;
- role reduction;
- missing policy declaration fails closed;
- project path mismatch;
- service identity required/absent;
- user token and service token dual-context behavior;
- audit propagation.

### Catalog and discovery

- only implemented services advertised;
- stable IDs and regions;
- URL configuration validation;
- omission of `volumev3` before Cinder readiness;
- OpenStack CLI discovery without contradictory version information.

## Explicit non-goals for the first implementation

- complete Keystone administration API;
- federation;
- OAuth/OIDC compatibility;
- application credentials;
- trusts;
- dynamic policy language;
- online key rotation without a separate accepted design;
- system-scoped administration unless included in a later profile.
