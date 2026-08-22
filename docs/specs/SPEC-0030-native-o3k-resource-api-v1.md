# SPEC-0030 — Native O3K Resource API v1

Status: Accepted

Related decision: [ADR-0173](../adr/ADR-0173-native-o3k-resource-api-and-resource-model.md) (human architecture/security approval 2026-08-21; this spec derives acceptance from that decision)
Related issue: [#727](https://github.com/kubedoio/o3k-rust/issues/727)
Related contract: [native resource envelope v1](../../contracts/native-resource-envelope-v1.schema.json)

Related normative sources:

- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166](../adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0168](../adr/ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169](../adr/ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [ADR-0171](../adr/ADR-0171-addressrealm-encapsulated-edge-fabric.md)
- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0026](SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0027](SPEC-0027-native-persistent-storage-v1.md)
- [SPEC-0029](SPEC-0029-addressrealm-encapsulated-edge-fabric-v2.md)

This specification derives acceptance from ADR-0173 (human architecture/security approval 2026-08-21). It defines the v1 native contract and the evidence required before that contract may be advertised. Runtime implementation must not claim advertised v1 native API support until SPEC-0030 §20 evidence gates pass.

## 1. Purpose

The native O3K API exposes canonical O3K Cloud Kernel and service-domain semantics without forcing them through OpenStack request/response models. It coexists with selected OpenStack-compatible APIs and must operate the same canonical resource authority where semantics overlap.

P12 optimizes for contract correctness and extensibility, not endpoint count.

## 2. API root and versioning

The v1 root is:

```text
/o3k/v1
```

Service resources are mounted under:

```text
/o3k/v1/{service-namespace}/{collection}
```

Examples:

```text
/o3k/v1/compute/servers
/o3k/v1/network/address-realms
/o3k/v1/network/endpoints
/o3k/v1/volume/volumes
```

The canonical service namespace MUST match the service registry namespace. The public route MAY use a plural collection label different from the internal resource type name only when the mapping is unambiguous and versioned.

Native v1 does not use OpenStack microversion headers. A future breaking generation uses a new accepted API generation such as `/o3k/v2`.

## 3. Common resource envelope

Every first-class native resource representation MUST conform to `contracts/native-resource-envelope-v1.schema.json` and provide common semantics equivalent to:

```json
{
  "api_version": "o3k.io/v1",
  "kind": "compute:server",
  "metadata": {
    "id": "...",
    "owner_scope": "...",
    "generation": 7,
    "created_at": "...",
    "updated_at": "..."
  },
  "spec": {},
  "status": {}
}
```

### 3.1 Common metadata

Required common metadata:

- stable canonical `id`;
- durable `owner_scope` for tenant-owned resources;
- monotonic `generation` where mutable desired state exists;
- creation timestamp where persisted;
- update timestamp where persisted.

Optional common metadata includes:

- region;
- availability domain;
- labels;
- annotations;
- deletion timestamp/finalization state if a later accepted lifecycle contract requires it.

Optional fields MUST NOT be fabricated merely to make all resources look identical.

### 3.2 Service-owned payload

`spec` and `status` are service-owned JSON objects validated by the owning service/resource schema.

The Cloud Kernel MUST NOT interpret arbitrary service-specific fields for authorization or provider mutation unless a shared primitive has been separately accepted.

Authorization targets use canonical resource type, resource ID, owner scope, action, and accepted context—not untrusted arbitrary `spec` fields as identity.

## 4. Resource identity

### 4.1 Existing resources

P12 preserves existing P0-P11 IDs. Migration to the native API does not allocate replacement IDs.

### 4.2 New resource classes

A newly introduced resource class may use an opaque service-owned stable ID. Where a UUID is selected, UUIDv7 is preferred.

The following MUST NOT determine canonical identity:

- display name;
- mutable hostname;
- tenant IP address;
- project display name;
- image/volume/server user label;
- provider-native resource path;
- compatibility URL.

Delete-and-recreate with the same display/natural key MUST create a distinct resource identity.

### 4.3 Deterministic identity

UUIDv5/deterministic IDs are allowed only where the specification for that entity explicitly requires deterministic identity, for example bounded idempotency records or compatibility/provider projections.

## 5. Authentication and request context

Native API uses O3K IAM.

HTTP requests carry:

```text
Authorization: Bearer <credential>
```

The credential is validated by O3K IAM and converted to canonical `AuthContext` before any protected application operation.

Initial native identity endpoints are conceptually:

```text
POST /o3k/v1/identity/tokens
GET  /o3k/v1/identity/me
```

The exact credential request payload may evolve during implementation, but it MUST map directly to O3K IAM and MUST NOT create a separate principal/role/project database.

## 6. Ownership and scope

