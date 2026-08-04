# Architecture

## Architectural direction

O3K Rust is an OpenStack-compatible control plane implemented as a modular
monolith first, with explicit service, provider, and process boundaries. The
initial deployment keeps public APIs and orchestration in `o3kd` so that
transactions, authorization, and recovery remain understandable. Privileged
host execution is delegated through versioned contracts.

The architecture deliberately distinguishes:

1. **OpenStack service semantics** — Keystone, Glance, Nova, Neutron, Cinder,
   and Placement-compatible behavior owned by `o3kd`;
2. **provider orchestration** — desired state, operations, scheduling,
   compensation, and reconciliation owned by the control plane;
3. **host execution** — bounded compute, network, and storage mutations owned
   by host-local agents.

Endpoint count is not an architectural goal. O3K advertises only a frozen,
executable compatibility profile.

## Layers

```text
Protocol adapters
  OpenStack HTTP, metadata HTTP, operator API
        |
Application services
  identity, image, compute, network, volume, placement
  commands, queries, authorization, orchestration
        |
Domain
  resource state, invariants, operation state machines
        |
Ports
  store, clock, signer, policy, provider contracts
        |
Adapters
  SQLite/PostgreSQL, fake providers, agent clients
        |
Execution agents
  libvirt/KVM, Linux networking, LVM/Ceph, optional CellHV
```

Dependencies point inward. The domain does not depend on Axum, SQLx,
protobuf-generated provider clients, libvirt bindings, or OpenStack JSON
representations.

## Logical OpenStack services

`o3kd` initially hosts the following logical services behind independent
application and domain boundaries:

- **Identity / Keystone-compatible:** authentication, projects, users, roles,
  token validation, service identity, policy context, catalog, and endpoint
  discovery;
- **Image / Glance-compatible:** image metadata, content authorization,
  checksums, immutable activation, and data-plane references;
- **Compute / Nova-compatible:** flavors, keypairs, servers, lifecycle,
  console, scheduling requests, and compute observations;
- **Network / Neutron-compatible:** networks, subnets, ports, IP/MAC ownership,
  binding intent, and network observations;
- **Volume / Cinder-compatible:** volumes, types, attachments, backend
  selection, and storage observations;
- **Placement-compatible:** resource providers, inventories, traits,
  allocations, generations, and capacity conflicts.

These are logical service boundaries, not an immediate requirement for six
separate daemons. A future split requires measured need, an accepted ADR, and
stable contracts.

## Keystone as the trust root

Identity is central to trust and discovery, but is not the transaction
coordinator for compute, network, image, or storage operations.

Keystone-compatible responsibilities include:

- authentication and token issuance;
- project, domain, user, group, role, and assignment identity;
- a single validated internal authorization context;
- service users and service-to-service identity;
- service catalog and endpoint discovery;
- token expiry, revocation policy, audit identity, and redaction.

Every service consumes a validated `AuthContext`; service-specific request
models must not independently reinterpret token claims. Keystone does not own
server, port, volume, allocation, or provider-operation state.

See:

- [ADR-0161](adr/ADR-0161-keystone-trust-and-service-identity.md);
- [SPEC-0020](specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md).

## Execution topology

### Control plane

```text
OpenStack CLI / SDK
        |
        v
      o3kd
        |
        +-- identity, image, compute, network, volume, placement
        +-- durable desired state and operations
        +-- scheduling, compensation, reconciliation
```

### Host-local execution boundaries

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

`o3k-compute` is the only mandatory real execution agent for the first
libvirt alpha. `o3k-network` and `o3k-storage` are target process names and
contract boundaries. They may remain embedded behind the compute agent or fake
providers until their contracts and failure models are accepted.

Process separation must not precede logical separation. Prematurely creating
three daemons would add enrollment, heartbeats, journals, cross-agent
transactions, startup ordering, and rollback complexity before the contracts
are stable.

See:

- [ADR-0160](adr/ADR-0160-service-topology-and-execution-boundaries.md);
- [ADR-0162](adr/ADR-0162-contract-first-staged-runner-validation.md);
- [execution boundary contract](../contracts/execution-boundaries.md).

