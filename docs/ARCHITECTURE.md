# Architecture

## Architectural direction

O3K is a lightweight, open, Rust-native **Cloud Operating System**.

The central architectural decision is that O3K does not internally reproduce
the historical OpenStack project topology.

Instead:

- OpenStack is a first-class northbound compatibility contract;
- the O3K Cloud Kernel is the canonical internal platform;
- O3K services consume shared IAM, authorization, resource, operation, audit,
  quota/limit, service-registry, and reconciliation contracts;
- infrastructure execution is delegated through typed provider/agent
  boundaries;
- existing external clouds use a distinct delegated/federated authority model.

The normative decision is
[ADR-0165](adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md).

Current maturity is narrower:

> O3K v0.2.0-alpha.1 is a Rust-native OpenStack-compatible libvirt TestLab
> alpha.

Architecture is direction. Release support remains evidence-gated.

![O3K Cloud OS architecture](architecture/o3k-cloud-os.svg)

A short visual explanation is available in
[`docs/architecture/O3K_CLOUD_OS_SUMMARY.md`](architecture/O3K_CLOUD_OS_SUMMARY.md).

## Normative ownership

This document is an overview.

Normative sources are listed in
[`docs/NORMATIVE_SOURCES.md`](NORMATIVE_SOURCES.md).

Especially important:

- Cloud OS / Cloud Kernel authority: ADR-0165;
- O3K IAM / Keystone compatibility: ADR-0166 and SPEC-0020;
- deployment/evidence profiles: ADR-0163 and SPEC-0024;
- topology/dependency/execution boundaries: ADR-0160, SPEC-0025, and the
  execution/core-boundary contracts;
- cross-service workflow/compensation: SPEC-0021;
- OpenStack compatibility baselines: SPEC-0022 and compatibility manifests.

Runtime behavior and release claims require executable evidence.

## Top-level architecture

```text
                       NORTHBOUND CONTRACTS
        +------------------------------------------------+
        | OpenStack-compatible APIs | future O3K Native |
        | CLI / SDK / Terraform     | APIs / operators  |
        +--------------------+---------------------------+
                             |
                             v
                  +-----------------------+
                  |    O3K CLOUD KERNEL   |
                  |-----------------------|
                  | IAM / principals      |
                  | authorization         |
                  | resource ownership    |
                  | service registry      |
                  | quotas / limits       |
                  | durable operations    |
                  | audit / events        |
                  | regions / AZs         |
                  | reconciliation        |
                  +-----------+-----------+
                              |
          +-------------------+-------------------+
          |                   |                   |
          v                   v                   v
       Compute             Network              Volume
       Image               Capacity             future services
       ...                 Placement            database / AI / etc.
          \                   |                   /
           +------------------+------------------+
                              |
                   typed execution contracts
                              |
          +-------------------+-------------------+
          |                   |                   |
          v                   v                   v
     o3k-compute         o3k-network        o3k-storage
     libvirt/CellHV      Linux/OVN/etc.     LVM/Ceph/etc.
```

The first alpha physically implements only the subset needed by its selected
profile.

## The Cloud Kernel

The Cloud Kernel is not "all business logic in one giant crate."

It is the shared contract layer that stops every service from independently
reinventing cloud platform semantics.

### IAM and principals

Canonical IAM defines:

- principal identity;
- service principal;
- authentication context;
- ownership/security scope;
- delegation/original-actor identity;
- expiry/assurance metadata;
- secret-redaction rules.

Keystone is one compatibility adapter into this model.

See ADR-0166 and SPEC-0020.

### Authorization

Protected operations converge on:

```text
Principal × Action × Resource × Context -> Allow | Deny
```

Examples:

```text
compute:CreateServer
network:CreatePort
volume:AttachVolume
```

A future service defines its own action/resource vocabulary but does not define
a new tenant-isolation architecture.

Default behavior is deny.

Authorization is evaluated before provider mutation, secret-bearing external
calls, and cross-tenant resource disclosure.

### Resource identity and ownership

The kernel provides stable concepts for:

- O3K public resource ID;
- resource type;
- owner/security scope;
- service namespace;
- desired/observed lifecycle association;
- provider/external mapping;
- region/AZ metadata where selected;
- bounded attributes/tags where selected.

OpenStack request paths, provider-native IDs, database row formats, and
filesystem paths do not become authorization identity.

### Service registry

The O3K service registry is richer than a URL directory.

It is designed to describe, where selected:

- service identity/namespace;
- ownership mode;
- API surfaces and versions;
- regions/endpoints;
- resource types;
- action vocabulary;
- capabilities/features;
- health/readiness;
- evidence/claim state.

