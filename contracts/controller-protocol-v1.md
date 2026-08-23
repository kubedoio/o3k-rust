# O3K Controller Protocol v1 — accepted invariants

The P12.5 Rust transport implementation is versioned as protobuf package
`o3k.controller.v1` in `crates/o3k-controller-protocol`. Its transport is
tonic gRPC with mandatory mutual TLS; the SDK exposes CA/server-name/client
certificate configuration and never exposes an insecure default. Registration
must bind the authenticated service endpoint to the accepted manifest identity,
negotiate an explicitly supported version, establish a session ID/generation,
and validate capabilities and health before readiness. Request contexts carry
operation, owner scope, action, deadline, session, replay, and audit identity.
The Rust kernel model returns actual `Observation` data for Observe and keeps
unknown mutation outcomes distinct from failures. The SDK replay ledger is
bounded and session-fenced; durable operation identity remains a kernel/store
responsibility. P12.5 does not add Database composition.

Status: Accepted
Related decision: `docs/adr/ADR-0174-service-manifest-and-resource-provider-controller.md`
Related spec: `docs/specs/SPEC-0031-service-extension-controller-v1.md`
Related issue: #727

This contract freezes protocol invariants before a concrete protobuf is accepted. It does not activate external controller support by itself.

## 1. Purpose

The controller protocol is the bounded external-service boundary for first-class O3K services that should not execute inside `o3kd`.

The reference implementation direction is versioned gRPC/protobuf over mutually authenticated transport. A later concrete protobuf may choose names and field numbering, but it MUST preserve the invariants below.

Dynamic Rust shared-library loading is explicitly not this contract.

## 2. Authority model

```text
O3K Cloud Kernel/application services
        |
        | authenticated/versioned controller protocol
        v
external service controller
```

The controller owns only the resource types declared by its accepted `ServiceManifest` namespace.

It does not become authority for:

- O3K users/projects/roles outside its service-owned records;
- another service's canonical resources;
- provider-native Compute/Network/Storage state;
- Cloud Kernel policy or quota decisions;
- arbitrary cross-tenant actions.

## 3. Required transport/session identity

Every external controller session MUST bind:

```text
service_id
service_namespace
service_principal_id
protocol_major
protocol_minor/controller capability set
controller_session_id or epoch
manifest digest/generation
```

The authenticated transport identity MUST match the accepted service principal/manifest binding.

A stale or replaced controller session MUST NOT mutate current service resources.

## 4. Registration sequence

A safe registration sequence has semantics equivalent to:

```text
1. establish authenticated transport
2. validate service principal
3. negotiate supported protocol version
4. submit/reference ServiceManifest
5. verify manifest digest/generation
6. validate namespace/resource/action ownership
7. create current controller session/epoch
8. evaluate health/readiness
9. publish Ready only after all checks succeed
```

Failure at any step is fail-closed. A failed controller does not become discoverable as Ready merely because its TCP/TLS connection succeeded.

## 5. Protocol versioning

The concrete protocol MUST carry an explicit version.

Rules:

- unsupported major version -> reject session;
- minor/capability negotiation may enable additive behavior only when both sides support it;
- unknown required field/capability -> fail closed;
- protocol downgrade MUST NOT bypass a security invariant;
- protocol version is auditable with each controller session.

## 6. Required operation context

Every mutation/reconciliation request that may cause side effects carries at least:

```text
request_id
operation_id
service_id
resource_type
resource_id
resource_generation
owner_scope
controller_session/epoch
deadline
idempotency/replay identity
```

When work is delegated from a user through a service, it additionally carries bounded delegated identity described in §7.

The controller MUST reject an operation whose resource type is outside its accepted namespace ownership.

## 7. Delegated actor context

Delegated work preserves:

```text
original_principal
original_owner_scope
calling_service_principal
parent_action
allowed_delegated_action
parent_resource/reference where applicable
request_id
operation_id
expiry/session bounds
```

Delegation invariants:

- action-bound;
- target/scope-bound;
- service-principal-bound;
- time/session-bound;
- auditable;
- not reusable for unrelated actions;
- not equivalent to system-admin authority.

A controller MUST NOT accept caller-supplied ownership scope that is not covered by the authenticated delegated context.

## 8. Minimum logical operations

The concrete protocol SHOULD support logical operations equivalent to:

```text
Register/Negotiate
Health
Capabilities
Reconcile
Observe
Delete
```

Exact RPC names are not frozen by this document.

### Reconcile

Input: accepted resource snapshot + operation/delegation context.

Output: structured accepted/in-progress/succeeded/failed/unknown result plus safe status/observation data.

### Observe

