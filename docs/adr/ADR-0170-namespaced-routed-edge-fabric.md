# ADR-0170 — Namespaced Routed Edge Fabric for P11

Status: Accepted
Date: 2026-08-19
Decision-accepted: explicit acceptance by the task requester, 2026-08-19
Human-approval: task requester, explicit acceptance recorded in task instruction, 2026-08-19
Supersedes: none
Superseded-by: none
Affected-services: network, compute, placement, scheduler, storage, kernel, edge, governance

Related issue: [#701](https://github.com/kubedoio/o3k-rust/issues/701)

Related decisions and specifications:

- [ADR-0160 — service topology and execution boundaries](ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0165 — O3K Cloud OS and Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169 — native persistent storage and the o3k-storage boundary](ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [SPEC-0026 — O3K Routed Fabric v1](../specs/SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0027 — native persistent storage v1](../specs/SPEC-0027-native-persistent-storage-v1.md)
- [SPEC-0028 — Namespaced Routed Edge Fabric v1](../specs/SPEC-0028-namespaced-routed-edge-fabric-v1.md)
- [P11 edge-fabric contract](../../contracts/edge-fabric-v1.md)
- [Execution-boundary contract](../../contracts/execution-boundaries.md)

This is a privileged multi-host networking and scheduling decision. The task
requester explicitly accepted this ADR and SPEC-0028 on 2026-08-19. The
architecture is now authorized for bounded implementation; architecture text
alone is not product evidence and does not create a runtime or support claim.

## Context

P9 proved the O3K-owned routed tenant-network model on a real host: canonical
`AddressRealm`/endpoint/route/public-address/policy intent, bounded `o3k-network`
execution, Linux routing and nftables/conntrack, public/floating-address
realization, real guest packet paths, replay/restart handling, and strict
foreign-state protection.

P10 proved backend-independent native persistent storage through both host-local
LVM and shared Ceph RBD. The next product step is therefore not another generic
abstraction layer. P11 must make the existing compute, network, and storage
semantics operate coherently across a small edge cloud of roughly 10–20
hypervisors while preserving the authority and failure semantics already proven
by P7–P10.

A multi-host network introduces a specific problem that P9 intentionally did
not solve: two endpoints in one O3K subnet may be scheduled on different
hypervisors. O3K needs cross-host reachability without making VXLAN/Geneve/OVN,
BGP, or a custom eBPF dataplane mandatory. It also needs to preserve normal
same-host VM behavior, including ordinary ARP and efficient local Ethernet
forwarding, without allowing that local bridge path to bypass anti-spoofing or
NetworkPolicy.

WireGuard is attractive for a 10–20-host edge profile because it supplies a
small authenticated encrypted host transport, but WireGuard does not know O3K
project, AddressRealm, endpoint, or NetworkPolicy identity. It must therefore
never become the tenant-isolation model.

## Decision

### 1. P11 is a namespaced routed edge profile, not a regional L2 overlay

The P11 reference profile provides:

- multi-host compute placement and host lifecycle;
- same-AddressRealm local Ethernet adjacency within one hypervisor;
- same-AddressRealm routed reachability across hypervisors;
- distributed, control-plane-derived neighbor resolution for remote endpoints;
- authenticated encrypted host-to-host transport;
- existing P9 public-address, NAT, and stateful-policy semantics;
- P10 host-local/shared-storage placement constraints;
- failure-safe drain, reconnect, replay, and reconciliation.

P11 v1 does **not** claim arbitrary regional Ethernet adjacency. Ethernet
broadcast, unknown-unicast flooding, and arbitrary L2 multicast are not
extended across hypervisors. Cross-host connectivity is routed.

The capability is therefore truthful as:

```text
l2_adjacency: host
cross_host_connectivity: routed
cross_host_broadcast: false
overlapping_cross_host_cidrs: false
encrypted_host_transport: true
```

A later explicitly accepted provider may add regional L2, overlapping address
realms, VXLAN/Geneve/EVPN/OVN, VRFs, or eBPF realization without redefining
canonical tenant identity.

### 2. `AddressRealm` remains canonical; Linux netns is execution state

ADR-0168 remains authoritative: O3K owns technology-independent connectivity
intent. P11 does not add Linux namespace names, bridge names, veth names,
WireGuard peers, nftables handles, routes, or kernel device names to canonical
public resources.

For the P11 provider, an active `AddressRealm` on a host is realized using a
bounded Linux network namespace for routed realm state. The namespace is
provider-native execution state with deterministic ownership metadata/journal
binding to:

```text
realm_id
host_id
provider_namespace
resource_generation
agent_epoch / accepted fabric generation
```

A matching kernel name alone is never ownership evidence.

### 3. Preserve the existing libvirt TAP path with one host-local realm bridge

P11 does not force QEMU/libvirt into tenant network namespaces merely to obtain
isolation. The existing P9 path pre-creates host TAP/bridge state and libvirt
consumes the bounded TAP attachment. To preserve that proven path, each active
AddressRealm on a compute host has one VM-facing host-local Linux bridge.

Conceptually:

```text
host namespace

VM TAP A ----\
              +-- br-realm-X -- realm-uplink-veth
VM TAP B ----/                     |
                                  v
                           realm-X network namespace
```

The bridge and TAP names are provider mappings, not canonical IDs.

Same-realm endpoints on the same host may use normal Ethernet forwarding and
normal ARP. They are not forced through the routed realm gateway solely for the
sake of policy enforcement.

### 4. Local L2 forwarding is allowed only with endpoint-bound enforcement

Allowing local bridge forwarding must not create a path around P9 policy.
`o3k-network` therefore enforces endpoint identity and stateful policy on the
VM/TAP/bridge path as well as on routed paths.

For every VM attachment, execution state is bound to the accepted canonical:

```text
endpoint_id
realm_id
project_id
MAC
fixed IP
generation
```

The provider rejects spoofed source MAC, IPv4 source, and ARP sender identity.
Canonical `NetworkPolicy` is compiled to the provider's local bridge/TAP
realization for local traffic and to the routed realm realization for routed
traffic. The canonical policy does not encode nftables bridge/inet families or
future eBPF hooks.

A same-host packet is not considered policy-compliant merely because Linux
forwarded it successfully.

### 5. Remote ARP is answered from a distributed realm endpoint directory

P11 does not flood ARP across the host fabric.

`o3kd` already owns endpoint identity and accepted endpoint/host placement. The
P11 planner derives a `RealmEndpointDirectory` (name is descriptive; exact
internal type may differ) from canonical endpoint state and accepted host
binding. It is derived desired/planner state, not a new tenant-visible resource
and not an independent authority service.

For each participating host the derived directory distinguishes:

```text
local endpoint:  realm + IP + MAC + endpoint ID + generation
remote endpoint: realm + IP + endpoint ID + target host + generation
```

When a VM ARPs for another active endpoint in the same AddressRealm:

- if the destination endpoint is local, the real local VM answers normally;
- if the destination endpoint is remote, local `o3k-network` answers on its
  behalf using the realm proxy MAC;
- if the endpoint is absent, stale, deleting, belongs to another realm, or has
  an unaccepted generation/placement, O3K must not synthesize an ARP reply.

The guest ARP cache is demand-driven; O3K does not require guest modification or
proactive injection into guest ARP tables.

### 6. Each AddressRealm has a deterministic local proxy MAC

Remote same-realm endpoints resolve to a deterministic locally administered
unicast **AddressRealm proxy MAC**, derived from stable realm identity through a
versioned collision-safe provider algorithm.

The proxy MAC is an execution-derived value. It is not the remote VM's real MAC
and does not imply cross-host Ethernet forwarding.

The same logical realm proxy MAC may be used on each participating host because
the local realm L2 islands are not one shared regional Ethernet segment.

Example guest view:

```text
10.40.1.11 -> 02:00:00:00:00:11   # local endpoint actual MAC
10.40.1.12 -> <realm-proxy-mac>    # remote endpoint
10.40.1.13 -> <realm-proxy-mac>    # remote endpoint
```

Returning a remote VM's actual MAC is rejected for P11 because the selected
fabric transports IP, not arbitrary Ethernet frames; doing so would require an
L2 overlay/FDB mechanism that P11 intentionally does not implement.

### 7. Remote endpoints are routed as host-location-specific endpoint routes

P11 does not allocate one tenant subnet per hypervisor. Endpoints from one
subnet may be placed on different eligible hosts.

The planner derives semantic fabric routing from accepted endpoint placement.
The first provider realizes remote endpoints with host routes, normally `/32`
IPv4 routes:

```text
10.40.1.10/32 -> host-01
10.40.1.11/32 -> host-07
10.40.1.12/32 -> host-03
```

This is execution state. Canonical Network/Subnet/AddressRealm allocation does
not become host-topology-dependent.

When a guest sends a frame to the realm proxy MAC, the local realm namespace
routes by destination IP into the host fabric. The remote host routes the IP
packet to the corresponding realm and destination endpoint.

### 8. One shared routed host fabric per compute host

P11 uses one shared O3K host fabric per compute host, not one WireGuard
interface, key, or tunnel per tenant/AddressRealm.

The reference realization uses a dedicated `o3k-fabric` Linux network namespace
for cleartext fabric routing and one WireGuard interface. Realm namespaces
connect to the fabric namespace through bounded provider-owned veths. The
physical underlay remains in the initial host namespace.

WireGuard may be created in the initial namespace and moved into the fabric
namespace so its UDP transport socket remains bound to the underlay while the
cleartext WireGuard interface and fabric routing stay isolated from unrelated
host networking.

The canonical model knows only semantic host/fabric capability and endpoint
location. It does not persist WireGuard configuration as tenant networking
state.

### 9. WireGuard provides transport security, not tenant isolation

The security responsibilities are intentionally separate:

```text
AddressRealm/netns + realm bridge boundaries = routing/isolation scope
NetworkPolicy + anti-spoofing                 = authorization/enforcement
Routed host fabric                            = reachability
WireGuard                                     = authenticated encryption
```

The fabric namespace defaults to fail-closed forwarding. A realm-to-fabric
packet is accepted only when source endpoint/realm identity and current plan
allow it. A received fabric packet is delivered only to the realm that owns the
current destination endpoint. Ordinary Linux route presence is insufficient to
authorize cross-realm traffic.

P11 v1 does not create implicit cross-realm routing. Any later cross-realm
service/gateway behavior must come from explicit canonical route/gateway intent
and matching policy.

### 10. Host fabric identity is fenced and private keys remain host-local

The WireGuard private key is generated and stored on the host by the bounded
network/fabric executor. It never enters:

- canonical Cloud Kernel tenant state;
- SQLite/PostgreSQL public resource records;
- public APIs;
- audit/events;
- normal logs;
- CI evidence artifacts.

The control plane may durably distribute bounded public fabric identity:

```text
host_id
fabric_provider
public_key
underlay_endpoint
fabric_generation
capabilities / MTU bounds
```

Host re-enrollment or key rotation creates a new accepted fabric generation and
fences stale peer identity. A stale peer/key/agent generation must not continue
to receive newly accepted endpoint routes.

WireGuard `AllowedIPs` may be used by the provider to bind endpoint `/32`s to
current peer hosts and to reject authenticated packets whose source addresses
are not assigned to that peer. `AllowedIPs` remains provider execution state,
not canonical Network identity.

### 11. P11 v1 requires a routable underlay and non-overlapping fabric addresses

The P11 reference profile requires mutually usable UDP/IP reachability between
configured host fabric endpoints. STUN, TURN, relay services, arbitrary NAT
traversal, and public rendezvous are outside P11.

Linux netns gives strong local routing separation, but one shared IP-routed
WireGuard fabric cannot distinguish two realms that present the same endpoint
IP without an additional realm identifier/encapsulation. Therefore P11 retains
the P9 routed-profile restriction that endpoint prefixes are non-overlapping
across the shared fabric.

This is a **profile restriction**, not a permanent O3K invariant. Future
VRF/encapsulation/OVN/EVPN/eBPF providers may support overlapping realms.

### 12. Neighbor changes are generation-bound and explicitly converged

The remote/local classification of an endpoint may change after a legitimate
cold relocation or recreation. O3K must not wait indefinitely for stale guest
neighbor caches.

After an accepted endpoint-location generation changes, the provider performs a
bounded neighbor-convergence action appropriate to the profile, such as
invalidating owned proxy-neighbor state and issuing a safe gratuitous ARP/ARP
announcement where required. Convergence actions are derived execution state
and must not spoof a foreign endpoint.

Live migration is not required by P11, but cold placement changes must converge
without requiring guest reconfiguration.

### 13. MTU is part of the fabric correctness contract

WireGuard and namespace/veth traversal add packet overhead and an additional
path. The provider derives an effective tenant MTU from the configured/detected
underlay and fabric overhead, publishes only the bounded capability needed by
planning, and propagates the selected MTU through the existing guest network
configuration path.

The exact kernel/provider MTU is not canonical tenant identity. P11 acceptance
requires real packet evidence near the supported MTU boundary; successful small
pings are insufficient.

### 14. Existing P9 north/south semantics remain distributed

P11 does not introduce a central Neutron-like gateway node. External egress,
SNAT, public/floating-address realization, and stateful policy remain bound to
the accepted P9 authority model and are realized on the endpoint-owning host or
other explicitly selected provider location.

Namespace/fabric work must integrate with existing P9 NAT/public-address
execution without creating a second desired-state authority or silently
changing advertised Neutron behavior.

### 15. Multi-host placement consumes capabilities and storage locality

P11 reuses the existing authenticated agent inventory, administrative state,
Placement allocations, scheduler, durable operations, controller work leases,
fencing tokens, agent epochs, and reconciliation machinery.

New placement must reject hosts that are administratively draining/disabled,
unavailable, missing required network/fabric capability, outside selected
failure/availability constraints, or unable to satisfy storage placement.

P10 placement semantics are consumed as typed capability/constraint data:

- host-local LVM constrains the workload to the owning eligible host;
- shared Ceph RBD may be attached on another eligible host only after previous
  attachment ownership is cleanly terminated or otherwise proven safe.

The scheduler must not branch on backend brand names where the existing typed
placement/capability model can express the constraint.

### 16. Drain is supported; blind automatic evacuation is not

Entering `Draining` immediately excludes the host from new placement. Existing
workloads continue to run and become explicit blockers together with host-local
storage/attachments or other non-relocatable state. P11 does not require live
migration, forced relocation, or storage migration.

A host becoming unreachable does not prove its VMs are stopped. Workload
observation becomes unknown/unavailable as appropriate, but O3K must not start a
duplicate workload elsewhere or reattach a single-writer shared volume merely
because a heartbeat expired.

Automatic evacuation requires a separately accepted fencing mechanism proving
that the prior executor/writer cannot continue. Without that proof, P11 fails
closed and requires recovery/reconnect/operator action.

### 17. Existing distributed failure semantics apply to fabric state

Every P11 plan/mutation preserves existing O3K invariants:

- durable intent before external mutation;
- deterministic command/idempotency identity;
- current controller work/fencing ownership;
- current target agent epoch;
- current resource/fabric generation;
- persisted acceptance before mutation;
- equivalent replay returns the durable outcome;
- conflicting fingerprint fails closed;
- timeout/transport loss after possible mutation is unknown outcome;
- observe before retry;
- partial namespace/bridge/veth/neighbor/route/policy/WireGuard realization is
  explicit and reconcilable;
- foreign or ambiguously owned host state is never adopted/deleted;
- host reboot reconstructs accepted network/fabric state before dependent O3K
  guests are considered ready.

## Consequences

### Positive

- Normal same-host VM ARP and local Ethernet performance are preserved.
- Remote ARP does not need broadcast flooding or an L2 overlay.
- Endpoint placement remains independent from subnet allocation.
- WireGuard adds encrypted authenticated transport without becoming tenant
  identity or policy.
- Per-realm routed namespaces improve failure/inspection boundaries while
  preserving the proven libvirt TAP path.
- `/32` endpoint routing is simple and inspectable for the intended 10–20-host
  edge profile.
- The design retains future eBPF, OVN, EVPN, VXLAN/Geneve, VRF, BGP, and
  overlapping-address providers behind explicit capabilities.

### Negative / accepted limitations

- The VM-facing realm bridge remains in the host namespace for libvirt
  compatibility; isolation therefore also depends on strict per-realm bridge
  ownership and bridge/TAP policy, not netns alone.
- The provider must compile policy consistently for local bridge and routed
  paths.
- Remote/local endpoint transitions require neighbor convergence.
- Endpoint `/32` route count grows with VM count; this is accepted for the P11
  edge target and is not a hyperscale claim.
- P11 v1 cannot carry overlapping endpoint IPs through the shared fabric.
- Regional broadcast/multicast and arbitrary L2 protocols do not work across
  hosts.
- WireGuard requires a usable UDP underlay and adds MTU/CPU overhead that must
  be measured.
- Namespace/veth/WireGuard lifecycle increases reconciliation and cleanup
  surface.

## Rejected alternatives

### Flood ARP/Ethernet across WireGuard

Rejected because WireGuard is an IP tunnel and carrying arbitrary Ethernet
requires another L2 encapsulation/FDB mechanism. P11 intentionally avoids
inventing a VXLAN/Geneve equivalent.

### Return the remote VM's actual MAC to local guests

Rejected for the routed profile because the remote MAC is not locally
reachable at Layer 2. Returning a stable realm proxy MAC makes the L3 boundary
explicit and keeps endpoint movement a route/neighbor convergence problem.

### One WireGuard interface or key per tenant/AddressRealm

Rejected because it couples tenant identity to tunnel topology and creates
`realms × hosts × peers` operational state. P11 uses one host fabric and keeps
realm isolation above it.

### One tenant subnet per hypervisor

Rejected because tenant address allocation must not encode current physical
placement. Endpoint `/32` routes follow placement instead.

### Force all same-host traffic through the realm router

Rejected because it needlessly removes normal local Ethernet behavior and adds
an extra routed hop. Local L2 is acceptable when policy and anti-spoofing are
enforced on the TAP/bridge path.

### Make the realm bridge itself live inside the realm netns

Rejected for the P11 reference path because the proven libvirt integration uses
host-visible pre-created TAP/bridge state. Moving the VM-facing bridge/TAP into
another namespace would require a different QEMU/libvirt attachment mechanism
or additional per-VM veth indirection. A future provider may choose that model
with separate evidence.

### Make VXLAN/Geneve/OVN/EVPN mandatory

Rejected because P11's product target does not require regional L2 or overlapping
CIDRs and the extra distributed control/dataplane surface would dominate the
milestone.

### Make custom eBPF the first multi-host dataplane

Rejected because P11 should prove multi-host product semantics using inspectable
kernel primitives. eBPF remains a future provider/acceleration path.

### Blindly evacuate unreachable hosts

Rejected because loss of control-plane connectivity does not prove the old VM
or shared-storage writer is stopped. Duplicate execution/data corruption is a
worse failure than temporary unavailability.

## Human acceptance record

The task requester explicitly accepted ADR-0170 and SPEC-0028 for P11
implementation on 2026-08-19. The requester identity is not exposed to the
agent beyond the authenticated task instruction.

```text
Decision: ACCEPT ADR-0170 and SPEC-0028 for P11 implementation
Reviewer: task requester (explicit authenticated task instruction)
Date: 2026-08-19 UTC
Conditions: preserve all SPEC-0028 evidence gates and non-goals
Evidence: acceptance instruction in the active task after PR #702 merged
```

This acceptance authorizes bounded implementation. It does not claim that the
runtime, privileged-host, real multi-host, target-scale, MTU, storage-placement,
failure-matrix, or cleanup evidence gates have passed.
