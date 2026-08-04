# Architecture

## Architectural direction

O3K Rust is an OpenStack-compatible control plane implemented as a modular
monolith first, with explicit service, provider, and process boundaries. The
initial deployment keeps public APIs and orchestration in `o3kd` so that
transactions, authorization, and recovery remain understandable. Privileged
host execution is delegated through versioned contracts.

The architecture deliberately distinguishes:

1. **OpenStack service semantics** — Keystone, Glance, Nova, Neutron, Cinder,
   and Placement-compatible behavior owned by `o3kd` when O3K implements the
   declared profile;
2. **external-hosted service profiles** — independently running OpenStack
   services that use O3K's declared satellite APIs and catalog without becoming
   O3K implementations;
3. **provider orchestration** — desired state, operations, scheduling,
   compensation, and reconciliation owned by the control plane;
4. **host execution** — bounded compute, network, and storage mutations owned
   by host-local agents.

Endpoint count is not an architectural goal. O3K advertises only frozen,
executable compatibility profiles.

## Normative ownership

This architecture document is an overview. Field-level identity semantics,
workflow phases, compensation order, compatibility operations, and execution
protocol invariants are normative only in the sources listed in
[`docs/NORMATIVE_SOURCES.md`](NORMATIVE_SOURCES.md).

When overview text conflicts with a listed normative source, the normative
source wins and this overview must be corrected. Documentation never replaces
executable compatibility evidence.

## Layers

```text
Protocol adapters
  declared OpenStack HTTP APIs and operator API
        |
Application services
  identity, image, compute, network, volume, placement
  commands, queries, authorization, orchestration
        |
Domain
  resource state, invariants, operation state machines
        |
Ports
  store, clock, signer, policy, provider and external-service contracts
        |
Adapters
  SQLite, stateful fakes, agent clients, external-service clients
        |
Execution agents
  libvirt/KVM, Linux networking, LVM/Ceph, optional CellHV
```

Dependencies point inward. The domain does not depend on Axum, SQLx,
protobuf-generated provider clients, libvirt bindings, external-service client
models, or OpenStack JSON representations.

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
  selection, and storage observations when the O3K-owned volume profile is
  implemented;
- **Placement-compatible:** resource providers, inventories, traits,
  allocations, generations, and capacity conflicts.

These are logical service boundaries, not an immediate requirement for six
separate daemons. A future split requires measured need, an accepted ADR, and
stable contracts.

An external Cinder service-under-test is different from the O3K-owned volume
service. It remains an independently operated Cinder deployment; O3K supplies
only the selected identity, image, compute, and catalog compatibility surface.
See [SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md).

## Keystone as the trust root

Identity is central to trust and discovery, but is not the transaction
coordinator for compute, network, image, or storage operations.

Keystone-compatible responsibilities include:

- authentication and token issuance;
- project, domain, user, group, role, and assignment identity;
- a single validated internal authorization context;
- service users and service-to-service identity;
- service catalog and endpoint discovery;
- token expiry, declared revocation policy, audit identity, and redaction.

Every service consumes a validated `AuthContext`; service-specific request
models must not independently reinterpret token claims. Keystone does not own
server, port, volume, allocation, or provider-operation state.

The bootstrap alpha remains smaller than the hosted-service identity profile.
Catalog entries and token-validation claims are advertised only after their
selected profile has executable evidence.

See:

- [ADR-0161](adr/ADR-0161-keystone-trust-and-service-identity.md);
- [SPEC-0020](specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md);
- [SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md).

## Execution topology

### Control plane