For ordinary tenant requests:

- effective owner/security scope comes from authenticated context plus durable resource ownership;
- caller-supplied JSON cannot override owner scope;
- a resource reference is re-authorized in the target resource's durable owner scope;
- a valid resource ID from another tenant MUST NOT reveal cross-tenant existence through success/error differences beyond the accepted information-disclosure policy.

The initial tenant API has no generic caller-selected `/projects/{id}` authority mechanism.

Cross-project/system administration is outside v1 tenant scope until a separate accepted authorization contract defines actor, target scope, actions, and audit semantics.

## 7. Canonical action mapping

Every protected endpoint maps to exactly one primary canonical action and may require additional dependency actions.

Examples:

```text
POST   /o3k/v1/compute/servers/{...} -> compute:CreateServer
GET    /o3k/v1/compute/servers/{id}  -> compute:ReadServer
DELETE /o3k/v1/compute/servers/{id}  -> compute:DeleteServer
```

OpenStack adapters and native adapters invoking the same application operation MUST use the same canonical action identity.

An adapter MUST NOT invent weaker native-only authorization merely because its URL differs.

## 8. Operation resource

### 8.1 Purpose

Long-running/uncertain mutations expose a first-class Operation resource rather than hiding asynchronous lifecycle state in HTTP connection duration.

An Operation has semantics at least equivalent to:

```text
id
service
action
owner_scope
target_resource
state
attempt
created_at
started_at
finished_at
error
result/request correlation
```

The operation state vocabulary MUST map to accepted durable store semantics. Implementation may add internal phases without exposing unstable provider detail.

### 8.2 HTTP completion semantics

Use:

- `200 OK` for completed synchronous action returning a representation;
- `204 No Content` for completed synchronous action without a body;
- `201 Created` for synchronously completed creation;
- `202 Accepted` when processing remains incomplete.

A `202` response MUST provide an operation reference. If the target canonical resource ID is already allocated, the response SHOULD also provide that target reference.

### 8.3 Unknown outcome

A transport timeout after possible side effect is an unknown outcome. The control plane observes/reconciles before issuing a duplicate non-idempotent side effect.

## 9. Idempotency

Mutation endpoints that can be safely retried MUST support an idempotency identity contract.

Initial HTTP form:

```text
Idempotency-Key: <opaque-client-key>
```

Rules:

- key scope includes authenticated ownership scope and canonical action;
- a key used for an equivalent request replays the same accepted result/operation identity;
- reuse for a conflicting payload/action fails explicitly;
- one tenant's key cannot collide with or reveal another tenant's request;
- deterministic internal derivation is allowed because idempotency identity is intentionally deterministic.

For a native canonical mutation, acceptance commits the canonical resource
intent (or validates the existing target for a lifecycle mutation), execution
Operation, canonical Operation metadata, and idempotency reservation in one
store transaction before external execution. The public Operation ID is the
same identity driven by service reconciliation; it is not a wrapper attached
to a pre-existing legacy journal row. Compatibility callers that do not claim
native canonical Operation exposure may continue using their existing legacy
journal acceptance path.

The raw key MUST NOT be used directly as a provider resource name without bounded encoding/validation.

## 10. Optimistic concurrency

Mutable desired-state resources expose `metadata.generation`.

When an endpoint supports compare-and-set semantics, clients provide an expected generation through a versioned precondition contract. Implementation may use an HTTP precondition header or explicit request field, but the choice must be consistent within v1 and documented before advertisement.

On generation mismatch:

- mutation fails with conflict/precondition semantics;
- no provider/external side effect is issued;
- the response does not silently retry against the newer desired state.

Observed generation/status does not authorize stale desired-state overwrite.

## 11. Collection pagination and filtering

Collection responses use bounded results and an opaque cursor when result count may exceed the profile's fixed safe bound.

Conceptual response:

```json
{
  "items": [],
  "next_cursor": "opaque-or-null"
}
```

Cursor rules:

- opaque to clients;
- bound to relevant filters/sort/owner scope;
- tamper-resistant or server-validated;
- no offset/database implementation contract;
- stale/invalid cursor fails safely;
- page iteration cannot cross authorization scope.

Default/max limits are implementation-profile values declared before advertisement and covered by tests.

## 12. Errors

Native errors use `application/problem+json` and RFC 9457-compatible fields.

Required behavior includes:

```json
{
  "type": "https://o3k.io/problems/resource-not-found",
  "title": "Resource not found",
  "status": 404,
  "code": "RESOURCE_NOT_FOUND",
  "request_id": "..."
}
```

O3K extension fields such as `code`, `request_id`, `resource_id`, and `operation_id` are allowed when safe.

Error responses MUST NOT include:

