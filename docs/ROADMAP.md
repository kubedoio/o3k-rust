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
- record supported, optional, and explicitly unsupported operations;
- map each operation to portable and real-host evidence.

Outputs:

- ADR-0160 through ADR-0162;
- SPEC-0020 through SPEC-0022;
- `contracts/execution-boundaries.md`;
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
- immutable dependency snapshots and operation state machines.

### Network

- network, subnet, port, MAC, fixed-IP, and binding intent;
- flat-network profile and deterministic cleanup.

### Volume design

- freeze the later Cinder-compatible volume and attachment baseline;
- implement fake storage provider and state machines without blocking the first
  ephemeral-root guest.

### Portable full-cloud gate

Run real O3K HTTP APIs, auth, stores, scheduler, operation journals,
reconciliation, and compensation with stateful fake compute/network/storage
providers.

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

This component belongs to the persistent-volume milestone:

- typed `StorageProvider` contract;
- local LVM reference backend;
- volume create/inspect/delete;
- attachment preparation and termination;
- secret-safe connection information;
- restart, unknown-outcome, and cleanup evidence.

`o3k-storage` is not a prerequisite for the first ephemeral-root guest.

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
- server create/show/list/inspect/stop/start/reboot/delete;
- `o3kd`, `o3k-compute`, and libvirt restart with identity preservation;
- no duplicate resource after retry or reconnect;
- no O3K-owned leak and no foreign-state change;
- clean-host installation, reset, reinstall, uninstall, and purge;
- real footprint and latency measurements;
- human architecture/security review;
- SBOM, provenance, checksums, known limitations, and signed release.

A skipped component or full-cloud integration environment is not passing
evidence.

## Phase 5 — Persistent-volume profile

After the ephemeral-root alpha:

- Cinder-compatible volume and attachment API baseline;
- `volumev3` catalog advertisement only after portable verification;
- Nova attach/detach integration;
- real `o3k-storage`/LVM component gate;
- full-cloud volume lifecycle;
- optional Ceph RBD backend;
- boot-from-volume only after a separate accepted spec and evidence gate.

## Phase 6 — Optional provider and process expansion

Only after stable contracts and measured need:

- separate `o3k-network` process;
- separate `o3k-storage` process;
- CellHV compute/network/storage provider conformance;
- PostgreSQL and external object storage;
- richer Neutron profiles;
- quotas and broader policy;
- small-cluster coordination and fencing;
- edge lifecycle and production-readiness work for supported SMB profiles.

## Roadmap governance

- endpoint count is not progress unless the declared profile and evidence
  advance;
- new public operations require a baseline change;
- process separation requires a failure-model and privilege ADR;
- Cinder work may proceed in parallel but does not block the first VM;
- the full runner is a final integration gate, not a requirements-discovery
  loop;
- current release claims must distinguish portable, component-real-host, and
  full-cloud evidence.
