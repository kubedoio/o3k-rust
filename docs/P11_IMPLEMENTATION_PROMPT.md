# P11 implementation prompt — Namespaced Routed Edge Fabric v1

Use this prompt only after the P11 architecture has been reviewed against the
current repository. ADR-0170 and SPEC-0028 are accepted; runtime implementation
is authorized only within their bounded contracts and evidence gates.

---

## Mission

**Complete P11 — O3K Namespaced Routed Edge Fabric v1.**

Repository: `kubedoio/o3k-rust`.

Starting from the current GitHub `main`, turn the already-proven O3K compute,
P9 routed networking, and P10 native persistent storage into a coherent small
multi-hypervisor edge cloud while preserving the authority, replay, fencing,
ownership, compatibility, and evidence rules already accepted by the project.

Do not treat this prompt's commit SHA, issue numbers, or repository state as
current. Inspect GitHub first.

## Mandatory first step: inspect before coding

Before modifying runtime code:

1. inspect current `main` and report its exact commit;
2. inspect all open P11 issues and PRs and do not create duplicates;
3. read `AGENTS.md` and repository-local instructions;
4. read `docs/NORMATIVE_SOURCES.md`;
5. read ADR-0160, ADR-0165, ADR-0168, ADR-0169, ADR-0170;
6. read SPEC-0021, SPEC-0024, SPEC-0026, SPEC-0027, SPEC-0028;
7. read `contracts/execution-boundaries.md` and `contracts/p11-edge-fabric.md`;
8. inspect the current P9 network domain/planner/provider/process and real packet
   evidence;
9. inspect the current scheduler/Placement/agent administrative-state model;
10. inspect P10 storage placement/attachment capability and LVM/RBD evidence;
11. inspect current compatibility/product-profile manifests before changing
    claims.

If ADR-0170 or SPEC-0028 is not `Accepted`, **stop runtime implementation**.
Do not activate privileged P11 behavior outside the accepted contracts.

If the repository now contains a later accepted decision that supersedes any
part of this prompt, follow the repository and explain the conflict.

## Product goal

P11 is complete only when O3K can operate a real multi-hypervisor routed edge
cloud in the exact topology it claims to support.

The final user outcome is:

> A tenant can create one AddressRealm/network/subnet, boot real VMs on multiple
> eligible KVM/libvirt hosts in that same subnet, use normal ARP/local Ethernet
> for same-host endpoints, reach remote same-realm endpoints without cross-host
> ARP flooding through O3K's distributed endpoint directory and endpoint `/32`
> routing, enforce the same stateful NetworkPolicy on both local and cross-host
> traffic, use P9 egress/public floating addresses, use P10 LVM/RBD placement
> correctly, drain/restart/disconnect/reconnect hosts safely, and delete the
> environment with zero owned leaks and zero foreign-state mutations.

## Frozen P11 network architecture

Do not redesign P11 as Neutron/OVN, a regional L2 overlay, a custom eBPF
network, or a generic SDN framework.

### Canonical authority stays technology-independent

`AddressRealm`, endpoint identity, fixed IP/MAC, NetworkPolicy, route/gateway/
public-address intent, server placement, and operation identity remain O3K
Cloud Kernel concepts.

The canonical domain must not persist Linux namespace names, bridge/veth/TAP
names, nftables handles, proxy-ARP command syntax, WireGuard private keys,
WireGuard peers/AllowedIPs, or raw kernel `/32` routes.

### VM-facing local L2 island

For each active AddressRealm on a compute host, create one provider-owned
host-local realm bridge compatible with the existing P9/libvirt pre-created TAP
path.

Conceptually:

```text
host namespace

VM-A TAP ----\
              +--- br-realm-X --- realm-uplink-veth
VM-B TAP ----/                        |
                                      v
                              realm-X netns
```

Do not move QEMU/libvirt into a tenant namespace merely to make the topology
look cleaner. Preserve the proven host-visible TAP attachment unless a concrete
repository constraint requires a separately reviewed change.

TAPs from different AddressRealms must never share a realm bridge.

### Same-host/same-realm ARP and L2