```text
OpenStack CLI / SDK or external hosted service
        |
        v
      o3kd
        |
        +-- identity, image, compute, network, volume, placement
        +-- durable desired state and operations
        +-- scheduling, compensation, reconciliation
        +-- typed clients for explicitly supported external-service workflows
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

- OpenStack-facing IDs and representations for O3K-implemented services;
- identity, project, catalog, role, policy, quota, and placement semantics;
- desired state and immutable request snapshots;
- operation identity, state, compensation, and reconciliation decisions;
- API compatibility and public error behavior;
- mappings between O3K IDs and provider-native IDs.

External-hosted services own their APIs, internal state, database, messaging,
workers, backend resources, migrations, and service-specific recovery. O3K may
own catalog records and satellite workflow state, but does not relabel an
external service as O3K-implemented.

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
4. creates dependent intent, such as a Neutron port, O3K volume attachment, or
   external Cinder attachment record;
5. dispatches one typed provider or external-service command;
6. observes provider or external-service state;
7. persists convergence, compensation, unknown outcome, or terminal failure;
8. emits audit, metric, trace, and compatibility evidence.

A timeout means the outcome is unknown. Reconciliation observes before
retrying any destructive or duplicating mutation.

Cross-service workflows and reverse-order compensation are normative in
[SPEC-0021](specs/SPEC-0021-cross-service-workflows-and-compensation.md). The
external Cinder attachment profile is normative in
[SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md).

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

The compatibility manifest distinguishes:

- upstream reference maximum;
- O3K advertised range;
- implemented range;
- verified range;
- service ownership (`o3k-implemented` or `external-hosted`).

See [SPEC-0022](specs/SPEC-0022-service-api-baseline-and-evidence-gates.md).

## Guest metadata

The first libvirt alpha uses config-drive/cloud-init for hostname, SSH public
key, metadata, user-data, and declared network-data delivery. An HTTP metadata
service is not part of that profile and must not be advertised in discovery,
catalog, architecture claims, or release notes.

A future link-local metadata service requires a separate accepted security and
networking decision plus executable guest and tenant-isolation evidence.

## Deployment profiles

### Portable simulated cloud

- one `o3kd` process;
- SQLite with embedded migrations;
- real HTTP, auth, stores, state machines, scheduling, and reconciliation;
- stateful fake compute, network, storage, and external-service providers;
- complete cross-service compensation tests;
- no privileged host mutation.

This profile is the primary fast integration environment.

### Libvirt TestLab alpha

- one `o3kd` process;
- `o3k-compute` on the compute host;
- local `qemu:///system` libvirt/KVM;
- image cache, qcow2 overlay, config-drive, console, and minimum flat
  networking execution;
- config-drive-only guest metadata;
- Cinder and persistent volumes are not prerequisites for an ephemeral-root
  first guest;
- runner evidence is collected at explicit component and full-cloud gates.

### External Cinder service-under-test

- O3K supplies the selected Keystone-, Glance-, and Nova-compatible satellite
  APIs and service catalog;
- a real external Cinder endpoint is registered as `external-hosted`;
- Cinder retains its own database, message bus, API/scheduler/volume services,
  backend, migrations, and upgrades;
- Nova volume-attachment compatibility and a typed outbound Cinder attachment
  client are required before real integration evidence;
- this profile is separately tested and does not block the first ephemeral-root
  alpha.

### Future separated agents

After contract conformance and measured need:

- `o3k-network` may own network execution independently;
- `o3k-storage` may own O3K-implemented volume execution independently;
- CellHV may implement compute, network, and storage provider contracts;
- PostgreSQL or a deliberately SQLite-only product posture requires a separate
  accepted database decision;
- distributed workers may be introduced by separate decisions.

## Validation architecture

Testing proceeds from cheap, isolated evidence toward privileged integration:

```text
spec and contract validation
  -> domain/store/provider tests
  -> portable simulated cloud
  -> process/public-client tests
  -> compute-only runner
  -> network-only runner
  -> storage-only or external-service runner
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
4. implement evidence-backed microversion discovery and SQLite concurrency
   posture;
5. implement the hosted-service Keystone profile and Nova/Cinder attachment
   bridge for external Cinder testing;
6. add the O3K-owned Cinder subset and `o3k-storage` contract independently;
7. separate `o3k-network` and `o3k-storage` processes only after conformance and
   failure evidence;
8. add optional CellHV providers;
9. add small-cluster coordination only after an explicit failure model.
