# Roadmap

## Product roadmap model

O3K develops one Rust-native platform through three explicit product profiles:

1. **native Rust TestLab/cloud**;
2. **external OpenStack service testbed**;
3. **small edge cloud for approximately 10–20 hypervisors**.

The profiles share identity, compatibility, operation, reconciliation, and
execution contracts, but they have separate dependencies and evidence gates.
See [SPEC-0024](specs/SPEC-0024-product-profiles-and-claims.md).

## Phase 0 — Repository and engineering baseline

- project charter, architecture, clean implementation, and agent rules;
- Rust workspace, pinned toolchain, CI, supply-chain policy, and provenance;
- health/readiness, domain state, stores, operations, and public contract
  skeletons;
- issue-driven ADR, SPEC, contract, test, and review lifecycle;
- machine-readable OpenStack targets and product profiles.

## Phase 1 — Architecture, trust, and profile freeze

Before expanding runtime behavior:

- accept service topology and execution boundaries;
- accept the Keystone trust, catalog, authorization-context, and service-
  identity model;
- accept compute, network, and storage provider contracts;
- define cross-service workflow phases and reverse compensation;
- freeze the operation-level Gazpacho 2026.1 native TestLab profile;
- freeze the external-service ownership and hosted-service model;
- freeze native Rust, external-service-testbed, and edge product claims;
- record supported, optional, and explicitly unsupported operations;
- map each operation and product claim to evidence.

Outputs:

- ADR-0160 through ADR-0163;
- SPEC-0020 through SPEC-0024;
- `contracts/execution-boundaries.md`;
- `compatibility/openstack-targets.yaml`;
- `compatibility/product-profiles.yaml`;
- `docs/NORMATIVE_SOURCES.md`.

## Phase 2 — Shared portable foundations

### Keystone-compatible trust core

- durable domains, projects, users, groups, roles, and assignments;
- normalized `AuthContext`;
- bootstrap users and service users;
- services, regions, interfaces, endpoints, and ownership mode;
- token issue and public validation;
- catalog containing only enabled and verified profiles;
- strict durable-ID versus display-name separation;
- policy, expiry, audit, and redaction behavior.

### Service modules

- Glance-compatible metadata and authenticated content;
- Nova-compatible flavors, keypairs, servers, actions, console, and selected
  attachment APIs;
- Neutron-compatible flat networks, subnets, ports, fixed IPs, and binding;
- Placement-compatible inventory, scheduling, and allocations;
- later native Cinder-compatible volume state machines;
- typed outbound clients for selected external hosted-service workflows.

### Database posture

- make SQLite concurrency, WAL, migrations, crash recovery, backup/restore, and
  filesystem limits explicit;
- retain SQLite as the supported minimal TestLab database;
- design PostgreSQL as the production-oriented profile without claiming support
  before a real adapter and conformance suite exist.

### Portable simulated profiles

Run real O3K HTTP APIs, auth, stores, scheduler, operation journals,
reconciliation, and compensation with stateful fake compute, network, storage,
and external-service providers.

Required failure coverage:

- failure after every persisted phase;
- timeout and unknown outcome;
- duplicate delivery;
- stale observation and epoch;
- restart and replay;
- insufficient capacity;
- cross-project denial;
- complete reverse compensation.

## Track A — Native Rust TestLab

### A1. Real compute and network component gates

Compute:

- `o3k-compute` registration, heartbeat, reconnect, resync, and command journal;
- local `qemu:///system` capability discovery;
- image transfer, cache, qcow2 overlay, config-drive, console, and lifecycle;
- deterministic owned domain XML;
- restart discovery and no duplicate domain;
- complete compute-owned cleanup.

Network:

- typed `NetworkProvider` contract;
- TAP, bridge, DHCP, MAC/fixed-IP binding, and observation;
- isolated connectivity evidence;
- restart reconciliation;
- complete network-owned cleanup and foreign-link protection.

The first network executor may remain inside `o3k-compute`. A separate
`o3k-network` daemon requires its own accepted process-boundary decision.