If endpoint A and endpoint B are in the same AddressRealm and on the same host:

- ordinary guest ARP is allowed;
- B replies with B's actual canonical endpoint MAC;
- Linux may forward the frame locally on the realm bridge;
- the packet MUST still pass O3K endpoint anti-spoofing and NetworkPolicy
  enforcement on the TAP/bridge path.

Do not rely on routed nftables rules alone: a local bridge packet can otherwise
bypass the realm router completely.

Compile canonical NetworkPolicy into a local bridge/TAP enforcement path and a
routed enforcement path. These are two realizations of one canonical policy,
not separate authorities.

### Endpoint anti-spoofing

For every TAP, enforce current canonical endpoint identity at minimum:

```text
endpoint_id
project_id
realm_id
accepted MAC
accepted fixed IPv4
endpoint generation
accepted host placement generation
```

Reject spoofed Ethernet source MAC, IPv4 source, ARP sender IP, and ARP sender
MAC. Do not infer endpoint authority from bridge FDB or observed ARP traffic.

### Per-AddressRealm routed namespace

For every active AddressRealm on a host, realize one bounded Linux network
namespace that owns the routed realm edge:

- gateway/proxy-neighbor behavior;
- routed nftables/conntrack policy;
- routed NAT/egress integration required by P9;
- realm-to-fabric attachment;
- realm-scoped observations/reconciliation.

Namespace/interface names are provider mappings. Matching names alone never
prove ownership.

### Distributed realm endpoint directory

The control plane already owns endpoint IP/MAC/realm identity and accepted host
placement. Derive a deterministic `RealmEndpointDirectory` or equivalent typed
planner state from those accepted resources.

The directory must distinguish current local and remote endpoints and be bound
to endpoint/placement/fabric generation. It is not a new tenant resource and it
is never learned from packet traffic.

For every endpoint entry validate:

- same project/realm ownership;
- fixed IP lies in accepted realm;
- current endpoint generation;
- current selected host/placement generation;
- current target-host fabric identity/generation;
- active P11 non-overlap rules.

Equivalent directory replay is idempotent. Same identity/generation with a
different fingerprint is conflict.

### Remote ARP behavior

**Never flood ARP across hypervisors.**

When a guest ARPs for a same-realm IP:

```text
if destination is current local endpoint:
    real local endpoint answers with its actual MAC

if destination is current remote endpoint:
    local o3k-network answers with the AddressRealm proxy MAC

otherwise:
    no synthetic ARP reply
```

Do not proactively modify guest ARP caches. Guests learn normally when they
communicate.

Do not answer remote ARP with the remote VM's actual MAC. The P11 fabric is IP
routed, not an Ethernet tunnel; returning the remote real MAC would require a
cross-host L2/FDB/encapsulation mechanism that is explicitly outside P11.

### AddressRealm proxy MAC

Use a deterministic versioned locally administered unicast MAC derived from
stable AddressRealm identity, with collision checking in the provider scope.

The proxy MAC is provider-derived execution state, not endpoint identity.
The same logical realm proxy MAC may exist on multiple hosts because the host
realm bridges are separate L2 islands.

### Remote endpoint routing

Do not assign one tenant subnet to one compute host.

One subnet may contain endpoints placed on many hosts. Derive semantic endpoint
location routes from accepted placement and realize remote endpoints normally
as IPv4 `/32` host routes.

Example:

```text
10.40.1.10/32 -> host-01
10.40.1.11/32 -> host-07
10.40.1.12/32 -> host-03
```

A route is execution state and is generation-bound. Kernel route presence alone
never authorizes traffic.

### One shared host fabric

Use one shared O3K routed fabric per compute host, not one WireGuard interface,
key, or tunnel per tenant.

Reference realization:

```text
initial host namespace
    physical underlay / UDP socket
             |
             v
      o3k-fabric netns
      ----------------
      wg-o3k
      fabric route validation
        /      |       \
       /       |        \
 realm-A    realm-B    realm-C
  netns      netns      netns
```

