# ADR-0160 — Service topology and execution boundaries

Status: Proposed

Date: 2026-08-04

## Context

O3K exposes OpenStack-compatible identity, image, compute, network, volume, and
placement behavior. Early implementation concentrated these concerns in
`o3kd` and delegated privileged libvirt and host-network actions through
`o3k-compute`.

The project now needs a stable direction for:

- the logical relationship between Keystone, Glance, Nova, Neutron, Cinder,
  and Placement-compatible modules;
- the process names `o3kd`, `o3k-compute`, `o3k-network`, and `o3k-storage`;
- which state and policy are authoritative in the control plane;
- which work may run with host privileges;
- whether Cinder is required before the first real guest;
- when logical boundaries become separate deployable processes.

Splitting every OpenStack-compatible module into a daemon immediately would
create several enrollment flows, heartbeats, journals, startup dependencies,
and cross-agent compensation paths before their contracts are stable. Keeping
all privileged execution inside a single compute process forever would instead
blur compute, network, and storage ownership.

## Decision

### 1. `o3kd` is the initial control-plane process

`o3kd` hosts the logical OpenStack service modules and owns:

- public API behavior;
- validated authorization context;
- projects, users, roles, services, endpoints, and policy;
- image, server, network, port, volume, and allocation intent;
- scheduling;
- operation identity and state;
- compensation and reconciliation;
- compatibility claims and evidence.

The service modules remain independent at application, domain, store, and
contract boundaries. They are not required to be independent processes in the
first releases.

### 2. Host execution is separated by capability domain

The target execution processes are:

- `o3k-compute`: libvirt/KVM domain lifecycle, image/overlay realization,
  config-drive attachment, console, and compute observations;
- `o3k-network`: TAP, bridge, DHCP, routing, policy enforcement, binding, and
  network observations;
- `o3k-storage`: volume backend lifecycle, attachment preparation, LVM/Ceph
  execution, and storage observations.

Every process uses a typed, versioned, mutually authenticated provider
contract. The contract carries command identity, operation identity, resource
identity, deadline, idempotency identity, capabilities, observations, and
redacted errors.

### 3. Logical separation precedes physical separation

The provider interfaces, state machines, conformance suites, and privilege
models must be accepted before a new daemon is introduced.

For the first libvirt alpha:

- `o3k-compute` is a separate process;
- minimum flat-network execution may remain hosted by `o3k-compute` behind the
  `NetworkProvider` contract;
- storage remains a fake or in-process provider unless the Cinder milestone is
  explicitly selected.

A later ADR may activate `o3k-network` or `o3k-storage` as separate daemons when
there is evidence that process isolation, independent scaling, or failure
containment justifies the operational complexity.

### 4. Cinder does not block the first ephemeral-root guest

The first real VM milestone requires:

- Keystone-compatible identity and catalog;
- Glance-compatible image access;
- Placement allocation;
- Nova-compatible server orchestration;
- the minimum Neutron-compatible port and flat-network path;
- real `o3k-compute` and libvirt/KVM execution.

Cinder-compatible persistent volumes, volume attachments, and boot-from-volume
are a separate milestone. Cinder design may proceed in parallel, but missing
Cinder behavior must not block an ephemeral qcow2-root guest.

### 5. OpenStack service names are compatibility surfaces, not process names

The public catalog continues to advertise standard service types such as
`identity`, `image`, `compute`, `network`, `placement`, and `volumev3` where the
corresponding compatibility profile is implemented.

Internal binaries use O3K process names. O3K does not create binaries named
`nova`, `neutron`, `cinder`, or `keystone`.

## Consequences

### Positive

- authorization and orchestration stay centralized and auditable;
- provider privileges are explicit and bounded;
- service APIs can be completed and tested without premature deployment
  complexity;
- network and storage can later become independent failure domains;
- the first real guest is not delayed by persistent-volume scope;
- fake-provider full-cloud tests can exercise real control-plane logic.

### Negative

- `o3kd` remains a larger process initially;
- the first compute agent temporarily hosts some network execution;
- later process extraction requires careful contract-preserving refactoring;
- cross-service workflows still require explicit compensation even inside one
  process.

## Rejected alternatives

### One daemon per OpenStack service immediately

Rejected because it reproduces distributed-system complexity before the
service contracts and supported API profile are stable.

### One permanently privileged all-purpose agent

Rejected because compute, network, and storage have different capabilities,
failure domains, ownership rules, and future deployment locations.

### Complete Cinder before any real VM validation

Rejected because persistent block storage is not required for the first
supported ephemeral-root Nova workflow and would delay evidence for the core
compute path.

### Implement every upstream endpoint before integration

Rejected because O3K supports a declared profile, not complete OpenStack API
parity. Unsupported behavior must be explicit rather than partially
implemented.

## Required follow-up

- define the Keystone trust model in ADR-0161 and SPEC-0020;
- define typed execution boundaries in `contracts/execution-boundaries.md`;
- define cross-service workflows in SPEC-0021;
- freeze the advertised API baseline in SPEC-0022;
- retain process-level conformance tests for every activated agent;
- require a separate ADR before physically activating `o3k-network` or
  `o3k-storage`.
