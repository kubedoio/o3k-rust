# Product Requirements

## Product boundary

O3K is a lightweight, open, Rust-native **Cloud Operating System**.

Its canonical internal architecture is the O3K Cloud Kernel. OpenStack remains
a first-class compatibility surface and reference ecosystem, not the internal
service topology that O3K must reproduce.

The normative product/architecture decisions are:

- [ADR-0165 — O3K Cloud Operating System and Cloud Kernel](adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility](adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0163 — deployment/evidence profiles](adr/ADR-0163-product-profiles-and-deployment-posture.md)

The primary OpenStack compatibility reference remains OpenStack 2026.1
Gazpacho.

Compatibility is declared per service, operation, API version, microversion,
extension, profile, and evidence state.

## Current maturity boundary

The architecture is broader than the currently released functionality.

The current release direction is still:

> O3K v0.2.0-alpha.1 — Rust-native OpenStack-compatible libvirt TestLab alpha.

This document must not be read as a claim that the full Cloud Kernel,
production HA, PostgreSQL, native Cinder, federation, managed databases,
Kubernetes, AI/ML, or other future services are already implemented.

## Cloud Kernel requirements

All first-class O3K services converge on shared kernel contracts instead of
reimplementing common cloud plumbing.

### IAM and principals

Required architectural behavior:

- one canonical principal model;
- one canonical `AuthContext`;
- explicit user and service-principal identity;
- strict ID versus display-name separation;
- bounded credential expiry and fail-closed validation;
- original actor plus service identity for delegated work;
- no raw token/password/key material in logs, traces, metrics, or evidence.

The current alpha may expose only the selected Keystone-compatible password/
project subset.

### Authorization

Every protected operation maps to:

```text
Principal × Action × Resource × Context -> Allow | Deny
```

Required behavior:

- default deny;
- stable service/action IDs;
- stable resource type/ID;
- owner/security scope recorded durably for O3K-owned tenant resources;
- ownership authorization before provider dispatch;
- no cross-tenant existence disclosure through idempotency;
- service-principal permissions explicit and least-privilege capable;
- compatibility role/policy names translated at protocol boundaries.

### Resource model

Every O3K-owned public resource that participates in authorization/recovery has:

- stable O3K ID;
- resource type;
- owner/security scope;
- desired state;
- observed state where applicable;
- operation/reconciliation identity where mutable;
- provider/external mapping where applicable;
- region/AZ metadata where applicable;
- bounded tags/attributes only when selected by a profile.

Provider-native IDs are not public O3K identity.

### Service registry

The internal service registry is designed to describe:

- stable service identity/namespace;
- ownership mode;
- API/version surfaces;
- regions/endpoints;
- resource types;
- action vocabulary;
- bounded capabilities;
- health/readiness;
- evidence/claim state where needed.

The Keystone catalog is the OpenStack-compatible projection of selected
verified endpoint data.

A public dynamic O3K-native service-registry API is not required by the first
alpha.

### Quotas and limits

New services should consume a common quota/limit vocabulary and enforcement
contract where quotas are part of the selected product profile.

The first implementation may support only the limits required by current
services.

A service must not create a permanently incompatible quota system without an
accepted exception.

### Durable operations and reconciliation

Every mutation that can cross a failure boundary has:

- stable operation ID;
- deterministic command/idempotency identity where required;
- intent persisted before side effects;
- explicit phase/state;
- unknown-outcome semantics;
- bounded retry;
- observation before potentially duplicating mutation;
- compensation/cleanup;
- restart reconciliation;
- foreign-state protection;
- audit/evidence identity.

### Audit and events

The common model must make it possible to answer:

```text
who did what to which resource, in which scope, through which service,
under which request/operation, and with what authorization result?
```

Service-local logs do not replace this identity model.

### Regions and availability domains

Services and schedulers use stable common region/AZ identities where the
selected profile supports them.

OpenStack region/AZ representations are compatibility projections.

## OpenStack compatibility requirements

The compatibility mapping is:

```text
Keystone  -> O3K IAM
Glance    -> O3K Image
Nova      -> O3K Compute
Neutron   -> O3K Network
Placement -> O3K Capacity/Placement
Cinder    -> O3K Volume
```

The O3K domain must not depend on OpenStack JSON envelopes, HTTP headers,
microversion request types, legacy policy-file representation, or service
process topology.

Public compatibility adapters may depend on those contracts.

## Profile A — OpenStack service testbed

### Required outcome

A developer/CI system can start O3K and attach a selected real OpenStack service
to the declared O3K compatibility surfaces without installing a complete
DevStack/full OpenStack control plane.

The first planned hosted-service scenario is external Cinder.