- SQL query/error text;
- provider credentials;
- service secrets/tokens;
- filesystem paths that reveal secrets;
- cryptographic private material;
- unauthorized foreign resource metadata.

Stable machine `code` values are contract fields and require compatibility review before removal/redefinition.

## 13. Discovery

P12 native API exposes discovery sufficient for generic tooling:

```text
GET /o3k/v1/services
GET /o3k/v1/resource-types
```

Each discoverable service/resource type comes from the validated service registry/manifest model defined by ADR-0174/SPEC-0031.

Discovery MUST NOT advertise a capability merely because a manifest syntactically mentions it. Enabled/readiness/support/claim state must remain distinguishable.

Generic dispatch resolves collection and lifecycle operation from the validated
ManifestRegistry descriptor. It invokes only operations explicitly declared by
that descriptor and uses the mapped canonical ActionId for authorization;
resource names and collection names are never used to infer actions.

OpenStack compatibility projection metadata is not required for native discovery.

## 14. Initial representative resources

P12 implementation should prove the contract with a bounded representative set before broad coverage.

Recommended first set:

- `compute:server`;
- one accepted canonical Network resource, preferably `network:address_realm` or `network:endpoint` depending on application-service readiness;
- `volume:volume`.

Additional resources are added only after their canonical domain semantics are confirmed.

Do not expose provider implementation resources such as Geneve VNI, WireGuard peer, Linux bridge/netns/veth, LVM LV path, Ceph image name, or libvirt domain name as canonical tenant resources.

## 15. Native/OpenStack authority convergence

Where semantics overlap, native and OpenStack-compatible surfaces operate one canonical resource authority.

Required representative evidence includes:

```text
create through supported OpenStack API
-> read same canonical resource through native API

create through native API
-> read through supported OpenStack API
```

This requirement does not force native-only capabilities into OpenStack shapes.

Compatibility response IDs may be projections only where an accepted compatibility contract requires different external identity; such mappings must be durable and unambiguous.

## 16. CLI contract

The `o3k` binary remains one CLI with existing operator commands and native cloud-user commands.

### 16.1 Rich core commands

Representative UX:

```text
o3k server list
o3k server show <id>
o3k server create ...
o3k volume list
o3k endpoint show <id>
```

### 16.2 Generic discovery/resource commands

P12 also requires generic operations conceptually equivalent to:

```text
o3k service list
o3k service show <service>
o3k resource-type list
o3k resource-type show <namespace:type>
o3k resource list <namespace:type>
o3k resource show <namespace:type> <id>
o3k resource create <namespace:type> --file <file>
o3k resource delete <namespace:type> <id>
```

The generic path consumes discovery/schema information and MUST NOT require a CLI rebuild merely to recognize a new conforming namespace/resource type.

JSON output is stable for scripting. Human table output may evolve without changing JSON field contracts.

## 17. Security tests

Before native API support is claimed, automated evidence MUST cover at least:

- token/authentication failure;
- cross-project read/list/create/update/delete isolation;
- foreign-resource IDOR/BOLA access;
- owner-scope injection attempts;
- unauthorized dependency references;
- operation visibility isolation;
- idempotency key cross-scope isolation and conflicting reuse;
- opaque cursor tampering/filter/owner-scope mismatch;
- malformed/oversized JSON;
- invalid resource type/namespace;
- generation mismatch before side effect;
- error/log secret redaction;
- authorization before provider/external mutation.

## 18. Persistence conformance

Native API common behavior is tested against each supported persistence profile relevant to the claim.

SQLite and PostgreSQL MUST agree on:

- ownership filtering;
- resource identity;
- generation preconditions;
- idempotency lookup;
- operation lookup/state;
- pagination ordering/cursor semantics for advertised collections.

Backend-specific performance differences are not public API semantics.

## 19. Non-goals

This spec does not define:

- full OpenStack API parity;
- Terraform provider;
- public Python/Go/Node SDKs;
- web dashboard;
- public WebSocket/event-stream API;
- multi-region resource federation;
- provider/dataplane redesign;
- production readiness/SLA.

Service/controller extensibility is defined separately in SPEC-0031 and is part of P12 architecture.

## 20. P12 native API evidence gate

The native API portion of P12 is not complete until evidence proves:

1. namespaced resource discovery;
2. canonical IAM/authorization/ownership enforcement;
3. representative Compute/Network/Volume read paths;
4. representative mutations with correct `201`/`202` semantics;
5. service-neutral Operation exposure;
6. idempotency and generation conflict safety;
7. opaque pagination scope safety;
8. SQLite/PostgreSQL conformance for the advertised profile;
9. no regression in selected OpenStack compatibility;
10. native/OpenStack access converges on one canonical authority where semantics overlap.
