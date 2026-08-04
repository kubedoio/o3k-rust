# SPEC-0023 — External Cinder service-under-test profile

Status: Proposed

Related documents:

- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [ADR-0163](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [Normative source ownership](../NORMATIVE_SOURCES.md)

## Purpose

O3K is a Rust-native OpenStack-compatible platform with multiple product
profiles. Two storage-related directions must not be conflated:

1. **O3K-owned native volume profile** — O3K exposes a Cinder-compatible API
   and executes storage operations through `o3k-storage` or an embedded
   `StorageProvider`.
2. **External Cinder service-under-test profile** — a real independently
   running Cinder deployment uses O3K as the surrounding Keystone-, Glance-,
   Nova-, and optionally Neutron/Placement-compatible testbed.

This specification defines the second profile. It does not replace the native
Rust Cinder roadmap, claim that O3K implements Cinder, or remove Cinder's own
runtime dependencies.

## Product intent

The profile allows storage developers and CI systems to test a real Cinder
deployment without installing a complete DevStack or full OpenStack control
plane. O3K supplies only the declared satellite APIs and orchestration needed
by the selected Cinder workflow.

The external Cinder deployment still owns and operates its supported:

- database and migrations;
- message bus used by its internal services;
- `cinder-api`, scheduler, volume, and other selected Cinder processes;
- storage backend and backend-specific dependencies;
- upgrades, configuration, service health, and operational lifecycle.

The profile can later be used within a small edge-cloud integration scenario,
but that does not automatically make external Cinder part of the native O3K
storage profile or prove broad cross-cloud compatibility.

## Catalog ownership

The catalog distinguishes:

```text
o3k-implemented
external-hosted
```

For external Cinder:

- service type is the selected supported Block Storage type, normally
  `volumev3`;
- endpoint URLs point to the real external Cinder API;
- service, region, interface, URL, enabled state, and ownership mode are
  durable records;
- endpoints are omitted when the hosted profile is disabled or unverified;
- catalog presence means endpoint discovery is configured, not that O3K
  implements Cinder.

## Required Keystone-compatible surface

Before this profile can be advertised, O3K provides the frozen subset required
by the selected Cinder version and public middleware behavior:

- durable domain, project, user, role, role-assignment, service, region, and
  endpoint identities;
- a service project and Cinder service user;
- project-scoped password authentication for the service user;
- token issuance with the configured catalog;
- public token validation through `GET /v3/auth/tokens`;
- public token existence checks through `HEAD /v3/auth/tokens` where required;
- one typed authorization context preserving original user/project audit
  context and authenticated service identity;
- explicit expiry, revocation limitations, policy behavior, and audit IDs;
- no password, token, signing key, or secret logging.

The exact routes and fields are recorded in the compatibility manifest before
implementation. Full Keystone parity, federation, and application credentials
are not implied.

Identity token validation uses the caller token in `X-Auth-Token` and the token
being inspected in `X-Subject-Token` according to the selected public Identity
contract. Tests exercise the public middleware/client behavior rather than an
internal verifier only.

## Required Glance-compatible surface

For selected image-backed Cinder operations, O3K provides only the declared
Glance subset, including authenticated image metadata and content access. The
profile records the exact Cinder operation requiring image access. One
successful download is not broad Glance compatibility evidence.

## Required Nova-compatible surface

For attachment workflows, O3K provides the frozen Nova volume-attachment API
subset required by the selected clients and Cinder integration. Exact methods,
paths, fields, policies, and microversions are declared before implementation.

O3K also provides a typed outbound Cinder v3 client for the selected attachment
sequence. A typical workflow is:

```text
validate user, project, server, and request
-> authenticate service identity
-> create or reserve Cinder attachment
-> provide connector data through the selected public flow
-> receive secret-safe connection information
-> attach through the compute execution boundary
-> complete the Cinder attachment
-> persist final Nova attachment state
```

The exact create/update/complete sequence follows the pinned public Cinder API
and version. It must not be guessed from another implementation.

## Database posture

O3K's database and Cinder's database are independent responsibilities.

- O3K currently uses SQLite for its minimal testbed control-plane state.
- PostgreSQL is an O3K production-profile target only after adapter conformance.
- The external Cinder deployment uses a database supported by the selected
  Cinder version.
- O3K must not claim that external Cinder runs without its own database or that
  O3K's SQLite database replaces it.

## Footprint reporting

A hosted-service footprint artifact reports separately:

- `o3kd` and any O3K execution agents;
- external Cinder API/scheduler/volume processes;
- Cinder database and message bus;
- storage backend dependencies;
- libvirt/QEMU or other selected execution dependencies.

The approximately 50 MB O3K control-plane target does not include the external
Cinder stack and is not a claim until measured for the exact profile.

## Security requirements

- Preserve user/project audit context and authenticated service identity.
- Treat tokens, passwords, connector data, connection information, backend
  paths, initiators, and credentials as secrets unless explicitly classified.
- Never upload or log complete Cinder configuration, connection information,
  private keys, tokens, service passwords, or unrestricted environments.
- Scope endpoint registration and token validation by policy.
- Fail closed on project, user, service, endpoint, region, role, or ownership
  ambiguity.
- Persist workflow phase before external mutation.
- Treat timeouts as unknown outcomes and observe before retry.
- Compensate without deleting foreign or externally owned resources blindly.

## Failure and compensation model

The attachment workflow records durable phases such as:

```text
validated
-> cinder_attachment_created
-> connection_prepared
-> compute_attached
-> cinder_attachment_completed
```

Failure tests cover at minimum:

- Cinder unavailable before mutation;
- timeout after attachment creation;
- connector update failure;
- compute attach failure;
- Cinder completion failure after compute attach;
- repeated request;
- control-plane restart at every persisted phase;
- detach and repeated detach;
- external Cinder observation disagreement;
- no orphan O3K attachment, leaked compute device, or unauthorized cleanup.

Compensation order follows the selected public Cinder and Nova workflow. A
failed compensation remains visible and reconcilable.

## Evidence ladder

1. product and compatibility manifest review;
2. Keystone, Glance, Nova, and outbound-Cinder contract tests;
3. stateful fake external Cinder tests;
4. portable process test using a fake Cinder HTTP service;
5. focused public-client or Tempest-compatible subset;
6. protected external-Cinder integration with source-bound artifacts;
7. failure/restart/cleanup matrix;
8. optional promotion into a later release or edge profile.

This profile is not a prerequisite for the first native ephemeral-root libvirt
alpha unless a later accepted decision explicitly changes the release gate.

## Protected integration profile

A real external Cinder test records:

- O3K and Cinder source/version identities;
- Cinder database, message bus, processes, and backend inventory without
  exposing secrets;
- registered service user, service, region, interface, endpoint, and ownership
  IDs;
- token issuance and public validation evidence;
- catalog discovery of the external endpoint;
- selected volume and attachment lifecycle;
- compute-side attachment observation;
- detach and cleanup;
- no O3K-owned leak and no unauthorized external-state mutation.

The valid claim is that O3K replaces the surrounding OpenStack control plane
required by the selected test workflow. It is not "Cinder without
dependencies."

## Non-goals

- full Keystone, Nova, Glance, or Cinder parity;
- O3K-owned volume storage in this profile;
- replacing the native Rust Cinder roadmap;
- boot from volume in the first profile;
- hiding or embedding unsupported Cinder dependencies;
- broad external-OpenStack or federation claims;
- production SLA or HA claims;
- making this profile block the first native ephemeral-root release.

## Public references

- Identity v3 API: https://docs.openstack.org/api-ref/identity/v3/
- Cinder installation: https://docs.openstack.org/cinder/latest/install/
- Cinder architecture: https://docs.openstack.org/cinder/latest/contributor/architecture.html
- Cinder attachment workflow: https://docs.openstack.org/cinder/latest/contributor/attach_detach_conventions_v2.html
- Nova Compute API: https://docs.openstack.org/api-ref/compute/