Input: resource/reference + operation context.

Output: current controller-owned observation suitable for O3K reconciliation.

### Delete

Deletion is a reconciliation operation, not permission for blind provider cleanup. Ownership and generation checks apply.

### Health/Capabilities

Health is bounded service-controller readiness and MUST remain distinct from product support/evidence claims.

## 9. Unknown outcomes and retries

A timeout after a request may have produced a side effect is `unknown`, not `failed`.

Required behavior:

1. preserve operation identity;
2. observe current state before retry when side effect may already exist;
3. retry only when the operation/controller contract proves replay safety;
4. never allocate a second canonical resource merely because the response was lost;
5. use the same idempotency/replay identity for equivalent retries;
6. reject conflicting reuse of operation/idempotency identity.

## 10. Generation and stale-session fencing

The controller MUST reject:

- stale resource generation attempting to overwrite newer desired state;
- stale/replaced controller session/epoch;
- evidence from a service principal no longer bound to the active manifest/controller;
- response whose operation/resource identity does not match the request.

A controller reconnect under a new accepted session may resume/reconcile existing durable operations, but old-session messages remain stale.

## 11. Structured failure categories

The concrete protocol MUST distinguish at least the semantic categories needed by O3K reconciliation:

```text
invalid_request
unauthorized/forbidden
conflict/stale_generation
not_found where disclosure is authorized
not_ready
retryable
non_retryable
unknown_outcome
incompatible
```

Exact wire enum names may differ.

Free-form error strings are diagnostic only and MUST NOT determine retry policy.

## 12. Input/output bounds

Before support is claimed, concrete protocol limits MUST exist for:

- maximum manifest size;
- maximum resource payload size;
- maximum list/map cardinality;
- maximum diagnostic/error string size;
- maximum deadline;
- maximum outstanding controller work per service/session where needed.

Unbounded controller payloads are forbidden.

## 13. Secret handling

Ordinary controller request/response/status/audit payloads MUST NOT expose:

- user passwords;
- bearer tokens unless an explicitly designed delegated credential transport requires a redacted/encapsulated token;
- TLS private keys;
- provider credentials;
- Ceph/LVM/libvirt secrets;
- WireGuard private keys;
- database passwords for unrelated services.

If a future service requires secret material, it needs an explicit secret-delivery contract rather than adding generic `secret: string` fields to this protocol.

## 14. Cross-service composition

An external service controller that needs Compute/Network/Volume resources calls canonical O3K application/service boundaries through bounded delegated actions.

It MUST NOT receive private SQL/database access to another service as the composition mechanism.

Parent/child operations preserve durable references and compensation semantics from SPEC-0021.

## 15. Health/readiness behavior

Controller health states are advisory inputs to O3K service lifecycle authority.

Loss of controller health:

- prevents new work when safe policy requires it;
- does not prove existing resources are deleted;
- does not authorize another controller/session to mutate them without accepted takeover/fencing;
- does not erase service/resource ownership from registry state.

## 16. Removal and replacement

Replacing a controller creates a new current session/epoch after identity/manifest validation.

Removing a service/controller is blocked or explicitly orphaned when any of the following remain:

```text
owned resources
in-flight operations
required dependency edges
cleanup/reconciliation obligations
active compatibility projections that would become invalid
```

A later force-orphan mode requires separate accepted semantics.

## 17. Audit requirements

Every side-effecting controller call MUST be correlatable with:

```text
original actor (if delegated)
calling service principal
service/controller session
canonical action
resource
owner scope
request ID
operation ID
outcome/failure category
```

Transport logs do not replace canonical audit records.

## 18. Required conformance tests

The first concrete controller implementation MUST prove at least:

- valid service registration/readiness;
- unsupported protocol rejection;
- duplicate namespace rejection;
- controller identity/manifest mismatch rejection;
- forged credential rejection;
- stale session/epoch rejection;
- stale resource generation rejection;
- cross-scope/delegation escalation rejection;
- replay of equivalent idempotent request;
- conflicting operation/idempotency reuse rejection;
- timeout -> observe-before-retry behavior;
- bounded payload rejection;
- secret-safe errors/logs;
- restart/reconnect continuation of a durable operation;
- safe service disable/removal blocking with owned work.

## 19. P12 conformance-service proof

The reference `database:instance` example must use this boundary (or the accepted concrete implementation derived from it) without requiring Database-specific business logic in `o3k-kernel`.

The proof must preserve:

```text
user -> database action
     -> service principal + bounded delegation
     -> canonical Compute/Network/Volume actions
     -> durable operation/audit correlation
```

A special hard-coded bypass for the example does not satisfy this contract.
