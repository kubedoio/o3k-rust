# ADR-0176 — Canonical Network and AddressRealm lifecycle separation

Status: Accepted
Date: 2026-08-24
Decision-accepted: 2026-08-24
Human-approval: project-requester (2026-08-24, explicit acceptance recorded in task instruction)
Reviewed-proposal-baseline: 9b5a74d9e470278c77907a5c3ca22a56af4fe64a
Supersedes: none
Superseded-by: none
Affected-services: api, network, store, kernel, compute, compatibility, governance

Related decisions and specifications:

- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0171 — AddressRealm-encapsulated edge fabric](ADR-0171-addressrealm-encapsulated-edge-fabric.md)
- [ADR-0172 — configurable edge-fabric transport ports](ADR-0172-configurable-edge-fabric-transport-ports.md)
- [SPEC-0026 — O3K Routed Fabric v1](../specs/SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0029 — AddressRealm-encapsulated Edge Fabric v2](../specs/SPEC-0029-addressrealm-encapsulated-edge-fabric-v2.md)
- [SPEC-0033 — canonical Network / AddressRealm lifecycle v1](../specs/SPEC-0033-canonical-network-addressrealm-lifecycle-v1.md)
- [P13 managed-resource requirements](../compatibility/p13-1/p13-2-managed-resource-requirements.yaml)

This architecture amendment was accepted against reviewed proposal baseline
`9b5a74d9e470278c77907a5c3ca22a56af4fe64a`. Acceptance makes the architecture
active authority, but does not claim runtime implementation or authorize P13.2
before the P13.1F canonical-model implementation and evidence gate.

## Context

P13.1 provider discovery established a standard lifecycle in which a Network
exists before its first Subnet and remains after the Subnet is deleted. The
current compatibility tables already represent this with independent durable
`NetworkRecord` and `SubnetRecord` identities. The canonical `NetworkIntent`,
however, contains a mandatory `AddressRealm`, and current reconstruction fails
when a Network has no Subnet. This is a general O3K lifecycle contradiction,
not a Terraform or Neutron adapter detail.

ADR-0168 makes Network technology-independent connectivity intent authoritative.
ADR-0171 makes AddressRealm the tenant routing/isolation identity and requires
realm-scoped endpoint and fabric state. Neither decision requires one realm to
exist before a Network, nor does either permit bare IP identity across realms.

## Decision

Adopt an independent canonical Network identity whose aggregate owns zero or
more AddressRealms. An AddressRealm has a stable identity and belongs to exactly
one Network. An AddressPool belongs to exactly one AddressRealm. A Network may
therefore be addressless, and deleting the last realm does not delete the
Network.

The canonical model is intentionally more capable than the initial P13 profile:

```text
Network 1 ─── 0..N AddressRealm 1 ─── 0..N AddressPool
                         |
                         +── 0..N EndpointIntent
                         +── realm-scoped routes/gateways/egress/policy context
```

P13 v1 admits at most one Neutron Subnet/AddressRealm per Network as an explicit
compatibility-profile rule. This is not a permanent domain invariant.

The accepted model is equivalent to Option C's independent lifecycle identities
and Option B's aggregate ownership. It does not introduce a provider-facing
canonical Subnet type.

## Options evaluated

### Option A — optional single AddressRealm

This fixes Network-before-Subnet with the smallest immediate change, but makes a
one-realm relationship part of the canonical shape and forces a second model
evolution for multiple isolated prefixes. It is rejected as the canonical model.

### Option B — Network owns zero-or-many AddressRealms

This preserves AddressRealm as the routing identity, supports zero-realms and
future multiple realms, and keeps P11 mappings naturally realm-scoped. It
requires planner, persistence, and endpoint references to become explicit. It
is selected as the aggregate shape.

### Option C — independent canonical Network and AddressRealm resources

This is the correct lifecycle identity statement: Network and Realm have
independent UUIDs, ownership, and deletion. It is selected semantically and may
be implemented as linked tables or one transactional aggregate. A compatibility
Network ID must not be the only authority.

### Option D — canonical Subnet above AddressRealm

No provider-independent semantic was found that requires a second Subnet
resource. Neutron Subnet maps directly to AddressRealm plus bounded pools. A
canonical Subnet would duplicate terminology and identity without adding
authority. It is rejected.

## Canonical ownership rules

- Network owns name, project scope, lifecycle, generation, and the set of realm
  references.
- AddressRealm owns prefix, overlap capability, lifecycle, and Network ownership.
- AddressPool owns allocation range and optional gateway within one realm.
- EndpointIntent carries `realm_id`; Network ID is an aggregate lookup context,
  never an IP disambiguator.
- EndpointLocation and RealmEndpointDirectory are realm-scoped. Their key is
  `(realm_id, endpoint identity/IP)`.
- RouteIntent, GatewayIntent, and EgressIntent are realm-scoped whenever their
  destination or next-hop is tenant-address meaningful. An external realm
  reference remains explicit for egress.
- PublicAddressBindingIntent is endpoint-scoped and retains explicit project
  ownership; its reachable private endpoint is resolved through that endpoint's
  realm.
