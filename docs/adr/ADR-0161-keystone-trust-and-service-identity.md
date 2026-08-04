# ADR-0161 — Keystone trust root and service identity

Status: Proposed

Date: 2026-08-04

## Context

Every OpenStack-compatible service needs a consistent answer to:

- who is making the request;
- which project, domain, or system scope applies;
- which roles and policy rules authorize the operation;
- which service endpoint is authoritative;
- how one O3K service acts on behalf of a user while proving its own service
  identity;
- how identity, tokens, catalog entries, and audit information survive restart
  without leaking secrets.

The current bootstrap subset intentionally implements one project-scoped
password flow. That is sufficient for early CLI compatibility, but it must not
become an accidental long-term security model in which each service interprets
tokens differently or uses the bootstrap administrator for internal work.

Keystone is central to trust and discovery. It must not become the owner of
Nova, Neutron, Cinder, Glance, or Placement resource state.

## Decision

### 1. Identity is the trust root

The Keystone-compatible module owns:

- domains, projects, users, groups, roles, and assignments;
- authentication methods supported by the selected O3K profile;
- token issuance, validation, expiry, revocation policy, and audit identity;
- service and endpoint registration;
- service users, service projects, and service roles;
- translation of a validated token into the internal authorization context.

It does not own servers, images, ports, volumes, allocations, or provider
operations.

### 2. All services consume one internal `AuthContext`

After token validation, protocol adapters construct one typed authorization
context containing at least:

- token identifier or safe digest;
- user identifier;
- project identifier and optional domain identifier;
- effective roles;
- optional system scope;
- issue and expiry timestamps;
- audit identifier and request identifier;
- authentication method metadata needed by policy without secrets;
- optional service identity when a service token is present.

Nova, Neutron, Cinder, Glance, and Placement-compatible modules authorize from
this context. They must not parse token payloads independently or accept a
project name where a durable project ID is required.

### 3. Names and durable IDs are distinct types

A display or login name is never interchangeable with a durable resource ID.
This applies to users, projects, domains, roles, services, endpoints, servers,
ports, volumes, and provider resources.

Public APIs may accept a name only where the advertised OpenStack contract
allows name resolution. Internal stores, operations, provider commands, and
authorization checks use durable IDs.

### 4. Service-to-service calls require dual identity

When an O3K service performs work on behalf of a user, the effective context
contains:

- the original user/project authorization context; and
- the authenticated calling service identity.

Examples include Nova resolving an image, binding a port, creating a Cinder
attachment, or reserving Placement resources. The service identity does not
replace the user scope, and the user token does not prove that the caller is an
authorized internal service.

The first modular-monolith implementation may pass this context in process.
Any future HTTP or message boundary must use mutually authenticated service
identity and preserve the original audit context.

### 5. Catalog entries are capability declarations

The service catalog advertises only implemented compatibility profiles. Each
endpoint includes:

- service type and stable service ID;
- interface;
- region;
- URL template;
- enabled state;
- supported API version or discoverable root.

A service type is omitted when its required baseline is not implemented. In
particular, `volumev3` must not be advertised merely because Cinder work is
planned.

### 6. Bootstrap is a profile, not the permanent data model

The bootstrap profile may create deterministic initial records for TestLab,
but:

- the password and token-signing key are configured separately;
- secrets are never returned in catalogs, logs, metrics, traces, or artifacts;
- the durable IDs are not inferred from names;
- restart preserves token verification when the configured key remains;
- a future migration to durable identity records does not change the
  authorization-context contract.

### 7. Revocation and expiry fail closed

Expired, malformed, revoked, incorrectly scoped, or unverifiable tokens are
rejected before service state is read or mutated. Clock-skew policy is explicit
and bounded. Error responses do not disclose whether a user, project, role, or
secret exists.

## Consequences

### Positive

- every service uses the same tenant-isolation rules;
- project name/ID confusion becomes a type and contract violation;
- service-to-service authorization is explicit;
- audit correlation survives later process separation;
- catalog contents accurately describe implemented behavior.

### Negative

- identity and policy contracts must be designed before broad endpoint work;
- service-token support adds configuration and testing requirements;
- migration from the bootstrap records requires compatibility tests.

## Rejected alternatives

### Let each service validate tokens independently

Rejected because it creates inconsistent expiry, role, project, and error
semantics and makes tenant isolation difficult to audit.

### Use the bootstrap admin identity for all internal calls

Rejected because it destroys least privilege, user attribution, and meaningful
service identity.

### Use project names as internal tenant keys

Rejected because names may be mutable or ambiguous and have already caused a
real protected-runner project-scope failure.

### Put orchestration state in Keystone

Rejected because identity is the trust root, not the resource transaction
coordinator.

## Required follow-up

- implement the normative context and catalog rules in SPEC-0020;
- add typed ID/name distinctions in domain and adapter boundaries;
- add service identity and service-token contract tests before cross-process
  service calls are introduced;
- test expiry, cross-project access, role reduction, catalog omission, and
  restart behavior;
- require security review before expanding authentication methods or token
  formats.