Prefer a dedicated `o3k-fabric` network namespace for cleartext WireGuard and
fabric routing. The physical underlay remains in the initial host namespace.
WireGuard may be created in the initial namespace and moved into the fabric
namespace if that is the safest implementation for keeping its UDP socket bound
to the underlay.

### WireGuard is transport security only

Never use WireGuard as the tenant authorization model.

The responsibilities are:

```text
AddressRealm/realm topology = isolation/routing scope
NetworkPolicy/anti-spoofing = authorization/enforcement
fabric routes                = reachability
WireGuard                    = authenticated encryption
```

The fabric namespace defaults deny. It must validate source realm/current local
endpoint and destination endpoint/current host mapping before forwarding.
Cross-realm forwarding is denied unless explicit canonical route/gateway and
policy intent authorizes it.

### WireGuard key handling

Generate the private key locally on the host. It must never enter:

- `o3kd` canonical tenant state;
- SQLite/PostgreSQL public resource rows;
- public APIs;
- audit/events;
- normal logs;
- CI evidence artifacts.

The control plane may distribute only bounded public host fabric identity:

```text
host_id
public key
underlay endpoint
fabric generation
provider/capability version
MTU bounds
```

Host re-enrollment/key rotation creates a new accepted fabric generation and
fences stale peer state.

Provider `AllowedIPs` may be derived from accepted endpoint `/32` ownership and
used for peer route/source validation. They remain execution state.

### Underlay limitation

P11 requires mutually usable configured UDP/IP reachability between compute
host fabric endpoints. Do not add STUN, TURN, relays, rendezvous, or arbitrary
NAT traversal.

### Non-overlapping addresses remain required in P11 v1

Linux netns gives local routing isolation, but one shared IP-routed WireGuard
fabric cannot distinguish identical destination IPs belonging to different
AddressRealms without another realm identifier/encapsulation.

Therefore P11 v1 keeps non-overlapping endpoint prefixes across the shared
fabric. Treat this only as a profile restriction. Do not change the canonical
architecture to permanently forbid overlap.

Do not implement VXLAN/Geneve/EVPN/OVN/VRF overlay/eBPF metadata merely to lift
this restriction during P11.

### Endpoint movement/neighbor convergence

P11 does not require live migration, but a legitimate cold placement change may
change an endpoint from local to remote or remote to local.

Reconcile:

- old/new directory generation;
- old/new proxy-neighbor state;
- old/new endpoint `/32` route;
- WireGuard peer `AllowedIPs`;
- local/remote policy path;
- stale guest neighbor state.

Use a bounded safe ARP announcement/gratuitous ARP or equivalent provider
mechanism where necessary. Never emit a neighbor claim for an endpoint whose
current accepted placement does not authorize the host to represent it.

### MTU

WireGuard overhead makes MTU a real correctness issue. Detect/configure a safe
fabric/tenant MTU from actual underlay capability and propagate the selected
value through the existing guest network configuration/DHCP path.

Do not hard-code a provider MTU into canonical Network resources.

Full-profile evidence must prove traffic near the selected MTU boundary and
must detect silent black holes.

## Preserve P9 north/south networking

Keep existing P9 authority and user semantics for:

- controlled egress/SNAT;
- public/floating IP;
- stateful policy;
- ownership/reconciliation.

Integrate provider-native rule placement into the realm/fabric topology without
creating another tenant network authority or a mandatory central gateway host.

Prove a public/FIP path to a VM running on a non-controller compute node.

## Multi-host scheduling and host lifecycle

Reuse the existing authenticated agent registry, Placement allocation,
scheduler, administrative state, durable operation/work lease, fencing token,
agent epoch, and reconciliation mechanisms. Do not create another host registry
or distributed coordination system.

New placement must filter on:

- current agent availability/epoch;
- current administrative state (`Enabled` only for new work; preserve existing
  repository semantics for `Draining`/`Disabled`);
- compute capacity/capability;
- P11 network/fabric readiness and generation;
- AddressRealm realization support;
- selected failure/availability constraints already modeled;
- storage placement/attachment scope from P10.

A placement decision is durable before execution dispatch. No agent may choose a
replacement host itself.

## Storage integration

### LVM

