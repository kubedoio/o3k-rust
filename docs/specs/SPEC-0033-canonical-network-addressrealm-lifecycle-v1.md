# SPEC-0033 — Canonical Network / AddressRealm lifecycle v1

Status: Accepted
Decision-accepted: 2026-08-24
Human-approval: project-requester (2026-08-24, explicit acceptance recorded in task instruction)
Reviewed-proposal-baseline: 9b5a74d9e470278c77907a5c3ca22a56af4fe64a
Decision: [ADR-0176](../adr/ADR-0176-canonical-network-and-addressrealm-lifecycle-separation.md)
Applies-to: canonical network domain, store, API, network execution planning, P13 profile

This specification derives acceptance from ADR-0176 and is active architecture
and contract authority. It defines the target model; it does not claim runtime
implementation or authorize P13.2 before P13.1F implementation and evidence.

## 1. Scope and invariants

O3K owns Network and AddressRealm identity, ownership, desired state, lifecycle,
operations, reconciliation, and provider mappings. OpenStack Network/Subnet and
Port are projections. The model must make sense without OpenStack or Terraform.

AddressRealm is the routing/isolation identity. Endpoint address lookup is
`(realm_id, ip)`, never bare IP. Distinct realms may overlap in CIDR and IP.
RealmEncapsulationBinding and VNI are provider state. NetworkPolicy remains the
canonical authorization authority. No central Neutron-like network node is
introduced.

## 2. Durable model

```text
Network {
  id: UUID; project_id; name; state; generation
}
AddressRealm {
  id: UUID; network_id; project_id; prefix; overlapping_prefixes; state; generation
}
AddressPool {
  id: UUID; realm_id; project_id; prefix; gateway; first_usable; last_usable;
  state; generation
}
EndpointIntent {
  id: UUID; project_id; realm_id; mac; fixed_ip; generation; state
}
```

Network owns zero-to-many realms. A Realm belongs to one Network and project.
Every pool belongs to one Realm and project. Every endpoint belongs to one
Realm and project. Cross-project or orphan relations are invalid.

The canonical Endpoint relation is:

```text
Endpoint { id, project_id, realm_id, fixed_ip, mac, generation, state }
```

Network ownership is derived through `realm_id`; a duplicate persisted
`network_id` is not authoritative. `realm_id` must be established before
interpreting an IP. Consequently `(network_id, IP)` is not endpoint identity.

Routes, gateways, egress, public-address bindings, policies, endpoint location,
realm directories, and fabric bindings carry explicit realm or endpoint scope
where address interpretation requires it. Provider-native handles never replace
these references.

PolicyIntent remains endpoint-targeted and derives realm context from its
durable Endpoint. It must not evaluate an address without an already-established
realm context. A future address-independent policy construct must carry or
inherit that context from its containing realm operation. Provider policy plans
are generation-bound to the endpoint/realm snapshot.

## 3. Field mapping

For P13 v1, Neutron `subnet.cidr` maps to `AddressRealm.prefix`; `network_id`
maps to Network ID; `subnet_id` maps to Realm ID; `gateway_ip` maps to the
bounded pool gateway; `allocation_pools` maps to one or more AddressPools;
`project_id` maps to canonical owner. `ip_version=4` is required. `enable_dhcp`
and DNS nameservers are compatibility fields until a canonical DHCP/DNS
contract exists; they must not be silently presented as native authority.
Subnet name/description are projection metadata unless separately accepted.

Canonical Realm may own multiple pools and a pool may have no gateway. P13 v1
admits one pool and the existing bounded IPv4 allocator semantics. Multiple
Neutron allocation ranges remain deferred, not forbidden by the domain.

## 4. State machines

Network is an independent canonical resource, not a compatibility container.
It owns stable ID, project scope, lifecycle, generation, persistence,
authorization, and reconciliation identity.

Network: `requested -> active -> deleting -> deleted`, with `error` reachable
from requested/active/deleting. A Network can be active with zero realms. Add
Realm is allowed only for an active Network; delete Network requires zero realms
and no dependents.

Realm: `requested -> active -> deleting -> deleted`, with error from any live
state. Realm creation persists identity and ownership before provider effects.
Deletion is rejected or coordinated with active endpoints/public bindings, then
prevents new allocations, enters a deleting generation, withdraws routes,
gateways, egress, directory, encapsulation binding, and execution state, proves
provider absence, and only then removes pools and Realm. The parent Network is
retained.