- PolicyIntent remains canonical policy authority and is endpoint/realm
  contextual. Endpoint-targeted policy derives its realm through the durable
  endpoint relation; PolicyIntent does not duplicate `realm_id`. A policy
  operation that names an address or prefix independently must already carry
  an established realm context. Compiled provider policy is generation-bound
  to the endpoint/realm snapshot used to compile it.
- NetworkPlanIntent may be compiled per realm. A Network with zero realms has
  no realm dataplane plan and cannot receive endpoint attachments.
- RealmEncapsulationBinding remains a provider mapping from `(fabric_domain,
  realm)` to provider VNI. It is never Network identity.

## Identity and lifecycle

The canonical Network is not a compatibility container. It owns stable ID,
project ownership, lifecycle, generation/version, authorization scope,
persistence, and reconciliation identity independently of its realms. The
canonical Network UUID is durable across zero realms, realm creation and
deletion, restart, import, and compatibility projection. For the bounded P13
projection, Neutron Network ID equals this canonical Network ID.

AddressRealm UUID is independently durable and project-owned. For the bounded
P13 projection, Neutron Subnet ID equals the AddressRealm ID. This is safe only
because the relationship is persisted and checked as one-to-exactly-one owner;
the Network ID must never be substituted for a Realm ID.

The canonical Endpoint relation contains `id`, `project_id`, `realm_id`, fixed
IP, MAC, generation, and lifecycle state. Network ownership is derived through
the Realm foreign key; storing a second authoritative `network_id` is not
required. Because a Realm is the address context, `(network_id, IP)` is never a
sufficient endpoint identity when multiple realms are possible.

An AddressPool retains its own durable ID and is not collapsed into the Realm
ID. P13 v1 admits one pool; the canonical model permits multiple pools.

Network deletion is forbidden while any realm, endpoint, port, public binding,
or other dependent state exists. Realm deletion is forbidden while active
endpoints or public bindings exist. Realm deletion first withdraws and proves
absence of provider realm bindings and execution state, then removes pools,
realm-scoped routes/gateway/egress state, endpoint directory state, and the
Realm. The required order is: reject/coordinate dependents, prevent new
allocations, enter deleting with a durable generation, withdraw directory and
routes, clean and fence VNI/namespace/provider state, prove absence, delete
pools, then remove Realm state. The Network remains readable and listable.

Network deletion is permitted only after zero child realms and zero other
dependents are durably proven. It returns a deterministic conflict/precondition
failure otherwise. Database cascade is not a substitute for cloud-resource
lifecycle and must not silently delete provider or canonical state.

Unknown outcomes are recovered by observing durable state and provider-owned
mapping before retrying. No client request ID is treated as exactly-once cloud
identity.

## P11 compatibility

The amendment preserves ADR-0171: realm ID remains the tenant routing
discriminator; overlapping prefixes remain valid in distinct realms; one active
realm maps to one current VNI per fabric domain; RealmEndpointDirectory and
EndpointLocation remain realm-scoped; WireGuard carries only host transport
addresses. VNI/provider state is cleaned up with Realm deletion, not Network
deletion. A Network with no realms has no VNI and no realm namespace.

## Network attachment

Nova/server attachment resolves Network ID to the admitted realm set. P13 v1
allows attachment only when exactly one active realm exists. An addressless
Network is not attachable. Multiple active realms require an explicit future
subnet/port selection and are not advertised by P13 v1.

## Migration and implementation gate

The implementation amendment must migrate existing embedded/payload state
without changing IDs. Existing Network ID, Realm ID, pool IDs, endpoint IDs,
project ownership, generations, provider bindings, and VNI assignments are
preserved, together with provider ownership evidence and operation/reconciliation
links. SQLite and PostgreSQL migrations must add explicit Network, Realm, Pool,
Endpoint, and mapping relations/columns as needed; be transactional; validate
duplicate or orphan relations; and be restartable without delete/recreate.
Rollback is restore-before-migration or a tested backward-compatible reader;
destructive down-migrations are not permitted. Existing compatibility records
may remain as derived projection metadata, but may not contradict or outlive
canonical state as a shadow authority.

Reconstruction must load Network, then all child realms and realm-scoped state
by durable relations. It must support zero realms, a realm with zero pools, a
realm with pools/endpoints, post-realm deletion, interrupted realm cleanup, and
pre-migration state. Orphan, cross-project, duplicate, or generation-incoherent
relations fail closed; adapters never invent a missing realm from observations.

P13 v1's second-subnet admission is a pre-mutation canonical/profile check. The
exact provider error behavior is not source-proven by provider 3.4.0. The
implementation contract should use the existing O3K conflict mapping (HTTP
409) with a stable profile error, but the exact wire message remains to be
verified by P13.2 conformance. No canonical or provider mutation may occur.

This accepted ADR does not itself claim runtime implementation or authorize
P13.2. P13.2 remains blocked until P13.1F implements and verifies the
migration/restart requirements.

## Consequences

Positive: Network-before-Subnet and Subnet-delete/Network-retain become truthful
canonical lifecycles; overlapping realms remain safe; P13 v1's one-subnet limit
does not constrain future O3K architecture.

Cost: domain payloads, planner inputs, persistence relations, deletion
workflows, and reconstruction must be made realm-explicit. Existing one-realm
fixtures need deterministic migration coverage.