The Keystone service catalog is an OpenStack-compatible projection of selected
verified endpoints.

### Quotas and limits

Quota/limit semantics should share a common vocabulary and enforcement contract
when selected.

This does not mean every service has identical limits.

It means a new O3K service should not create an incompatible quota framework by
default.

### Durable operations

The current Rust work already implements one of the most important Cloud Kernel
properties:

```text
intent
-> durable operation
-> side effect
-> observation
-> convergence / compensation
```

A timeout is an unknown outcome, not proof of failure.

Operation identities, phases, idempotency, retries, compensation, and
reconciliation are shared platform concepts rather than handler-local control
flow.

### Audit and events

The common identity model should make it possible to answer:

```text
who
did what
to which resource
in which ownership scope
through which service
under which request/operation
with what decision/outcome
```

Service-local logging is useful but does not replace canonical audit identity.

### Regions and availability domains

Services and schedulers use stable O3K location identity.

OpenStack region/AZ fields are compatibility projections over selected O3K
location semantics.

## OpenStack compatibility boundary

OpenStack remains strategically important because it provides:

- standard CLI workflows;
- SDKs;
- Terraform ecosystem;
- service integrations;
- operator knowledge;
- mature public API semantics.

O3K should preserve that value without importing historical implementation
topology.

Conceptual mapping:

```text
Keystone API
  -> IAM compatibility adapter
  -> O3K IAM

Glance API
  -> Image compatibility adapter
  -> O3K Image

Nova API
  -> Compute compatibility adapter
  -> O3K Compute

Neutron API
  -> Network compatibility adapter
  -> O3K Network

Placement API
  -> Capacity compatibility adapter
  -> O3K Capacity/Placement

Cinder API
  -> Volume compatibility adapter
  -> O3K Volume
```

Compatibility models remain at protocol edges:

- JSON envelopes;
- URLs;
- headers;
- microversions;
- OpenStack error shapes;
- Keystone catalog response shapes;
- legacy policy names where required.

The core domain must not depend on them.

## O3K-native APIs

A future O3K-native API may exist beside OpenStack compatibility when:

- a new first-class O3K service has no suitable OpenStack API;
- an O3K capability would be distorted by forcing it through an unrelated
  historical API;
- a common Cloud Kernel operation needs a clean native contract.

A native API is not a reason to break supported OpenStack compatibility.

Its public contract requires a separate specification/evidence profile.

## Architectural layers

```text
Protocol adapters
  OpenStack-compatible HTTP
  future O3K-native API
  operator API
  selected external-service clients
        |
        v
Application services
  IAM, Image, Compute, Network, Capacity, Volume, future services
  commands, queries, scheduling, orchestration
        |
        v
Cloud Kernel + Domain
  principals, authorization, resource identity/ownership
  state machines, operations, compensation, audit/event identity
        |
        v
Narrow ports
  store, clock, signer/credential, policy
  service registry, provider, external-service/federation contracts
        |
        v
Adapters
  SQLite, future PostgreSQL
  stateful fakes
  agent clients
  compatibility translators
  external-service clients
        |
        v
Execution boundaries
  libvirt/KVM, Linux networking, LVM/Ceph, optional CellHV
```

Dependencies point inward.

The domain does not depend on Axum, SQLx, protobuf-generated clients, libvirt,
OpenStack JSON, or external-service client models.

## Control-plane process topology

`o3kd` remains the initial modular control-plane process.

```text
OpenStack clients / future O3K clients
                  |
                o3kd
                  |
      Cloud Kernel + service modules
                  |
          typed execution ports
```

`o3kd` owns:

- public compatibility/native API behavior;
- canonical authenticated authorization context;
- O3K-owned public resource identity;
- desired state and immutable dependency snapshots;
- policy/authorization decisions;
- capacity/placement and scheduling;
- operation/compensation/reconciliation state;
- service registry and compatibility advertisement;
- mappings to provider/external identities;
- public errors/claim/evidence identity.

A logical service does not require its own process.

## Service modules

Current O3K service domains are:

### IAM

Responsibilities:

- canonical principals/service identity;
- authorization context;
- ownership/security-scope compatibility mapping;
- policy/authorization engine boundary;
- compatibility token/credential handling;
- service registry identity;
- Keystone compatibility projection.

It does not own compute/network/storage resource lifecycle.

### Image

Responsibilities:

- image metadata;
- content authorization;
- content identity/digest;
- activation/visibility;
- provider/cache translation.

Glance is its compatibility API where selected.

### Compute

Responsibilities:

- flavors/keypairs/server intent;
- dependency snapshots;
- server lifecycle/orchestration;
- capacity requests;
- compute execution mapping;
- observed-state projection;
- console authorization.

