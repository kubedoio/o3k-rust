# SPEC-0031 — O3K Service Extension and Controller v1

Status: Proposed

Related decision: [ADR-0174](../adr/ADR-0174-service-manifest-and-resource-provider-controller.md)
Related issue: [#727](https://github.com/kubedoio/o3k-rust/issues/727)
Related contracts:

- [Service Manifest v1](../../contracts/service-manifest-v1.schema.json)
- [OpenStack Compatibility Projection v1](../../contracts/openstack-compatibility-projection-v1.schema.json)
- [Controller Protocol v1](../../contracts/controller-protocol-v1.md)

Related normative sources:

- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166](../adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0173](../adr/ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [execution-boundary contract](../../contracts/execution-boundaries.md)

This specification is `Proposed` and does not activate dynamic service registration or external controller support until ADR-0174 receives required human architecture/security approval.

## 1. Purpose

This specification defines the minimum platform contract required for O3K to add first-class cloud services without reproducing a new independent cloud framework per service.

A conforming service reuses O3K Cloud Kernel semantics for:

- principals/service identity;
- authorization;
- durable ownership scope;
- service/resource/action registration;
- operation/request correlation;
- quotas where selected;
- audit/event identity;
- cross-service delegation/compensation;
- health/failure identity.

It owns only its service-specific resource schema, lifecycle logic, controller behavior, and domain-specific dependencies.

## 2. Service Manifest v1

Every externally registered service has a manifest conforming to `contracts/service-manifest-v1.schema.json`.

Required identity fields:

```text
manifest_version
service_id
namespace
service_version
ownership_mode
resource_types
actions
controller
```

Optional/conditional declarations include:

```text
capabilities
dependencies
quota_dimensions
regions
availability_domains
health/readiness metadata
```

OpenStack compatibility metadata MUST NOT be embedded as required native service identity. It is declared separately through the compatibility projection contract.

## 3. Identifier rules

### 3.1 Service ID

`service_id` is a stable O3K service identity. It is not a display name and MUST NOT change solely because branding changes.

### 3.2 Namespace

The namespace is a lower-case canonical authority label matching existing O3K `ServiceNamespace` restrictions unless a later accepted contract broadens the allowed grammar.

Examples:

```text
compute
network
volume
database
```

A namespace is exclusive among simultaneously active services in one control-plane authority.

### 3.3 Resource type

Each resource type is represented canonically as:

```text
namespace:name
```

Examples:

```text
compute:server
network:endpoint
volume:volume
database:instance
```

A manifest may declare only resource types in its own namespace unless an explicit accepted shared-ownership contract exists.

### 3.4 Action

Actions use the existing canonical format:

```text
namespace:Action
```

A manifest may declare only actions in its own namespace except for explicitly declared dependencies on actions owned by another service.

## 4. Manifest validation

Registration MUST fail closed for:

- unsupported `manifest_version`;
- empty/invalid service ID or namespace;
- duplicate active namespace;
- resource type outside owned namespace;
- action outside owned namespace;
- duplicate resource/action ownership;
- malformed or unbounded arrays/strings;
- unsupported controller protocol version;
- controller identity binding missing or invalid;
- dependency on an unknown resource/action when the dependency is marked required;
- incompatible declared region/AZ constraints;
- manifest digest mismatch where a stored accepted manifest is generation-bound.

Validation happens before the service becomes discoverable as Ready.

## 5. Registry state and durability

The current static P0-P11 registry remains active runtime authority until the P12 migration is implemented and proven.

The target registry persists or deterministically reconstructs accepted service registration state across controller restart.

A registered service has stable lifecycle identity with state equivalent to:

```text
Declared
Ready
NotReady
Disabled
Incompatible
```

Exact Rust enum names may differ.

Rules:

- syntactic registration does not imply Ready;
- NotReady does not erase resource ownership;
- Disabled services remain known while owned resources/in-flight operations require safe reconciliation;
- a controller reconnect does not silently create a second service authority;
- service manifest generation/version changes are explicit and auditable.

## 6. Controller model

### 6.1 Process boundary

A first-party logical service may remain in-process inside `o3kd` if ADR-0160 process-boundary criteria do not justify extraction.

An external service uses the versioned controller protocol defined by `contracts/controller-protocol-v1.md`.

Dynamic Rust shared-library loading is not part of v1.

### 6.2 Reference transport

The intended first external implementation is gRPC/protobuf over authenticated transport, with mTLS as the reference service-controller identity mechanism.

The exact protobuf is intentionally deferred until ADR-0174 is accepted. The normative protocol invariants in the contract MUST be preserved regardless of final transport framing.

### 6.3 Controller identity

The authenticated controller identity is bound to:

```text
service_id
namespace
controller protocol version
controller/session generation
```

A controller presenting valid transport credentials for one service cannot register or reconcile another namespace.

## 7. Registration handshake

A controller session performs semantics equivalent to:

```text
1. authenticate transport/service identity
2. negotiate protocol version
3. submit/reference ServiceManifest + digest/generation
4. validate manifest and namespace ownership
5. register controller session/epoch
6. evaluate health/readiness
7. publish discoverable Ready state only after all required checks pass
```

Re-registration of the same service after restart creates a new current controller session/epoch. Calls/evidence from a stale replaced session MUST be rejected.

## 8. Service principal and delegation

### 8.1 Service principal

Every external controller acts as an O3K service principal distinct from end users.

Transport authentication alone does not grant authorization to create arbitrary resources.

### 8.2 Delegated workflow context

When a user request causes a higher-level service to compose another service's resources, the delegated context preserves:

```text
original_principal
original_owner_scope
calling_service_principal
parent_action
allowed_delegated_action
target_resource/reference
request_id
operation_id
audit correlation
expiry/session bounds
```

The delegated credential/context MUST be:

- purpose-bound;
- scope-bound;
- action-bound;
- time/session bounded;
- non-transferable to another service identity;
- auditable.

A service principal cannot turn a delegated `compute:CreateServer` grant into unrelated admin/list/delete authority.

## 9. Controller operations

A controller receives service-domain work only for resource types it owns.

The protocol supports semantics equivalent to:

```text
Reconcile(resource snapshot, operation context)
Observe(resource reference, operation context)
Delete(resource snapshot/reference, operation context)
Health()
Capabilities()
```

Exact RPC names may differ.

Controller requests carry:

- service/controller identity;
- protocol version;
- request ID;
- operation ID where applicable;
- resource ID/type/generation;
- owner/delegation context where applicable;
- deadline;
- idempotency/replay identity where side effects are possible.

The controller MUST NOT be given direct database credentials for unrelated O3K domain stores merely to implement these operations.

## 10. Side-effect safety

A controller side effect follows the same O3K rules as core providers/application workflows:

- authorize before side effect;
- persist enough durable intent/operation state before non-idempotent mutation;
- treat timeout after possible mutation as unknown outcome;
- observe before retrying uncertain side effect;
- reject stale resource generation;
- reject stale controller session/epoch;
- classify retriable/non-retriable/unknown outcomes explicitly;
- compensate cross-service partial workflows according to SPEC-0021.

## 11. Cross-service composition

A higher-level service composes existing O3K resources through canonical application/service APIs.

Example conformance composition:

```text
database:instance
  -> compute:server
  -> network:endpoint
  -> volume:volume
```

Rules:

- child resources remain authoritative resources of their owning service;
- the parent stores durable references/dependency snapshots sufficient for compensation and reconciliation;
- parent deletion does not blindly delete a child it does not own;
- child creation/deletion uses delegated canonical actions;
- provider-native state is never used as the cross-service API;
- partial failure records durable operation state and performs bounded compensation.

## 12. Ownership of composed resources

A composed child resource MUST declare durable ownership/reference semantics sufficient to answer:

- which tenant/security scope owns the resource;
- which higher-level parent operation/resource requested it;
- whether deletion/compensation is authorized;
- whether the child is exclusive to the parent or independently user-managed.

A controller MUST NOT infer ownership solely from names/tags/provider IDs.

The precise parent-child ownership representation may be implemented using existing resource/dependency records or a later shared reference primitive, but it must not be service-private if authorization/cleanup depends on it across services.

## 13. Quota integration

A service manifest may declare quota dimensions owned by that service.

Each quota dimension declares at least:

```text
key
unit
scope
```

Quota enforcement uses the canonical Cloud Kernel quota/limit mechanism where selected.

A service MUST NOT silently bypass dependency quotas. For example, a Database service creating Compute/Volume resources must satisfy the delegated tenant's Compute/Volume quotas in addition to any Database-specific quota.

## 14. Audit and events

Every externally controlled mutation produces canonical audit identity sufficient to correlate:

```text
original user
calling service
canonical action
target resource
owner scope
request
operation
controller session
outcome
```

Service-local logs supplement but do not replace O3K audit identity.

The controller protocol MUST NOT require transporting secrets in ordinary audit/event payloads.

## 15. Health and readiness

Controller health is separate from service support/claim state.

Minimum semantics:

- `Ready`: controller is authenticated, compatible, manifest-valid, and able to accept declared work;
- `NotReady`: service authority remains registered but controller cannot safely accept new work;
- `Incompatible`: protocol/manifest version cannot be safely used;
- `Disabled`: operator configuration excludes new work.

Loss of health does not imply resources are gone or safe to fail over blindly.

## 16. Upgrade and compatibility

Service/controller upgrades are versioned.

Rules:

- protocol version is negotiated explicitly;
- unsupported major protocol is fail-closed;
- manifest `service_version` change is auditable;
- resource schema migrations must preserve public identity/ownership/generation;
- a new controller MUST NOT reinterpret older persisted resources without explicit migration compatibility;
- rollback must not accept state written by an incompatible newer schema unless the version contract explicitly allows it.

P12 v1 does not claim arbitrary zero-downtime third-party service upgrades; it defines the safety requirements that later implementation/evidence must prove.

## 17. Service removal/disable

A service cannot be forgotten merely because its controller disappears.

Before removal from authoritative registry, the control plane must determine at least:

- owned resources remaining;
- in-flight operations;
- dependent resources/services;
- cleanup/reconciliation responsibility;
- compatibility endpoints that would become invalid.

Unsafe removal fails closed or requires an explicit operator force/orphan policy defined by a later accepted contract.

## 18. OpenStack compatibility projection

OpenStack metadata conforms to `contracts/openstack-compatibility-projection-v1.schema.json` and is stored/derived separately from native ServiceManifest identity.

Projection may include:

```text
service_id
OpenStack service_type
catalog endpoint interfaces/regions/URLs
API surface/version/microversion range
selected compatibility capabilities
```

Rules:

- no projection is required for a native-only service;
- projection advertisement is evidence-gated by SPEC-0022;
- disabling/removing a projection does not rename the native service/resource namespace;
- OpenStack compatibility models remain protocol-edge data.

## 19. Generic discovery

Only validated registry state is exposed through native service/resource-type discovery.

Discovery distinguishes at least:

- declared service identity/version;
- Ready/NotReady/Disabled/Incompatible state;
- resource types/actions/capabilities;
- whether an OpenStack compatibility projection exists;
- support/evidence state where the product-profile machinery exposes it.

Discovery MUST NOT claim production support merely because a controller is Ready.

## 20. Minimal service SDK

P12 includes a Rust-first helper crate for the controller protocol.

Expected responsibilities:

- manifest parsing/validation helpers;
- mTLS/controller bootstrap wiring;
- protocol version negotiation;
- typed request/operation/resource references;
- delegated context verification/handling;
- correlation IDs;
- structured controller errors;
- replay/session-generation helpers.

The SDK MUST NOT:

- grant authorization by itself;
- hold system-admin credentials by default;
- implement service business logic;
- expose Cloud Kernel private store tables as an API.

## 21. Mandatory conformance service

P12 extension support is proven with a non-production example service, preferred namespace `database`.

Minimum declared surface:

```text
service_id: database-example
namespace: database
resource_type: database:instance
actions:
  database:CreateInstance
  database:ReadInstance
  database:DeleteInstance
```

Required proof:

1. manifest registers without Database-specific kernel code;
2. service/resource type appears in generic discovery;
3. generic CLI can create/show/delete the resource without Database-specific CLI code;
4. user authorization/owner scope is enforced;
5. controller service identity is authenticated;
6. at least representative Compute/Network/Volume composition occurs through canonical application boundaries where environment capability permits;
7. child-resource quotas/authorization are enforced;
8. request/operation/audit correlation preserves original actor and service principal;
9. controller restart/reconnect rejects stale session evidence;
10. cleanup leaves no owned child resources or falsely claimed deletion.

The example MUST NOT be advertised as production managed PostgreSQL/DBaaS.

## 22. Security conformance

Before extension support is claimed, tests MUST cover:

- duplicate namespace rejection;
- foreign namespace resource/action claim rejection;
- malformed/oversized manifest rejection;
- controller identity/manifest mismatch;
- forged/expired controller credentials;
- stale controller session replay;
- unauthorized delegated action;
- delegation to wrong owner scope;
- delegation reuse outside parent operation;
- cross-tenant child-resource reference;
- timeout/unknown outcome replay;
- unsafe service removal with resources/in-flight work;
- secret-safe errors/logging/audit;
- compatibility projection cannot advertise unsupported endpoints without the existing evidence gate.

## 23. Non-goals

P12 v1 does not define:

- dynamic Rust `.so` plugins;
- arbitrary untrusted code in `o3kd`;
- public marketplace/package distribution;
- production third-party ecosystem support claim;
- public Python/Go/Node service SDKs;
- multi-region/federated controller authority;
- direct external-cloud provider equivalence;
- Kubernetes CRDs as canonical O3K resources.

## 24. P12 extension evidence gate

The extension half of P12 is not complete until evidence proves:

1. validated ServiceManifest registration;
2. namespace/resource/action ownership conflicts fail closed;
3. separate OpenStack compatibility projection;
4. authenticated versioned external controller session;
5. bounded service-principal delegation preserving original actor/scope;
6. generic discovery and generic CLI operation for the conformance service;
7. cross-service composition through canonical application boundaries;
8. durable operation/audit correlation and unknown-outcome safety;
9. restart/reconnect stale-session fencing;
10. the conformance service requires no Database-specific business logic in `o3k-kernel`.