An existing host-local LVM volume constrains placement to its owning eligible
host. Attempting to place the workload elsewhere must fail before libvirt or
storage mutation.

### Ceph RBD

Prove the shared-storage property with a serial cross-host journey:

1. attach RBD volume to VM on host A;
2. write random guest payload and record checksum;
3. cleanly detach/terminate attachment;
4. attach the same volume to an eligible VM on host B;
5. verify the exact checksum;
6. cleanly detach/delete according to the P10 contract.

This is not multi-attach. Never permit simultaneous single-writer attachment.

## Drain

Entering `Draining` immediately removes a host from new placement.

Existing workloads continue. Report explicit blockers including resident VMs,
active attachments, host-local LVM volumes, or incomplete operations.

Do not implement live migration, hidden cold migration, forced shutdown, or
storage migration just to make drain appear complete.

## Host failure

A missed heartbeat/controller partition does not prove the hypervisor or VM is
off.

For an unreachable host:

- stop new placement there;
- mark observations unavailable/unknown as appropriate;
- do not create duplicate VM execution elsewhere;
- do not reattach an exclusive RBD volume elsewhere merely because the host is
  unreachable;
- require clean previous ownership release or a separately accepted fencing
  proof before replacement execution/writer activation.

Blind automatic evacuation is forbidden in P11.

## Reuse existing failure semantics

Every P11 mutation must use current O3K rules:

- authorize and persist desired/planner state before external mutation;
- deterministic command/idempotency identity;
- controller work/fencing ownership;
- target agent identity/epoch;
- endpoint/placement/fabric generation;
- persisted acceptance before mutation;
- duplicate equivalent replay returns durable result;
- conflicting fingerprint fails closed;
- timeout/transport loss after possible mutation is unknown outcome;
- observe before retry;
- partial realization is explicit and reconcilable;
- ambiguous/foreign kernel state is never adopted/deleted.

After host reboot, restore/verify required accepted network/fabric state before
dependent O3K guests are considered network ready.

## Implementation program

Work through bounded issues/PRs. Reuse existing P11 child issues if present.
Do not make one uncontrolled PR containing the entire milestone.

A reasonable ordering is:

1. accepted architecture + contract tests;
2. typed host/fabric/endpoint-location planner state and persistence where
   genuinely required;
3. realm bridge + AddressRealm netns lifecycle, ownership and reconciliation;
4. endpoint anti-spoof + local bridge NetworkPolicy realization;
5. distributed realm endpoint directory + deterministic remote proxy ARP;
6. shared `o3k-fabric` namespace + WireGuard host identity/key lifecycle;
7. endpoint `/32` route/peer `AllowedIPs` realization and cross-host traffic;
8. P9 egress/FIP integration and MTU;
9. placement/drain/storage-locality integration;
10. failure/restart/reconnect matrix;
11. real three-hypervisor functional gate;
12. target-count control-plane/operational evidence and claim update.

This ordering is guidance, not permission to create duplicate issues or ignore
newer repository state.

## Required real functional gate

Use at least three **independent KVM/libvirt compute hosts** unless the accepted
SPEC is strengthened. Do not count multiple agent identities/netns on one
machine as three real hypervisors.

Prove with real VMs:

### Local ARP

VM-A and VM-B in the same AddressRealm on the same host:

- ARP resolves B to B's actual endpoint MAC;
- real traffic succeeds when allowed;
- policy denial actually blocks traffic;
- spoofed MAC/IP/ARP sender traffic is rejected.

### Remote ARP

VM-A and VM-C in the same AddressRealm on different hosts:

- VM-A's ARP request is answered locally by `o3k-network`;
- ARP cache resolves C to the deterministic realm proxy MAC, not C's real MAC;
- no ARP broadcast packet is required on the remote host/underlay;
- traffic follows the current endpoint `/32` mapping and succeeds when allowed;
- policy denial actually blocks traffic.

### Fabric encryption

Show through bounded packet capture/observation that workload IP packets are not
visible in cleartext on the physical underlay and that the path uses the current
WireGuard peer identity.

Do not upload private keys.

### Cross-realm denial