Nova is its compatibility API where selected.

### Network

Responsibilities:

- network/subnet/port intent;
- IP/MAC allocation;
- binding intent;
- network policy where selected;
- network execution mapping/observation.

Neutron is its compatibility API where selected.

### Capacity / Placement

Responsibilities:

- resource providers;
- inventories;
- traits/capabilities where selected;
- generation-protected allocations;
- scheduling inputs.

Placement is its compatibility API where selected.

### Volume

Later native profile responsibilities:

- volume/type/attachment intent;
- backend selection;
- attachment orchestration;
- storage execution observation.

Cinder is its compatibility API where selected.

### Future services

The architecture permits future first-class services such as managed database,
Kubernetes, AI/ML, DNS, load balancing, object storage, or other capabilities.

These are examples, not current roadmap/support claims.

Every future service consumes the Cloud Kernel and defines its own bounded
resource/action/API/evidence profile.

## O3K IAM versus Keystone

The old conceptual statement:

> Keystone is the common O3K trust root.

is superseded.

The accepted architecture is:

```text
Keystone-compatible API
        |
        v
Keystone compatibility mapping
        |
        v
O3K IAM / authorization
        |
        v
all first-class O3K services
```

Keystone remains critical for OpenStack client/service compatibility.

It is not the internal architecture that every future O3K service must speak.

## Service-to-service authorization

When one service performs work on behalf of another actor, O3K preserves:

```text
original actor
+ original ownership/security scope
+ calling service principal
+ delegated action
+ request/audit/operation identity
```

A service principal does not silently become an administrator.

The modular monolith may pass this context in process.

A cross-process boundary requires authenticated service identity and explicit
delegation.

## Resource authority

For O3K-owned resources, O3K is authoritative for:

- public resource identity;
- tenant/security-scope ownership;
- desired state;
- operation identity/phase;
- scheduling/capacity allocation;
- compatibility representation;
- compensation/reconciliation;
- provider mapping.

Execution agents/providers are authoritative only for:

- current capabilities/health;
- provider-native ID;
- provider existence/observed state;
- bounded local artifacts;
- redacted provider failures.

No provider may:

- create an unknown O3K public identity;
- authorize a caller;
- change ownership scope;
- independently reschedule;
- rewrite desired state.

## Host execution boundaries

Target host-local process boundaries remain:

```text
                     versioned mTLS contracts
                              |
          +-------------------+-------------------+
          |                   |                   |
          v                   v                   v
    o3k-compute          o3k-network         o3k-storage
    libvirt/KVM          TAP/bridge/OVN       LVM/Ceph RBD
    image/overlay        routing/policy       volume lifecycle
    config-drive         binding observation  attachment prep
    console              network observation  storage observation
```

`o3k-compute` is the only mandatory separate execution process for the first
libvirt alpha.

Minimum network execution may remain hosted inside it behind a logical network
provider boundary.

A new daemon requires an accepted decision covering privilege, locality,
identity, enrollment, mTLS, heartbeat/epoch fencing, protocol versioning,
journal/idempotency, reconnect/resync, ownership, cleanup, and failure
containment.

## Execution provider versus delegated cloud

This distinction is mandatory.

### Execution provider

O3K owns cloud semantics and lifecycle.

Examples:

- libvirt;
- CellHV;
- Linux bridge/OVN when selected;
- LVM;
- Ceph RBD.

```text
O3K desired state
-> provider command
-> bounded mutation
-> observation
-> O3K reconciliation
```

### Delegated/federated cloud

The external system already owns significant cloud semantics.

Possible future examples:

- another OpenStack cloud;
- vSphere/vCenter;
- Proxmox;
- KubeVirt;
- public cloud.

It may own:

- scheduling;
- quotas;
- authorization;
- resource IDs;
- lifecycle;
- network/storage semantics.

Therefore it requires explicit authority mapping and must not be forced through
the same provider contract as libvirt.

## External-hosted OpenStack services

An external-hosted OpenStack service remains independently operated.

For an external Cinder testbed:

- O3K owns selected IAM/catalog/token-validation and satellite compatibility
  surfaces;
- Cinder owns its API/database/message bus/processes/backend/migrations/
  upgrades/health;
- O3K owns only its explicit compatibility records/workflow state/test
  resources.

An external endpoint is never presented as native O3K implementation.

## Operation and compensation model

A mutating workflow normally:

1. validates credential, authorization, schema, quota/limit, dependency state,
   and profile support;
2. snapshots immutable inputs and persists desired state;
3. creates operation identity and required allocations;
4. persists workflow phase before each external side effect;
5. dispatches one typed provider/external action;
6. observes result;
7. persists convergence, compensation, unknown outcome, or terminal failure;
8. emits audit/evidence identity.

