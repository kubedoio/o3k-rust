# ADR-0165 — O3K as a Cloud Operating System with a shared Cloud Kernel

Status: Accepted
Date: 2026-08-12
Human-approval: Senol Colak, 2026-08-12
Supersedes: none
Superseded-by: none
Affected-services: governance, identity, compute, network, image, placement, volume, future-services

## Context

O3K began as a lightweight OpenStack-compatible control plane and has already
moved beyond an API emulator. The Rust architecture owns durable desired state,
resource identity, scheduling, operations, compensation, reconciliation, and
provider mappings while host-local agents perform bounded infrastructure
execution.

That is a cloud control-plane architecture.

The project now needs an explicit answer to a larger question:

> Is O3K primarily a smaller implementation of the historical OpenStack service
> topology, an infrastructure overlay above existing clouds, or a next-generation
> cloud operating system that preserves OpenStack compatibility?

The answer is the third option.

Traditional OpenStack is an ecosystem of independently deployed services. Its
public APIs and client ecosystem remain strategically valuable, but its service
names, per-service authorization conventions, catalog model, and process
topology must not become the internal architecture of O3K by accident.

O3K has no requirement to reproduce Nova, Neutron, Cinder, Keystone, Glance, or
Placement implementation boundaries internally. It does need to provide
well-tested compatibility surfaces where those APIs are selected.

The project also intends to make future first-class cloud services—such as
managed databases, Kubernetes, AI/ML, object services, load balancing, DNS, or
other platform capabilities—materially cheaper to build than they are in a
traditional OpenStack deployment. Requiring every new service to independently
solve token parsing, tenant ownership, service identity, authorization, quota,
audit, operation tracking, eventing, and lifecycle recovery would reproduce the
same extensibility tax the project is trying to remove.

## Decision

### 1. O3K is a Cloud Operating System

O3K's long-term architectural identity is:

> **A lightweight, open, Rust-native Cloud Operating System with OpenStack
> compatibility northbound and pluggable infrastructure execution southbound.**

"Cloud Operating System" is an architectural statement, not a current
production-readiness claim.

O3K is the authority for the cloud model it owns:

- principals and authorization context;
- public O3K resource identities;
- resource ownership and tenancy;
- desired state;
- operations and long-running workflow state;
- scheduling and capacity decisions;
- quota/limit decisions for declared profiles;
- compensation and reconciliation;
- audit and event identity;
- compatibility projections;
- mappings to provider-native resources.

Execution providers are not the cloud authority. They execute bounded mutations
and report observations.

### 2. The O3K Cloud Kernel is the shared platform layer

O3K will converge on a common Cloud Kernel that provides reusable platform
primitives to every first-class O3K service.

The kernel owns or defines stable contracts for:

- **IAM and principals** — users, service principals, workload/service identity,
  authentication context, delegation boundaries, and federation hooks;
- **authorization** — `Principal × Action × Resource × Context -> Allow/Deny`,
  default deny, typed actions and resources, and shared policy evaluation;
- **tenant/resource ownership** — stable ownership scopes, durable IDs, tags,
  region/AZ placement metadata, and resource type identity;
- **service registry** — service identity, API surfaces, capabilities, resource
  types, action vocabulary, regions, endpoints, and lifecycle/health metadata;
- **quotas and limits** — common limit vocabulary and enforcement hooks rather
  than unrelated service-local quota semantics;
- **operations** — durable operation identity, phases, idempotency, unknown
  outcome, compensation, and reconciliation;
- **audit and events** — shared request/actor/resource/action identity and
  source-bound event semantics;
- **regions and availability domains** — common location identity usable by
  services and schedulers;
- **shared lifecycle contracts** — health, readiness, capability discovery,
  failure classification, and evidence identity.

Metering, billing, secrets, configuration, richer organization hierarchy, and
other shared platform capabilities may be added through later accepted
decisions. They are not implied to be implemented by this ADR.

### 3. OpenStack is a compatibility contract, not O3K's internal domain model

The OpenStack API ecosystem remains a first-class northbound compatibility
target.

The mapping is conceptually:

```text
Keystone API   -> O3K IAM compatibility adapter
Nova API       -> O3K Compute compatibility adapter
Neutron API    -> O3K Network compatibility adapter
Glance API     -> O3K Image compatibility adapter
Placement API  -> O3K Capacity/Placement compatibility adapter
Cinder API     -> O3K Volume compatibility adapter
```

