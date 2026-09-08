# ADR-0181 — End-to-end edge-to-datacenter building-block Cloud OS

Status: Accepted
Date: 2026-09-08
Human-approval: Senol Colak, 2026-09-08
Supersedes: none
Superseded-by: none
Affected-services: governance, cloud-kernel, identity, compute, network, image, placement, volume, service-registry, deployment, compatibility, future-services

Normative specification: [SPEC-0038](../specs/SPEC-0038-edge-to-datacenter-building-block-cloud.md)

## Context

O3K already has the foundations of a small OpenStack-compatible cloud: a shared
Cloud Kernel, native IAM, compute, network, image, placement and volume domains,
typed execution boundaries, OpenStack-compatible northbound APIs, Terraform/
OpenTofu compatibility, a native service framework, and a proven small
multi-hypervisor edge profile.

That success creates a product-design risk. O3K could accidentally stop at
"smaller OpenStack" or split into different products for small edge deployments
and larger datacenter deployments.

The intended product is broader and simpler:

> **O3K is one Cloud Operating System from edge to datacenter. A small office
> server room, a customer-owned datacenter cage, and a large datacenter cloud
> use the same cloud authority, APIs, resource model, IAM, service catalog,
> automation, and execution contracts. Capacity and failure domains are added
> as building blocks instead of forcing a replatform.**

The word "same" is the important invariant. It does **not** mean that a three-
host edge deployment and a thousand-host datacenter must have identical process
counts, database topology, scheduler internals, or network realization. Large
scale may require cells, sharding, hierarchy, regional partitioning, or other
internal scale mechanisms. Those mechanisms must remain implementation details
behind the same O3K product contracts rather than creating a second cloud
architecture for customers to migrate to.

OpenStack compatibility has a second strategic purpose beyond client/API
compatibility. It is also an ecosystem compatibility layer. A deployment should
be able to select specialized upstream OpenStack services such as Octavia,
Designate, Barbican, Manila, or other services when their dependency contracts
are proven against O3K, without requiring O3K to reimplement every service.

## Decision

### 1. O3K is one operating system across the scale continuum

O3K SHALL target the following continuum with one product architecture:

```text
small office / branch / factory
        -> customer-owned server room
        -> dedicated datacenter cage
        -> multi-rack private cloud
        -> datacenter-scale cloud
```

Moving along this continuum must not require changing the tenant resource model,
IAM model, public IDs, API family, Terraform/OpenTofu model, Araf product model,
or service-discovery model merely because the deployment became larger.

Evidence and supported limits remain profile-specific. A proven 10–20-host edge
profile is not evidence for 100, 1,000, or more hosts.

### 2. The deployment building block is capability- and failure-domain based

O3K defines a **deployment building block** as an independently joinable unit of
capacity and capability with explicit failure-domain identity.

A building block may contribute one or more of:

- compute execution capacity;
- network execution capability;
- storage execution capability;
- control-plane participation where the selected deployment profile requires it;
- region/availability/failure-domain identity;
- service capabilities used by placement and catalog discovery.

A building block is **not** a mandated server SKU, fixed rack layout, or promise
that every process runs on every node. The exact physical composition belongs to
a deployment profile.

The architectural rule is:

> **Scale by adding or partitioning building blocks while preserving O3K cloud
> semantics. Do not require customers to replace the cloud architecture as they
> grow.**

### 3. Large-scale internal topology may evolve without becoming a new product

Datacenter scale may require implementation mechanisms such as:

- scheduler partitions or cells;
- control-plane sharding;
- database partitioning or replicated PostgreSQL topologies;
- regional or failure-domain work ownership;
- hierarchical capacity aggregation;
- network/fabric scale mechanisms different from the first small-edge provider;
- storage providers appropriate to larger failure domains.

These are allowed and expected when evidence requires them.

They must not silently introduce a second canonical resource model, a second IAM
model, incompatible APIs, or an edge-to-datacenter migration boundary inside
O3K itself.

### 4. The service catalog is a product composition mechanism

The O3K service registry/catalog SHALL support operator-selected cloud
capabilities.

A deployment may expose a minimal profile such as:

```text
Identity
Image
Compute
Network
Volume
```

and may add selected capabilities such as:

```text
Load Balancing
DNS
Secrets
Object Storage
Metering
Kubernetes
Database
AI/GPU services
```

A catalog entry must identify its authority/ownership mode. At minimum the
architecture distinguishes:

- **native O3K service** — O3K owns canonical resource state and lifecycle;
- **external-hosted service** — an independently operated service owns its own
  service state while authenticating/discovering/consuming selected O3K
  compatibility surfaces;
- future delegated/federated modes only where separately specified.

The catalog must never present an external-hosted service as an O3K-native
implementation.

### 5. OpenStack compatibility is also an ecosystem extension boundary

Before implementing a specialized cloud service natively, O3K SHOULD evaluate
whether an upstream OpenStack service can be hosted against the existing O3K
compatibility surface.

Examples include Octavia, Designate and Barbican. Such integration is not
assumed to be plug-and-play. Each service requires a bounded dependency-contract
discovery and conformance profile covering the exact Keystone/Nova/Neutron/
Glance/Placement/Cinder or other behavior it consumes.

The default sequence is:

