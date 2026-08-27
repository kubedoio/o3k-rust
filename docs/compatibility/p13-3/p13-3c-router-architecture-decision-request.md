# P13.3C — Router authority decision request

Status: Resolved — Option 1 accepted by project requester on 2026-08-27
Provider baseline: terraform-provider-openstack/openstack 3.4.0
Provider source tag commit: 4fd8eba1f85edfdc7aed2d17bae3f3c814abad41

## Finding

The pinned provider exposes an independently identified
`openstack_networking_router_v2` resource with create, read, update, delete,
import, external-gateway, and interface operations. The accepted O3K model has
`AddressRealm`, gateway intent, route intent, and endpoint relationships, but
no durable router identity or relation that defines router-to-realm and
router-to-interface cardinality.

`AddressRealm` cannot be silently reused as Router identity: it is a
project-owned address-space resource, may be created without a gateway, and
does not represent a Neutron router's independent lifecycle. Conversely,
creating a compatibility-only Router row would introduce a second desired-state
authority unless a new canonical model is accepted.

## Decision requested

Choose one of:

1. Accept a new provider-independent canonical L3 gateway/router resource and
   define its identity, ownership, lifecycle, interface relations, gateway
   semantics, persistence, and compiler boundary; or
2. Explicitly constrain the P13.3 Router projection to a documented
   one-to-one mapping onto an existing canonical resource, including the
   cardinality and deletion semantics that make that mapping truthful.

The decision must cover external gateway selection, `enable_snat`, interface
attach/detach, restart recovery, import identity, and project isolation.

## Current implementation state

The canonical L3Gateway/L3GatewayAttachment resources and bounded Router/
Router Interface projection are implemented. Gateway execution now has its own
provider-independent plan and Linux provider boundary; the existing
NamespacedRoutedFabricPlan remains Realm-scoped. Realm-to-Realm connectivity
and external SNAT traffic remain separately bounded evidence gates.

## Evidence already captured

- `docs/compatibility/p13-3/p13-3cde-l3-provider-contract-discovery.json`
- `contracts/iac-openstack-profile-v1.yaml`
- `docs/specs/SPEC-0032-openstack-terraform-opentofu-compatibility-profile-v1.md`

The project requester accepted Option 1 on 2026-08-27. The canonical
L3Gateway/L3GatewayAttachment implementation and bounded real-provider Router
and Router Interface lifecycle gate are now recorded in the accepted ADR/SPEC
and the companion lifecycle evidence artifact. Realm-to-Realm connectivity
and external SNAT traffic remain separately bounded evidence gates.