OpenStack JSON models, service names, headers, microversions, policy vocabulary,
catalog shapes, and error envelopes remain protocol/compatibility concerns.

The canonical O3K domain must not become a collection of OpenStack wire models.

The O3K-native API may grow beside the OpenStack compatibility API when a
first-class O3K capability cannot be represented cleanly by an existing
OpenStack contract. Adding a native API does not remove the selected OpenStack
compatibility profiles.

### 4. O3K IAM replaces Keystone as the internal identity/authorization architecture

Keystone compatibility remains required for selected OpenStack profiles, but
Keystone is not the canonical internal security model.

The O3K kernel will define a service-neutral authorization contract around:

```text
Principal
Action
Resource
Context
Decision
```

Every first-class O3K service declares:

- resource types it owns;
- actions it exposes;
- ownership scope required by each action;
- applicable condition/context keys;
- service-to-service privileges;
- audit identity requirements.

A new O3K service must not need to invent an independent tenant-isolation model
or parse Keystone token claims directly.

The detailed IAM/Keystone compatibility boundary is defined in ADR-0166 and
SPEC-0020.

### 5. Cloud services are first-class modules above the kernel

Current infrastructure services become O3K service domains:

- Identity/IAM;
- Image;
- Compute;
- Network;
- Capacity/Placement;
- Volume.

Future services may include, for example:

- managed database;
- Kubernetes/container platform;
- AI/ML inference or training;
- load balancing;
- DNS;
- object storage;
- secrets;
- other operator-selected cloud capabilities.

These examples are architectural possibilities, not roadmap commitments or
current support claims.

A first-class O3K service consumes Cloud Kernel contracts instead of
reimplementing common cloud plumbing.

### 6. Infrastructure independence lives below the control plane

O3K remains infrastructure-pluggable without becoming an authority-neutral
overlay.

For O3K-owned resources:

```text
O3K desired state
  -> O3K scheduling / orchestration
  -> typed execution contract
  -> provider mutation
  -> provider observation
  -> O3K reconciliation
```

Examples of execution providers include:

- libvirt/QEMU/KVM;
- CellHV;
- Linux bridge, OVS, or OVN where explicitly supported;
- LVM;
- Ceph RBD;
- later providers with accepted capability contracts.

O3K owns the public cloud identity and lifecycle. The provider owns only bounded
execution and observation.

### 7. Existing external clouds use a separate delegated/federated model

An existing control plane such as another OpenStack cloud, vSphere/vCenter,
Proxmox, KubeVirt, or a public cloud is not equivalent to libvirt or Ceph.

A system that already owns scheduling, resource lifecycle, policy, quotas, or
resource identity must not be forced through an execution-provider interface
that assumes O3K authority.

Future integration with existing clouds therefore requires a separate
**delegated/federated cloud connector** model with explicit authority mapping.

That model is intentionally not implemented by this ADR. Each connector requires
its own trust, resource-identity, outage, retry, drift, policy, and ownership
decision.

### 8. Process boundaries follow privilege and failure domains, not OpenStack project names

`o3kd` remains the initial modular control-plane process.

The existing direction remains valid:

```text
o3kd
  -> o3k-compute
  -> future o3k-network
  -> future o3k-storage
```

A new process is justified by:

- privilege separation;
- host locality;
- failure containment;
- independent scaling;
- deployment topology;
- security boundary;
- lifecycle ownership.

A new process is not justified merely because OpenStack historically has a
separate service daemon.

### 9. The current libvirt alpha is not replanned by this decision

The immediate release remains:

> O3K v0.2.0-alpha.1 — Rust-native OpenStack-compatible libvirt TestLab alpha.

Its release gate remains the already-declared Identity/Image/Network/
Placement/Compute workflow and real-host evidence.

This ADR must not be used to inject database, AI, Kubernetes, native API, richer
IAM, federation, or broad Cloud Kernel implementation work into the current
alpha critical path.

After the alpha, architecture convergence should make the current working
vertical slice consume the shared kernel contracts incrementally instead of
starting a second implementation beside it.

### 10. "Next-generation OpenStack" means compatibility plus architectural freedom

The phrase "next-generation OpenStack" is acceptable as product vision only
when it means:

