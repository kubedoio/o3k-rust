# Architecture

## Architectural direction

O3K is a lightweight, Rust-native OpenStack-compatible control plane built as a
modular monolith first, with explicit logical service, provider, process, and
product-profile boundaries.

The architecture supports three product profiles:

1. **OpenStack service testbed** — O3K supplies selected surrounding OpenStack
   APIs to a real independently running service such as Cinder;
2. **native Rust cloud** — O3K implements declared Keystone-, Glance-, Nova-,
   Neutron-, Placement-, and later Cinder-compatible behavior itself;
3. **small edge cloud** — O3K operates a lightweight control plane for
   approximately 10–20 hypervisors and may integrate explicitly selected
   external OpenStack services.

The profile definitions, database posture, footprint target, and claim rules
are normative in [ADR-0163](adr/ADR-0163-product-profiles-and-deployment-posture.md)
and [SPEC-0024](specs/SPEC-0024-product-profiles-and-claims.md).

Endpoint count is not an architectural goal. O3K advertises only frozen,
executable compatibility profiles.

## Normative ownership

This document is an overview. Field-level identity semantics, workflow phases,
compensation order, compatibility operations, product claims, and execution
protocol invariants are normative only in the sources listed in
[`docs/NORMATIVE_SOURCES.md`](NORMATIVE_SOURCES.md).

Runtime behavior and release claims require executable evidence. Documentation
is not implementation proof.

## Architectural layers

```text
Protocol adapters
  OpenStack-compatible HTTP, operator API, selected external-service clients
        |
Application services
  identity, image, compute, network, volume, placement
  commands, queries, policy, scheduling, orchestration
        |
Domain
  identities, ownership, state machines, operations, compensation
        |
Ports
  store, clock, signer, policy, provider and external-service contracts
        |
Adapters
  SQLite, future PostgreSQL, stateful fakes, agent clients, service clients
        |
Execution boundaries
  libvirt/KVM, Linux networking, LVM/Ceph, optional CellHV
```

Dependencies point inward. The domain does not depend on Axum, SQLx,
protobuf-generated clients, libvirt bindings, external-service client models,
or OpenStack JSON representations.

## Control-plane topology

`o3kd` is the initial control-plane process.

```text
OpenStack CLI / SDK / Terraform / external OpenStack service
                            |
                          o3kd
                            |
       identity | image | compute | network | volume | placement
                            |
          policy | scheduling | operations | reconciliation
```

`o3kd` owns:

- public OpenStack-compatible behavior;
- validated authentication and authorization context;
- durable projects, users, roles, services, endpoints, resources, and
  operations;
- desired state and immutable dependency snapshots;
- Placement, scheduling, compensation, and reconciliation;
- public errors, compatibility profiles, release claims, and evidence links;
- mappings between O3K identities and provider-native identities.

The logical services are separate at application, domain, store, policy, and
contract boundaries. They are not required to be separate processes in early
releases.

## Keystone-compatible trust root

Identity is the common trust and service-discovery root, but it is not the
transaction coordinator for servers, ports, volumes, images, or allocations.

Keystone-compatible responsibilities include:

- domains, projects, users, groups, roles, and assignments;
- password authentication and token issuance;
- public token validation for declared hosted-service profiles;
- a normalized typed `AuthContext`;
- service users and service projects;
- services, regions, interfaces, endpoints, and catalog generation;
- expiry, declared revocation behavior, audit identity, and redaction.

Every service consumes one validated authorization context. Service-specific
request models must not reinterpret identity independently.

The bootstrap identity profile is intentionally smaller than the hosted-service
profile. A catalog entry is published only when its ownership and evidence are
explicit.

See [ADR-0161](adr/ADR-0161-keystone-trust-and-service-identity.md),
[SPEC-0020](specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md), and
[SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md).

## Native OpenStack-compatible modules

When O3K owns a service profile, `o3kd` hosts logically separate modules for:

- **Identity / Keystone-compatible** — trust, catalog, policy context;
- **Image / Glance-compatible** — metadata, content authorization, activation;
- **Compute / Nova-compatible** — flavors, keypairs, servers, actions, console,
  attachment orchestration;
