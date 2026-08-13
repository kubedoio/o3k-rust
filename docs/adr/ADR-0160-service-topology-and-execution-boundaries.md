# ADR-0160 — Service topology and execution boundaries

Status: Accepted
Date: 2026-08-04
Supersedes: none
Superseded-by: none
Affected-services: compute, network, storage, image, placement, identity, governance

## Context

O3K is one Rust-native OpenStack-compatible platform serving three product
profiles:

- a surrounding control plane for selected real OpenStack services;
- O3K-owned native Rust service profiles;
- a small edge cloud targeting approximately 10–20 hypervisors.

The project exposes or hosts identity, image, compute, network, volume, and
placement behavior. Early implementation concentrated control-plane concerns in
`o3kd` and delegated privileged libvirt and minimum host-network actions through
`o3k-compute`.

The project needs stable boundaries for:

- Keystone-, Glance-, Nova-, Neutron-, Cinder-, and Placement-compatible
  modules;
- external-hosted OpenStack services versus O3K implementations;
- `o3kd`, `o3k-compute`, future `o3k-network`, and future `o3k-storage`;
- control-plane authority versus host execution;
- protocol adapters, application services, domain rules, store ports, and
  concrete persistence/execution adapters;
- single-node TestLab versus 10–20-hypervisor edge deployments;
- when logical boundaries become separate crates or processes.

Splitting every logical service or capability into a daemon immediately would
create enrollment, heartbeat, protocol, journal, startup, rollback, and
cross-agent failure complexity before the contracts are stable. Keeping all
privileged execution in one agent forever would blur compute, network, and
storage authority.

A second risk is subtler: a modular-looking workspace can still become a
monolith if application services depend directly on SQLite, protobuf-generated
wire models, Axum request/response types, libvirt types, external-service client
models, or filesystem metadata formats. The Rust rewrite must not reproduce the
Go implementation's accumulated coupling under different crate names.

Product-profile claims are normative in ADR-0163 and SPEC-0024. This ADR defines
service, dependency, persistence, and execution topology.

## Decision

### 1. `o3kd` is the initial control-plane process

`o3kd` hosts logically separate OpenStack-compatible modules and owns:

- public API behavior;
- validated authorization context;
- projects, users, roles, services, endpoints, and policy;
- image, server, network, port, volume, and allocation intent where O3K owns the
  declared profile;
- catalog and satellite workflow records for external-hosted profiles;
- scheduling;
- operation identity and state;
- compensation and reconciliation;
- compatibility claims and evidence.

The modules remain independent at application, domain, store, policy, and
contract boundaries. They are not required to be separate processes in the
first releases.

### 2. Dependencies point inward and the domain is canonical

The target dependency direction is:

```text
OpenStack/operator protocol adapters
            |
            v
     application services
            |
            v
   domain + narrow ports
            ^
            |
 concrete adapters
 SQLite / filesystem blobs / provider clients / agent protocol / libvirt
```

The core domain owns canonical durable identities, lifecycle states,
transitions, ownership rules, operation semantics, and invariants for O3K-owned
resources.

Protocol, persistence, and provider representations are translations at the
edges. They must not become competing sources of lifecycle semantics. In
particular:

- OpenStack strings such as `ACTIVE`, JSON envelopes, headers, and microversion
  shapes remain in protocol adapters;
- SQL rows and serialized database values remain persistence representations;
- protobuf messages, provider observations, libvirt XML, and external-service
  client models remain adapter representations;
- application services must use typed domain values rather than free-form
  lifecycle strings where a canonical domain type exists.

The domain must not depend on Axum, SQLx, protobuf-generated clients, libvirt,
external OpenStack service clients, provider-native models, or OpenStack JSON
representations.

### 3. Application services depend on ports, not concrete adapters

Application services may depend on narrow repository/provider/clock/signer/
policy ports required by their use cases. Concrete adapters are selected at the
composition root (`o3kd` or the relevant execution binary).

A service must not require `SqliteStore`, SQLx, a libvirt implementation, an
agent protobuf client, or an external Cinder client as its application-level
contract when the dependency can be represented by a bounded port.

Existing direct adapter dependencies are migration debt, not precedent. They
may be temporarily listed in the machine-readable architecture-boundary
ratchet, but:

- no new exception is added merely to make a feature convenient;
- an exception must name the exact crate/file and follow-up removal scope;
- reducing the exception set is always allowed;
- increasing the exception set requires an explicit architecture review.

PostgreSQL is not implemented merely by introducing repository ports. The ports
exist to keep SQLite-specific assumptions out of application semantics and to
make later adapter conformance possible.