```text
real upstream service
-> discover exact dependency calls and semantics
-> classify existing O3K compatibility coverage
-> implement only bounded missing compatibility/canonical capability
-> prove real service lifecycle, restart, failure and cleanup
-> publish the service in the catalog only for the proven profile
```

This preserves OpenStack ecosystem value without recreating OpenStack's internal
service topology inside O3K.

### 6. Fast edge adoption is a product target, not an unmeasured claim

O3K SHALL optimize for very fast bootstrap on prepared infrastructure: initialize
an O3K control plane, select a service profile, enroll prepared building blocks,
and expose O3K/OpenStack-compatible endpoints with minimal operator steps.

Marketing language such as "in seconds" may be used only after a repeatable
profile-specific benchmark proves the measured boundary being claimed.

A control-plane bootstrap measurement must not be presented as the time required
to provision physical servers, switches, Ceph clusters, external databases,
external OpenStack services, images, certificates, BGP, or other dependencies
that were pre-existing or prepared separately.

### 7. Edge and datacenter are evidence profiles, not separate product identities

"Small edge cloud" remains a valid bounded evidence/support profile. It is the
first proven rung of the end-to-end scale model, not the final architectural
ceiling of O3K.

Future larger profiles SHALL publish exact tested limits and topology, for
example host count, control-plane topology, database profile, provider topology,
failure domains, capacity and latency budgets, and required external
dependencies.

No larger-scale claim follows automatically from the small-edge profile.

### 8. Araf and automation stay continuous across scale

The intended operator/tenant continuity is:

- the same Araf product model;
- the same O3K native API family;
- the same selected OpenStack compatibility contracts;
- the same IAM/AuthContext semantics;
- the same resource ownership and operation model;
- the same Terraform/OpenTofu compatibility approach;
- capability-driven UI/catalog changes instead of separate edge/datacenter
  applications.

This ADR does not require multi-cloud aggregation in Araf. It requires that an
O3K deployment does not become a different product merely because it grows.

## Product model

```text
                           O3K CLOUD OS

                 same IAM / API / resource model
                 same operations / service catalog
                 same automation / execution contracts
                                |
        +-----------------------+-----------------------+
        |                       |                       |
     EDGE BLOCKS            CAGE / RACK BLOCKS      DC-SCALE BLOCKS
        |                       |                       |
  few prepared hosts      larger failure domains   cells/shards if needed
        |                       |                       |
        +-----------------------+-----------------------+
                                |
                     one O3K cloud architecture
```

Service composition is orthogonal to physical size:

```text
Core native O3K
  + optional native O3K services
  + proven external-hosted OpenStack services
  = operator-selected service catalog
```

## Consequences

### Positive

- O3K has a clear product identity beyond "smaller OpenStack".
- Edge installations can grow without a planned product migration to a separate
  datacenter architecture.
- Scale-specific internals remain free to evolve behind stable contracts.
- The service catalog becomes a first-class composition mechanism.
- OpenStack compatibility gains strategic value as an ecosystem extension layer,
  not only a client-compatibility feature.
- Specialized services can be reused from upstream where that is cheaper and
  operationally sound.
- Araf, OpenStack clients, SDKs and Terraform/OpenTofu can remain stable as
  infrastructure grows.

### Negative

- Preserving semantic continuity across radically different scales is a strong
  constraint on future architecture changes.
- Datacenter scale will still require substantial new implementation and real
  evidence; the building-block model does not remove distributed-systems work.
- Hosted OpenStack services can reintroduce operational dependencies such as
  service databases and message buses, so their footprint must be explicit.
- A customizable service catalog increases compatibility/version/support matrix
  complexity.
- "Edge to datacenter" can be over-marketed unless every scale claim remains
  evidence-bound.

## Rejected alternatives

### Separate O3K Edge and O3K Datacenter products

Rejected because customers should not need to replatform when a successful edge
installation grows into a larger private cloud.

### Require identical process topology at every scale

Rejected because this would confuse product continuity with implementation
uniformity and would prevent necessary sharding, cells, HA and scale-specific
provider designs.

### Reimplement every useful OpenStack service in Rust

Rejected because OpenStack compatibility is deliberately valuable as an
ecosystem boundary. Native implementation should be chosen only when it creates
a material architectural, operational, performance or product advantage.

### Ship one fixed service distribution

Rejected because an office edge cloud, an AI/GPU edge installation, a sovereign
private cloud and a large datacenter may require different capabilities. The
catalog must be explicit and composable.

### Treat a service-catalog entry as proof of implementation

Rejected. Catalog exposure is allowed only for a declared ownership mode and a
versioned, evidence-backed support profile.

## Required follow-up

- SPEC-0038 defines the enforceable building-block, scale-continuity, catalog and
  claim requirements;
- product/profile documentation must describe the small-edge profile as the
  first bounded scale rung rather than O3K's architectural maximum;
- future scale work must preserve the same Cloud Kernel/public-contract model
  even if cells, sharding or hierarchical scheduling are introduced;
- hosted-service profiles should use black-box dependency-contract discovery in
  the style proven by the OpenTofu/OpenStack compatibility work;
- release and benchmark documentation must separate fast O3K bootstrap from
  external infrastructure provisioning time;
- README/product wording should present "one Cloud OS from edge to datacenter"
  as product direction while keeping current support claims evidence-bound.
