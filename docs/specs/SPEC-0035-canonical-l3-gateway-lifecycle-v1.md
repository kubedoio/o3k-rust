# SPEC-0035 — Canonical L3 gateway lifecycle v1

Status: Accepted
Accepted-by: project-requester on 2026-08-27; bounded runtime and compatibility verification are complete for P13.3
Decision: [ADR-0178](../adr/ADR-0178-canonical-l3-gateway-and-realm-connectivity.md)
Applies-to: canonical L3 gateway domain, store, network execution, Neutron Router projection

Current evidence note: the committed gateway lifecycle/execution artifacts were
run against `bfcdf38`. Subsequent gateway changes invalidate those artifacts as
current-code verification; the bounded runtime and compatibility gates require
a rerun before this specification is used to support a current P13.3 claim.

## 1. Canonical model

```text
L3Gateway {
  id: UUID; project_id; name; external_realm_id?; enable_snat;
  state; generation
}
L3GatewayAttachment {
  id: UUID; gateway_id; realm_id; project_id; state; generation
}
```

`L3Gateway` is independent of `AddressRealm`. A gateway may be unattached and
one gateway may attach multiple Realms. An attachment is unique by its durable
UUID and by the active pair `(gateway_id, realm_id)`. All relations are
project-scoped and require matching parent ownership.

## 2. Lifecycle and fencing

Gateway lifecycle is `requested -> active -> deleting -> deleted`, with
`error` available for recovery. Attachment lifecycle is `requested -> active
-> deleting -> deleted`, with `error` only when recovery requires it. Create,
update, transition, attach, detach, and finalization validate expected positive
generation and atomically increment it. Gateway deletion conflicts while any
attachment or dependent public-address/egress relation remains.

Deletion is two-step: reserve the child/gateway deletion durably, withdraw the
complete provider snapshot, observe safe absence, then finalize the canonical
row. Restart resumes from the durable state.

## 3. Routing semantics

Attached Realms share a deterministic L3 connectivity plan generated from the
canonical gateway graph. An external Realm/uplink is explicit. Default gateway
and route behavior remain Realm-scoped; AddressRealm remains the address
discriminator. `enable_snat=true` maps to canonical bounded SNAT egress. A
provider/profile that cannot realize `enable_snat=false` must reject it before
provider mutation.

No unattached Realm is implicitly connected. Detaching the last relation
withdraws only that gateway's connectivity. Existing public-address authority
remains `PublicAddress`/`PublicAddressBinding`.

### 3.1 Execution boundary

Gateway execution is independent of the one-Realm
`NamespacedRoutedFabricPlan`. The service compiles a complete
`L3GatewayExecutionPlan`; a gateway provider owns the provider topology and
may atomically rebuild it. A provider-local Realm execution directory resolves
canonical Realm IDs to existing Realm namespace/bridge/interface contexts.
Those names are derived execution state and are never stored in the canonical
gateway model. Per-attachment observation remains truthful when an aggregate
provider topology is rebuilt.
The Linux provider also writes an O3K-owned fingerprint marker inside the
gateway namespace. This is derived realization evidence that binds the
observed aggregate topology to the durable execution record; it never
reconstructs canonical gateway or Realm resources.

## 4. Neutron projection

`openstack_networking_router_v2` projects Router identity and metadata onto
`L3Gateway`. Router interface add/remove projects onto
`L3GatewayAttachment`, using the provider's router ID plus subnet/port identity
at the protocol edge. No Neutron-only desired-state table is authoritative.

## 5. Persistence and evidence

Canonical gateway and attachment tables use composite project foreign keys,
positive generations, lifecycle checks, and active-pair uniqueness. SQLite and
PostgreSQL must produce identical reconstruction and generation behavior.
Provider realization is derived, observable, restart-safe state. Existing
Network/AddressRealm/Policy/PublicAddress resources are not migrated into fake
gateways.