### 4. Durable metadata and blob/execution state have different authority

For O3K-owned control-plane resources, the durable store is authoritative for:

- public resource identity and project ownership;
- desired and observed control-plane state;
- immutable dependency snapshots;
- selected host/backend and provider mappings;
- operation, compensation, and reconciliation state;
- network/IP/port allocation intent;
- Placement inventory/allocation state required for recovery.

Filesystem or backend storage may remain authoritative for bounded bytes and
host-local execution artifacts such as:

- image content and verified caches;
- qcow2 overlays;
- config-drive images;
- console artifacts;
- agent-local journals and ownership manifests;
- backend-native resources and observations.

A JSON file, directory name, cache entry, or host manifest must not silently
become the only source of public control-plane metadata. Host-local manifests
prove execution ownership; they do not authorize tenants or create public O3K
identities.

The current file-backed image/network/Placement implementations may be migrated
incrementally, but new control-plane metadata must not deepen that coupling.

### 5. OpenStack service names are compatibility surfaces

The public catalog uses standard service types such as `identity`, `image`,
`compute`, `network`, `placement`, and `volumev3` where the corresponding
profile is implemented or explicitly hosted.

Catalog ownership distinguishes:

- `o3k-implemented`;
- `external-hosted`.

An external-hosted service remains independent. Catalog registration does not
make it an O3K implementation.

Internal binaries use O3K process names. O3K does not create binaries named
`nova`, `neutron`, `cinder`, or `keystone`.

### 6. Host execution is separated by capability domain

The target execution processes are:

- `o3k-compute`: libvirt/KVM lifecycle, image/overlay realization,
  config-drive, console, and compute observations;
- `o3k-network`: TAP, bridge, DHCP, routing, policy enforcement, binding, and
  network observations;
- `o3k-storage`: O3K-owned volume backend lifecycle, attachment preparation,
  LVM/Ceph execution, and storage observations.

Every activated process uses a typed, versioned, mutually authenticated
contract carrying command, operation, resource, generation, deadline,
idempotency, capability, observation, epoch, and redacted failure data.

`o3k-storage` belongs to the native O3K volume profile. It is not the runtime of
an external-hosted Cinder service.

### 7. Logical separation precedes physical separation

Provider interfaces, state machines, conformance suites, privilege models,
restart semantics, and ownership rules must be accepted before a new daemon is
introduced.

For the first native libvirt alpha:

- `o3k-compute` is separate;
- minimum flat-network execution may remain hosted by `o3k-compute` behind
  `NetworkProvider`;
- native storage remains fake or in-process unless its profile is selected.

A later ADR may activate `o3k-network` or `o3k-storage` when process isolation,
independent scaling, deployment location, or failure containment justifies the
complexity.

The same rule applies to Rust crates. A logical module does not need a new
workspace crate merely because OpenStack gives it a service name. Prefer
internal modules until a compile-time dependency, ownership, testing, or
execution boundary is materially clearer as a separate crate.

### 8. Native Cinder does not block the first ephemeral-root guest

The first real native guest requires:

- Keystone-compatible identity and catalog;
- Glance-compatible image access;
- Placement allocation;
- Nova-compatible server orchestration;
- minimum Neutron-compatible port and flat-network behavior;
- real `o3k-compute` and libvirt/KVM execution.

O3K-owned Cinder-compatible persistent volumes, attachments, and boot from
volume are a later milestone.

### 9. External-service testbeds may progress independently but are not the first-alpha critical path

A real external Cinder testbed may progress before native O3K Cinder. It
requires the selected O3K Identity, catalog, token validation, Glance, Nova
attachment, and optional networking/Placement surfaces.

The external Cinder deployment retains its supported database, message bus,
service processes, backend, migrations, upgrades, and operational ownership.
It does not use `o3k-storage` unless a future explicit integration profile says
so.

Work on an external-hosted profile must not consume or redefine the release
criteria of the native ephemeral-root TestLab. Shared fixes may benefit both
profiles, but the first native alpha remains the release-blocking path unless a
later accepted human decision explicitly changes that priority.

### 10. The Rust rewrite is a behavioral replacement, not an architectural translation

The public Go O3K repository is useful for requirements discovery, route
inventory, failure scenarios, operational lessons, and black-box comparison.
It is not the architecture authority for Rust.

Migration proceeds as:

```text
Go/public-client behavior or operational lesson
-> official OpenStack/public-source verification
-> Rust compatibility/spec/contract requirement
-> Rust domain/application design
-> executable black-box or failure test
-> Rust implementation
```