### Required O3K capabilities

Depending on the selected scenario:

- O3K IAM projected through the Keystone-compatible service/user/project/role/
  token/catalog subset;
- public token validation;
- selected Glance-compatible image operations;
- selected Nova-compatible attachment operations;
- optional selected Neutron/Placement behavior;
- typed external-service client where O3K orchestrates part of the workflow;
- exact user/scope audit context plus authenticated service principal;
- durable operations/compensation where O3K owns workflow state;
- stateful fake external-service tests before real integration;
- source-bound client/Tempest-compatible evidence.

### External-service boundary

The external service retains its own:

- database/migrations;
- message bus where required;
- API/scheduler/workers;
- backend;
- upgrade/health/operational lifecycle.

O3K must not claim those dependencies are replaced.

## Profile B — Native O3K TestLab / Cloud

### Required user journey

Given a supported Linux host with QEMU/KVM/libvirt, a user can:

1. install/start O3K with SQLite;
2. retrieve generated credentials safely;
3. discover the selected OpenStack compatibility catalog;
4. authenticate through the Keystone-compatible API;
5. create/list/show/delete image metadata and upload image content;
6. create/list/show/delete a flat network, subnet, and port;
7. create/list/show/delete a flavor and keypair;
8. create/list/show/start/stop/reboot/delete a server;
9. observe capacity allocation, port binding, provider execution, and final
   resource state;
10. retrieve real bounded guest console output;
11. restart `o3kd`, `o3k-compute`, and libvirt without losing/duplicating the
    instance;
12. delete and clean all O3K-owned state;
13. run documented install/diagnostic/reset/uninstall workflows.

Native volumes are later and do not block the first guest.

### Current IAM compatibility subset

- selected Keystone v3 password authentication;
- stable ID/name separation;
- project-scoped compatibility token;
- one normalized O3K `AuthContext`;
- explicit policy declaration;
- minimal catalog containing only implemented/enabled profiles;
- service identity where selected;
- explicit expiry/validation;
- no secret logging.

### Images

- metadata create/list/show/delete;
- authenticated content upload/download;
- checksum/format/size/visibility/activation rules;
- inactive/missing image rejection before compute dispatch;
- no control-plane host path in public/provider contracts.

### Networking

First alpha:

- flat provider network;
- IPv4 subnet/allocation/gateway/DNS validation;
- durable port/MAC/fixed-IP/project/host-binding intent;
- deterministic realization/cleanup;
- stateful fake provider;
- real TAP/bridge/DHCP/binding/connectivity evidence.

Not in first alpha:

- routers;
- floating IP;
- security groups;
- tenant VLAN/VXLAN;
- OVS/OVN.

### Capacity/Placement

- compute resource providers;
- VCPU/MEMORY_MB/DISK_GB inventory;
- generation-protected inventory/allocations;
- deterministic host selection;
- insufficient-capacity rejection before dispatch;
- allocation retention for unknown outcomes;
- exact release after terminal failure/deletion.

### Compute

- flavors/keypairs;
- server lifecycle through typed execution boundary;
- immutable dependency snapshots;
- libvirt/KVM through `o3k-compute` and local `qemu:///system`;
- image/base/overlay/config-drive realization;
- config-drive/cloud-init as first-alpha metadata mechanism;
- bounded console;
- persisted desired/observed state;
- idempotent lifecycle/no duplicate provider resource;
- CellHV optional later execution provider.

### Native volumes/storage

A later O3K Volume profile requires:

- Cinder-compatible selected volume/type/attachment operations;
- canonical O3K Volume state independent from Cinder wire models;
- `StorageProvider`/`o3k-storage` conformance;
- local LVM reference backend before optional Ceph RBD;
- secret-safe connection information;
- timeout/unknown-outcome recovery;
- Nova compatibility projection for attach/detach where selected.

Boot-from-volume requires a separate verified profile.

## Profile C — Small edge cloud

### Target scale

Initial target: approximately 10–20 hypervisors.

This is a target profile, not a production claim.

### Required capabilities

- multi-host capability inventory/capacity/scheduling;
- host enrollment/mTLS/epoch fencing/heartbeat/reconnect/resync;
- typed compute/network/storage execution boundaries;
- restart/network-partition recovery without duplicate mutation;
- backup/restore/upgrade/diagnostic/rollback;
- security-scope isolation/authorization;
- quotas/limits where claimed;
- measured performance/footprint/failure behavior;
- database profile suitable for claimed concurrency/availability.

## Future first-class O3K services

The Cloud Kernel is intentionally designed to support new service domains
without requiring OpenStack to already define that service.

Possible future examples include:

