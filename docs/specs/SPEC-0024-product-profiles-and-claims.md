# SPEC-0024 — Product profiles and claims

Status: Proposed

Related decisions and specifications:

- [ADR-0163](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0023](SPEC-0023-external-cinder-service-under-test.md)
- [Machine-readable product profiles](../../compatibility/product-profiles.yaml)

## Purpose

This specification defines the supported O3K product profiles, their claims,
required dependencies, compatibility boundaries, database posture, and evidence
gates.

O3K is one Rust-native OpenStack-compatible platform with multiple deployment
profiles. A feature or measurement from one profile must not be silently
promoted to another.

## Profile A — OpenStack service testbed

### User outcome

A developer or CI system can run a selected real OpenStack service against O3K
without installing a complete DevStack or full OpenStack control plane.

The first declared hosted-service scenario is real external Cinder.

### O3K responsibilities

Depending on the selected hosted service, O3K may provide declared subsets of:

- Keystone-compatible authentication, projects, users, roles, token issuance,
  public token validation, service identity, service catalog, regions, and
  endpoints;
- Glance-compatible image metadata and authenticated content access;
- Nova-compatible server and attachment APIs;
- Neutron-compatible networks, ports, bindings, or other explicitly selected
  satellite behavior;
- Placement-compatible capacity and allocation behavior;
- fake, simulated, or real compute/network execution appropriate to the test.

### External service responsibilities

The hosted service retains its own:

- service code and API implementation;
- supported database and migrations;
- message bus where required;
- scheduler, worker, and service processes;
- backend and backend-specific dependencies;
- upgrades, health, and operational ownership.

Catalog registration uses `ownership_mode: external-hosted`. It must not imply
that O3K implements the external service.

### Acceptance

The profile is accepted only when:

- the selected external service version is pinned;
- its required O3K satellite operations are frozen in compatibility manifests;
- service-user authentication and public token validation pass;
- catalog endpoint discovery passes;
- selected client or Tempest-compatible workflows pass;
- failure boundaries identify O3K versus the external service precisely;
- secrets and connector information are redacted;
- cleanup covers O3K-owned and explicitly managed external test resources.

## Profile B — Native Rust OpenStack-compatible cloud

### User outcome

O3K provides its own Rust-native implementation of a declared OpenStack service
profile through standard OpenStack clients and public APIs.

### Native service direction

The native roadmap includes declared subsets of:

- Keystone-compatible identity;
- Glance-compatible image;
- Nova-compatible compute;
- Neutron-compatible networking;
- Placement-compatible capacity and allocations;
- Cinder-compatible volumes and attachments.

A service route is not sufficient evidence. Every supported operation requires
specification, executable contracts, implementation, policy, durable state,
portable evidence, and the real-host evidence appropriate to the operation.

### First native milestone

The first native real-cloud milestone is an ephemeral-root libvirt TestLab:

```text
authenticate
-> upload image
-> create network/subnet/port
-> create flavor/keypair
-> allocate compute resources
-> create and boot server
-> inspect console and lifecycle
-> restart and reconcile
-> delete and prove cleanup
```

Native Cinder-compatible persistent volumes are a later profile and do not block
this first guest.

## Profile C — Small edge cloud

### User outcome

An operator can run O3K as a lightweight control plane for approximately 10–20
hypervisors and optionally integrate selected external OpenStack services.

The host-execution topology is:

```text
o3kd
  -> o3k-compute
  -> future o3k-network
  -> future o3k-storage
```

Logical `ComputeProvider`, `NetworkProvider`, and `StorageProvider` boundaries
are required before process extraction.

### Required edge capabilities

An edge release claim requires evidence for at least:

- multi-host inventory, scheduling, and Placement allocations;
- host enrollment, identity, epoch fencing, heartbeat, reconnect, and resync;
- no duplicate mutation after retry, restart, or network interruption;
- host-local compute, network, and selected storage ownership;
- project isolation and policy;
- upgrades, backup/restore, diagnostic and recovery procedures;
- resource and latency budgets;
- failure and cleanup behavior across the supported host count;
- a database profile appropriate to the claimed availability and concurrency.

### Interoperation with existing OpenStack environments

“Connect to another OpenStack” is not a single capability. The following are
separate profiles or decisions:

- external-hosted services registered in O3K's catalog;
- O3K using an external Keystone;
- O3K registering endpoints into an external Keystone;
- consuming external Glance, Cinder, Neutron, or Placement services;
- federation, project mapping, or resource sharing across clouds.

No broad cross-cloud compatibility claim is permitted without operation-level
contracts, trust boundaries, failure semantics, and executable evidence.

## Database profiles

### SQLite

SQLite is the currently supported default database for the minimal TestLab and
portable simulated-cloud profiles.

Support requires:

- foreign keys;
- bounded busy timeout;
- reviewed WAL and synchronous policy;
- deterministic migrations;
- concurrent API/reconciler tests;
- crash/restart tests;
- documented backup, restore, checkpoint, and filesystem requirements.

A single-controller edge profile may use SQLite only within measured and
published limits.

### PostgreSQL

PostgreSQL is the intended database for production-oriented, stronger
availability, or multi-controller profiles.

It is not supported merely because the architecture mentions it. Support
requires:

- a real adapter and store-conformance suite;
- migrations and upgrade/rollback behavior;
- transaction and isolation semantics;
- backup/restore and operational documentation;
- process and failure tests;
- release artifacts that identify PostgreSQL as verified.

Until these gates pass, user-facing text must say `planned` or
`production-profile target`, not `supported` or `recommended installation`.

## Footprint claims

The minimal O3K control plane targets approximately 50 MB steady-state memory.
This target is profile-specific and evidence-backed.

Every published footprint artifact records:

- product profile;
- exact O3K processes included;
- external processes excluded or reported separately;
- binary or bundle size separately from RSS;
- source commit, Rust toolchain, build profile, and features;
- host CPU, memory, kernel, filesystem, and virtualization details;
- idle and workload phases;
- measurement duration and method.

The following must not be hidden inside an O3K-only number:

- PostgreSQL;
- RabbitMQ;
- external Cinder services;
- libvirt daemon;
- QEMU guests;
- Ceph, LVM, or other storage backends;
- external identity or networking services.

## Product-profile manifest

`compatibility/product-profiles.yaml` is the machine-readable profile registry.
Each profile records:

- profile ID and maturity;
- user outcome;
- O3K-owned services;
- external-hosted services;
- required database posture;
- required execution boundaries;
- compatibility and evidence dependencies;
- release-claim state;
- footprint-claim state.

The manifest is descriptive until CI validation and release-claim enforcement
are implemented. It must never promote planned work to verified support.

## Claim states

Claims use independent states:

```text
planned
specified
implemented
portable-verified
component-real-host-verified
full-profile-verified
release-claimed
```

Database, footprint, service, microversion, metadata, edge-scale, and external
integration claims are tracked independently.

## Required product wording

A valid concise description is:

> O3K is a lightweight, Rust-native OpenStack-compatible control plane for
> reproducible OpenStack service testbeds, progressively native Rust cloud
> services, and small edge clouds.

The following standalone claims are invalid without qualifying evidence:

- “O3K supports all of Gazpacho.”
- “O3K replaces Cinder.”
- “O3K runs in 50 MB.”
- “PostgreSQL is supported for production.”
- “O3K connects to any OpenStack cloud.”

## Release-gate requirements

Every release identifies:

- included product profile or profiles;
- O3K-implemented versus external-hosted services;
- database support state;
- exact compatibility operations and microversions;
- component and full-profile evidence;
- footprint measurements or explicit absence of them;
- known limitations and unsupported integrations.
