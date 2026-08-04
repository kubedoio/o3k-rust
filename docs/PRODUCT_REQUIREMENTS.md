# Product Requirements

## Product boundary

O3K is a lightweight, Rust-native OpenStack-compatible control plane with three
product profiles:

1. **OpenStack service testbed** — run a selected real OpenStack service against
   O3K without deploying a complete DevStack or full OpenStack control plane;
2. **native Rust cloud** — progressively provide O3K-owned Rust implementations
   of declared Keystone, Glance, Nova, Neutron, Placement, and Cinder profiles;
3. **small edge cloud** — operate a lightweight control plane for approximately
   10–20 hypervisors and integrate only explicitly selected external OpenStack
   services.

The normative profile and claim rules are defined in
[SPEC-0024](specs/SPEC-0024-product-profiles-and-claims.md).

O3K advertises only operations that are specified, implemented, and backed by
executable compatibility evidence. The primary compatibility reference is
OpenStack 2026.1 Gazpacho. Compatibility is declared per service, operation,
API version, microversion, and extension.

## Profile A — OpenStack service testbed

### Required outcome

A developer or CI system can start O3K and attach a selected real OpenStack
service to the declared O3K satellite APIs without installing the rest of a
full OpenStack control plane.

The first planned hosted-service scenario is real external Cinder.

### Required O3K capabilities

- durable service project, service user, roles, assignments, services, regions,
  endpoints, and ownership mode;
- project-scoped service-user authentication;
- public token validation through the declared Identity API;
- catalog entries for explicitly enabled external-hosted endpoints;
- the selected Glance-compatible image operations;
- the selected Nova-compatible volume-attachment operations;
- a typed outbound external-service client where O3K orchestrates part of the
  workflow;
- exact user/project audit context plus authenticated service identity;
- stateful fake external-service tests before real integration;
- source-bound external-service, client, and Tempest-compatible evidence.

### External service boundary

The hosted service retains its own supported:

- database and migrations;
- message bus where required;
- API, scheduler, worker, and service processes;
- backend and backend dependencies;
- upgrade and operational lifecycle.

O3K must not claim to implement or eliminate those dependencies.

## Profile B — Native Rust TestLab

### Required user journey

Given a supported Linux host with QEMU/KVM and libvirt, a user can:

1. install and start O3K with SQLite;
2. retrieve generated credentials safely;
3. discover the enabled catalog;
4. authenticate through the Keystone-compatible API;
5. create/list/show/delete image metadata and upload image content;
6. create/list/show/delete a flat network, subnet, and port;
7. create/list/show/delete a flavor and keypair;
8. create/list/show/start/stop/reboot/delete a server;
9. observe Placement allocation, port binding, provider execution, and final
   resource state;
10. retrieve real bounded guest console output;
11. restart `o3kd`, `o3k-compute`, and libvirt without losing or duplicating the
    instance;
12. delete the instance and clean every O3K-owned resource;
13. run documented install, diagnostic, reset, and uninstall commands.

Native Cinder-compatible volumes and `o3k-storage` are later requirements and do
not block the first ephemeral-root guest.

### Identity and trust

- Keystone v3 password authentication for the bootstrap profile;
- durable distinction between IDs and names;
- project-scoped tokens;
- one normalized internal `AuthContext` consumed by every service;
- explicit roles and fail-closed policy declarations;
- minimal catalog containing only implemented and enabled profiles;
- service identity for hosted-service and cross-service workflows;
- explicit token expiry, validation, and declared revocation limitations;
- no token, password, signing key, or private credential logging.

### Images

- metadata create/list/show/delete;
- authenticated content upload/download;
- checksum, format, size, visibility, and immutable activation rules;
- inactive/missing-image rejection before compute dispatch;
- no host path in public or provider contracts.

### Networking

- flat provider network;
- IPv4 subnet, allocation pool, gateway, and DNS validation;
- durable port, MAC, fixed-IP, project, and host-binding intent;
- deterministic execution and cleanup;
- stateful fake provider for portable integration;
- real TAP, bridge, DHCP/binding, and connectivity evidence.

The first profile does not include routers, floating IPs, security groups,
VLAN/VXLAN tenant networks, OVS, or OVN.

### Placement

- compute resource providers;
- VCPU, MEMORY_MB, and DISK_GB inventory;
- generation-protected inventory and allocations;
- deterministic host selection;
- insufficient-capacity rejection before dispatch;
- allocation retention for unknown outcomes and exact release after terminal
  failure or deletion.

### Compute

- flavors and keypairs;
- server lifecycle through a typed execution boundary;
- immutable dependency snapshots;
- libvirt/KVM through `o3k-compute` and local `qemu:///system`;
- image/base/overlay and config-drive realization;
- config-drive/cloud-init as the only first-alpha guest metadata mechanism;
- bounded real console output;
- persisted desired and observed states;
- idempotent lifecycle and no duplicate provider resources;
- CellHV as an optional later provider.

### Native volumes and storage

The native persistent-volume profile requires:

