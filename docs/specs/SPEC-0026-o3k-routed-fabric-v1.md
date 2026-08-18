# SPEC-0026 — O3K Routed Fabric v1

Status: Accepted
Human-approval: Senol Colak, 2026-08-18

Related decisions and specifications:

- [ADR-0160 — service topology and execution boundaries](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0163 — product profiles and deployment posture](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [ADR-0165 — O3K Cloud OS and Cloud Kernel](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0168 — O3K Routed Fabric and node-local network execution](../adr/ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [SPEC-0021 — cross-service workflows and compensation](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022 — service API baseline and evidence gates](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024 — product profiles and claims](SPEC-0024-product-profiles-and-claims.md)
- [Execution-boundary contract](../../contracts/execution-boundaries.md)

Program tracker: [#655](https://github.com/kubedoio/o3k-rust/issues/655)

This specification is accepted together with ADR-0168. Runtime P9 work remains
issue- and evidence-gated; acceptance of this contract does not claim that any
runtime, privileged-host, or product evidence gate has passed.

## Purpose

P9 changes O3K networking from a TestLab-only flat network into the first useful
routed tenant network profile while preserving the O3K Cloud Kernel authority
model and future dataplane freedom.

The P9 product outcome is:

> A tenant can boot a real VM on an O3K-owned network, receive its durable fixed
> address, reach an approved external network through controlled egress, attach
> a public/floating address, enforce stateful project-owned network policy, and
> survive supported restart/takeover/retry scenarios without duplicate network
> mutation, O3K-owned leaks, or foreign-state changes.

P9 is not a goal to "implement Neutron". O3K adds the exact compatibility
operations required for this user journey while keeping canonical network state
technology-independent.

## P9 product profile

Profile name:

```text
p9-routed-fabric-v1
```

Parent product identity:

```text
O3K Cloud OS / native O3K cloud
```

Initial execution topology:

```text
                  northbound clients
              OpenStack compatibility
                         |
                         v
                       o3kd
      canonical network desired state / operations
                         |
                 versioned mTLS
                         |
                 host-local executor
                         |
                    o3k-network
                         |
          Linux routed/nftables realization
                         |
                        VM
```

The profile may run with the already-supported standalone or Kubernetes-hosted
control-plane deployment where its required database/control-plane evidence is
satisfied. Privileged `o3k-network` execution remains host-local by default.

## Authority model

### `o3kd` owns

- canonical public resource IDs;
- project/security ownership;
- `AuthContext` and authorization decisions;
- quota decisions and reservations where selected;
- address allocation intent;
- external/public-address allocation and binding intent;
- route/gateway/egress desired state;
- network-policy desired state;
- endpoint/host binding desired state;
- operation identity and phase;
- compensation and reconciliation decisions;
- compatibility projection/public errors;
- audit/event identity.

### `o3k-network` owns only bounded execution/observation

- host capabilities and health;
- O3K-owned provider-native network resources on that host;
- realization of the selected `NodeNetworkPlan`;
- provider-native observation and redacted failure classification;
- local journals/ownership records required to prove safe replay and cleanup.

`o3k-network` must not:

- authorize users/projects;
- invent tenant/public O3K IDs;
- allocate a different fixed/public address than the accepted control-plane
  intent;
- create an independent desired-state database;
- reschedule an endpoint;
- broaden policy;
- mutate ambiguous or foreign host networking state.

## Canonical network concepts

P9 must define durable typed domain/application values for the following
semantics. Existing accepted network/subnet/port types may be extended or
projected into these concepts rather than duplicated.

### Network / connectivity domain

A project-owned connectivity scope with explicit provider/profile capabilities.
It is not inherently an Ethernet broadcast domain and is not inherently tied to
a Linux bridge.

### Address realm

An address-isolation namespace. P9 v1 supports a routed realm whose prefixes
must not overlap within that realm. The type must not prevent a future realm
from using VRF/overlay isolation for overlapping prefixes.

### Prefix and address pool

Durable IP prefix, gateway/allocation policy, and allocatable address ranges.
P9 v1 is IPv4-first.

### Endpoint

Stable O3K network endpoint identity attached to a tenant resource. Existing
Neutron-compatible port identity may project to this concept.

### Attachment

Intent that binds an endpoint to a selected host/execution location. Provider
TAP/interface names are not public attachment identity.

### Route and gateway/egress intent

Technology-neutral routing and external-connectivity desired state. A
Neutron-compatible router/interface is a northbound projection over this state,
not a required internal daemon/object topology.

### Public address binding

Stable binding between an allocated external/public address and a project-owned
endpoint. A Neutron floating IP is a compatibility projection.

### Network policy

Project-owned stateful L3/L4 policy expressed using typed selectors, protocols,
ports/ranges, direction and action. A Neutron security group/rule is a
compatibility projection.

The canonical policy model must not expose nftables chains/sets/handles or eBPF
map/program details.

## Network capability model

Every activated execution provider reports a bounded, versioned capability set.
P9 needs enough capability vocabulary to make provider selection and future
profiles explicit without pretending capability advertisement is evidence.

At minimum model capabilities equivalent to:

```text
endpoint_attachment
ipv4
ipv6
l2_adjacency_scope
routing
stateful_policy
nat
public_address_realization
overlapping_address_realms
encapsulation_modes
qos_features
route_advertisement_modes
```

The P9 Linux routed provider is expected to advertise only what it genuinely
implements and proves. Unsupported fields/features fail closed before mutation.

## NodeNetworkPlan contract

Network application desired state is compiled into a semantic per-node plan.
The exact serialized/wire form may be added to the existing execution protocol,
but the application-level plan must be transport- and dataplane-independent.

A plan is identified by at least:

```text
plan_id
node/agent identity
controller/work fencing identity where required
resource generations
operation/request identity
canonical plan fingerprint
deadline
```

A plan may contain typed intents such as:

```text
EndpointAttachment
AddressAssignment
RouteIntent
NeighborIntent
NatIntent
PolicyIntent
AdvertisementIntent
QoSIntent                 # only when a selected profile supports it
EncapsulationIntent       # only for a future provider/profile
```

Rules:

- intent order/dependencies are explicit where partial application matters;
- equivalent replay is idempotent;
- same identity with a different fingerprint is a conflict;
- unsupported intent is rejected before unsafe partial mutation where
  practical;
- provider-native handles remain observations/mappings, not canonical intent;
- raw shell commands, nft syntax, BPF bytecode/maps, OVS flows, OVN rows, or
  unrestricted provider payloads are forbidden in canonical application
  state;
- a timeout after mutation is an unknown outcome until observation resolves it.

## First Linux realization

The first P9 provider uses ordinary Linux host networking and should be designed
for inspectability and deterministic cleanup.

Expected primitives where required:

```text
TAP / endpoint attachment
Linux routing and neighbor state
forwarding controls
nftables
conntrack
SNAT/DNAT
proxy ARP/NDP or equivalent public-address neighbor handling
tc only for selected behavior
```

Implementation requirements:

- prefer structured kernel interfaces/Netlink where practical;
- apply logically related rule/state changes transactionally or in bounded
  batches where the underlying interface permits it;
- use deterministic O3K identity/tagging/table/set naming sufficient to prove
  ownership without trusting names alone;
- never flush or rewrite global host firewall/routing state outside explicitly
  owned objects;
- preserve unrelated administrator and foreign workload state;
- persist acceptance/ownership evidence before destructive external mutation
  where recovery requires it;
- reconcile actual kernel state against durable desired state after restart.

P9 does not require a custom eBPF dataplane, OVS, OVN, VXLAN, Geneve, EVPN,
WireGuard, or BGP.

## P9 required product behaviors

### 1. Existing fixed endpoint connectivity remains correct

- network/subnet/endpoint creation preserves durable project ownership;
- fixed IP and MAC identity remain stable across restart/retry;
- guest receives the intended address/configuration;
- existing flat TestLab behavior is not silently broken during
  `o3k-network` process extraction.

### 2. External/provider network

- an operator-configured external network/address realm is explicit;
- ordinary tenant users cannot arbitrarily promote a private network into an
  external/provider network;
- external address pools have durable bounded allocation state;
- provider/uplink configuration is validated before mutation;
- foreign uplinks/routes/addresses are not adopted by name alone.

### 3. Routed egress and SNAT

A tenant VM on the P9 routed profile can reach the selected external network
through node-local forwarding and controlled SNAT when egress is enabled.

Required evidence includes real guest traffic, not only control-plane API
success or host rule presence.

Egress disabled by policy/profile must fail closed.

### 4. Public/floating address

- allocate a public address from the selected external pool;
- associate it with one authorized endpoint;
- disassociate/reassociate idempotently;
- realize inbound/outbound traffic on the endpoint's host;
- reject cross-project binding/existence leaks;
- retain enough durable intent to recover after controller/network-executor
  restart;
- cleanup releases only O3K-owned address/NAT/neighbor state.

The initial profile may restrict association topology and public-address
advertisement modes. Those restrictions are compatibility-profile facts, not
hidden implementation accidents.

### 5. Stateful network policy

At minimum P9 proves:

- project-owned policy/rules;
- ingress and egress directions;
- IPv4 CIDR/address and endpoint/group selectors selected by the profile;
- TCP/UDP/ICMP or the exact smaller frozen baseline;
- bounded port/range semantics;
- established/related return traffic where required by the selected stateful
  contract;
- default behavior explicitly specified, not inherited from host firewall
  defaults;
- policy updates converge without silently broadening access;
- cross-project access is denied unless an explicit accepted policy permits it.

Real traffic tests must prove both positive and negative cases.

## Neutron compatibility baseline

P9 compatibility must remain operation-level. Before implementation, exact
method/path/fields/errors/policy actions/version behavior are added to the
accepted compatibility manifest and fixtures from official/public OpenStack
sources.

Candidate P9 compatibility families are:

```text
routers and router interfaces
external-network semantics required by the selected workflow
floating IP create/list/show/update/delete/associate/disassociate
security groups and the bounded rule subset required by the workflow
```

These are candidates, not support claims. The existing manifest remains
unchanged/unsupported until implementation and evidence are complete.

Do not opportunistically add:

```text
VLAN/VXLAN tenant networks
OVS/OVN-specific extensions
trunks/QinQ/VLAN transparency
SR-IOV/hardware offload
broad QoS
full IPv6 feature parity
LBaaS/VPNaaS/DNSaaS
```

## Workflow and compensation

Every mutation follows SPEC-0021 and the execution-boundary rules.

Representative public-address association phases may be:

```text
validated
-> binding_intent_persisted
-> node_plan_accepted
-> provider_state_observed
-> active
```

Representative disassociation/delete reverses dependency order only after
ownership and current generation are proven.

Policy/routing/NAT changes must define what constitutes:

- accepted but not yet realized;
- partially realized;
- active;
- unknown outcome;
- retryable failure;
- terminal error requiring compensation/reconciliation.

No success is inferred merely from command dispatch.

## Multi-controller and fencing requirements

P9 inherits the P7/P8 control-plane correctness model rather than creating a
parallel coordination mechanism.

Before mutating network work is dispatched:

- the responsible controller owns the applicable durable work lease;
- stale controller fencing tokens are rejected;
- network agent stream ownership/current epoch is proven;
- stale plan/observation generations are rejected;
- durable accepted commands/plans remain recoverable across controller
  takeover.

A second controller must not duplicate NAT, public-address, policy or link
mutation after takeover.

## Restart and failure matrix

The final P9 evidence must include at least:

1. `o3kd` graceful restart with active VM/network/public address/policy;
2. abrupt `o3kd` loss and controller takeover where the selected deployment
   profile supports it;
3. `o3k-network` graceful restart;
4. abrupt `o3k-network` loss during an accepted mutation;
5. transport interruption after plan acceptance but before terminal
   observation;
6. duplicate equivalent API request/plan delivery;
7. conflicting replay with same identity but different fingerprint;
8. stale controller work token;
9. stale network-agent epoch;
10. partial NAT/policy realization followed by reconcile;
11. public-address association interrupted at each externally mutating phase;
12. delete/cleanup interrupted and resumed;
13. foreign same-name/similar route/link/firewall/address state present;
14. external/uplink unavailable then recovered;
15. policy update under real traffic proving no unintended fail-open interval
    beyond the explicitly documented atomicity model.

Every scenario records bounded machine-readable evidence and final owned/foreign
inventory comparison.

## Real guest acceptance workflow

The full-profile P9 gate must drive public APIs/clients and real VM traffic.
A representative workflow is:

```text
authenticate
-> create private network/subnet/endpoint
-> create/boot VM
-> prove fixed IP and guest connectivity
-> create/attach router/gateway compatibility state
-> prove controlled outbound Internet/external connectivity
-> allocate and associate public/floating address
-> prove permitted inbound traffic reaches the VM
-> apply stateful policy allowing one selected flow
-> prove allowed flow succeeds
-> remove/deny that flow
-> prove denied flow fails
-> restart/take over supported control/execution components
-> prove the same identities/connectivity/policy reconverge
-> disassociate/delete public address
-> delete VM/network resources
-> prove zero O3K-owned network leaks and zero foreign-state mutation
```

Exact external destinations and traffic markers must be deterministic and safe
for the protected test environment.

## Ownership and cleanup evidence

Independent inventory must cover every P9-owned class applicable to the
provider, for example:

- interfaces/TAPs/bridges;
- routes and rules;
- owned addresses/neighbor/proxy state;
- nftables tables/chains/sets/rules;
- NAT state/configuration;
- DHCP/config fragments and processes where selected;
- network agent journals/manifests;
- durable address allocations/bindings/operations.

Cleanup passes only when:

```text
owned leaks = 0
owned inconsistencies = 0
foreign mutations = 0
```

Ambiguous ownership fails closed rather than being "cleaned up" by name.

## Evidence ladder

P9 implementation proceeds in this order:

1. accepted ADR-0168 / this SPEC / compatibility records;
2. canonical domain, IAM/authorization, quota, store, migration and planner
   tests;
3. Linux provider conformance with fake/isolated kernel-facing adapters;
4. portable process-level `o3kd` + `o3k-network` protocol/replay tests;
5. privileged network-component gate on a trusted Linux host;
6. real VM egress/public-address/policy gate;
7. restart/unknown-outcome/takeover/failure matrix;
8. independent cleanup/foreign-state verification;
9. human architecture/security review;
10. compatibility/product claim promotion.

The privileged host is a final verifier, not requirements discovery.

## P9 non-goals

The first P9 profile intentionally excludes:

- broad Neutron API parity;
- custom eBPF dataplane implementation;
- OVS/OVN as a required dependency;
- VXLAN/Geneve/EVPN tenant overlay implementation;
- overlapping-CIDR runtime support;
- tenant VLAN networks;
- trunks, QinQ or VLAN transparency;
- SR-IOV or hardware offload;
- P11 cross-host WireGuard/BGP/overlay fabric;
- zero-disruption live migration between stateful dataplane providers;
- full IPv6 feature parity;
- LBaaS, VPNaaS, DNSaaS, service chaining, IDS/IPS or DDoS service products;
- P10 native persistent storage;
- P12 native O3K API redesign.

## Future provider evolution

The v1 canonical model and plan contract must allow, without redefining tenant
resource ownership/authorization:

### eBPF provider

Compile the same endpoint/route/NAT/policy/public-address intents into a bounded
BPF realization using TC/XDP/map/program mechanisms selected by a future ADR.
Provider-native connection tracking is not canonical Cloud Kernel state.

### OVN/OVS or EVPN/overlay provider

Support selected L2 adjacency, overlapping address realms, encapsulation,
trunks, VLAN transparency, distributed routing/QoS or other profile capabilities
without requiring P9's Linux-routed implementation to emulate them.

### P11 edge routed fabric

Add host-to-host routing for approximately 10–20 hypervisors using an accepted
fabric provider. Candidate future mechanisms include host-level WireGuard for a
small encrypted edge fabric and BGP advertisement where the physical network
supports routed workload prefixes. Neither is a P9 prerequisite.

### External Neutron authority

If O3K consumes an independently authoritative external Neutron service rather
than directly realizing O3K network intent, that integration uses an explicit
external/delegated authority profile. It is not modeled as a host-local Linux
provider.

## Acceptance

P9 Routed Fabric v1 is complete only when all of the following are true:

- ADR-0168 and this specification are human-accepted;
- current fixed network functionality remains correct after `o3k-network`
  activation;
- the canonical network model contains no dependency on nftables/eBPF/OVN/etc.;
- Linux provider conformance passes;
- real guest routed egress passes;
- real public/floating-address ingress/egress passes;
- stateful network-policy allow and deny traffic both pass;
- supported controller/network-executor restart and unknown-outcome cases
  reconverge without duplicate mutation;
- multi-controller fencing remains correct where that deployment profile is
  selected;
- independent cleanup reports zero owned leaks/inconsistencies and zero foreign
  changes;
- exact Neutron operation records and black-box compatibility tests pass before
  those operations are advertised;
- no P11/eBPF/OVN/overlay/native-API scope is claimed merely because the design
  preserves a future path.