A timeout is an unknown outcome.

Reconciliation observes before retrying a mutation that could duplicate or
destroy resources.

## Process/crate extraction rule

Process boundaries follow:

- privilege;
- failure domain;
- host locality;
- scaling;
- deployment ownership;
- security.

Crate boundaries follow:

- dependency direction;
- stable domain ownership;
- compile-time isolation;
- conformance/testing value.

Neither follows historical OpenStack project count.

## Database architecture

### SQLite

Currently supported default for minimal TestLab/portable profiles.

It requires explicit WAL/concurrency, bounded lock handling, migrations, crash
recovery, backup/restore, and filesystem constraints.

### PostgreSQL

Intended production-oriented/stronger-availability profile.

It is not supported merely because ports are designed for it.

Support requires adapter/conformance/migrations/transaction/failure/
backup-restore evidence.

Multi-controller/HA requires a separate coordination/fencing/database decision.

## Footprint architecture

The minimal O3K control plane targets approximately 50 MB steady-state memory.

Every published measurement names exact profile/process/build/host/workload/
method and reports external dependencies separately.

Cloud Kernel growth must not silently invalidate small-profile footprint goals;
shared platform services should be compiled/activated according to explicit
profiles where practical.

## Compatibility and metadata

Primary OpenStack reference: 2026.1 Gazpacho.

Backward reference: 2025.2 Flamingo where declared.

Only implemented/verified contiguous compatibility windows are advertised.

The first native alpha uses config-drive/cloud-init for guest metadata.

HTTP metadata requires a separate accepted security/network profile and
executable guest-isolation evidence.

## Validation architecture

```text
architecture/spec/profile validation
  -> Cloud Kernel/domain/store/policy tests
  -> provider/external-service conformance
  -> portable simulated profile
  -> process/public-client tests
  -> execution/hosted-service component gate
  -> full native/testbed/edge profile gate
  -> failure/restart matrix
  -> release gate
```

The protected runner is a final integration verifier, not a requirements
discovery loop.

## Product-profile architectures

### Portable simulated cloud

- one `o3kd`;
- real HTTP/auth/store/scheduler/operations/reconciliation;
- SQLite;
- stateful fake execution/external-service providers;
- no privileged host mutation.

### Native libvirt TestLab

- `o3kd` + `o3k-compute`;
- local `qemu:///system`;
- O3K IAM compatibility subset;
- O3K Image/Compute/Network/Capacity domains exposed through selected OpenStack
  compatibility APIs;
- image cache/qcow2/config-drive/console/flat networking;
- native volumes later.

### External OpenStack service testbed

- O3K Cloud Kernel/compatibility surfaces provide selected surrounding APIs;
- external service remains independently operated;
- external dependencies explicit;
- fake-service tests precede real integration.

### Small edge cloud

- approximately 10–20 hypervisors initially;
- one or more explicitly supported control-plane nodes;
- multi-host capacity/scheduling/fencing/reconnect/resync;
- host-local execution;
- backup/restore/upgrade/rollback/diagnostics;
- selected network/storage/security/database evidence.

## Growth path

The release-critical first step does not change:

1. complete and release `v0.2.0-alpha.1` libvirt TestLab;
2. converge the working IAM path on the accepted O3K IAM/Keystone boundary
   without breaking the released compatibility workflow;
3. extract/reuse shared authorization action/resource/ownership primitives;
4. make service registry/catalog projection explicit;
5. converge current Image/Compute/Network/Capacity services on Cloud Kernel
   contracts;
6. implement native O3K Volume/Cinder compatibility as an independent service
   domain;
7. prove multi-host edge behavior;
8. add PostgreSQL after store conformance;
9. extract `o3k-network`/`o3k-storage` only when privilege/failure evidence
   justifies it;
10. add the first genuinely new O3K-native service only after Cloud Kernel
    service-extension contracts are strong enough to prove the developer model;
11. define delegated/federated cloud connectors only when a concrete integration
    target enters the roadmap;
12. add multi-controller/HA only after a separate coordination/failure model.

## Architectural fitness questions

Every significant change should answer:

1. Is this O3K canonical domain state or a compatibility/provider
   representation?
2. Is the resource O3K-authoritative, external-hosted, or delegated to another
   cloud?
3. Is authorization expressed through the shared principal/action/resource/
   context model?
4. Does a new service reuse shared operation/ownership/audit primitives?
5. Does an execution adapter have more authority than it needs?
6. Is process separation justified by privilege/failure/locality/scale?
7. Is a release claim backed by the correct profile evidence?
8. Does this change preserve the current alpha critical path unless explicitly
   replanned?