- Cinder-compatible volume types and volume lifecycle;
- Nova attachment and detach operations;
- `StorageProvider` and `o3k-storage` conformance;
- a local LVM reference backend before optional Ceph RBD;
- secret-safe connection information;
- timeout and unknown-outcome recovery.

No boot-from-volume claim is made until separately specified and verified.

## Profile C — Small edge cloud

### Target scale

The initial edge target is approximately 10–20 hypervisors. This is a target
profile, not a current production claim.

### Required capabilities

- multi-host capability inventory, Placement, scheduling, and allocations;
- host enrollment, mTLS identity, epoch fencing, heartbeat, reconnect, and
  resynchronization;
- typed `o3k-compute`, `NetworkProvider`, and `StorageProvider` boundaries;
- restart and network-partition recovery without duplicate mutation;
- backup, restore, upgrade, diagnostics, and rollback procedures;
- project isolation, quotas, policy, and resource ownership;
- profile-specific performance, footprint, and failure evidence;
- a database profile suitable for the claimed concurrency and availability.

### External OpenStack interoperation

Each of the following requires a separate profile and acceptance criteria:

- external-hosted service registration in O3K;
- O3K trusting an external Keystone;
- O3K registering endpoints in an external Keystone;
- consuming external Glance, Cinder, Neutron, or Placement services;
- federation, project mapping, or cross-cloud resource sharing.

A generic “connects to OpenStack” claim is not permitted.

## Database requirements

### SQLite

SQLite is the currently supported default for minimal TestLab and portable
profiles.

Required SQLite evidence includes:

- WAL and synchronous policy;
- foreign keys and bounded busy timeout;
- deterministic migrations;
- concurrent API/reconciler writers;
- crash, restart, checkpoint, backup, and restore behavior;
- documented local-filesystem requirements and limits.

A single-controller edge profile may use SQLite only within measured and
published limits.

### PostgreSQL

PostgreSQL is the intended database for production-oriented, stronger
availability, and possible multi-controller profiles.

It is not supported or recommended for installation until the repository has:

- a real adapter;
- store-conformance tests;
- migrations and upgrade/rollback behavior;
- transaction/isolation decisions;
- backup/restore documentation;
- process and failure evidence.

Until then, PostgreSQL is a planned production-profile target.

## Footprint requirements

The minimal O3K control plane targets approximately 50 MB steady-state memory.
This target is not a release claim without measurement evidence.

Every published number identifies:

- product profile;
- exact included O3K processes;
- binary/bundle size separately from RSS;
- source commit, toolchain, build profile, and features;
- host, kernel, workload phase, duration, and measurement method;
- external dependencies reported separately.

External Cinder, RabbitMQ, PostgreSQL, libvirt, QEMU guests, Ceph, LVM, and other
hosted dependencies must not be hidden in an O3K-only number.

## Operations and reconciliation

- operation ID for every mutation;
- deterministic command and idempotency identity;
- structured state, phase, and error information;
- intent persisted before external side effects;
- reverse-order compensation;
- reconciliation after control-plane, agent, or external-service restart;
- unknown outcomes observed before mutation retry;
- bounded retries and visible terminal failure;
- no adoption or deletion of ambiguous or foreign resources.

## Development and evidence requirements

Before privileged full-profile testing, the selected profile passes:

1. ADR/spec/contract validation;
2. machine-readable product and API profile validation;
3. domain, store, migration, and policy tests;
4. stateful provider or external-service conformance;
5. portable simulated-profile integration using real HTTP, auth, stores,
   scheduler, operations, and reconciliation;
6. process-level public client tests.

Privileged evidence is staged by profile:

- compute component gate;
- network component gate;
- native storage component gate where selected;
- external hosted-service gate where selected;
- full native or edge profile gate;
- restart/failure matrix;
- release gate.

The protected runner is a final verifier, not the main source of missing API
requirements.

## First-alpha exit criteria

The first native alpha is ready only when evidence proves:

- the declared identity, image, network, placement, and compute profile;
- portable success, failure, restart, compensation, duplicate, and
  unknown-outcome scenarios;
- real image transfer and qcow2 realization;
- real config-drive attachment and guest-side consumption;
- real owned libvirt guest boot;
- expected port/MAC/fixed-IP and networking;
- real bounded console output;
- lifecycle and service/libvirt restart without duplication;
- complete O3K-owned cleanup and unchanged foreign state;
- clean installation and reproduction;
- measured compatibility, latency, and footprint artifacts;
- SBOM, checksums, provenance, known limitations, human approval, and signed
  release artifacts.

External Cinder remains a separately verified testbed profile. Native Cinder and
small-edge production claims remain planned until their own gates pass.

## Compatibility and claim policy

Every supported operation and product claim records:

- product profile;
- ownership mode (`o3k-implemented` or `external-hosted`);
- official public reference;
- request, response, discovery, and error contract;
- auth scope and policy;
- supported fields and microversion;
- state transition, dependencies, and compensation;
- database and execution requirements;
- known deviations;
- portable, component, full-profile, and release evidence.

Unsupported behavior must not be advertised. Architecture intent alone does not
prove PostgreSQL, edge-production, native Cinder, metadata HTTP, cross-cloud, or
50 MB claims.
