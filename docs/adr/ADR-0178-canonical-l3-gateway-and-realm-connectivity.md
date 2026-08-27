# ADR-0178 — Canonical L3 gateway and Realm connectivity

Status: Proposed
Date: 2026-08-27
Human-approval: project-requester (independent canonical L3 gateway choice recorded 2026-08-27; final document acceptance follows repository governance)
Reviewed-proposal-baseline: c8d64e4b87762825a1b3d353cc0b447b32bd09dc
Supersedes: none
Superseded-by: none
Affected-services: api, network, store, compatibility, governance

Related decisions and specifications:

- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0171 — AddressRealm-encapsulated edge fabric](ADR-0171-addressrealm-encapsulated-edge-fabric.md)
- [ADR-0176 — Canonical Network and AddressRealm lifecycle separation](ADR-0176-canonical-network-and-addressrealm-lifecycle-separation.md)
- [SPEC-0035 — Canonical L3 gateway lifecycle](../specs/SPEC-0035-canonical-l3-gateway-lifecycle-v1.md)

## Context

Neutron Router has an independent identity and lifecycle, while the accepted
O3K model currently has AddressRealm, gateway intent, route intent, and
endpoint relationships but no independent gateway authority. AddressRealm is
the routing and address-interpretation identity; it may exist without a
gateway and must not be reused as Router identity.

## Decision

Introduce an O3K-native canonical `L3Gateway`. It is project-owned, has an
independent UUID, lifecycle, generation, and optional external Realm/uplink
relationship. `L3GatewayAttachment` is an independently identifiable durable
relation from one gateway to one AddressRealm. A gateway may exist unattached
and may attach multiple Realms. AddressRealm remains independently valid and
continues to define address interpretation and overlap isolation.

The canonical authority direction is:

```text
L3Gateway + L3GatewayAttachment + AddressRealm
        -> deterministic realm-scoped route/gateway/egress plan
        -> execution provider
```

Provider state, nftables, and Neutron Router rows are derived observations or
projections and never reconstruct canonical gateway state.

The first bounded profile represents external gateway selection explicitly and
supports SNAT-enabled external egress. `enable_snat=false` is admitted only
where the selected provider can realize and observe a non-NAT external path;
otherwise it is rejected as an unsupported profile rather than silently
treated as true.

Deletion is dependency-fenced: a gateway cannot be deleted while Realm
attachments or dependent public-address/egress relations remain. Attachments
are detached through their own generation-fenced lifecycle; deletion never
cascades a Realm or public address.

Neutron Router maps to L3Gateway and Router Interface maps to
L3GatewayAttachment. `external_gateway_info` maps to the canonical external
Realm/uplink relation and `enable_snat` maps to canonical egress behavior.
Neutron and OpenTofu remain compatibility clients.

## Migration posture

Existing AddressRealm, RouteIntent, GatewayIntent, EgressIntent, PublicAddress,
and provider state are not promoted or reinterpreted as L3Gateway resources.
No synthetic gateway is created for an existing Realm. New gateway rows are
created only by an explicit canonical or compatibility operation.

## Consequences

The store must provide durable gateway and attachment identities, composite
project ownership checks, positive generation fencing, restart reconstruction,
and SQLite/PostgreSQL parity. The compiler and provider adapter must consume
the canonical graph and publish complete realm-scoped snapshots.