## Resource ownership

O3K owns:

- OpenStack-facing IDs and representations;
- identity, project, catalog, role, policy, quota, and placement semantics;
- desired state and immutable request snapshots;
- operation identity, state, compensation, and reconciliation decisions;
- API compatibility and public error behavior;
- mappings between O3K IDs and provider-native IDs.

Execution agents own only bounded provider-native work:

- local runtime identifiers and observations;
- VM, network, or volume host mutations;
- host capability reporting;
- owned artifact materialization and cleanup;
- provider-native failure details after redaction.

An agent must never become authoritative for OpenStack authorization, project
ownership, scheduling policy, or public resource state.

## Operation pattern

A mutating request normally:

1. validates authentication, authorization, schema, quota, dependency state,
   and compatibility-profile support;
2. snapshots immutable inputs and persists desired state;
3. creates an operation identity and any required Placement allocation;
4. creates dependent intent, such as a Neutron port or Cinder attachment;
5. dispatches one typed provider command;
6. observes provider state;
7. persists convergence, compensation, unknown outcome, or terminal failure;
8. emits audit, metric, trace, and compatibility evidence.

A timeout means the outcome is unknown. Reconciliation observes before
retrying any destructive or duplicating mutation.

Cross-service workflows and reverse-order compensation are normative in
[SPEC-0021](specs/SPEC-0021-cross-service-workflows-and-compensation.md).

## Compatibility profile

O3K targets OpenStack 2026.1 Gazpacho as the primary reference and maintains a
2025.2 Flamingo compatibility profile where declared. Compatibility is
service-specific and operation-specific; O3K does not claim support for an
entire named OpenStack release.

Each advertised operation records:

- method, path, service type, and microversion or extension;
- auth scope and policy;
- request, response, and error contracts;
- state transition and dependencies;
- idempotency and retry semantics;
- portable and real-host evidence.

See [SPEC-0022](specs/SPEC-0022-service-api-baseline-and-evidence-gates.md).

## Deployment profiles

### Portable simulated cloud

- one `o3kd` process;
- SQLite with embedded migrations;
- real HTTP, auth, stores, state machines, scheduling, and reconciliation;
- stateful fake compute, network, and storage providers;
- complete cross-service compensation tests;
- no privileged host mutation.

This profile is the primary fast integration environment.

### Libvirt TestLab alpha

- one `o3kd` process;
- `o3k-compute` on the compute host;
- local `qemu:///system` libvirt/KVM;
- image cache, qcow2 overlay, config-drive, console, and minimum flat
  networking execution;
- Cinder and persistent volumes are not prerequisites for an ephemeral-root
  first guest;
- runner evidence is collected at explicit component and full-cloud gates.

### Future separated agents

After contract conformance and measured need:

- `o3k-network` may own network execution independently;
- `o3k-storage` may own volume execution independently;
- CellHV may implement compute, network, and storage provider contracts;
- PostgreSQL and distributed workers may be introduced by separate decisions.

## Validation architecture

Testing proceeds from cheap, isolated evidence toward privileged integration:

```text
spec and contract validation
  -> domain/store/provider tests
  -> portable simulated cloud
  -> compute-only runner
  -> network-only runner
  -> storage-only runner
  -> full-cloud runner
  -> failure/restart matrix
  -> release gate
```

The full runner is a final integration verifier, not the primary mechanism for
discovering missing endpoint requirements. Component runner gates must retain
inspectable resources long enough to collect domain XML, provider state, logs,
and ownership evidence before cleanup.

## Growth path

1. freeze identity, service, API, and provider contracts;
2. prove the complete portable simulated cloud;
3. prove Keystone + Glance + Placement + Nova + minimum Neutron with a real
   ephemeral-root libvirt guest;
4. add the Cinder subset and `o3k-storage` contract without blocking the first
   guest milestone;
5. separate `o3k-network` and `o3k-storage` processes only after conformance and
   failure evidence;
6. add optional CellHV providers;
7. add small-cluster coordination only after an explicit failure model.
