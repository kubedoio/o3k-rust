# Roadmap

## Product roadmap model

O3K develops one product:

> **O3K Cloud OS — a lightweight, open, Rust-native Cloud Operating System.**

The project uses three primary deployment/evidence profiles:

1. native O3K TestLab/cloud;
2. external OpenStack service testbed;
3. small edge cloud for approximately 10–20 hypervisors.

The profiles share the O3K Cloud Kernel architecture but have separate
dependencies and evidence gates.

See:

- [ADR-0165](adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0163](adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [SPEC-0024](specs/SPEC-0024-product-profiles-and-claims.md)

## Non-negotiable sequencing rule

The accepted Cloud OS architecture does **not** replan the current alpha.

The immediate release remains:

> `v0.2.0-alpha.1` — Rust-native OpenStack-compatible libvirt TestLab alpha.

Broad IAM redesign, native APIs, managed databases, Kubernetes, AI/ML,
federation, PostgreSQL, or other Cloud Kernel expansion must not enter the
first-alpha critical path unless a separate accepted human decision explicitly
replans the release.

## Phase 0 — Repository and engineering baseline

- Rust workspace/toolchain/CI/supply-chain/provenance;
- ADR/SPEC/contract/test/evidence lifecycle;
- core domain/store/operation foundations;
- machine-readable OpenStack targets/profiles;
- architecture-boundary ratchets.

## Phase 1 — Current architecture and first-alpha contract freeze

- accepted service/execution topology;
- provider/agent authority;
- durable operation/reconciliation model;
- selected OpenStack 2026.1 compatibility baseline;
- external-hosted-service ownership;
- database/footprint claim discipline;
- first-alpha real-host evidence matrix.

Outputs include ADR-0160/0162 and the existing execution/rewrite contracts.

## Track A — Finish and release the native libvirt alpha

### A1. Real compute/network gate

Compute:

- secure `o3k-compute` registration/heartbeat/reconnect/resync;
- local `qemu:///system` capabilities;
- image transfer/cache/qcow2 overlay;
- config-drive;
- console;
- deterministic owned domain XML;
- restart discovery;
- complete owned cleanup.

Network:

- typed network-provider boundary;
- TAP/bridge/DHCP/MAC/IP binding;
- restart reconciliation;
- foreign-link protection;
- complete network cleanup.

### A2. Release `v0.2.0-alpha.1`

```text
Keystone compatibility
-> O3K IAM/AuthContext subset
-> Glance compatibility / O3K Image
-> Neutron compatibility / O3K Network
-> Placement compatibility / O3K Capacity
-> Nova compatibility / O3K Compute
-> o3k-compute
-> libvirt/QEMU guest
```

Required evidence remains:

- standard OpenStack CLI discovery/lifecycle;
- real image/overlay/config-drive/network/guest/console;
- restart identity preservation;
- no duplicate mutation;
- no owned leaks;
- unchanged foreign state;
- clean install/reset/reinstall/uninstall/purge;
- measured profile footprint/latency;
- human architecture/security review;
- SBOM/provenance/checksums/known limitations/signed release.

Native volumes do not block this release.

## Phase 2 — Cloud Kernel convergence after the alpha

This phase converts the already-working vertical slice into the reusable
platform architecture without starting a second cloud implementation.

### K1. Canonical O3K IAM boundary

- accept/use ADR-0166 and SPEC-0020;
- keep the existing Keystone-compatible user journey working;
- make `AuthContext` canonical;
- ensure services consume typed principal/scope/service identity;
- remove any service-local Keystone token interpretation;
- prove cross-scope denial before provider dispatch.

### K2. Shared authorization model

Introduce stable:

```text
Principal
Action
Resource
Context
Decision
```

for current O3K services.

Required work:

- typed action IDs;
- typed resource types;
- durable owner/security-scope requirement;
- default-deny policy engine boundary;
- service-principal/delegation checks;
- shared authorization conformance suite;
- compatibility role/policy translation at edges.

Do not build a huge generic policy language before the current service
vocabulary proves the required semantics.

### K3. Service registry and compatibility projection

- canonical service identity/namespace;
- ownership mode;
- API/version/region/endpoint metadata;
- resource/action vocabulary;
- bounded capability metadata;
- evidence/advertisement state;
- Keystone catalog generated as a compatibility projection.

Dynamic plugin installation is not required initially.

### K4. Shared resource/operation/audit primitives

Converge current services on:

- resource identity/ownership;
- operation identity/phase;
- idempotency;
- unknown outcome;
- audit identity;
- standard failure categories;
- common region/AZ identity;
- quota/limit hooks where actually required.

### K5. Architecture fitness functions

Extend machine-readable/CI checks so new code cannot:

- make OpenStack wire types canonical domain types;
- make Keystone token structures application-domain state;
- introduce service-local tenant ownership without a kernel contract;
- let provider adapters authorize callers;
- let provider-native IDs replace O3K public IDs;
- treat delegated clouds as ordinary execution providers.

## Track B — Native O3K Volume / Cinder compatibility

After the first alpha and enough Cloud Kernel convergence:

- O3K Volume canonical state machine;
- selected Cinder-compatible API projection;
- `volumev3` advertised only after portable verification;
- Nova compatibility attachment integration;
- typed storage provider;
- `o3k-storage`;
- local LVM reference backend;
- optional Ceph RBD;
- secret-safe connection information;
- unknown-outcome/attachment cleanup;
- boot-from-volume only after a separate profile.

## Track C — External OpenStack service testbed

### C1. Hosted-service IAM/catalog

- service principals/roles/scopes through O3K IAM;
- Keystone-compatible token validation/catalog projection;
- explicit `external-hosted` ownership;
- disabled/unverified endpoint omission.

### C2. First real external service: Cinder

- selected Cinder version/workflow frozen;
- selected Image compatibility surface;
- selected Compute volume-attachment compatibility;
- typed outbound Cinder client;
- fake external-Cinder failure/compensation matrix;
- focused client/Tempest evidence;
- protected real integration.

The external service keeps its own database/message bus/processes/backend/
migrations/upgrades/health.

### C3. Additional service-under-test profiles

Each new hosted service requires:

- required compatibility API inventory;
- IAM/service identity model;
- dependency/version declaration;
- fake and real integration gates;
- security/failure/cleanup/claim evidence.

## Track D — Small edge cloud

### D1. Multi-host foundation

- approximately 10–20 hypervisors;
- multi-host capability inventory/capacity;
- scheduling/allocations;
- host enrollment/mTLS/epochs/heartbeat/reconnect/resync;
- failure-safe operation replay/fencing;
- host-aware network binding/cleanup;
- backup/restore/upgrade/rollback/diagnostics;
- authorization/quotas appropriate to selected profile.

### D2. Database and availability

- measured single-controller SQLite limits may support an initial edge profile;
- PostgreSQL adapter/conformance before PostgreSQL support claim;
- multi-controller/HA requires explicit coordination/fencing/failover/database
  evidence.

### D3. Execution-process growth

Only after stable contracts and measured need:

- separate `o3k-network`;
- separate `o3k-storage`;
- CellHV provider conformance;
- richer network/storage provider profiles.

## Track E — First genuinely new O3K-native cloud service

This track begins only after the Cloud Kernel has proved that service
extensibility is real rather than theoretical.

The first service should be chosen for product value and architectural learning,
not for marketing breadth.

Candidate classes may include:

- managed database;
- Kubernetes/container platform;
- AI inference/training;
- DNS/load balancing/object/secrets.

No candidate is committed by this roadmap.

Before implementation, freeze:

- service namespace;
- resource/action vocabulary;
- ownership/authorization;
- quota/limit semantics;
- API contract;
- durable operations;
- provider/external execution model;
- audit/events;
- failure/recovery;
- evidence gate.

Success criterion:

> The service should reuse Cloud Kernel IAM, authorization, ownership,
> operations, audit, and registry primitives instead of constructing a parallel
> cloud framework.

## Track F — Delegated/federated clouds

Do not begin with a generic "multi-cloud provider" abstraction.

Select a concrete target first, then define:

- authority boundary;
- principal/scope mapping;
- resource-ID mapping;
- scheduler responsibility;
- quota/policy ownership;
- desired-state/drift ownership;
- outage/retry/unknown-outcome behavior;
- adoption/import semantics;
- deletion authority;
- evidence.

Possible future targets may include external OpenStack, vSphere/vCenter,
Proxmox, KubeVirt, or public-cloud APIs.

## Footprint roadmap

The minimal control plane retains an approximately 50 MB steady-state target.

Measure separately:

- portable/TestLab `o3kd`;
- `o3kd + o3k-compute`;
- Cloud Kernel convergence overhead;
- hosted-service testbed O3K processes;
- edge profile at supported host count;
- external dependencies.

Cloud Kernel features should be profile-selectable where practical so product
extensibility does not automatically turn the minimal TestLab into a large
platform installation.

## Roadmap governance

- the Cloud OS vision does not replace release evidence;
- OpenStack endpoint count is not progress by itself;
- OpenStack service names do not mandate crate/process boundaries;
- new first-class services must reuse Cloud Kernel contracts;
- new public APIs require specifications/evidence;
- new process boundaries require privilege/failure/locality justification;
- external-hosted and O3K-implemented services remain distinct;
- delegated clouds remain distinct from execution providers;
- PostgreSQL, HA, edge-production, native Cinder, metadata HTTP, federation,
  future service breadth, and footprint guarantees fail closed until their
  profiles pass;
- normative rules live in `docs/NORMATIVE_SOURCES.md`.
