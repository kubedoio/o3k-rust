# SPEC-0022 — Service API baseline and evidence gates

Status: Proposed

Primary reference profile: OpenStack 2026.1 Gazpacho

Backward reference profile: OpenStack 2025.2 Flamingo where explicitly listed

Related decisions:

- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0162](../adr/ADR-0162-contract-first-staged-runner-validation.md)

## Purpose

O3K implements declared OpenStack-compatible profiles, not entire upstream
services. This specification defines how a profile is frozen, implemented,
tested, advertised, and released.

The baseline prevents two failure modes:

- discovering fundamental API requirements one endpoint at a time on a
  privileged runner;
- delaying all integration until an undefined goal of “complete Nova,
  Neutron, Cinder, and Keystone” is reached.

## Compatibility record

Every advertised operation has a machine-readable record containing:

- profile ID and service type;
- operation ID;
- HTTP method and canonical path;
- API version, microversion range, or extension;
- auth scope and policy ID;
- required request headers, path/query parameters, and body fields;
- response headers, status code, body fields, and error envelopes;
- supported and explicitly unsupported fields;
- idempotency behavior;
- domain state transition;
- cross-service dependencies;
- provider capability requirement;
- official public reference;
- public client/SDK/Tempest evidence;
- portable test IDs;
- component-runner evidence;
- full-cloud evidence;
- known deviations and release claim.

A route without this record is unsupported. A record without executable
evidence is planned or implemented, not verified.

## Evidence states

Each operation and workflow records these independent states:

```text
missing
specified
implemented
portable-contract-verified
process-verified
component-real-host-verified
full-cloud-verified
release-claimed
```

A later state requires all earlier states. `skipped`, `ready`, unit-test-only,
or fake-provider-only results cannot be promoted to real-host verification.

## TestLab core profile

The first ephemeral-root TestLab profile is intentionally bounded.

### Identity / Keystone-compatible

Required operations and behavior:

- version discovery for the advertised identity root;
- project-scoped password authentication;
- `X-Subject-Token` issuance;
- token expiry and verification;
- service catalog for enabled profile services;
- project ID/path consistency;
- generic auth failure and redaction.

Required semantics are further defined in SPEC-0020.

### Image / Glance-compatible

Required operations:

- create image metadata;
- upload content;
- list images;
- show image;
- authenticated content download;
- delete image.

Required behavior:

- size and checksum validation;
- supported disk/container formats;
- queued/saving/active or documented equivalent transitions;
- immutable active content for the profile;
- project visibility;
- missing/inactive-image rejection before compute dispatch;
- no host path exposure.

### Network / Neutron-compatible

Required operations:

- create/list/show/delete network;
- create/list/show/delete subnet;
- create/list/show/delete port.

Required behavior:

- flat provider-network profile;
- IPv4 subnet, gateway, allocation pool, and DNS validation;
- stable MAC and fixed-IP ownership;
- network/subnet/port dependency rules;
- port binding intent and selected host;
- conflict detection;
- complete cleanup.

Not included in the first profile:

- routers;
- floating IPs;
- security groups;
- VLAN/VXLAN tenant networks;
- OVS/OVN compatibility.

### Placement-compatible

Required operations or equivalent internal/public profile behavior:

- resource-provider registration and discovery;
- VCPU, MEMORY_MB, and DISK_GB inventory;
- usage and available capacity;
- generation-protected inventory update;
- candidate selection required by the scheduler;
- allocation create/show/delete;
- capacity and generation conflict responses.

If public Placement endpoints are advertised, their exact operations must be
listed in the compatibility manifest. Internal-only behavior must not be
advertised as public Placement parity.

### Compute / Nova-compatible

Required operations:

- flavor create/list/detail/show/delete;
- keypair import/list/show/delete;
- server create/list/detail/show/delete;
- server stop/start/hard reboot;
- console-log retrieval;
- version discovery and the selected microversion range.

Required create validation:

- token/project path;
- active image;
- immutable flavor snapshot;
- owned keypair when requested;
- valid network/port dependencies;
- Placement capacity;
- idempotency identity;
- provider capability.

Required lifecycle behavior:

- public state derives from accepted observations;
- timeout becomes unknown outcome;
- duplicate delivery does not create a second resource;
- delete compensates dependencies and is idempotent;
- console output is real, bounded, authorized, and path-safe;
- restart preserves resource identity.

Not included in the first profile:

- resize;
- rebuild;
- rescue;
- live migration;
- evacuation;
- server groups;
- NUMA or PCI passthrough;
- full Nova extension parity.

### Volume / Cinder-compatible

