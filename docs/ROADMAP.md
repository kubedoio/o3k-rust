# Roadmap

## Phase 0 — Repository and engineering baseline

- project charter, architecture, clean implementation, and agent rules;
- Rust workspace, pinned toolchain, CI, supply-chain policy, and provenance;
- health/readiness, domain state, stores, operations, and public contract
  skeletons;
- issue-driven ADR, SPEC, contract, test, and review lifecycle.

## Phase 1 — Architecture and compatibility freeze

Before expanding runtime behavior:

- accept the service topology and execution boundaries;
- accept the Keystone trust, catalog, authorization-context, and service-
  identity model;
- accept compute, network, and storage provider contracts;
- define cross-service workflow phases and reverse compensation;
- freeze the operation-level Gazpacho 2026.1 TestLab profile;
- define the external-service ownership model separately from O3K-implemented
  services;
- record supported, optional, and explicitly unsupported operations;
- map each operation to portable and real-host evidence.

Outputs:

- ADR-0160 through ADR-0162;
- SPEC-0020 through SPEC-0023;
- `contracts/execution-boundaries.md`;
- `docs/NORMATIVE_SOURCES.md`;
- updated compatibility and traceability manifests.

## Phase 2 — Portable service modules and simulated cloud

Complete the declared profile without privileged host dependencies:

### Identity

- Keystone-compatible bootstrap and durable ID/name separation;
- normalized `AuthContext`;
- fail-closed policy declarations;
- service catalog containing only enabled profiles;
- service-identity contract tests.

### Image

- metadata and authenticated content lifecycle;
- checksum, format, visibility, and immutable activation;
- no host-path leakage.

### Compute and Placement

- flavors, keypairs, servers, actions, console contracts;
- resource providers, inventory, scheduling, and allocations;
- immutable dependency snapshots and operation state machines;
- evidence-backed Nova and Placement version advertisement.

### Network

- network, subnet, port, MAC, fixed-IP, and binding intent;
- flat-network profile and deterministic cleanup.

### Volume and hosted-service design

- freeze the later O3K-owned Cinder-compatible volume and attachment baseline;
- freeze the separate external Cinder service-under-test profile;
- implement fake storage and fake external-Cinder workflows without blocking
  the first ephemeral-root guest.

### Database posture

- enable and verify safe SQLite concurrency for the current profile;
- decide explicitly whether SQLite is the supported first product database or a
  real PostgreSQL adapter remains committed work;
- do not advertise PostgreSQL support before adapter conformance exists.

### Portable full-cloud gate

Run real O3K HTTP APIs, auth, stores, scheduler, operation journals,
reconciliation, and compensation with stateful fake compute/network/storage and
external-service providers.

Required failure coverage:

- failure after every persisted phase;
- timeout and unknown outcome;
- duplicate delivery;
- stale observation and epoch;
- restart and replay;
- insufficient capacity;
- cross-project denial;
- complete reverse compensation.

## Phase 3 — Real component gates

### Compute component

- `o3k-compute` registration, heartbeat, reconnect, and command journal;
- local `qemu:///system` libvirt/KVM capability discovery;
- image transfer, base cache, qcow2 overlay, and config-drive;
- deterministic owned domain XML;
- config-drive guest consumption evidence;
- console and lifecycle actions;
- bounded diagnostic hold before cleanup;
- restart discovery and no duplicate domain;
- complete compute-owned cleanup.

### Network component

- typed `NetworkProvider` contract;
- TAP, bridge, DHCP, MAC/fixed-IP binding, and observation;
- isolated connectivity evidence;
- restart reconciliation;
- complete network-owned cleanup and foreign-link protection.

The first implementation may host the network executor inside `o3k-compute`.
`o3k-network` becomes a separate daemon only after a dedicated process-boundary
ADR and conformance evidence.

### Storage component

This component belongs to the O3K-owned persistent-volume milestone:

- typed `StorageProvider` contract;
- local LVM reference backend;
- volume create/inspect/delete;
- attachment preparation and termination;
- secret-safe connection information;
- restart, unknown-outcome, and cleanup evidence.

`o3k-storage` is not a prerequisite for the first ephemeral-root guest or for
running an external Cinder service-under-test.

## Phase 4 — Libvirt TestLab full-cloud alpha (`v0.2.0-alpha.1`)

Integrate only after the portable cloud and required component gates pass:

```text
Keystone
-> Glance
-> Neutron
-> Placement
-> Nova
-> o3k-compute
-> libvirt/QEMU guest
```

Required evidence:

- standard OpenStack CLI discovery and lifecycle;
- real image, overlay, config-drive, network binding, guest boot, and console;
- guest-side proof that the config-drive metadata profile was consumed;
- config-drive-only metadata claim; no unverified HTTP metadata service;
- server create/show/list/inspect/stop/start/reboot/delete;
- `o3kd`, `o3k-compute`, and libvirt restart with identity preservation;
- no duplicate resource after retry or reconnect;
- no O3K-owned leak and no foreign-state change;
- clean-host installation, reset, reinstall, uninstall, and purge;
- real footprint and latency measurements per deployment profile;
- human architecture/security review;
- SBOM, provenance, checksums, known limitations, and signed release.

A skipped component or full-cloud integration environment is not passing
evidence.

## Phase 5 — External Cinder service-under-test profile

After the hosted-service identity and Nova attachment contracts pass:

- durable service project, Cinder service user, roles, services, regions, and
  external endpoint records;
- public token validation required by the selected Cinder middleware profile;
- catalog entry marked `external-hosted`, never `o3k-implemented`;
- Nova volume-attachment API subset;
- typed outbound Cinder v3 attachment client;
- fake external-Cinder failure and compensation matrix;
- focused Tempest/public-client evidence;
- protected real external-Cinder integration.

The real Cinder deployment retains its own supported database, message bus,
API/scheduler/volume processes, backend, migrations, and upgrades. O3K replaces
only the surrounding control-plane services needed by the selected workflow.

This profile is not a prerequisite for `v0.2.0-alpha.1` unless a later accepted
decision explicitly changes the release gate.

## Phase 6 — O3K-owned persistent-volume profile

Independently of the external Cinder profile:

- Cinder-compatible volume and attachment API baseline implemented by O3K;
- `volumev3` catalog advertisement only after portable verification;
- Nova attach/detach integration;
- real `o3k-storage`/LVM component gate;
- full-cloud volume lifecycle;
- optional Ceph RBD backend;
- boot-from-volume only after a separate accepted spec and evidence gate.

## Phase 7 — Optional provider and process expansion

Only after stable contracts and measured need:

- separate `o3k-network` process;
- separate `o3k-storage` process;
- CellHV compute/network/storage provider conformance;
- external Keystone mode only after an explicit trust/failure decision;
- PostgreSQL and external object storage only after conformance evidence;
- richer Neutron profiles;
- quotas and broader policy;
- small-cluster coordination and fencing;
- edge lifecycle and production-readiness work for supported SMB profiles.

## Roadmap governance

- endpoint count is not progress unless the declared profile and evidence
  advance;
- new public operations require a baseline change;
- process separation requires a failure-model and privilege ADR;
- external-hosted and O3K-implemented services are different profiles;
- Cinder-related work may proceed in parallel but does not block the first VM;
- the full runner is a final integration gate, not a requirements-discovery
  loop;
- current release claims must distinguish implemented, advertised, portable,
  component-real-host, hosted-service, and full-cloud evidence;
- detailed rules live in the normative sources listed by
  `docs/NORMATIVE_SOURCES.md`.