Attempt unauthorized traffic between two AddressRealms and prove it fails even
when ordinary Linux kernel routes could theoretically exist.

### Public path

Prove P9 public/floating-IP ingress and controlled egress for a VM on a remote
compute host.

### MTU

Prove near-boundary packet behavior for the selected tenant MTU.

### Storage

Prove LVM wrong-host placement rejection and the serial RBD cross-host checksum
journey.

### Host lifecycle

Prove drain/undrain, graceful restart, abrupt controller disconnect, one
WireGuard peer interruption, reconnect/resync, and controller restart/takeover
at the evidence tier required by current project contracts.

## Mandatory failure matrix

Cover at least:

1. duplicate equivalent realm/fabric plan;
2. same identity/different fingerprint;
3. stale controller fence;
4. stale compute-agent epoch;
5. stale network-agent epoch;
6. stale storage-agent epoch where relevant;
7. stale fabric generation/key;
8. partial realm bridge creation;
9. partial realm netns/veth creation;
10. partial local policy application;
11. partial remote proxy-neighbor application;
12. partial endpoint `/32` route application;
13. partial WireGuard peer/AllowedIPs application;
14. local policy applied while routed policy update is interrupted;
15. routed policy applied while local policy update is interrupted;
16. controller restart with active hosts;
17. controller takeover during placement/fabric update;
18. simultaneous agent reconnect/resync;
19. host graceful restart;
20. host/controller partition;
21. WireGuard peer/underlay interruption and recovery;
22. drain/undrain with blockers;
23. insufficient capacity;
24. missing network/fabric capability;
25. LVM locality rejection;
26. RBD serial cross-host persistence;
27. local-to-remote endpoint placement change and ARP convergence;
28. remote-to-local endpoint placement change and ARP convergence;
29. interrupted cleanup/reconciliation;
30. foreign bridge/netns/veth/route/nftables/WireGuard/key preservation.

## Independent completion counters

Final evidence must independently report at least:

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

A successful command, kernel object listing, route table, or WireGuard handshake
alone is not product evidence. Required claims must be proven by real packet
paths, real guest data, failure/recovery behavior, and independent cleanup.

## Target-count evidence

Separately exercise approximately the roadmap target host count for registration,
heartbeat, inventory, scheduling candidate selection, endpoint-directory/fabric
plan fanout, reconnect/resync, and controller concurrency.

Simulated agents may supplement this test but cannot expand the real-hypervisor
support claim. State the exact real topology actually tested.

Measure/report bounded P11 control-plane and host overhead at the tested scale,
including at least endpoint-directory size/fanout, route count, WireGuard peer
count, reconciliation latency, and representative CPU/memory impact. Do not turn
P11 into an optimization project unless measurements show a real blocker.

## Explicit non-goals

Do not implement as part of P11:

- regional L2 adjacency;
- cross-host ARP/Ethernet flooding;
- overlapping cross-host CIDRs;
- VXLAN/Geneve/EVPN;
- OVN/OVS;
- custom eBPF dataplane;
- internal BGP;
- STUN/TURN/relay/NAT traversal;
- live migration;
- automatic unfenced evacuation;
- storage migration;
- multi-attach;
- SR-IOV/DPDK;
- multi-region;
- P12 native O3K API/CLI;
- Terraform/UI/ecosystem work;
- broad Neutron parity unrelated to the verified P11 journey.

## Claim discipline

Do not update compatibility/product-profile claims until the exact required
operations and topology have passed the required evidence gates.

P11 architecture acceptance does not mean P11 implementation is complete.
Three real hosts do not prove twenty real hosts. Twenty simulated agents do not
prove twenty real hypervisors. State exactly what was actually tested.

## Completion rule

Close the P11 program only when the accepted ADR/SPEC/contract, real multi-host
functional gate, target-count control-plane evidence, failure matrix,
MTU/performance evidence, storage-placement evidence, and independent cleanup
all pass with the required zero-count invariants.

At completion, update the parent P11 tracker with the exact tested topology,
commit, evidence run IDs/artifacts, known profile limits, and any intentionally
unsupported behavior.