- managed database;
- Kubernetes/container platform;
- AI/ML inference/training;
- load balancing;
- DNS;
- object storage;
- secrets;
- other operator-selected services.

These examples are not support commitments.

Before a new service becomes first-class it must define:

- namespace;
- resource types;
- action vocabulary;
- ownership model;
- API surface(s);
- quota/limit semantics where applicable;
- operation/reconciliation model;
- provider/external dependencies;
- audit/events;
- security threat model;
- compatibility mapping if an external standard/OpenStack service exists;
- portable and real evidence gates.

The service should reuse shared kernel contracts instead of creating a parallel
identity/policy/operation framework.

## Service development objective

One long-term product measure is the cost of adding a new service.

A mature O3K service foundation should make the following mostly shared:

- authentication;
- authorization;
- principal/service identity;
- resource ownership;
- quota/limit hooks;
- operation IDs;
- idempotency/reconciliation primitives;
- audit/events;
- service registration/discovery;
- standard health/readiness;
- common error categories;
- metrics/trace identity.

The service developer should primarily implement the service's domain.

This is a target developer experience, not a current generator/SDK claim.

## Infrastructure execution requirements

For O3K-owned resources, infrastructure providers are bounded executors.

They must not:

- authorize users/tenants;
- invent O3K public IDs;
- silently change desired state;
- reschedule outside an accepted O3K operation;
- delete ambiguous/foreign resources.

Capabilities must be typed, versioned, and evidence-backed.

## Existing-cloud integration requirements

Another OpenStack cloud, vSphere/vCenter, Proxmox, KubeVirt, or public cloud is
not treated as an ordinary execution provider by default.

Each delegated/federated connector requires:

- authority model;
- principal/scope mapping;
- resource-ID mapping;
- desired-state ownership;
- scheduler responsibility;
- quota/policy responsibility;
- drift semantics;
- outage/retry/unknown-outcome semantics;
- import/adoption rules;
- cleanup/deletion authority;
- security/evidence profile.

A generic "connects to everything" claim is forbidden.

## Database requirements

### SQLite

Currently supported default for minimal TestLab/portable profiles.

Required evidence:

- WAL/synchronous policy;
- foreign keys;
- bounded busy timeout;
- deterministic migrations;
- concurrent API/reconciler writers;
- crash/restart/checkpoint/backup/restore;
- documented local-filesystem requirements/limits.

### PostgreSQL

Intended production-oriented/stronger-availability target.

Not supported/recommended for production until there is:

- real adapter;
- conformance suite;
- migrations/upgrade/rollback;
- transaction/isolation decision;
- backup/restore;
- process/failure evidence.

## Footprint requirements

The minimal O3K control plane targets approximately 50 MB steady-state memory.

Every number identifies:

- exact profile;
- included O3K processes;
- binary/bundle size separately from RSS;
- source/toolchain/build/features;
- host/kernel/workload/measurement method;
- external dependencies separately.

## Development/evidence requirements

Before privileged full-profile testing:

1. ADR/spec/contract validation;
2. machine-readable profile/API validation;
3. domain/store/migration/policy tests;
4. provider/external-service conformance;
5. portable simulated-profile integration using real HTTP/auth/store/scheduler/
   operations/reconciliation;
6. process-level public-client tests.

Privileged evidence is staged:

- compute component;
- network component;
- native storage component where selected;
- external-hosted service where selected;
- full native/edge profile;
- restart/failure matrix;
- release gate.

## First-alpha exit criteria

The first native alpha is ready only when evidence proves the already-declared:

- identity/image/network/capacity/compute compatibility profile;
- portable success/failure/restart/compensation/duplicate/unknown-outcome paths;
- real image/qcow2;
- config-drive guest consumption;
- real owned libvirt guest;
- expected port/MAC/IP networking;
- real bounded console;
- lifecycle/service/libvirt restart without duplication;
- complete owned cleanup and unchanged foreign state;
- clean install/reproduction;
- measured compatibility/latency/footprint artifacts;
- SBOM/checksums/provenance/known limitations/human approval/signed artifacts.

Cloud Kernel expansion beyond what that journey requires does not block the
release.

## Compatibility and claim policy

Every supported behavior records, as applicable:

- deployment/evidence profile;
- ownership mode;
- official external reference;
- request/response/discovery/error contract;
- auth action/scope/ownership policy;
- supported fields/version/microversion;
- state/dependencies/compensation;
- database/execution requirements;
- known deviations;
- portable/component/full-profile/release evidence.

Unsupported behavior must not be advertised.

Architecture intent alone does not prove production Cloud OS readiness,
PostgreSQL, native Cinder, metadata HTTP, federation, future service breadth, or
the 50 MB target.