- **Network / Neutron-compatible** — networks, subnets, ports, addressing,
  binding intent;
- **Placement-compatible** — resource providers, inventories, traits,
  allocations, generations;
- **Volume / Cinder-compatible** — later O3K-owned volumes, types, attachments,
  backend selection, and storage observations.

Native Cinder-compatible storage is part of the long-term Rust OpenStack goal.
It is independent from hosting a real external Cinder service.

## External-hosted service profiles

An external-hosted service remains an independently operated OpenStack service.
O3K may provide the selected satellite APIs and catalog records required by its
workflow.

For a real external Cinder testbed:

- O3K owns the declared Identity, catalog, token-validation, Glance, Nova, and
  optional networking/Placement compatibility surfaces;
- the catalog marks the Cinder endpoint `external-hosted`;
- Cinder owns its API, database, RabbitMQ or supported message bus, scheduler,
  workers, volume services, backend, migrations, upgrades, and health;
- O3K owns only its catalog records, satellite workflow state, attachment
  orchestration, and explicitly managed test resources.

An external endpoint must never be presented as an O3K implementation.

See [SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md).

## Host execution boundaries

The target host-local process names are:

```text
                     versioned mTLS contracts
                              |
          +-------------------+-------------------+
          |                   |                   |
          v                   v                   v
    o3k-compute          o3k-network         o3k-storage
    libvirt/KVM          TAP/bridge/DHCP      LVM/Ceph RBD
    image/overlay        routing/policy       volume lifecycle
    config-drive         binding observation  attachment prep
    console              network observation  storage observation
```

`o3k-compute` is the only mandatory separate execution process for the first
libvirt alpha. Minimum network execution may remain behind `NetworkProvider`
inside `o3k-compute`. Native storage may remain fake or in-process until its
profile is selected.

Logical separation precedes physical separation. A new daemon requires an
accepted decision for:

- privilege and deployment location;
- identity, enrollment, mTLS, heartbeat, and epoch fencing;
- protocol and versioning;
- command journal and idempotency;
- reconnect, resync, restart, and unknown outcome;
- ownership, cleanup, and failure containment.

See [ADR-0160](adr/ADR-0160-service-topology-and-execution-boundaries.md) and
[execution-boundary contracts](../contracts/execution-boundaries.md).

## Resource authority

O3K is authoritative for:

- public O3K/OpenStack-compatible identities and representations;
- user, project, service, policy, quota, and catalog state;
- desired state and immutable request snapshots;
- operation identities, workflow phases, compensation, and reconciliation;
- scheduling and Placement decisions;
- mappings to provider or external-service identities;
- compatibility and release claims.

Execution agents are authoritative only for bounded local capabilities,
provider-native identifiers, existence, observed state, and redacted provider
failures.

External-hosted services remain authoritative for their own public APIs,
internal state, database, messaging, workers, backend resources, migrations,
and service-specific recovery.

No agent or external service may silently create O3K public identities,
authorize tenants, or rewrite O3K desired state.

## Operation and compensation model

A mutating workflow normally:

1. validates identity, policy, schema, quota, dependency state, and profile
   support;
2. snapshots immutable inputs and persists desired state;
3. creates an operation identity and required allocations;
4. persists the workflow phase before each side effect;
5. dispatches one typed provider or external-service action;
6. observes the result;
7. persists convergence, compensation, unknown outcome, or terminal failure;
8. emits source-bound audit and evidence.

A timeout is an unknown outcome. Reconciliation observes before retrying a
mutation that could duplicate or destroy resources.

Cross-service workflows are normative in
[SPEC-0021](specs/SPEC-0021-cross-service-workflows-and-compensation.md).

## Product-profile architectures

### Portable simulated cloud

- one `o3kd` process;
- real HTTP, identity, stores, scheduling, operations, and reconciliation;
- SQLite;
- stateful fake compute, network, storage, and external-service providers;
- no privileged host mutation.

This is the primary fast integration profile.

### Native libvirt TestLab

- `o3kd` plus `o3k-compute`;
- local `qemu:///system`;
- image cache, qcow2 overlay, config-drive/cloud-init, console, and minimum flat
  networking;