The first ephemeral-root TestLab profile does not require Cinder and does not
advertise `volumev3`.

The later persistent-volume profile requires a separately frozen baseline.
Minimum planned operations:

- volume type list/show and operator-managed create where selected;
- volume create/list/show/delete;
- attachment create/show/update/delete or the selected Cinder attachment
  sequence;
- Nova volume attach/detach surface required by the client workflow;
- backend capability and availability evidence.

Minimum planned behavior:

- project ownership and policy;
- size and type validation;
- idempotent backend create/delete;
- attachment state machine;
- secret-safe connection information;
- local LVM reference backend before optional Ceph RBD;
- timeout/unknown-outcome recovery;
- no boot-from-volume claim until separately specified and verified.

## Service module definition of complete

A logical service module is ready for integration only when:

1. its advertised profile is frozen;
2. HTTP contracts and discovery are generated or validated;
3. domain state machines and invariants are tested;
4. store migrations and conformance tests pass;
5. policy declarations fail closed;
6. dependencies and compensation rules are executable;
7. a stateful fake provider covers success and required failure modes;
8. process-level OpenStack client tests pass;
9. compatibility inventory and known deviations are current.

This definition does not require every upstream endpoint.

## Portable simulated-cloud gate

Before the full runner, the following public workflow must pass against real
O3K HTTP APIs, stores, state machines, scheduler, and reconciler with only
provider execution faked:

```text
discover
-> authenticate
-> create image and upload bytes
-> create network/subnet/port
-> create flavor and keypair
-> create server
-> observe ACTIVE-equivalent fake state
-> console
-> stop/start/reboot
-> optional volume create/attach/detach profile
-> delete
-> restart/replay and cleanup
```

The gate also injects failure after every persisted phase and verifies reverse
compensation, unknown outcomes, duplicate delivery, project isolation, and
foreign-state preservation.

## Component real-host gates

### Compute component gate

Required evidence:

- exact source commit and host capabilities;
- real image transfer and digest;
- `qemu-img` format/backing-chain inspection;
- config-drive attachment;
- deterministic owned libvirt XML;
- `virsh list --all` evidence during a bounded pre-cleanup hold point;
- domain start, inspect, stop, start, reboot, console, and delete;
- no compute-owned leak or foreign-domain change.

### Network component gate

Required evidence:

- selected port ID, MAC, fixed IP, subnet, and host;
- owned TAP and bridge membership;
- DHCP configuration and lease or equivalent binding evidence;
- guest or isolated namespace connectivity test;
- restart reconciliation;
- no network-owned leak or foreign-link change.

### Storage component gate

Required only for the persistent-volume profile:

- selected backend and capability;
- real volume create/show/delete;
- attachment preparation and teardown;
- bounded secret-safe evidence;
- restart reconciliation;
- no storage-owned leak or foreign-volume change.

## Full-cloud gate

The full-cloud gate runs only after the relevant component gates pass.

For the ephemeral-root profile:

```text
Keystone
-> Glance
-> Neutron
-> Placement
-> Nova
-> o3k-compute/libvirt
-> guest boot and console
-> restart matrix
-> delete and complete cleanup
```

For the persistent-volume profile, Cinder and `o3k-storage` are added only
after the storage component gate passes.

The full-cloud workflow must use public OpenStack APIs and standard clients. It
must not repair state through direct database updates or unadvertised operator
shortcuts.

## Diagnostic mode

Protected component and full-cloud workflows support an opt-in diagnostic mode
that:

- is manual and trusted-runner only;
- records the exact source SHA;
- pauses after the failing or requested phase for a bounded duration;
- emits redacted commands for inspecting owned state;
- always performs ownership-checked cleanup after timeout or cancellation;
- never uploads tokens, passwords, private keys, user-data, connection
  information, or unrestricted daemon environments.

Diagnostic mode is not passing evidence by itself.

## Baseline change control

A baseline change requires:

- issue and rationale;
- official public source;
- compatibility inventory update;
- spec and contract change;
- portable tests before implementation completion;
- known-deviation and release-impact assessment;
- human review when public API, auth, persistent state, or privileged execution
  changes.

LLM agents must not expand the profile because an adjacent endpoint appears
easy to implement.

## Release claims

Release documentation names:

- the primary OpenStack reference release;
- per-service API versions and microversion ranges;
- verified operations;
- portable-only operations;
- known deviations;
- unsupported services and extensions;
- component and full-cloud evidence IDs.

“O3K supports Gazpacho” is not a valid standalone claim. The valid claim is the
published operation-level compatibility profile tested against Gazpacho-era
specifications and clients.