Pool: create only under an active Realm; update only for fields explicitly
allowed by the selected profile; delete only after dependent allocations are
released. A pool cannot outlive its Realm.

Forbidden: Network deletion with a Realm or other dependent; Realm deletion
with an active Endpoint/PublicAddress; cross-project references; accepting an
observed provider VNI as desired state; attaching a server to zero or ambiguous
realms. Database cascade is not cloud lifecycle semantics.

## 5. P13 v1 admission

The canonical model supports 0..N realms. P13 v1 compatibility admission allows
at most one Neutron Subnet/Realm per Network and at most one bounded pool for
that Realm. A second subnet is rejected before canonical/provider mutation as a
deterministic unsupported-profile cardinality conflict, not as a canonical
uniqueness constraint. Existing O3K conflict conventions indicate HTTP 409,
but the exact wire message remains an implementation-contract requirement for
P13.2 because provider source does not freeze that representation. Multiple
realms require a later profile and explicit subnet/port selection.

## 6. Persistence and reconstruction

The target store has explicit durable Network, Realm, Pool, Endpoint, and
provider-mapping relations with project-scoped foreign keys and indexes. Realm
and endpoint address uniqueness is scoped by `(realm_id, ip)`; active VNI
uniqueness is scoped by `(fabric_domain_id, vni)`.

Reconstruction loads Network first, then all owned Realms, pools, endpoints,
routes/gateways/egress/policy context, locations, and provider mappings in
stable ID order. Zero realms is a valid result, not NotFound. Missing, duplicate,
foreign, or generation-inconsistent rows fail closed as corruption. No adapter
creates a missing Realm from a provider observation.

The reconstruction matrix includes: Network with zero realms; Realm with zero
pools while temporarily valid; Realm with pools; Realm with endpoints; Network
after Realm deletion; Realm in deleting state with incomplete provider cleanup;
and migrated pre-P13 state.

Required restart cases are: zero realms; one realm; post-realm deletion; realm
without endpoints; realm with endpoints; interrupted provider cleanup; and
pre-migration state.

## 7. Migration contract

Migration is forward, transactional, ID-preserving, and restartable for SQLite
and PostgreSQL. It extracts the current embedded mandatory Realm and pools into
explicit owned rows, sets `network_id` on each Realm, and preserves all existing
Network/Realm/Pool/Endpoint IDs, project IDs, generations, provider bindings,
VNI mappings, provider ownership evidence, route/policy references, and
operation/reconciliation correlation. Compatibility Network/Subnet records may
remain as derived projection metadata only.

Before commit it rejects duplicate realm ownership, orphan pools/endpoints,
cross-project references, invalid prefixes, and conflicting provider bindings.
Migration records a schema version and is idempotent. PostgreSQL uses a
transaction with foreign-key validation and concurrent-index planning; SQLite
uses its supported table-rebuild/rename sequence with foreign keys enabled and
the same validation. Rollback means restore the pre-migration snapshot or use a
tested compatibility reader; no destructive down migration is required.

## 8. P11 realization ordering

Realm creation allocates no VNI until the Realm is durably active and provider
capability is selected. Realm deletion withdraws endpoint directory/routes,
then removes realm execution and encapsulation state, proves absence, and only
then removes pools/Realm. Network deletion occurs last. WireGuard host transport
is independent and is never removed merely because a Network loses its last
Realm.

## 9. Native and compatibility consequences

Native Network create/read/list/delete works with zero realms. Native Realm
create/read/list/delete is independently addressable. Endpoint creation requires
an explicit Realm and validates Network ownership.

OpenStack projection maps Network one-to-one to canonical Network, Subnet to
Realm, and Port to Endpoint. P13 v1 Network-without-Subnet is readable but not
server-attachable. A single admitted Realm permits bounded server attachment;
multiple realms require a future explicit selection mechanism.

## 10. Evidence and acceptance gates

Implementation must add domain invariant, authorization, migration, restart,
reconstruction, dependency-deletion, provider-mapping cleanup, and P13
conformance tests. Acceptance of ADR-0176 and this SPEC activates the
architecture; P13.2 remains gated on P13.1F implementation and evidence. This
SPEC and P13 discovery do not themselves advance runtime support claims.
