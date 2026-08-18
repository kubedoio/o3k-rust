# ADR-0168 — O3K Routed Fabric and node-local network execution

Status: Proposed
Date: 2026-08-18
Supersedes: none
Superseded-by: none
Affected-services: network, compute, kernel, api, store, governance, edge

Related issue: [#654](https://github.com/kubedoio/o3k-rust/issues/654)

Related decisions and specifications:

- [ADR-0160 — service topology and execution boundaries](ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0163 — product profiles and deployment posture](ADR-0163-product-profiles-and-deployment-posture.md)
- [ADR-0165 — O3K Cloud OS and Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [SPEC-0021 — cross-service workflows and compensation](../specs/SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0024 — product profiles and claims](../specs/SPEC-0024-product-profiles-and-claims.md)
- [SPEC-0026 — O3K Routed Fabric v1](../specs/SPEC-0026-o3k-routed-fabric-v1.md)
- [Execution-boundary contract](../../contracts/execution-boundaries.md)

This is a high-risk architecture and privileged-execution proposal. It must not
be treated as accepted merely because an agent authored it, CI passes, or an
issue/PR is merged. Human maintainer approval is required before its status may
become `Accepted`.

## Context

The first native O3K TestLab intentionally implemented only the minimum network
behavior required to boot a real libvirt guest: project-owned networks,
subnets, ports, deterministic fixed IP/MAC allocation, a host bridge/TAP, DHCP,
and strict foreign-state ownership fences.

That foundation is sufficient for a TestLab but not for a useful tenant cloud.
The next product milestone needs real north/south connectivity, controlled
Internet egress, public/floating addresses, routing, and stateful network
policy. O3K must add those capabilities without accidentally recreating the
historical internal architecture of Neutron or permanently binding its network
domain to one Linux dataplane technology.

ADR-0160 already establishes the target host execution topology and explicitly
allows a later ADR to activate `o3k-network` when privilege separation, failure
containment, deployment locality, or independent lifecycle justifies the
process boundary. ADR-0165 establishes that OpenStack objects are compatibility
projections rather than O3K's canonical internal domain.

P9 is the point at which the network execution privilege set becomes materially
different from compute execution. Routing, NAT, public-address advertisement,
firewall/policy realization, forwarding controls, neighbor state, and later
fabric functions require host network authority that should not remain bundled
with libvirt/QEMU lifecycle merely because the first TestLab reused the compute
agent for minimum TAP/bridge behavior.

The design must also preserve a credible path to later dataplanes. O3K should be
able to add an eBPF realization, OVN/OVS, EVPN/VXLAN/Geneve, WireGuard, BGP,
SR-IOV or other network providers where product profiles justify them without
rewriting tenant resource identity, authorization, operations, quotas, or
northbound APIs.

## Decision proposal

### 1. O3K owns technology-independent network intent

The canonical O3K Network domain describes **connectivity intent**, not a
specific packet-processing technology and not Neutron's implementation model.

Conceptually the domain may contain types such as:

```text
Network / ConnectivityDomain
AddressRealm
Segment
Prefix
AddressPool
Endpoint
Attachment
RouteIntent
GatewayIntent / EgressIntent
PublicAddressBinding
NetworkPolicy
PolicyRule / Selector
QoSPolicy                         # later profile when selected
```

The exact v1 fields and lifecycle requirements are frozen by SPEC-0026. Names
used by OpenStack compatibility adapters do not become mandatory names for
canonical O3K resources.

The canonical domain must not depend on or persist implementation-specific
objects such as:

```text
nftables rules or handles
iptables syntax
eBPF programs or BPF map layout
OVS bridges/flows
OVN logical-flow/database rows
VXLAN/Geneve VNIs
WireGuard peer configuration
BGP daemon configuration
raw `ip`/`tc` command lines
```

Provider-native identity needed for observation/cleanup is stored only as a
bounded provider mapping, never as the public O3K resource identity.

### 2. `o3kd` remains the network control authority

For O3K-owned networks, `o3kd` and the durable Cloud Kernel state remain
authoritative for:

- public O3K network/resource IDs;
- project/security ownership;
- authorization decisions;
- quota/limit decisions where selected;
- address and public-address allocation intent;
- desired connectivity, route, gateway, NAT, and policy state;
- endpoint/host binding intent;
- operation identity and workflow phase;
- compensation and reconciliation decisions;
- compatibility projection and public errors;
- audit/event identity.

Network execution never authorizes a tenant, allocates a different public O3K
resource identity, changes project ownership, or reschedules a workload on its
own.

### 3. P9 activates a separate node-local `o3k-network` process

P9 activates the future execution boundary anticipated by ADR-0160.

`o3k-network` runs on a host that realizes O3K networking and owns only bounded
host-local execution and observation, including the selected profile's:

- endpoint/TAP attachment;
- local routing and neighbor realization;
- DHCP realization where selected;
- forwarding state;
- NAT state;
- stateful network-policy enforcement;
- public-address local realization/advertisement where selected;
- cleanup/reconciliation of O3K-owned network resources;
- capability and health reporting.

The process is justified by a distinct privilege and failure domain. It must be
possible to give network execution the minimum host networking privileges
without giving it libvirt/QEMU lifecycle authority, and to give `o3k-compute`
libvirt/storage-artifact privileges without making it the long-term owner of
routing/NAT/policy state.

`o3k-network` is **not a mini-Neutron**. It has no tenant-facing API, independent
cloud database, scheduler, project authorization model, public resource-ID
allocator, or independent desired-state authority.

The first extraction must preserve the accepted TAP/bridge/DHCP ownership and
foreign-state safeguards from earlier network ADRs. Process extraction does not
weaken those invariants.

### 4. P9 is L3-first, but O3K networking is not permanently L3-only

The P9 Routed Fabric v1 product profile deliberately optimizes for a small,
understandable routed cloud:

- IPv4 first;
- routed tenant connectivity;
- controlled egress/SNAT;
- external/provider networks;
- public/floating-address binding;
- stateful L3/L4 policy;
- real guest connectivity evidence.

This is a **profile choice**, not a permanent O3K network invariant.

Canonical Network capabilities must be explicit enough to represent future
profiles with different behavior, for example:

```text
l2_adjacency: none | host | region
routing: supported | unsupported
overlapping_realms: true | false
stateful_policy: true | false
nat: true | false
public_address_advertisement: <capability>
encapsulation: none | vlan | vxlan | geneve | <future>
```

A provider may support only a subset. Capability advertisement is not support
evidence until the corresponding conformance and real-profile gates pass.

### 5. Non-overlapping address space is a P9 restriction, not an architecture rule

The first routed profile may require tenant prefixes to be non-overlapping
within its configured routed `AddressRealm`. This substantially simplifies the
initial routing, operations, troubleshooting, and evidence model.

O3K must not encode this as a global invariant across all future network
profiles.

`AddressRealm` exists so a later provider may support overlapping address space
through explicit isolation such as VRFs, overlays, OVN logical datapaths, or
another accepted mechanism. A future Neutron compatibility profile may therefore
support overlapping tenant CIDRs without redefining canonical O3K endpoint,
policy, operation, or ownership semantics.

### 6. The first dataplane is deliberately conservative Linux networking

P9's first real execution backend uses stable Linux kernel networking
primitives rather than making a custom eBPF dataplane or OVN a release
prerequisite.

The expected realization includes, as required by the selected host/profile:

- TAP or equivalent endpoint attachment;
- Linux routing/neighbor state;
- nftables and conntrack for stateful filtering/NAT;
- forwarding controls;
- proxy ARP/NDP or equivalent public-address neighbor behavior where required;
- `tc` only when a selected v1 behavior requires it.

The implementation should prefer structured kernel interfaces such as Netlink
and transactional/batched updates where practical. Safe bounded command
adapters may remain where already accepted, but raw command text is not a
canonical application contract.

This choice is intentionally operational rather than ideological: P9 should
prove the O3K network semantics using a dataplane that is easy to inspect,
recover, and debug on the target Linux hosts.

### 7. Normal P9 north/south traffic has no mandatory central network node

The default P9 data path is distributed to the host that owns the endpoint.

Conceptually:

```text
Internet / physical network
          |
       host uplink
          |
   node-local routing/NAT/policy
          |
     VM endpoint
```

Controlled VM egress and a public/floating address should not require every
packet to traverse an unrelated central Neutron-like L3 agent or dedicated
network node when the physical network and selected provider can realize the
state locally.

Some future products may require dedicated gateways, service appliances,
centralized egress, or external routers. Those are explicit provider/service
profiles, not hidden assumptions in the base network domain.

### 8. Desired state compiles to semantic per-node plans

O3K must not define the execution abstraction as Neutron-shaped provider CRUD:

```text
create_router()
create_security_group()
create_floating_ip()
```

That interface would merely move OpenStack object topology into a Rust trait.

Instead, network application state is compiled into a semantic, versioned
per-node plan such as `NodeNetworkPlan`. The plan contains only the minimum
technology-neutral intents required by the selected provider, for example:

```text
EndpointAttachment
AddressAssignment
RouteIntent
NeighborIntent
NatIntent
PolicyIntent
AdvertisementIntent
QoSIntent                 # when selected
EncapsulationIntent       # when selected by a future profile
```

Each intent carries stable resource/operation/generation/ownership identity,
not raw provider commands. `o3k-network` realizes the plan using its activated
provider and reports observations through the existing execution authority
rules.

The planner/compiler belongs above the provider boundary. Providers must not
invent connectivity policy that is absent from durable desired state.

### 9. Neutron remains a compatibility projection

P9 expands selected Neutron-compatible user outcomes, but Neutron resource names
do not define the internal process or persistence model.

Conceptual mappings include:

```text
Neutron network           -> O3K Network / connectivity domain
Neutron segment           -> O3K Segment
Neutron subnet            -> Prefix + AddressPool
Neutron port              -> Endpoint + Attachment
Neutron router            -> route/gateway intent projection
router interface          -> route attachment projection
floating IP               -> PublicAddressBinding
security group            -> NetworkPolicy
security-group rule       -> PolicyRule
address group              -> policy selector/address set
QoS policy                -> QoSPolicy when a later profile selects it
trunk                      -> future multiplexed attachment profile
```

Only operations frozen in the compatibility manifest and proven at the required
evidence tier may be advertised. Writing this ADR does not promote routers,
floating IPs, security groups, VLAN, VXLAN, OVN, or any other currently
unsupported Neutron operation to supported status.

### 10. Future eBPF and overlay providers must not require a domain rewrite

A later eBPF provider may compile the same endpoint, route, NAT, public-address,
and policy intents into BPF programs/maps and use TC/XDP or other kernel hooks.
The canonical resource model does not encode nftables so this can be done
without changing tenant identity or northbound API semantics.

A later OVN/OVS or EVPN/VXLAN/Geneve provider may support overlapping address
realms, regional L2 adjacency, trunks, VLAN transparency, distributed routing,
QoS, or other explicitly selected capabilities.

A direct `O3K -> OVN` adapter may be an execution provider when O3K remains the
resource/control authority. By contrast, `O3K -> external Neutron -> OVN` puts
another cloud networking service in the authority path and therefore requires
an explicit external/delegated authority profile rather than pretending
Neutron is equivalent to a host-local executor.

P11 may add host-level routed fabric providers such as WireGuard for a small
edge cloud and BGP route advertisement for suitable datacenter networks. P9
must preserve that evolution path but must not implement it speculatively.

### 11. Provider portability does not imply zero-disruption live dataplane migration

O3K network semantics and durable desired state should be portable between
providers when their capabilities satisfy the selected profile.

O3K does **not** promise that an active host can switch a stateful dataplane from
nftables/conntrack to an eBPF connection-tracking/NAT implementation without
flow disruption. Provider-native state may have incompatible connection state,
program/map layout, or advertisement behavior.

Any live provider migration requires a separate explicit migration contract and
evidence. A restart/reconcile using the same provider is part of P9; seamless
cross-provider migration is not.

### 12. Existing Cloud Kernel failure semantics apply to networking

Every P9 mutation crossing a failure boundary must preserve existing O3K
invariants:

- authorize before provider mutation;
- persist intent and operation phase before external side effects;
- deterministic command/idempotency identity where replay is possible;
- controller work ownership and fencing before mutating dispatch;
- target agent identity/epoch fencing;
- generation validation on observations;
- timeout means unknown outcome;
- observe before retrying potentially duplicating/destructive mutation;
- partial realization is explicit and reconcilable;
- cleanup is ownership-checked and reverse-dependency-aware;
- ambiguous/foreign routes, links, firewall objects, NAT state, address
  advertisements, processes, or configuration are never adopted or deleted;
- evidence is bounded and secret-safe.

Network-policy denial and tenant isolation must be proven through real traffic,
not inferred merely from successful rule installation.

## Consequences

### Positive

- P9 produces a useful tenant cloud capability rather than another control-plane
  abstraction milestone.
- O3K does not inherit Neutron's internal service/process topology simply to
  preserve Neutron API compatibility.
- Routing/NAT/policy privileges are separated cleanly from libvirt compute
  execution.
- The first dataplane remains small, inspectable, and operationally familiar.
- eBPF can later improve performance/identity enforcement/observability without
  changing tenant resource identity or the Cloud Kernel authorization model.
- OVN/overlay/EVPN providers remain possible for legacy L2, overlapping CIDRs,
  trunks, or larger network topologies.
- P11 can add a multi-host routed fabric without redesigning P9's tenant
  resources.

### Negative

- O3K must design and maintain its own technology-neutral network semantics and
  planner rather than delegating all meaning to Neutron/OVN.
- A separate `o3k-network` binary adds enrollment, health, upgrade, protocol,
  restart, and packaging work.
- The non-overlapping routed P9 profile will intentionally support less legacy
  network flexibility than broad Neutron deployments.
- Provider capability negotiation and conformance become important as more
  dataplanes are added.
- Stateful provider migration cannot be assumed to be transparent.

## Rejected alternatives

### Recreate Neutron services and agents internally

Rejected because public compatibility does not require reproducing Neutron's
historical process/database topology and would undermine the Cloud Kernel
architecture.

### Make OVN/OVS mandatory for P9

Rejected because it adds a substantial distributed control/dataplane dependency
before the P9 tenant semantics are proven. OVN remains a valid future provider.

### Build the first P9 dataplane directly on custom eBPF

Rejected because it would couple P9 product validation to a large amount of
kernel/dataplane engineering and operational surface. eBPF remains an explicit
future provider path.

### Permanently require globally unique tenant CIDRs

Rejected because it unnecessarily prevents future overlapping-address realms,
VRF/overlay providers, and broader Neutron compatibility. Non-overlap is only a
P9 routed-profile restriction.

### Define a provider trait by mirroring Neutron object CRUD

Rejected because it would preserve Neutron's object model as O3K's execution
architecture and make alternate dataplanes implement artificial "router" or
"security-group" objects rather than semantic connectivity intents.

### Keep routing/NAT/policy permanently inside `o3k-compute`

Rejected because P9 creates a distinct privileged networking failure domain and
ADR-0160 already anticipates separating it when justified.

### Require a central network node for all egress and floating-IP traffic

Rejected as the default because it introduces an avoidable shared traffic and
failure bottleneck. Explicit gateway profiles may be added later when required.

## Required follow-up

Before runtime implementation:

1. human-review and accept/reject this ADR and SPEC-0026;
2. keep current Neutron compatibility claims unchanged while the design is only
   proposed;
3. implement P9 under program issue #655 through coherent issue/PR slices;
4. add exact action/resource/quota vocabulary and persistent schemas with the
   first implementation slice rather than inventing them in provider code;
5. activate `o3k-network` through the accepted execution protocol with separate
   network capabilities/privilege documentation;
6. prove egress, public-address, policy, restart/reconciliation, cleanup, and
   foreign-state safety at the evidence tiers required by SPEC-0026;
7. update compatibility manifests only after the corresponding operation gates
   pass;
8. defer cross-host WireGuard/BGP/overlay fabric realization to P11 or another
   explicitly accepted profile.
