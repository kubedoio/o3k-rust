# SPEC-0023 — External Cinder service-under-test profile

Status: Proposed

Related documents:

- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [Normative source ownership](../NORMATIVE_SOURCES.md)

## Purpose

O3K has two different storage-related product directions that must not be
conflated:

1. **O3K-owned volume profile** — O3K exposes a Cinder-compatible API and
   executes storage operations through `o3k-storage` or an embedded storage
   provider.
2. **External Cinder service-under-test profile** — a real independently
   running Cinder API/scheduler/volume deployment uses O3K as the surrounding
   Keystone-, Glance-, Nova-, and optionally Neutron-compatible testbed.

This specification defines the second profile. It does not claim that O3K
implements Cinder or removes Cinder's own runtime dependencies.

## Product intent

The profile allows storage developers and CI systems to test a real Cinder
deployment without installing a complete DevStack or full OpenStack control
plane. O3K supplies only the declared satellite APIs and orchestration needed
by the selected Cinder workflow.

The external Cinder deployment still owns and operates its supported:

- database;
- message bus used by its internal services;
- `cinder-api`, scheduler, volume, and other selected Cinder processes;
- storage backend and backend-specific dependencies;
- upgrades, migrations, and internal service health.

O3K documentation must state these dependencies explicitly.

## Catalog ownership

The catalog distinguishes service ownership:

```text
o3k-implemented
external-hosted
```

For external Cinder:

- service type is the selected supported Block Storage type, normally
  `volumev3`;
- endpoint URLs point to the real external Cinder API;
- endpoint records are durable, region/interface-specific, enabled explicitly,
  and omitted when the hosted profile is disabled;
- catalog presence means endpoint discovery is configured, not that O3K
  implements the Cinder API.

## Required Keystone-compatible surface

Before this profile can be advertised, O3K provides the frozen subset required
by the selected Cinder version and its public middleware behavior:

- durable domain, project, user, role, role-assignment, service, region, and
  endpoint identities required by the profile;
- a service project and Cinder service user;
- project-scoped password authentication for the service user;
- token issuance with the configured service catalog;
- public token validation through `GET /v3/auth/tokens`;
- public token existence checks through `HEAD /v3/auth/tokens` when required;
- one typed authorization context preserving both the original user/project
  context and authenticated service identity;
- explicit expiry, revocation limitations, policy behavior, and audit IDs;
- no secret or token logging.

The exact routes and fields are recorded in the compatibility manifest before
implementation. Full Keystone parity, federation, and application credentials
are not implied.

Official Identity token validation requires the caller token in
`X-Auth-Token` and the token being inspected in `X-Subject-Token`. The selected
profile must test the actual public middleware/client behavior rather than an
internal token verifier.

## Required Glance-compatible surface

For selected image-backed Cinder operations, O3K provides only the declared
Glance subset, including authenticated image metadata and content download.
The profile must record the exact Cinder operation that needs image access.
O3K does not infer broad Glance compatibility from one successful download.

## Required Nova-compatible surface

For attachment workflows, O3K provides the frozen Nova volume-attachment API
subset required by the selected clients and Cinder integration. The exact
method/path/microversion set is declared before implementation.

O3K also contains a typed outbound Cinder v3 client for the selected attachment
sequence. A typical accepted workflow is:

```text
validate user, project, server, and volume request
-> authenticate service identity
-> create/reserve Cinder attachment
-> provide connector data or update the attachment
-> receive secret-safe connection information
-> attach through the compute execution boundary
-> complete the Cinder attachment
-> persist final Nova attachment state
```

The exact create/update/complete sequence follows the selected public Cinder
API and version. It must not be guessed from another implementation.

## Security requirements

- Preserve user/project audit context and authenticated service identity.
- Treat tokens, passwords, connector data, connection information, backend
  paths, initiators, and credentials as secrets unless a field is explicitly
  classified otherwise.
- Never upload or log complete Cinder configuration, connection information,
  private keys, tokens, or service passwords.
- Scope endpoint registration and token validation by policy.
- Fail closed on project, user, service, endpoint, region, or role ambiguity.
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
failed compensation remains visible and reconcilable; it is not silently
reported as success.

## Evidence ladder

This profile proceeds through:

1. compatibility manifest and public-source review;
2. Keystone, Glance, Nova, and outbound-Cinder contract tests;
3. stateful fake external Cinder tests;
4. portable process test using a fake Cinder HTTP service;
5. focused Tempest/public-client subset where appropriate;
6. protected external-Cinder integration with source-bound artifacts;
7. failure/restart/cleanup matrix;
8. optional promotion into a later release profile.

It is not a prerequisite for the first ephemeral-root libvirt alpha unless a
later accepted decision explicitly changes the release gate.

## Protected integration profile

A real external Cinder test records:

- O3K and Cinder source/version identities;
- Cinder database, message-bus, service-process, and backend inventory without
  exposing credentials;
- registered service user, service, region, and endpoint IDs;
- token issuance and validation evidence;
- catalog discovery of the external endpoint;
- selected volume and attachment lifecycle;
- compute-side attachment observation;
- detach and cleanup;
- no O3K-owned leak and no unauthorized external-state mutation.

The profile must never claim “Cinder without dependencies.” The valid claim is
that O3K replaces the rest of the OpenStack control plane required by the
selected test workflow, while the real Cinder deployment retains its own
supported dependencies.

## Non-goals

- full Keystone, Nova, Glance, or Cinder parity;
- O3K-owned volume storage in this profile;
- boot from volume in the first profile;
- hiding or embedding unsupported Cinder dependencies;
- production SLA or HA claims;
- making this profile block the first ephemeral-root guest release.

## Public references

- Identity v3 API: https://docs.openstack.org/api-ref/identity/v3/
- Cinder installation: https://docs.openstack.org/cinder/latest/install/
- Cinder architecture: https://docs.openstack.org/cinder/latest/contributor/architecture.html
- Cinder attachment workflow: https://docs.openstack.org/cinder/latest/contributor/attach_detach_conventions_v2.html
- Nova Compute API: https://docs.openstack.org/api-ref/compute/
