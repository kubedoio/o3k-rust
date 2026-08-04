# ADR-0160 — Service topology and execution boundaries

Status: Proposed

Date: 2026-08-04

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
- single-node TestLab versus 10–20-hypervisor edge deployments;
- when logical boundaries become separate processes.

Splitting every logical service or capability into a daemon immediately would
create enrollment, heartbeat, protocol, journal, startup, rollback, and
cross-agent failure complexity before the contracts are stable. Keeping all
privileged execution in one agent forever would blur compute, network, and
storage authority.

Product-profile claims are normative in ADR-0163 and SPEC-0024. This ADR defines
only service and execution topology.

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

### 2. OpenStack service names are compatibility surfaces

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

### 3. Host execution is separated by capability domain

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

### 4. Logical separation precedes physical separation

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

### 5. Native Cinder does not block the first ephemeral-root guest

The first real native guest requires:

- Keystone-compatible identity and catalog;
- Glance-compatible image access;
- Placement allocation;
- Nova-compatible server orchestration;
- minimum Neutron-compatible port and flat-network behavior;
- real `o3k-compute` and libvirt/KVM execution.

O3K-owned Cinder-compatible persistent volumes, attachments, and boot from
volume are a later milestone.

### 6. External-service testbeds may progress independently

A real external Cinder testbed may progress before native O3K Cinder. It
requires the selected O3K Identity, catalog, token validation, Glance, Nova
attachment, and optional networking/Placement surfaces.

The external Cinder deployment retains its supported database, message bus,
service processes, backend, migrations, upgrades, and operational ownership.
It does not use `o3k-storage` unless a future explicit integration profile says
so.

### 7. The edge profile reuses the same boundaries

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
- service APIs can be completed and tested without premature deployment
  complexity;
- external-service testbeds do not erase the native Rust service roadmap;
- network and storage can later become independent failure domains;
- the first native guest is not delayed by persistent-volume scope;
- the same contracts can grow from TestLab into a small edge profile.

### Negative

- `o3kd` remains a larger process initially;
- the first compute agent temporarily hosts some network execution;
- later process extraction requires contract-preserving refactoring;
- external-hosted and native workflows need distinct evidence and ownership;
- edge production needs additional operational and database work.

## Rejected alternatives

### One daemon per OpenStack service immediately

Rejected because it reproduces distributed-system complexity before service and
execution contracts are stable.

### One permanently privileged all-purpose agent

Rejected because compute, network, and storage have different privileges,
failure domains, ownership, and deployment locations.

### Complete native Cinder before any real VM validation

Rejected because persistent block storage is not required for the first
supported ephemeral-root workflow.

### Treat real external Cinder as the native O3K storage implementation

Rejected because external service hosting and native Rust service ownership are
different product profiles.

### Implement every upstream endpoint before integration

Rejected because O3K supports declared profiles, not complete OpenStack parity.

## Required follow-up

- define product profiles in ADR-0163 and SPEC-0024;
- define Keystone trust in ADR-0161 and SPEC-0020;
- define execution invariants in `contracts/execution-boundaries.md`;
- define cross-service workflows in SPEC-0021;
- freeze API operations in SPEC-0022;
- define external Cinder in SPEC-0023;
- require process-level conformance tests for every activated agent;
- require a separate ADR before activating `o3k-network`, `o3k-storage`,
  external Keystone, federation, or multi-controller coordination.
