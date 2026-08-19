# SPEC-0028 — Namespaced Routed Edge Fabric v1

Status: Proposed

Decision-accepted: pending
Human-approval: pending

Related decision: [ADR-0170](../adr/ADR-0170-namespaced-routed-edge-fabric.md)
Related issue: [#701](https://github.com/kubedoio/o3k-rust/issues/701)
Related contract: [P11 edge fabric](../../contracts/p11-edge-fabric.md)

Related normative sources:

- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0168](../adr/ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169](../adr/ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [SPEC-0026](SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0027](SPEC-0027-native-persistent-storage-v1.md)
- [execution-boundary contract](../../contracts/execution-boundaries.md)

## Purpose and gate

P11 turns the proven P9 compute/network dataplane and P10 persistent-storage
semantics into a bounded small multi-hypervisor edge-cloud profile. This spec
freezes the first multi-host fabric, neighbor, placement, drain, failure, and
evidence semantics before privileged implementation begins.

Runtime implementation is blocked while ADR-0170 or this SPEC remains
`Proposed`. Human acceptance does not itself create a support claim; the exact
real topology and host count proven by evidence bound the final claim.

## P11 product outcome

A completed P11 profile lets an operator enroll and operate a small set of
independent KVM/libvirt compute hosts and lets a tenant:

1. create an O3K AddressRealm/network/subnet;
2. boot real VMs on different eligible hypervisors without subnet allocation
   depending on host topology;
3. communicate with same-realm local endpoints using normal ARP/local L2;
4. communicate with same-realm remote endpoints without cross-host ARP flooding
   or a regional L2 overlay;
5. preserve P9 stateful NetworkPolicy, egress, and public/floating-address
   behavior;
6. use P10 host-local LVM and shared Ceph RBD according to typed placement
   constraints;
7. drain/restart/disconnect/reconnect hosts without duplicate resource mutation;
8. fail closed when host ownership is uncertain; and
9. delete the environment with zero owned leaks and zero foreign-state
   mutations.

## Profile limits

The P11 v1 reference profile is deliberately bounded:

```text
IP family                         IPv4
AddressRealm overlap on fabric    unsupported
same-host L2 adjacency            supported within one AddressRealm
cross-host L2 adjacency           unsupported
cross-host ARP broadcast          unsupported
cross-host Ethernet broadcast     unsupported
cross-host multicast              unsupported unless separately proven
cross-host connectivity           routed
fabric encryption                 WireGuard reference provider
central network/gateway node      not required
live migration                    unsupported
blind failure evacuation          forbidden
```

This profile does not advertise broader Neutron semantics solely because the
internal fabric exists.

## Canonical and derived state

### Canonical state

P11 reuses the existing O3K canonical resources and authority. Public identity
continues to come from existing typed Cloud Kernel resources such as:

- AddressRealm / NetworkIntent;
- endpoint / port identity and fixed IP/MAC;
- NetworkPolicy / security-group projection;
- Server placement/allocation;
- Volume / VolumeAttachment / Snapshot;
- durable operation and audit identity.

Canonical resources must not contain:

```text
Linux netns names
bridge/veth/TAP kernel names
nftables handles or syntax
proxy-neighbor command text
realm proxy MAC as public resource identity
WireGuard private keys
WireGuard peer configuration
WireGuard interface names
AllowedIPs configuration
raw endpoint /32 kernel routes
```

### Derived planner state

P11 may define typed internal planner values such as:

```text
EndpointLocation {
    endpoint_id
    realm_id
    project_id
    fixed_ipv4
    mac
    selected_host
    endpoint_generation
    placement_generation
}

RealmEndpointDirectory {
    realm_id
    directory_generation
    endpoints[]
}

FabricHostIdentity {
    host_id
    public_key
    underlay_endpoint
    fabric_generation
    mtu_capability
}

FabricEndpointRoute {
    realm_id
    endpoint_id
    fixed_ipv4
    target_host
    endpoint_generation
    placement_generation
}
```

Exact Rust type names may differ. These values remain control-plane planner
state and do not become new tenant-visible resources merely because the
provider needs them.

## Host-local topology

For every AddressRealm with endpoints on a host, the reference provider
realizes:

```text
initial host namespace

VM TAP(s)
   |
realm bridge (one per active realm on that host)
   |
realm-uplink veth
   |
   +--------------------------+
                              |
                    AddressRealm netns
                    ------------------
                    gateway/proxy-neighbor
                    routed nft policy/NAT
                    realm-to-fabric veth
                              |
                              v
                    o3k-fabric netns
                    ----------------
                    fabric route validation
                    wg-o3k
                              |
                       host underlay UDP
```

The VM-facing bridge remains host-visible for compatibility with the existing
pre-created TAP/libvirt path. The bridge is one realm's local L2 island only;
TAPs from another realm must never be attached to it.

A host does not need a realm namespace/bridge when it has no accepted endpoint
or other explicitly required P11 state for that realm. Empty-state cleanup must
be deterministic and ownership-checked.

## Endpoint identity and anti-spoofing

Every attached VM endpoint is bound to the accepted canonical tuple:

```text
endpoint_id
project_id
realm_id
fixed_ipv4
mac
endpoint_generation
selected_host / placement generation
```

On the VM/TAP/bridge path the provider must reject at minimum:

- Ethernet frames with an unaccepted source MAC;
- IPv4 packets with an unaccepted source address for that endpoint;
- ARP messages with an unaccepted sender protocol address;
- ARP messages with an unaccepted sender hardware address;
- stale endpoint generations;
- frames originating from a TAP not owned by the accepted endpoint binding.

A correct interface/bridge name is never sufficient ownership evidence.

## Local same-realm neighbor behavior

When source and destination endpoints are active in the same AddressRealm and
on the same host:

- the source guest may issue normal ARP;
- the destination guest may answer with its actual canonical MAC;
- Linux may forward the frame locally on the realm bridge;
- the packet remains subject to endpoint anti-spoofing and the canonical
  NetworkPolicy/security-group semantics.

Local switching must not create a security-policy bypass. The provider must
compile the canonical policy to the local bridge/TAP path, using nftables bridge
family or another explicitly accepted realization. Installing policy only in a
routed `inet` chain is insufficient for P11.

## Remote same-realm neighbor behavior

ARP/neighbor broadcasts are never transported to other hypervisors in P11 v1.

For a destination endpoint in the same AddressRealm but on another host:

1. local `o3k-network` must have a current accepted `RealmEndpointDirectory`;
2. it recognizes the destination IP as a current remote endpoint;
3. it answers the local guest's ARP request with the AddressRealm proxy MAC;
4. the guest sends the IP packet in an Ethernet frame addressed to the realm
   proxy MAC;
5. the realm netns routes the destination IP using current endpoint-location
   state;
6. the host fabric delivers the IP packet to the current target host;
7. the remote host routes it into the matching realm local L2 island and to the
   destination endpoint.

If the remote endpoint is absent, stale, deleting, belongs to another realm, or
has a placement/generation conflict, no synthetic neighbor reply is emitted.

## AddressRealm proxy MAC

Each AddressRealm has a deterministic provider-derived proxy MAC for this
profile. Requirements:

- locally administered unicast form;
- deterministic from stable realm identity plus a versioned derivation domain;
- collision checked within the local provider scope;
- not equal to a tenant endpoint MAC on the local realm bridge;
- not exposed as a public tenant resource identity;
- may be identical on separate hosts because their realm bridges are not one
  regional L2 domain.

The provider must never answer remote ARP using the remote endpoint's actual MAC
in this routed profile.

## Realm endpoint directory semantics

The endpoint directory is produced only from accepted control-plane endpoint
and placement state. It is not populated from observed ARP traffic or kernel
learning.

A directory entry is usable only when:

- endpoint ownership/project/realm are internally consistent;
- endpoint generation is current;
- selected host/placement generation is current;
- target host fabric identity/generation is current and eligible;
- destination address belongs to the accepted non-overlapping AddressRealm.

Directory publication is deterministic and versioned. Equivalent replay has
the same fingerprint. Conflicting directory content using the same identity is
rejected.

A host must be able to resynchronize the entire accepted directory after
restart/reconnect without inventing endpoint placement from local kernel state.

## Fabric route semantics

The reference provider uses host-specific endpoint routes, normally `/32`
IPv4 routes, for remote endpoints. Tenant subnet allocation is not host
allocation.

The accepted semantic mapping is:

```text
endpoint IP -> accepted target host
```

not:

```text
tenant subnet -> permanently assigned hypervisor
```

A remote endpoint route is withdrawn or replaced only after a newer accepted
endpoint/placement generation. A stale host cannot keep newly accepted endpoint
ownership by retaining an old route locally.

## Shared host fabric

Each compute host participating in the P11 fabric has one host fabric identity
and one reference WireGuard fabric interface, not one interface/key per tenant.

The preferred topology is:

- physical underlay NICs and ordinary host services remain in the initial host
  namespace;
- `wg-o3k` cleartext interface and fabric routing live in `o3k-fabric` netns;
- realm namespaces connect to the fabric netns with provider-owned veth pairs;
- the WireGuard UDP socket may remain in the initial namespace according to the
  provider's safe setup procedure.

The underlay contract requires mutually reachable configured host UDP
endpoints. P11 does not implement STUN/TURN/relay/NAT-traversal services.

## Fabric isolation

WireGuard encryption does not authorize tenant traffic.

The fabric netns must enforce default-deny forwarding except for accepted
fabric plans. At minimum it validates:

- ingress realm veth corresponds to the source endpoint's accepted realm;
- source IP is an accepted current endpoint of that realm on the local host;
- destination is an accepted endpoint reachable through the planned local or
  remote path;
- remote received source IP is permitted by the authenticated peer's current
  endpoint ownership;
- destination route resolves to the correct local realm;
- cross-realm forwarding is denied unless a separate explicit canonical
  route/gateway/policy intent authorizes it.

A route accidentally present in the kernel must not itself authorize traffic.

## WireGuard identity and key handling

Private key requirements:

- generated locally by the bounded host network/fabric executor;
- stored only in an exact provider-owned protected path or equivalent secret
  storage on the host;
- never sent to `o3kd`;
- never stored in canonical SQLite/PostgreSQL resource state;
- never emitted in normal logs, audit events, API responses, diagnostics, or
  CI evidence;
- never copied into another host during ordinary re-enrollment.

Control-plane-visible public fabric identity may include only bounded:

```text
host_id
public_key
underlay endpoint
fabric generation
provider/capability version
MTU bounds
```

Key rotation/re-enrollment increments/fences fabric generation. Old public keys,
agent epochs, and peer plans become stale and must not receive new endpoint
assignments.

The WireGuard provider may derive peer `AllowedIPs` from the accepted endpoint
routes. `AllowedIPs` must be regenerated from current accepted endpoint
location, not learned from packet traffic.

## Non-overlapping address restriction

P11 v1 retains non-overlapping endpoint prefixes across the shared routed
fabric. Separate Linux namespaces allow local duplicate routing contexts in
principle, but the shared IP-routed host fabric does not carry a realm ID with
each packet and therefore cannot disambiguate equal endpoint IPs from different
realms.

Attempting to schedule/realize conflicting active cross-host prefixes in this
profile fails before external network mutation.

Overlapping cross-host realms require a later explicit provider using VRF plus
encapsulation, OVN/EVPN, eBPF metadata, or another accepted mechanism.

## NetworkPolicy realization

One canonical policy may require two provider realizations:

```text
local endpoint <-> local endpoint    -> bridge/TAP enforcement
local/remote routed traffic           -> realm routed enforcement
```

These are not two independent policies. They must be compiled from the same
canonical policy generation and must converge atomically enough that a policy
update cannot silently create a permissive bypass between local and routed
paths.

Required P11 evidence includes both positive and negative traffic cases for:

- same-host/same-realm traffic;
- cross-host/same-realm traffic;
- attempted cross-realm traffic;
- policy update while traffic is active.

## North/south networking

P9 public-address/NAT/egress semantics remain authoritative. The P11 provider
may relocate provider-native rules into the realm/fabric topology as required,
but canonical public-address binding, project ownership, policy, operation
identity, and failure handling remain unchanged.

P11 does not introduce a central gateway node. A public/FIP packet path must be
proven for a VM placed on a non-controller compute host.

## MTU contract

The fabric provider reports bounded underlay/fabric MTU capability and derives a
safe tenant MTU. The selected tenant MTU must be reflected through the existing
network configuration/DHCP path.

Full-profile evidence must include:

- ordinary small ICMP/TCP traffic;
- payload close to the selected tenant MTU;
- failure or correct fragmentation/PMTU behavior above the supported size as
  documented by the profile;
- no silent black-hole caused by WireGuard overhead.

The exact WireGuard overhead or interface MTU is execution/provider state, not a
canonical Network field.

## Multi-host placement

P11 reuses the existing authenticated agent inventory and administrative state.
Do not invent a parallel host registry.

New placement must consider at least:

- current agent epoch/availability;
- administrative state (`Enabled` eligible; `Draining`/`Disabled` ineligible
  for new work according to the existing contract);
- vCPU/memory/disk capacity;
- required compute capabilities;
- P11 network/fabric readiness and generation;
- AddressRealm realization support;
- selected availability/failure-domain constraints where already modeled;
- storage placement/attachment scope from P10.

A decision is durable before execution dispatch. An agent never reschedules a
workload independently.

## Storage placement integration

P11 must prove both directions of the P10 placement model:

### Host-local LVM

A VM requiring an existing host-local LVM volume may be placed only on the
owning eligible host. A request that otherwise selects another host fails before
libvirt/storage mutation.

### Shared Ceph RBD

After a clean detach/termination from host A, the same single-writer RBD volume
may be attached serially to an eligible VM on host B and the exact guest payload
checksum must remain valid.

P11 does not implement multi-attach or storage migration.

## Drain semantics

`Draining` immediately excludes a host from new placement. Existing VMs remain
running. The control plane reports explicit drain blockers including resident
workloads, host-local volumes, active attachments, or incomplete operations.

P11 v1 does not automatically live-migrate, cold-migrate, stop, or destroy
blockers merely to reach drained state. Operators or later accepted workflows
resolve blockers.

Returning a host to `Enabled` requires current agent/fabric readiness before new
placement resumes.

## Host failure and fencing

Host/control-plane disconnection creates uncertainty, not proof of shutdown.
When a host becomes unavailable:

- no new placement targets that host;
- existing resource observation becomes unavailable/unknown as appropriate;
- O3K does not create a duplicate VM elsewhere based only on missed heartbeat;
- O3K does not reattach a single-writer RBD volume elsewhere until prior
  attachment/writer ownership is safely released or an independently accepted
  fencing mechanism proves it cannot continue;
- reconnect with a new agent/fabric epoch resynchronizes durable desired state
  and rejects stale events/plans.

Automatic evacuation is outside P11 unless separately accepted with real
fencing evidence.

## Restart ordering

After a host reboot/restart, accepted network/fabric realization required by an
O3K guest must be restored or confirmed before that guest is considered network
ready and before an automatic resume path can depend on stale kernel state.

At minimum reconcile:

- realm bridge ownership;
- realm namespace/veth ownership;
- local endpoint anti-spoof/policy;
- realm proxy-neighbor entries;
- endpoint routes;
- fabric namespace/interface/peer generation;
- routed realm/fabric policy;
- P9 egress/public-address state;
- compute/libvirt endpoint attachment observation.

## Failure matrix

The implementation/evidence program must cover at least:

1. duplicate equivalent realm/fabric plan delivery;
2. conflicting fingerprint replay;
3. stale controller fencing token;
4. stale compute-agent epoch;
5. stale network-agent epoch;
6. stale storage-agent epoch where storage participates;
7. stale fabric generation/public key;
8. partial realm bridge creation;
9. partial realm namespace/veth creation;
10. partial proxy-neighbor publication;
11. partial endpoint `/32` route publication;
12. partial WireGuard peer/AllowedIPs realization;
13. policy local-path applied but routed-path interrupted;
14. policy routed-path applied but local-path interrupted;
15. controller restart with multiple active hosts;
16. controller takeover during endpoint placement/fabric update;
17. simultaneous multi-agent reconnect/resync;
18. graceful compute host restart;
19. abrupt host/controller disconnection;
20. one fabric peer unavailable and later recovered;
21. underlay UDP endpoint unavailable and later recovered;
22. drain/undrain with running blockers;
23. insufficient compute capacity;
24. missing P11 network/fabric capability;
25. LVM locality placement rejection;
26. serial Ceph RBD attach across two hosts with checksum verification;
27. endpoint local -> remote placement change and ARP cache convergence;
28. endpoint remote -> local placement change and ARP cache convergence;
29. interrupted environment cleanup followed by reconciliation;
30. foreign bridge/netns/veth/route/nftables/WireGuard/key preservation.

Every scenario ends in a known state or an explicitly unresolved operator state;
unknown outcome must never be converted to success by retry alone.

## Real functional evidence topology

The core multi-host functional gate must use at least **three independent
KVM/libvirt compute hosts** unless human review selects a stronger minimum. It
must not be three simulated agent identities inside one host namespace and
called multi-hypervisor evidence.

The gate must demonstrate at minimum:

- VM A and VM B on the same host/same realm: real ARP to the actual local MAC;
- VM A and VM C on different hosts/same realm: local proxy ARP to the realm
  proxy MAC and successful routed packet path;
- no cross-host ARP broadcast dependency;
- packet capture/observation showing workload traffic crosses the WireGuard
  fabric rather than cleartext workload IP traffic on the underlay;
- same-host and cross-host NetworkPolicy allow/deny;
- attempted unauthorized cross-realm traffic denied;
- public/FIP path to a VM on a non-controller host;
- supported MTU boundary;
- serial RBD payload persistence across two hosts;
- LVM placement rejection on the wrong host;
- drain/reconnect/fabric interruption/recovery;
- final independent cleanup inventory.

## Target-count control-plane evidence

Separately exercise approximately the declared roadmap target host count for:

- authenticated registration;
- heartbeats/availability;
- capability inventory;
- scheduler candidate filtering;
- endpoint directory fanout;
- fabric-plan fanout;
- reconnect/resync concurrency;
- controller restart/takeover behavior;
- bounded memory/CPU/latency measurements.

Mocks/simulated agents may supplement this scalability test, but they do not
upgrade the real-hypervisor support claim. The final product profile must state
exactly how many real hypervisors and which topology were proven.

## Independent cleanup/evidence counters

P11 completion evidence must independently inventory and report at least:

```text
duplicate compute resources = 0
duplicate network resources = 0
duplicate storage resources = 0
stale fenced mutations accepted = 0
unfenced duplicate workloads = 0
owned compute leaks = 0
owned network leaks = 0
owned storage leaks = 0
owned realm bridge leaks = 0
owned netns/veth leaks = 0
owned proxy-neighbor leaks = 0
owned fabric route leaks = 0
owned WireGuard peer/interface leaks = 0
foreign compute mutations = 0
foreign network mutations = 0
foreign storage mutations = 0
foreign host/fabric mutations = 0
```

WireGuard private-key material must be absent from uploaded evidence.

## Explicit non-goals

P11 v1 does not implement or claim:

- cross-host overlapping CIDRs;
- regional L2 adjacency;
- ARP/Ethernet flooding across hypervisors;
- VXLAN, Geneve, EVPN, OVN/OVS;
- custom eBPF dataplane;
- internal BGP routing protocol;
- STUN/TURN/relay/NAT traversal;
- live migration;
- automatic unfenced evacuation;
- storage migration;
- multi-attach;
- SR-IOV or DPDK;
- multi-region;
- P12 native API/CLI;
- Terraform/UI/ecosystem work;
- broad new Neutron compatibility unrelated to the proven journey.

## Definition of done

P11 is complete only when:

1. ADR-0170 and SPEC-0028 have explicit human acceptance;
2. the canonical domain remains provider-independent;
3. the real multi-host topology passes local-ARP and remote-proxy-ARP packet
   paths with policy enforcement;
4. host endpoint `/32` routing and shared WireGuard transport are proven;
5. no cross-host ARP/Ethernet flooding is required;
6. public/FIP and MTU evidence pass;
7. LVM locality and serial shared-RBD placement/data persistence pass;
8. drain/reconnect/restart/failure matrix passes at the declared tier;
9. target-count control-plane evidence passes;
10. all required duplicate/leak/stale/foreign-state counters are zero; and
11. the product claim is updated only to the exact tested topology.

No installed bridge, namespace, route, peer, or successful API response alone
is P11 completion evidence.
