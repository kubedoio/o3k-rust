# P11 Namespaced Routed Edge Fabric contract

Status: Accepted

Related architecture:

- [ADR-0170](../docs/adr/ADR-0170-namespaced-routed-edge-fabric.md)
- [SPEC-0028](../docs/specs/SPEC-0028-namespaced-routed-edge-fabric-v1.md)
- [ADR-0168](../docs/adr/ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [execution-boundary contract](execution-boundaries.md)

This contract is active for bounded P11 implementation now that ADR-0170 and
SPEC-0028 have explicit acceptance. It remains an implementation contract, not
runtime or product evidence.

## Purpose

Define the bounded semantic contract between O3K control-plane authority and the
P11 host-local realm/fabric executor. The contract deliberately separates:

```text
AddressRealm / endpoint / policy identity  -> canonical O3K authority
endpoint placement / fabric route plans    -> control-plane derived intent
bridge/netns/veth/neighbor/routes           -> network execution state
WireGuard peers/keys/AllowedIPs             -> fabric-provider state
```

Provider-native networking must not become tenant-visible canonical identity.

## Authority

### `o3kd` / Cloud Kernel owns

- endpoint, AddressRealm, project and NetworkPolicy identity;
- accepted endpoint fixed IP/MAC and generation;
- accepted server/endpoint host placement;
- host administrative state and scheduling decisions;
- durable operation/work/fencing identity;
- derivation of current realm endpoint directory;
- derivation of current semantic fabric endpoint routes;
- accepted host fabric public identity/generation;
- public/FIP/egress desired state;
- storage placement constraints used by scheduling;
- retry, compensation, reconciliation and support-claim decisions.

### `o3k-network` / activated fabric provider owns only

- exact provider-owned realm bridges and VM-facing policy realization;
- exact provider-owned AddressRealm network namespaces and veths;
- proxy-neighbor realization for current remote endpoints;
- local kernel endpoint `/32` route realization;
- exact provider-owned `o3k-fabric` namespace;
- WireGuard private key local storage and interface/peer realization;
- provider-native `AllowedIPs`, MTU and route state derived from accepted plans;
- bounded observations of current owned kernel/fabric state;
- deterministic cleanup of proven owned state.

The executor does not allocate a tenant IP/MAC, change endpoint placement,
choose a new destination host, authorize cross-realm traffic, or invent a
replacement public identity.

## Semantic input: endpoint location

The control-plane-to-network boundary provides a typed endpoint-location value
with at least the semantics:

```text
endpoint_id
project_id
realm_id
fixed_ipv4
mac
endpoint_generation
selected_host
placement_generation
```

Requirements:

- values come only from accepted durable state;
- a location update has a monotonic accepted generation/version;
- duplicate equivalent publication is replay-safe;
- same identity/generation with different content is conflict;
- an executor cannot infer current placement from ARP/FDB/kernel learning;
- stale placement cannot override newer accepted placement.

## Semantic input: realm endpoint directory

For each AddressRealm/host, the planner may publish a deterministic derived
directory containing current local and remote endpoints.

The directory includes only endpoint identity, IP/MAC where canonical,
placement host, and generation data required to realize neighbor/fabric state.
It contains no WireGuard private data or raw provider command text.

An executor must refuse the directory when:

- realm/project binding conflicts;
- endpoint address is outside the accepted realm;
- overlapping-address restriction for the active P11 profile is violated;
- endpoint generation is stale;
- target-host fabric identity/generation is absent or stale;
- canonical fingerprint/command identity conflicts.

## Local L2 contract

A same-AddressRealm VM-facing host bridge is one host-local L2 island.

For local endpoints:

- ordinary ARP may resolve the destination endpoint's actual canonical MAC;
- local bridge forwarding is permitted only through the activated endpoint
  anti-spoofing and NetworkPolicy realization;
- no TAP from another AddressRealm is attached to the bridge;
- source MAC, source IPv4 and ARP sender identity are checked against the
  current endpoint binding;
- a bridge/FDB observation never creates endpoint authority.

The provider must have an enforcement path for local traffic. Routed-only
policy enforcement is non-conformant because local bridge traffic could bypass
it.

## Remote neighbor contract

A remote same-realm endpoint is represented to a local guest through proxy
neighbor resolution, not cross-host ARP flooding.

For an ARP request for destination `D`:

```text
if D is current local same-realm endpoint:
    allow the real endpoint to answer
else if D is current remote same-realm endpoint:
    answer with realm proxy MAC
else:
    do not synthesize a reply
```

The provider must not answer on behalf of:

- another AddressRealm;
- deleting/absent endpoint;
- stale endpoint generation;
- stale target-host/fabric generation;
- ambiguous duplicate IP;
- foreign host/kernel-learned endpoint.

Remote ARP uses an explicit accepted endpoint directory/proxy entry rather than
learning authority from observed traffic.

## AddressRealm proxy MAC contract

The realm proxy MAC is:

- deterministic and versioned;
- locally administered and unicast;
- collision checked locally;
- derived from stable realm identity without tenant-controlled raw MAC input;
- never used as the public endpoint MAC;
- never treated as proof of remote endpoint ownership.

The remote endpoint's actual MAC is **not** returned for routed cross-host
resolution in P11 v1.

## Fabric endpoint route contract

Cross-host routes are semantic endpoint-location routes, not subnet placement.
The P11 reference provider normally realizes current remote endpoints as IPv4
`/32` routes.

Requirements:

- route destination equals the accepted endpoint fixed IP;
- route target equals the accepted current host fabric identity;
- route generation is bound to endpoint/placement/fabric generation;
- stale route replacement/withdrawal is deterministic;
- a kernel route without an accepted semantic plan cannot authorize traffic;
- tenant subnet/address-pool allocation never becomes tied to one hypervisor.

## Fabric namespace contract

The reference provider realizes one `o3k-fabric` network namespace per host.
It is shared by all P11 AddressRealms on that host.

Provider-owned realm-to-fabric veths terminate in this namespace. The namespace
owns cleartext routed fabric policy, endpoint routes and the WireGuard
interface. The host physical underlay remains outside it.

The fabric forwarding policy defaults deny. Provider configuration must verify
at least:

- local source endpoint belongs to the ingress realm and current host;
- destination endpoint is present in the accepted directory;
- outgoing peer is the accepted target host for the destination;
- incoming peer is permitted to source the packet's endpoint address;
- local delivery enters the AddressRealm owning the destination;
- cross-realm forwarding is absent unless explicit canonical route/gateway and
  policy intent permit it.

## WireGuard provider contract

WireGuard is the P11 reference encrypted transport only.

### Private key

The private key:

- is generated locally by the executor/provider;
- remains host-local;
- is stored in an exact provider-owned protected location or equivalent;
- is not uploaded, logged, audited, returned through the agent protocol, or
  persisted in canonical tenant state;
- is destroyed only under exact host/fabric ownership rules.

### Public identity

The bounded public identity may contain:

```text
host_id
public_key
underlay_ip_or_endpoint
fabric_generation
provider_version
mtu bounds
```

### Peer state

Peer configuration/`AllowedIPs` is derived from accepted current endpoint
placement. It may be used for both route selection and authenticated source
address validation, but it remains provider state.

A stale peer key/fabric generation must not receive newly accepted routes.

## MTU contract

The provider reports bounded MTU capability and derives a safe tenant MTU from
the configured underlay and fabric path. The selected MTU is propagated through
the existing guest network configuration path.

The provider must not silently assume underlay MTU 1500. Full-profile evidence
must include near-boundary packet tests and detect a black-hole condition.

## Policy contract

One canonical NetworkPolicy generation may compile to multiple execution
locations:

- VM/TAP/bridge path for local same-host forwarding;
- AddressRealm routed path for local-to-remote/remote-to-local forwarding;
- fabric validation path only for realm/source/destination ownership fences.

These are one policy authority. The executor cannot create a permissive local
rule that contradicts the canonical routed rule or vice versa.

A partially applied policy update is an explicit non-terminal/unknown outcome
until reconciliation observes and converges the required paths. Fail-open
policy transitions are forbidden.

## Endpoint movement / neighbor convergence

When accepted endpoint placement changes:

- old proxy-neighbor and `/32` route state becomes stale;
- new directory/route publication is generation-bound;
- local-to-remote or remote-to-local transitions trigger bounded neighbor
  convergence appropriate to the provider;
- stale guest ARP state must not cause indefinite black-holing;
- the executor may emit a safe ARP announcement/gratuitous ARP only for an
  endpoint/realm identity it is currently authorized to represent.

P11 does not require live migration.

## Host lifecycle and scheduling contract

P11 reuses the existing agent registry and administrative state. `Draining` and
`Disabled` are not eligible for new placement. Unavailable hosts are not
eligible for new placement.

Network/fabric capability and readiness are scheduler inputs; they are not
independent schedulers.

Drain does not implicitly migrate or destroy existing workloads. Host-local
storage and active workload/attachment state remain blockers.

Host unreachability is uncertainty. No duplicate VM or shared-storage writer may
be created elsewhere without an accepted fencing proof beyond heartbeat loss.

## Storage-placement contract

The scheduler consumes P10 provider placement constraints:

- host-local LVM -> only owning eligible host;
- shared Ceph RBD -> another eligible host only after clean previous
  detach/termination or stronger accepted fencing.

Backend brand names are not intended as scheduler policy when typed attachment
scope/failure-domain capability already expresses the constraint.

## Ownership and cleanup

The executor may delete only state proven owned through accepted resource
identity plus provider ownership metadata/journal. Names are hints only.

This applies to:

- realm bridges;
- TAP policy objects;
- AddressRealm namespaces;
- veth pairs;
- proxy-neighbor entries;
- kernel routes;
- nftables tables/chains/sets/rules;
- fabric namespace;
- WireGuard interface/peers;
- protected private-key file/material;
- provider journals/manifests.

Foreign or ambiguous state fails closed and surfaces operator-visible evidence.

## Reconnect/replay/fencing

P11 fabric mutations use the common command envelope and existing rules:

```text
command_id
operation_id
resource identity/generation
controller fence/work ownership
target agent identity/epoch
fabric generation
deadline
canonical payload fingerprint
```

Equivalent replay returns durable state. Same identity/different fingerprint is
conflict. Timeout after possible mutation is unknown outcome. Observation
precedes mutation retry. Stale controller, agent, endpoint, placement, or fabric
generation fails closed.

## Evidence contract

Full-profile evidence must prove, not infer:

- local real-MAC ARP for same-host same-realm endpoint;
- remote realm-proxy-MAC ARP for cross-host same-realm endpoint;
- no dependency on cross-host ARP broadcast;
- local bridge policy allow/deny;
- cross-host routed policy allow/deny;
- unauthorized cross-realm traffic denied;
- endpoint `/32` route ownership and peer mapping;
- encrypted WireGuard path with no cleartext workload-IP packet on the underlay;
- P9 public/FIP behavior on a remote compute host;
- supported tenant MTU boundary;
- LVM locality rejection;
- serial RBD cross-host checksum preservation;
- drain/reconnect/peer-failure recovery;
- independent zero-leak and zero-foreign-mutation inventory.

Private WireGuard key bytes/fingerprints that would expose private material are
forbidden in ordinary evidence.

## Non-goals

This contract does not define regional L2, overlapping cross-host CIDRs,
VXLAN/Geneve/EVPN/OVN, custom eBPF, BGP, NAT traversal, live migration,
automatic unfenced evacuation, storage migration, multi-attach, SR-IOV/DPDK,
multi-region, or P12 native API behavior.