### A2. Native libvirt alpha (`v0.2.0-alpha.1`)

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
- real image, overlay, config-drive, networking, guest boot, and console;
- guest-side config-drive/cloud-init consumption;
- service and libvirt restart with identity preservation;
- no duplicate mutation after retry or reconnect;
- no O3K-owned leak and no foreign-state change;
- clean installation, reset, reinstall, uninstall, and purge;
- measured profile-specific footprint and latency;
- human architecture/security review;
- SBOM, provenance, checksums, limitations, and signed release.

Native persistent volumes do not block this release.

### A3. Native Rust Cinder profile

After the ephemeral-root alpha:

- O3K-owned Cinder-compatible volume and attachment baseline;
- `volumev3` advertisement only after portable verification;
- Nova attach/detach integration;
- typed `StorageProvider`;
- `o3k-storage` and local LVM component gate;
- optional Ceph RBD backend;
- boot-from-volume only after a separate accepted profile.

## Track B — External OpenStack service testbed

### B1. Hosted-service identity and catalog

- service project, service users, roles, services, regions, interfaces, and
  endpoints;
- public token validation required by selected OpenStack middleware;
- explicit `external-hosted` ownership mode;
- catalog omission for disabled or unverified endpoints.

### B2. First real service: Cinder

- selected external Cinder version and workflow frozen;
- selected Glance-compatible image surface;
- Nova volume-attachment API subset;
- typed outbound Cinder attachment client;
- fake external-Cinder failure and compensation matrix;
- focused public-client or Tempest-compatible evidence;
- protected real external-Cinder integration.

The real Cinder deployment keeps its own database, message bus,
API/scheduler/volume processes, backend, migrations, upgrades, and health.
O3K replaces only the surrounding OpenStack control-plane services required by
the selected test workflow.

### B3. Additional service-under-test profiles

New hosted-service profiles require their own:

- required satellite API inventory;
- identity and catalog model;
- version and dependency declaration;
- stateful fake and real integration gate;
- security, failure, cleanup, and claim evidence.

## Track C — Small edge cloud

### C1. Multi-host foundation

- target approximately 10–20 hypervisors;
- multi-host capability inventory and Placement scheduling;
- host enrollment, mTLS identity, epochs, heartbeats, reconnect, and resync;
- failure-safe operation replay and fencing;
- host-aware network binding and cleanup;
- backup, restore, upgrade, rollback, and diagnostic operations;
- quotas, policy, and project isolation appropriate to the profile.

### C2. Database and availability

- measured single-controller SQLite limits may support an initial edge profile;
- PostgreSQL adapter and conformance are required before PostgreSQL is claimed;
- multi-controller or HA claims require explicit coordination, fencing,
  failover, and database evidence;
- no production recommendation is made from architecture intent alone.

### C3. Execution-process growth

Only after stable contracts and measured need:

- separate `o3k-network` process;
- separate `o3k-storage` process;
- CellHV provider conformance;
- richer Neutron and native storage profiles.

### C4. External OpenStack interoperation

Treat each as a separate product profile:

- hosted external services in the O3K catalog;
- optional external Keystone mode;
- endpoint registration into external Keystone;
- external Glance, Cinder, Neutron, or Placement consumption;
- federation or cross-cloud project/resource mapping.

A generic “connects to OpenStack” feature is not accepted.

## Footprint roadmap

The minimal O3K control plane targets approximately 50 MB steady-state memory.
The project must measure and publish separately:

- `o3kd` minimal portable/TestLab profile;
- `o3kd + o3k-compute` libvirt profile;
- hosted-service-testbed O3K processes;
- edge profile at supported host counts;
- external Cinder, RabbitMQ, PostgreSQL, libvirt, QEMU, and storage backend
  footprints as separate dependencies.

A footprint target becomes a release claim only after source-bound measurement.

## Roadmap governance

- endpoint count is not progress unless a declared profile and its evidence
  advance;
- new public operations require a baseline change;
- process separation requires a failure-model and privilege ADR;
- external-hosted and O3K-implemented services are different profiles;
- external-service and native-service work may proceed in parallel without
  replacing one another;
- the full runner is a final integration gate, not a requirements-discovery
  loop;
- PostgreSQL, edge-production, cross-cloud, native-Cinder, metadata-HTTP, and
  footprint claims fail closed until their profile evidence exists;
- detailed rules live in the normative sources listed by
  `docs/NORMATIVE_SOURCES.md`.