- O3K-owned Identity, Image, Compute, Network, and Placement profiles;
- native persistent volumes are optional and later.

### External OpenStack service testbed

- `o3kd` provides declared satellite APIs and catalog;
- the selected real OpenStack service runs independently;
- external dependencies are explicit;
- fake-service process tests precede protected real-service integration;
- external-hosted claims are separate from O3K-native service claims.

### Small edge cloud

- approximately 10–20 hypervisors;
- one or more explicitly supported control-plane nodes;
- multi-host inventory, Placement, scheduling, fencing, reconnect, and resync;
- host-local compute and declared network/storage execution;
- backup, restore, upgrade, rollback, diagnostics, and measured operations;
- external OpenStack integration only through explicit profiles.

The target host count does not itself prove production readiness. The selected
database, availability, failure, network, storage, policy, and operational gates
must pass.

## Database architecture

SQLite is the currently supported default for minimal TestLab and portable
profiles. Its architecture includes explicit WAL/concurrency policy, bounded
lock handling, migrations, crash recovery, backup/restore, and filesystem
constraints.

PostgreSQL is the intended production-oriented and stronger-availability
profile. It is not supported merely because it appears as a port or roadmap
item. Support requires an adapter, conformance suite, migrations, transaction
semantics, backup/restore, and failure evidence.

A single-controller edge profile may use SQLite only within measured and
published limits. Multi-controller or HA claims require a separate coordination
and database decision.

## Footprint architecture

The minimal O3K control plane targets approximately 50 MB steady-state memory.
The number is a profile-specific target until measured.

Published measurements separate:

- `o3kd`;
- `o3k-compute`, future network, and future storage agents;
- external Cinder and its database/message bus;
- PostgreSQL;
- libvirt and QEMU guests;
- Ceph, LVM, and other backends.

Binary size, release bundle size, RSS, CPU, startup time, and lifecycle resource
usage are different metrics and must not be conflated.

## OpenStack interoperation boundaries

“Connect to another OpenStack” is decomposed into separate architectural
profiles:

- host an external service endpoint in O3K's catalog;
- trust an external Keystone;
- register O3K endpoints into an external Keystone;
- consume external Glance, Cinder, Neutron, or Placement services;
- federate identities or map projects;
- share or migrate resources across clouds.

Each requires explicit trust, ownership, outage, retry, policy, and evidence
semantics. No generic interoperability flag is permitted.

## Compatibility and metadata

O3K uses OpenStack 2026.1 Gazpacho as the primary reference and 2025.2 Flamingo
as a backward reference where declared. The upstream maximum is not the O3K
advertised maximum. Only contiguous implemented and verified windows are
advertised.

The first native alpha uses config-drive/cloud-init for guest metadata. An HTTP
metadata service is not advertised without a separate security/networking
profile and executable guest isolation evidence.

See [SPEC-0022](specs/SPEC-0022-service-api-baseline-and-evidence-gates.md).

## Validation architecture

```text
spec and profile validation
  -> domain/store/policy tests
  -> provider and external-service conformance
  -> portable simulated profile
  -> process/public-client tests
  -> compute/network/storage or hosted-service component gate
  -> full native/testbed/edge profile gate
  -> failure/restart matrix
  -> release gate
```

The protected runner is a final integration verifier, not the primary source of
missing requirements. Diagnostic modes retain owned resources only for a
bounded protected interval and always perform ownership-checked cleanup.

## Growth path

1. accept product profiles, trust, compatibility, and execution contracts;
2. prove the portable simulated profiles;
3. complete the native ephemeral-root libvirt TestLab;
4. complete hosted-service Identity and the real external Cinder testbed;
5. implement native Rust Cinder-compatible storage independently;
6. prove multi-host scheduling and operations for the 10–20-hypervisor edge
   profile;
7. add PostgreSQL only after adapter conformance;
8. extract `o3k-network` and `o3k-storage` only after contract and failure
   evidence;
9. add optional CellHV and explicit external-OpenStack integration profiles;
10. add multi-controller or HA behavior only after a separate failure and
    coordination model.
