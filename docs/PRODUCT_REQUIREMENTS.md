# Product Requirements — TestLab Alpha (`v0.2.0-alpha.1`)

## Product boundary

O3K TestLab is a lightweight OpenStack-compatible profile, not a claim of
complete OpenStack service parity. The release advertises only operations that
are specified, implemented, and backed by executable compatibility evidence.

The primary compatibility reference is OpenStack 2026.1 Gazpacho. Compatibility
is declared per service, operation, API version, microversion, and extension.

## Required user journey

Given a supported Linux host with QEMU/KVM and libvirt, a user can:

1. install and start O3K;
2. retrieve generated admin credentials safely;
3. discover the enabled service catalog;
4. run `openstack token issue`;
5. create/list/show/delete image metadata and upload image content;
6. create/list/show/delete a flat network, subnet, and port;
7. create/list/show/delete a flavor and keypair;
8. create/list/show/start/stop/reboot/delete a server;
9. observe the operation, Placement allocation, port binding, and final
   resource state;
10. retrieve the real guest console log through the Nova-compatible operation;
11. restart `o3kd`, `o3k-compute`, and libvirt without losing or duplicating
    the instance;
12. delete the instance and clean every O3K-owned resource;
13. run the complete workflow using documented install, diagnostic, reset, and
    uninstall commands.

A later persistent-volume profile adds Cinder-compatible volume create,
attachment, detach, and delete. It does not block the first ephemeral-root
libvirt guest.

## Functional requirements

### Identity and trust

- Keystone v3 password authentication for the bootstrap profile;
- durable distinction between IDs and names;
- project-scoped tokens;
- one normalized internal `AuthContext` consumed by every service;
- explicit roles and fail-closed policy declarations;
- minimal service catalog containing only implemented services;
- service identity for future cross-process service calls;
- explicit token expiry and restart-safe verification;
- no token, password, signing key, or private credential logging.

### Images

- metadata create/list/show/delete;
- authenticated content upload/download for the local backend;
- checksum, format, and size validation;
- immutable image content after activation in the alpha profile;
- project visibility and inactive/missing image rejection before compute
  dispatch;
- no control-plane host path in public or provider contracts.

### Networking

- flat provider network;
- IPv4 subnet, allocation pool, gateway, and DNS validation;
- durable port, MAC, fixed-IP, project, and host-binding intent;
- deterministic network execution and cleanup;
- stateful fake provider for portable full-cloud tests;
- real TAP, bridge, DHCP/binding, and connectivity evidence at the network
  component gate.

The first profile does not include routers, floating IPs, security groups,
VLAN/VXLAN tenant networks, OVS, or OVN.

### Placement

- compute resource providers;
- VCPU, MEMORY_MB, and DISK_GB inventory;
- generation-protected inventory and allocations;
- deterministic host selection for the selected scheduler profile;
- insufficient-capacity rejection before provider dispatch;
- allocation retention for unknown outcomes and exact release after terminal
  failure or deletion.

### Compute

- flavors and keypairs;
- server lifecycle through the versioned provider boundary;
- immutable server dependency snapshots;
- libvirt/KVM through `o3k-compute` and local `qemu:///system` as the primary
  real backend;
- image/base/overlay and config-drive realization;
- bounded real console output;
- fake provider for fast contract, compensation, and failure-path tests;
- persisted desired and observed states;
- idempotent lifecycle and no duplicate provider resources;
- CellHV as an optional later provider, independently releasable.

### Volumes and storage

The first alpha does not require or advertise a Cinder-compatible volume
service. The persistent-volume follow-up requires:

- volume types;
- volume create/list/show/delete;
- attachment preparation, attach, detach, and termination;
- `o3k-storage` contract and provider conformance;
- a local LVM reference backend before optional Ceph RBD;
- secret-safe connection information;
- timeout and unknown-outcome recovery.

No boot-from-volume claim is made until separately specified and verified.

### Operations and reconciliation

- operation ID for every mutation;
- deterministic command and idempotency identity;
- structured state, phase, and error information;
- intent persisted before external side effects;
- reverse-order compensation;
- reconciliation after control-plane or agent restart;
- unknown outcomes observed before mutation retry;
- bounded retries and visible terminal failure;
- no adoption or deletion of ambiguous or foreign resources.

## Development and evidence requirements

Before full-cloud real-host testing, the declared profile must pass:

1. normative ADR/spec/contract validation;
2. operation-level compatibility inventory;
3. domain, store, migration, and policy tests;
4. stateful compute/network/storage provider conformance;
5. portable simulated-cloud integration using real HTTP, auth, stores,
   scheduler, operations, and reconciliation;
6. process-level OpenStack client contract tests.

Privileged evidence is staged:

1. compute component gate;
2. network component gate;
3. storage component gate when the persistent-volume profile is selected;
4. full-cloud gate;
5. restart/failure matrix;
6. release gate.

The full runner is a final integration verifier, not the primary source for
missing endpoint requirements.

## `v0.2.0-alpha.1` exit criteria

The release is ready only when executable tests and source-bound evidence show:

- the declared identity, image, network, placement, and compute API profile is
  portable-contract verified;
- the simulated full-cloud workflow passes success, failure, restart,
  compensation, duplicate, and unknown-outcome scenarios;
- a real image is transferred, verified, and realized as a qcow2 base/overlay;
- a real config-drive is attached and guest-side consumption is proven;
- a real owned libvirt domain boots and remains inspectable before cleanup;
- the expected port/MAC/fixed-IP and network execution are proven;
- real guest console output is retrievable and bounded;
- inspect/list/stop/start/reboot/delete pass;
- `o3kd`, `o3k-compute`, and libvirt restart without loss, duplication, or
  ownership change;
- deletion leaves no O3K-owned domain, overlay, config-drive, console, TAP,
  DHCP binding, Neutron ownership, Placement allocation, unfinished operation,
  process, or temporary file;
- foreign host state is unchanged;
- clean supported Linux installation and reproduction commands pass;
- compatibility status, footprint/latency measurements, SBOM, checksums,
  provenance, known limitations, and signed release artifacts are published.

Cinder and `o3k-storage` are documented as planned or portable-only unless the
persistent-volume profile independently passes its component and full-cloud
gates.

## Non-functional requirements

- clean-host install documented and tested;
- zero external message queue in the alpha profile;
- SQLite default; PostgreSQL compatibility planned;
- structured JSON logs with request, audit, operation, resource, and trace IDs;
- Prometheus metrics and explicit readiness semantics;
- no secret values in metrics, traces, logs, errors, or general CI artifacts;
- dedicated non-root agent accounts with minimum reviewed capabilities;
- bounded inputs, artifacts, retries, evidence, and diagnostic holds;
- signed release and SBOM before public alpha;
- p95 API latency and resource footprint measured, not guessed.

## Compatibility policy

Every supported operation is listed in a compatibility manifest with:

- official public reference;
- request, response, discovery, and error contract;
- auth scope and policy;
- supported fields and microversion;
- state transition, dependencies, and compensation;
- provider capability requirement;
- known deviations;
- portable, component, and full-cloud evidence.

Unsupported fields or extensions must not be silently advertised. Implementing
an adjacent endpoint requires an explicit baseline change rather than an
opportunistic code addition.