- preserve useful OpenStack APIs and ecosystem compatibility;
- learn from OpenStack's proven cloud resource semantics;
- remove unnecessary deployment/process topology inheritance;
- remove per-service identity/authorization duplication;
- make shared cloud primitives first-class;
- make new cloud services cheaper to add;
- retain strong desired/observed state and recovery semantics;
- remain infrastructure-pluggable.

It must not be used to claim full OpenStack API parity or upstream
interchangeability without evidence.

## Architectural model

```text
                  NORTHBOUND CONTRACTS
          +--------------------------------+
          | OpenStack APIs | O3K Native API|
          +----------------+---------------+
                           |
                           v
                  +------------------+
                  |  O3K CLOUD KERNEL |
                  |------------------|
                  | IAM / principals |
                  | authorization    |
                  | resources/owner  |
                  | service registry |
                  | quotas / limits  |
                  | operations       |
                  | audit / events   |
                  | regions / AZs    |
                  | reconciliation   |
                  +---------+--------+
                            |
               +------------+-------------+
               |            |             |
               v            v             v
           Compute        Network       Volume       ... future services
           Image          Capacity      Database/AI/etc.
               \            |             /
                +-----------+------------+
                            |
                  typed execution contracts
                            |
        +-------------------+-------------------+
        |                   |                   |
        v                   v                   v
    o3k-compute        o3k-network         o3k-storage
    libvirt/CellHV     Linux/OVN/etc.      LVM/Ceph/etc.
```

## Consequences

### Positive

- O3K gains one coherent product identity rather than three loosely related
  product stories.
- OpenStack compatibility is preserved without forcing OpenStack's internal
  project topology into the Rust architecture.
- Keystone ceases to be the internal extensibility bottleneck.
- New services can reuse IAM, authorization, resource ownership, quotas,
  operations, audit, and service registration.
- Current provider/reconciliation work remains valid.
- The architecture can support both small clouds and future richer services
  without a handler-for-handler OpenStack rewrite.
- Execution-provider independence remains strong.
- Existing-cloud federation can be added later without corrupting provider
  authority semantics.

### Negative

- O3K now owns the design of shared cloud primitives that OpenStack historically
  spread across projects.
- A real authorization engine, service manifest, and common resource model are
  substantial security-sensitive work.
- OpenStack compatibility and O3K-native semantics may diverge and require
  explicit translation.
- A shared Cloud Kernel can itself become a monolith if service-specific
  behavior leaks into it.
- "Cloud Operating System" raises expectations; release wording must continue
  to distinguish architectural vision from verified maturity.
- The project must resist prematurely adding many platform services before the
  core IaaS path is solid.

## Rejected alternatives

### Keep Keystone as the permanent internal identity architecture

Rejected because every future service would continue to inherit
OpenStack-specific project/role/catalog constraints and service-specific
authorization work.

### Recreate OpenStack service topology in Rust

Rejected because deployment/process boundaries are historical implementation
choices, not compatibility requirements.

### Build a generic overlay over existing clouds

Rejected as the primary architecture because it creates double control planes,
double scheduling, conflicting desired state, and ambiguous ownership.

### Use one generic provider abstraction for libvirt, OpenStack, VMware, and every cloud

Rejected because an execution provider and an already-authoritative cloud have
different ownership semantics. Lowest-common-denominator abstractions either
erase useful capabilities or leak provider-specific branches everywhere.

### Drop OpenStack compatibility and invent an entirely new API first

Rejected because OpenStack clients, SDKs, Terraform workflows, operator
knowledge, and service interoperability remain a major asset. O3K should earn
the right to add native APIs without discarding compatibility.

### Implement the full Cloud Kernel before shipping the libvirt alpha

Rejected because architecture is not evidence and the existing vertical slice
is already close to a bounded release. The Cloud Kernel should be converged
incrementally after the first alpha.

## Required follow-up

- apply ADR-0166 for O3K IAM and Keystone compatibility;
- update SPEC-0020 to make O3K IAM canonical and Keystone a compatibility
  projection;
- update ADR-0163/SPEC-0024 so the three profiles are deployment/evidence
  profiles of one Cloud OS rather than separate product identities;
- add Cloud Kernel boundary fitness functions as implementation moves;
- define a service manifest/registry specification before the first
  non-OpenStack-native O3K service;
- define a shared authorization policy specification before broad service
  expansion;
- define delegated/federated cloud connectors in a separate ADR only when
  implementation becomes a real roadmap item;
- keep `v0.2.0-alpha.1` release criteria unchanged.