There is no route-count parity milestone and no requirement to preserve Go
package boundaries, database layout, synchronous control flow, provider wiring,
or process topology. Compatibility breadth expands because a declared user
workflow or accepted product profile needs it, not because the Go repository
contains another handler.

### 11. The edge profile reuses the same boundaries

The target small edge cloud uses:

```text
o3kd
  -> o3k-compute on compute hosts
  -> future o3k-network where independent network execution is justified
  -> future o3k-storage where native storage execution is selected
```

Approximately 10–20 hypervisors is a target profile, not a production claim.
Multi-host inventory, scheduling, fencing, resync, database, failure, upgrade,
backup, policy, network, and storage evidence are required before release.

External OpenStack integration in the edge profile must be decomposed into
specific hosted-service, external-identity, endpoint-registration,
service-consumption, federation, or resource-sharing decisions.

## Consequences

### Positive

- authorization and orchestration remain centralized and auditable;
- provider privileges are explicit and bounded;
- canonical domain state is not duplicated across HTTP, SQL, and provider
  representations;
- SQLite can remain the current implementation without becoming an application
  architecture constraint;
- service APIs can be completed and tested without premature deployment
  complexity;
- external-service testbeds do not erase the native Rust service roadmap;
- network and storage can later become independent failure domains;
- the first native guest is not delayed by persistent-volume or hosted-service
  scope;
- Go O3K remains valuable as behavioral evidence without dictating Rust
  structure;
- the same contracts can grow from TestLab into a small edge profile.

### Negative

- several current crates contain known boundary debt that must be refactored;
- repository traits and adapter mappings add explicit code compared with direct
  SQLite access;
- some file-backed metadata needs migration into durable store authority;
- `o3kd` remains a larger process initially;
- the first compute agent temporarily hosts some network execution;
- later process extraction requires contract-preserving refactoring;
- external-hosted and native workflows need distinct evidence and ownership;
- edge production needs additional operational and database work.

## Rejected alternatives

### One daemon per OpenStack service immediately

Rejected because it reproduces distributed-system complexity before service and
execution contracts are stable.

### One Rust crate per OpenStack service regardless of dependency boundary

Rejected because crate count is not modularity. Internal modules are preferred
until dependency direction or ownership warrants a separate crate.

### Let application services use concrete SQLite or provider adapters directly

Rejected because it makes persistence/execution details part of service
semantics, blocks store conformance, and recreates architectural coupling.

### Keep multiple independent lifecycle representations

Rejected because free-form API strings, SQL values, and provider states must be
mapped to one canonical domain model rather than evolving independently.

### Use filesystem metadata as the control-plane source of truth

Rejected because portable TestLab recovery, multi-host scheduling, transaction
semantics, backup, and later store adapters require explicit durable metadata
authority.

### One permanently privileged all-purpose agent

Rejected because compute, network, and storage have different privileges,
failure domains, ownership, and deployment locations.

### Complete native Cinder before any real VM validation

Rejected because persistent block storage is not required for the first
supported ephemeral-root workflow.

### Treat real external Cinder as the native O3K storage implementation

Rejected because external service hosting and native Rust service ownership are
different product profiles.

### Port Go O3K handler-for-handler before proving a vertical workflow

Rejected because route parity reproduces legacy coupling and delays evidence for
the architecture O3K actually intends to ship.

### Implement every upstream endpoint before integration

Rejected because O3K supports declared profiles, not complete OpenStack parity.

## Required follow-up

- define product profiles in ADR-0163 and SPEC-0024;
- define Keystone trust in ADR-0166 and SPEC-0020;
- define execution invariants in `contracts/execution-boundaries.md`;
- define cross-service workflows in SPEC-0021;
- freeze API operations in SPEC-0022;
- define external Cinder in SPEC-0023;
- define Rust rewrite/convergence sequencing in SPEC-0025;
- maintain a machine-readable architecture-boundary ratchet and run it in
  normal CI;
- consolidate canonical resource identities and lifecycle states into the core
  domain before broad endpoint expansion;
- replace direct `SqliteStore` application dependencies with narrow repository
  ports while keeping SQLite as the supported first adapter;
- migrate control-plane image/network/Placement metadata toward durable store
  authority while retaining files for bounded blobs and host-local artifacts;
- split the public API crate internally by service/adapter concern before
  considering additional service crates;
- require process-level conformance tests for every activated agent;
- keep the native ephemeral-root TestLab as the first-alpha release-blocking
  path;
- require a separate ADR before activating `o3k-network`, `o3k-storage`,
  external Keystone, federation, or multi-controller coordination.
