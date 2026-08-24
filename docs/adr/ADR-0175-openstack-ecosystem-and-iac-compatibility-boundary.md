# ADR-0175 — OpenStack Ecosystem and Infrastructure-as-Code Compatibility Boundary

Status: Proposed
Date: 2026-08-24
Supersedes: none
Superseded-by: none
Affected-services: governance, cloud-kernel, identity, image, compute, network, volume, api, compatibility

Related issue: P13 — Ecosystem Compatibility & Infrastructure as Code

Related decisions and specifications:

- [ADR-0160 — Service topology and execution boundaries](ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0163 — Product profiles and deployment posture](ADR-0163-product-profiles-and-deployment-posture.md)
- [ADR-0165 — O3K Cloud Operating System and shared Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility boundary](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169 — Native persistent storage and the O3K storage boundary](ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [ADR-0173 — Native O3K Resource API and Resource Model](ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [ADR-0174 — Service Manifest and Resource Provider/Controller Architecture](ADR-0174-service-manifest-and-resource-provider-controller.md)
- [SPEC-0020 — Keystone trust, catalog, and auth context](../specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021 — Cross-service workflows and compensation](../specs/SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022 — Service API baseline and evidence gates](../specs/SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024 — Product profiles and claims](../specs/SPEC-0024-product-profiles-and-claims.md)
- [SPEC-0025 — Rust rewrite and architecture convergence](../specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md)
- [SPEC-0030 — Native O3K Resource API v1](../specs/SPEC-0030-native-o3k-resource-api-v1.md)
- [SPEC-0031 — O3K Service Extension and Controller v1](../specs/SPEC-0031-service-extension-controller-v1.md)
- [IaC OpenStack compatibility profile v1 contract](../../contracts/iac-openstack-profile-v1.yaml)

## Context

P12 established the native O3K resource API, service framework, and external
controller protocol. O3K now exposes a first-class native API alongside the
existing OpenStack-compatible surface, and both invoke the same canonical Cloud
Kernel authority.

The next ecosystem milestone is Infrastructure as Code (IaC) compatibility.
Terraform and OpenTofu are the dominant tools for managing cloud infrastructure
in the OpenStack ecosystem. Existing OpenStack Terraform/OpenTofu configurations
that use the resources and attributes declared by the
`p13-iac-compatibility-v1` profile should be able to target O3K through the
standard, unmodified `terraform-provider-openstack` provider while all resulting
cloud resources remain canonical O3K resources and all OpenStack concepts remain
compatibility projections.

This creates several architectural challenges:

1. **Client authority.** Terraform is a desired-state management tool with its
   own state file, idempotency semantics, and retry behavior. It must not
   become a second control-plane authority.

2. **Operation semantics.** Terraform expects specific OpenStack API behaviors
   (create returns immediately with an ID, read returns the full current state,
   update may replace in-place or destroy-recreate). These must be mapped to
   O3K's durable operation semantics.

3. **Resource identity.** Terraform stores resource IDs in state and uses them
   for subsequent read/update/delete operations. O3K must preserve stable
   identity across restarts, reconciliation, and provider changes.

4. **Compatibility projection.** OpenStack resources that Terraform manages
   (networks, ports, instances, volumes) are compatibility projections onto
   canonical O3K resources. The mapping must be transparent and stable.

5. **Provider selection.** The standard `terraform-provider-openstack` is an
   unmodified upstream provider. It communicates with O3K through the same
   OpenStack-compatible API surface that OpenStack CLI and SDK use.

6. **State convergence.** Terraform refresh, import, and drift detection require
   the O3K API to return state that accurately reflects the actual resource
   state, not just the desired state.

This ADR establishes the architecture boundary for IaC compatibility before any
runtime implementation begins.

## Decision

### 1. Terraform/OpenTofu is a client, not a control-plane authority

Terraform and OpenTofu (collectively "IaC tools") are clients of O3K's
OpenStack-compatible API surface. They do not own:

- O3K resource IDs;
- owner/security scope;
- desired state;
- scheduling;
- authorization;
- durable operations;
- reconciliation;
- provider selection;
- backend state.

O3K continues to own all cloud authority. The IaC tool's state file is
client-side desired-state bookkeeping. O3K must continue to work correctly if:
the state file is lost, the client disappears, a second client reads the same
resource, or a resource is created natively and imported later.

### 2. One canonical authority

OpenStack compatibility and native O3K API must invoke the same application/
domain authority. There is no synchronization between duplicate databases. A
resource created through the native API is visible through the OpenStack
compatibility surface and vice versa, because both projection paths read from
the same canonical store.

This is already the architectural direction established by SPEC-0025 and proven
by P12. P13 adds no second authority.

### 3. Standard provider, unmodified

The ecosystem claim requires the standard upstream
`terraform-provider-openstack/openstack`. No O3K fork or patch is permitted.

If upstream provider behavior exposes a genuine unsupported OpenStack operation,
either:

- add the operation through the accepted O3K compatibility profile; or
- explicitly classify the Terraform resource as unsupported in the profile
  contract.

The standard provider must remain unmodified.

### 4. No terraform-provider-o3k in P13

A first-party native O3K Terraform provider is explicitly out of scope for P13.
It may become a future milestone only for genuinely O3K-native capabilities
that cannot be represented correctly through OpenStack semantics.

### 5. Operation-level compatibility

P13 does not claim "Nova compatible", "Neutron compatible", "Cinder compatible",
or "OpenStack compatible" without profile qualification. All claims are
resource/operation-level and evidence-backed.

The profile contract (SPEC-0032 and `contracts/iac-openstack-profile-v1.yaml`)
lists every supported Terraform resource, its required OpenStack API operations,
the evidence tier, and known deviations.

### 6. Projection identity

OpenStack resources that are compatibility projections map to canonical O3K
identities as follows:

- **One-to-one semantic mapping (preferred):** Where a real one-to-one mapping
  exists between an OpenStack resource and an O3K domain resource, the
  canonical O3K resource ID is reused. The OpenStack ID is the same value as
  the O3K resource ID. Examples: `OS::Nova::Server` → `compute:server`,
  `OS::Glance::Image` → `image:image`, `OS::Neutron::Network` →
  `network:network`.

- **Compatibility-projection identity:** Where an OpenStack compatibility
  resource has lifecycle identity that does not correspond one-to-one to an O3K
  domain resource, a bounded compatibility-projection identity/mapping is
  defined. This mapping is part of the compatibility adapter, not the canonical
  O3K model. Example: Neutron security-group rules projected from canonical
  NetworkPolicy semantics.

Compatibility projection metadata is not a second cloud authority. It is
local to the compatibility adapter and does not pollute the canonical O3K
resource model.

### 7. Network translation

Neutron compatibility resources map to canonical O3K Network semantics as
follows:

| Terraform resource | O3K canonical mapping | Projection type |
|---|---|---|
| `openstack_networking_network_v2` | Canonical `network` resource | One-to-one |
| `openstack_networking_subnet_v2` | Canonical subnet/AddressRealm | One-to-one (lifecycle/cardinality TBD in P13.1) |
| `openstack_networking_port_v2` | Canonical endpoint/port resource | One-to-one |
| `openstack_networking_secgroup_v2` | Canonical NetworkPolicy (projection) | Compatibility mapping |
| `openstack_networking_secgroup_rule_v2` | Canonical NetworkPolicy rule (projection) | Compatibility mapping |
| `openstack_networking_router_v2` | AddressRealm gateway/route (projection) | Compatibility mapping; full lifecycle identity TBD in P13.1 |
| `openstack_networking_router_interface_v2` | Route/endpoint (projection) | Compatibility mapping; TBD in P13.1 |
| `openstack_networking_floatingip_v2` | Canonical PublicAddress resource | One-to-one |

New canonical O3K domain concepts (NeutronRouter, SecurityGroup, FloatingIp)
are NOT introduced. Compatibility projections live in the adapter layer.

For Neutron router and router-interface, the compatibility projection has
durable identity and lifecycle semantics that do not map one-to-one to any
single existing O3K canonical resource. The exact mapping (persistent
compatibility-only state vs. future canonical L3 Gateway/Router resource) must
be frozen in P13.1 provider contract discovery before P13.3 implementation.

For subnet, the one-to-one mapping to an AddressRealm/subnet is the target, but
the lifecycle/cardinality must be verified against real provider behavior in
P13.1 before being frozen.

### 8. Storage translation

ADR-0169 remains authoritative. Native O3K Volume is the authority. Cinder is a
northbound compatibility projection only.

External-hosted Cinder remains a separate service-testbed authority profile and
MUST NOT be confused with native Volume's Cinder projection.

For the initial P13 storage profile:

- `openstack_blockstorage_volume_v3` maps to canonical O3K `volume:volume`
  where the native Volume domain is active.
- `openstack_compute_volume_attach_v2` maps to canonical
  `volume:volume_attachment`.
- In the external-hosted Cinder testbed profile, these operations delegate to
  the external Cinder service through the existing adapter.

### 9. Operation semantics and the OpenStack compatibility boundary

Terraform provider behavior does not replace O3K's durable operation semantics.

O3K preserves internally:

- durable acceptance before external side effects;
- canonical Operation identity;
- generation fencing;
- unknown-outcome semantics on timeout;
- observe-before-retry;
- compensation on failure.

**O3K Operations must remain internal to the Cloud Kernel.** The OpenStack
compatibility API layer must expose exactly the status code(s), body,
headers, and polling semantics accepted by the pinned upstream provider /
Gophercloud call path for each resource operation.

In practice this means:

`POST /servers` -> O3K internally establishes the canonical server identity
and a durable Operation -> the compatibility layer returns a standard Nova
server response containing the canonical server ID (`status: BUILD`) ->
the provider polls `GET /servers/{id}` -> O3K projects internal state as
BUILD/ACTIVE/ERROR following standard OpenStack status transitions.

The status code returned for each operation MUST match what the pinned
`terraform-provider-openstack` (v3.4.0, backed by Gophercloud v2.8.0)
expects:

- `compute/v2/servers.Create` accepts `OkCodes: []int{200, 202}` — either
  code is valid, but the response must be a standard Nova server body (never
  an O3K Operation representation).
- `blockstorage/v3/volumes.Create` accepts **only** `OkCodes: []int{202}` —
  this is the standard OpenStack behavior and must be echoed by the O3K
  compatibility layer.
- Most Neutron v2.0 operations accept `200`/`201`.

The client must never depend on the O3K native Operation API. A `202` where
it occurs is still a standard OpenStack `202` with the resource ID and
initial status in the body, not an O3K Operation reference.

Exact per-operation status codes belong in the P13.1 behavioral contract.
This ADR does not freeze a generic `200/201` rule.

#### Idempotency boundary — internal execution vs. client-transport guarantees

O3K guarantees internal idempotency: for one accepted canonical Operation, O3K
will not duplicate the provider-side effect, even after timeout or restart.

O3K does NOT guarantee client-level exactly-once creation when an upstream
client loses the response. Example: O3K commits Server A and returns the
canonical ID in a standard Nova response. If the response is lost in transit,
Terraform has no ID in state. A later apply issues another create. O3K cannot
safely infer that this is the same intent vs. a legitimate second server merely
from identical fields/name — the OpenStack protocol has no durable client token
or idempotency key at this level.

Do not add unsafe retry behavior, server-name deduplication, or blind
replay-detection logic solely to appease a client. O3K's safety properties
take precedence over Terraform's retry expectations.

### 10. Terraform state

Terraform/OpenTofu state is client-side desired-state bookkeeping. It is not
authoritative cloud state. O3K must continue to work correctly if:

- the state file is lost;
- the client disappears mid-operation;
- a second client reads the same resource;
- a resource is created natively and imported later via `terraform import`.

### 11. Tool baseline

The P13 profile freezes:

| Tool | Version | Role |
|---|---|---|
| `terraform-provider-openstack` | v3.4.0 | Standard upstream provider, unmodified |
| OpenTofu | v1.12.6 | Mandatory default IaC engine |
| Terraform CLI | latest (secondary; unverified until P13 evidence exists) | Secondary test target |

OpenTofu is the default mandatory executable IaC engine. Terraform CLI
compatibility may be tested where legally and operationally appropriate, but
P13 never requires vendoring, redistributing, or modifying proprietary or BSL
artifacts merely to claim OpenStack provider compatibility.

Lockfiles and checksums are handled by the standard OpenTofu/Terraform
workflow. Future provider versions are tested when the P13 profile is updated.

The verified version for the P13 release gate is v3.4.0. Later versions are
"reference/latest" and are tested separately.

### 12. Security

IaC compatibility must not weaken:

- project isolation: Terraform operations are scoped to the authenticated
  project, same as any OpenStack client;
- resource reference authorization: reading one resource does not leak metadata
  about another project's resources;
- IDOR/BOLA defenses: every read, mutation, and reference lookup is
  authorized against the durable owner/project scope. Non-guessability is
  not an authorization control. Security must hold even when an attacker
  knows another project's valid resource ID. Foreign-resource existence and
  metadata are not disclosed beyond the defined disclosure policy;
- operation isolation: one Terraform run cannot observe or mutate another's
  in-progress operations;
- idempotency isolation: idempotency keys are scoped to the authenticated
  principal;
- secret handling: provider passwords, private keys, and user data are never
  logged or exposed in API responses;
- provider-side-effect ordering: Terraform's implicit ordering does not bypass
  O3K's dependency validation.

### 13. Product profile

A named P13 IaC profile is defined in SPEC-0032 and
`contracts/iac-openstack-profile-v1.yaml`.

The profile remains separate from:
- broad OpenStack parity;
- external-Cinder testbed;
- production SLA;
- arbitrary large-scale cloud;
- multi-region;
- native Terraform-provider support.

## Consequences

1. O3K's existing OpenStack compatibility API surface is the IaC compatibility
   surface. No new API routes specific to Terraform are needed; the same
   Keystone, Glance, Nova, Neutron, and Cinder-compatible endpoints serve both
   CLI and IaC clients.

2. Some operations that work for CLI may need additional error/status
   information to satisfy Terraform's polling and error-handling expectations.
   These are additive and backward-compatible.

3. The native O3K API is not an IaC compatibility surface. Terraform and
   OpenTofu always target the OpenStack compatibility API.

4. Resource import (`terraform import`) requires that the O3K OpenStack
   compatibility layer supports GET-by-ID for every managed resource type.
   Most already do.

5. Terraform's refresh and drift detection rely on the O3K API returning actual
   observed state. The existing observation/reconciliation loop already provides
   this; no separate "read-only refresh mode" is needed.

## Non-goals

- `terraform-provider-o3k` native provider;
- Pulumi support;
- Ansible modules;
- tenant web UI;
- metering/billing;
- DNS/Designate;
- LBaaS/Octavia;
- Swift/S3;
- Kubernetes-as-a-service;
- Trove production service;
- arbitrary OpenStack endpoint parity;
- live migration;
- automatic unfenced evacuation;
- new hypervisors;
- XCP-ng;
- Proxmox;
- SR-IOV;
- DPDK;
- multi-region;
- provider/dataplane redesign;
- production SLA claims.
