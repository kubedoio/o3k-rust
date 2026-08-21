# ADR-0174 — O3K Service Manifest and Resource Provider/Controller Architecture

Status: Accepted
Date: 2026-08-21
Human-approval: project-requester (2026-08-21, explicit architecture/security approval recorded in PR #729 review)
Supersedes: none
Superseded-by: none
Affected-services: governance, cloud-kernel, identity, api, cli, service-registry, future-services

Related issue: [#727](https://github.com/kubedoio/o3k-rust/issues/727)

Related decisions and specifications:

- [ADR-0160 — Service topology and execution boundaries](ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0165 — O3K Cloud Operating System and shared Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility boundary](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0173 — Native O3K Resource API and Resource Model](ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [SPEC-0031 — O3K Service Extension and Controller v1](../specs/SPEC-0031-service-extension-controller-v1.md)
- [service manifest contract](../../contracts/service-manifest-v1.schema.json)
- [OpenStack compatibility projection contract](../../contracts/openstack-compatibility-projection-v1.schema.json)
- [controller protocol contract](../../contracts/controller-protocol-v1.md)

This ADR establishes a new extensibility/security boundary and external service identity model. Human architecture/security approval was recorded on 2026-08-21 under ADR-0154 (PR #729 review).

## Context

ADR-0165 requires future first-class O3K services to reuse the shared Cloud Kernel rather than independently rebuilding tenant isolation, authorization, quotas, durable operations, audit/event identity, and lifecycle recovery.

The current `KernelRegistry` and `contracts/cloud-kernel-services.yaml` were appropriate for the P0-P11 foundation, but they are static inventories and still mix native service identity with OpenStack compatibility metadata such as `service_type`, compatibility API surfaces, and Keystone endpoint concepts.

If P12 merely adds more hard-coded service descriptors and API routes, O3K risks recreating the extensibility tax it is intended to remove. A future service such as `database` must not require Database-specific fields or branches inside `o3k-kernel` merely to become a first-class O3K service.

At the same time, unrestricted in-process plugin loading would create unacceptable ABI, privilege, failure-containment, and trust problems. O3K therefore needs an explicit service manifest and versioned controller boundary.

## Decision

### 1. O3K defines a canonical Service Manifest

A first-class service is described by a versioned `ServiceManifest` with semantics equivalent to:

```text
ServiceManifest {
    manifest_version
    service_id
    namespace
    service_version
    ownership_mode
    resource_types[]
    actions[]
    capabilities[]
    dependencies[]
    quota_dimensions[]
    region / availability-domain scope
    controller
    health/readiness
}
```

The exact machine-readable contract is `contracts/service-manifest-v1.schema.json` and SPEC-0031.

The manifest describes O3K-native service identity and capabilities. It is not a Keystone catalog entry.

### 2. OpenStack compatibility metadata is a separate projection

An O3K service does not need an OpenStack service type, public/internal/admin endpoint set, or microversion range in order to exist.

Compatibility metadata is represented separately through an `OpenStackCompatibilityProjection` contract containing only selected verified compatibility surfaces.

Conceptually:

```text
ServiceManifest
   +-- native discovery
   +-- namespace ownership
   +-- resource/action vocabulary
   +-- quota/capabilities/dependencies
   +-- controller binding

OpenStackCompatibilityProjection
   +-- Keystone service type/catalog endpoints
   +-- OpenStack API surfaces/versions
   +-- compatibility capability advertisement
```

A future `database` service is valid without any OpenStack projection.

### 3. Service namespaces are exclusive authority boundaries

Every active service owns exactly one accepted namespace unless a later ADR explicitly defines a multi-namespace service.

A service owning `database` may declare resources/actions such as:

```text
database:instance
database:backup
database:CreateInstance
database:DeleteInstance
```

It MUST NOT claim another service's namespace, such as `compute`, `network`, or `volume`, merely because it composes those resources.

Registration fails closed on:

- duplicate active service namespace;
- duplicate resource type ownership;
- duplicate action ownership;
- malformed identifiers;
- manifest version unsupported by the control plane;
- undeclared dependency/capability requirements that cannot be satisfied safely;
- controller identity inconsistent with the manifest.

### 4. First-party modules and external controllers share service semantics but not necessarily process topology

A logical O3K service does not require a separate daemon. First-party core services may continue to run in the modular `o3kd` composition when privilege/failure/scaling boundaries do not justify extraction.

External/new services may use a versioned controller protocol.

The service model therefore distinguishes logical service ownership from process placement.

### 5. No dynamic Rust `.so` plugin ABI

P12 MUST NOT establish arbitrary in-process dynamic library loading as the extension model.

Reasons:

- Rust has no stable cross-version ABI appropriate for a long-lived third-party plugin ecosystem;
- in-process plugins share memory, privileges, crash fate, and secret exposure with `o3kd`;
- safe upgrade/rollback/version negotiation is harder;
- language choice would be artificially restricted to Rust.

The preferred external model is a versioned, language-neutral protocol. gRPC/protobuf over authenticated transport is the reference direction unless implementation evidence later justifies another protocol.

### 6. External controllers authenticate as service principals

An external controller is not trusted because it can reach a socket.

The control plane authenticates controller identity and binds it to the accepted service manifest. mTLS is the preferred transport identity for the initial controller protocol because O3K already uses typed gRPC/mTLS boundaries elsewhere.

Controller identity MUST be distinct from tenant user identity.

### 7. Delegated work preserves original actor and scope

When a service performs work on behalf of a user, the effective authorization context preserves at least:

```text
original actor
original owner/security scope
calling service principal
delegated action
target resource/reference
request ID
operation ID
audit correlation
```

A service principal MUST NOT silently become an administrator.

The controller receives only the delegation required for the accepted workflow. It cannot use a user operation as a general-purpose bearer grant for unrelated actions.

### 8. Higher-level services compose canonical O3K resources through application contracts

A higher-level service such as `database:instance` may depend on:

```text
compute:server
network:endpoint
volume:volume
```

It performs composition through canonical O3K application/service APIs with explicit delegation and durable operation identity.

It MUST NOT directly manipulate:

- libvirt/QEMU provider state;
- Ceph/LVM provider state;
- WireGuard/Geneve/Linux networking internals;
- another service's private database tables;
- another service's provider mappings.

Cross-service compensation follows SPEC-0021.

### 9. The Cloud Kernel remains service-neutral

Adding a new service MUST NOT require adding business-specific fields, states, reconciliation branches, or type-specific authorization logic to `o3k-kernel`.

Kernel changes are justified only when a genuinely shared primitive is required across services and receives normal architecture review.

The extension success test is therefore stronger than “the service can register a URL.” The service must be able to participate in ownership, authorization, operations, audit, quotas, and generic discovery without kernel knowledge of its business domain.

### 10. The service registry evolves from static inventory to validated registry semantics

The existing static registry remains the current runtime source until this ADR is accepted and P12 implementation migrates it safely.

The target registry supports validated descriptors/manifests and separate compatibility projections. It MUST retain deterministic discovery for first-party services and MUST NOT create a second source of truth competing with durable accepted configuration.

Runtime registration semantics, persistence, restart behavior, disable/remove behavior, and upgrade compatibility are specified in SPEC-0031.

### 11. Service lifecycle is explicit

A service/controller has lifecycle state at least sufficient to represent:

```text
Declared
Ready
NotReady
Disabled
Incompatible
```

Exact wire values are specified by SPEC-0031.

A service becoming `NotReady` does not transfer authority to another controller implicitly. Pending operations follow accepted retry/unknown-outcome/fencing semantics.

Removal/disable MUST fail safely when owned resources or in-flight operations require continued reconciliation.

### 12. Controller calls are bounded and replay-safe

Controller protocol requests carry stable request/operation identity and controller generation/session identity where required.

The protocol MUST support:

- version negotiation;
- bounded request size;
- explicit deadlines;
- idempotent/replay-safe operation keys;
- stale controller/session rejection;
- structured failure categories;
- health/readiness;
- no secret leakage in ordinary status/errors;
- safe reconnect/replay after control-plane or controller restart.

A timeout is an unknown outcome when a side effect may have occurred.

### 13. Minimal service SDK is part of P12

P12 may defer public user SDKs, but it requires a minimal service/controller SDK for the reference protocol.

The initial SDK may be Rust-first but the underlying protocol remains language-neutral.

The SDK provides protocol/platform plumbing only, such as:

- service identity/registration;
- manifest loading/validation helpers;
- delegated auth context handling;
- resource/operation references;
- request/audit correlation;
- controller response/failure classification.

It MUST NOT contain Database/DNS/AI/etc. business logic.

### 14. Generic API/CLI discovery is mandatory

A registered service/resource type must become visible through native service/resource-type discovery.

The generic CLI path defined by ADR-0173/SPEC-0030 MUST allow at least basic CRUD/action invocation supported by the manifest without rebuilding the CLI specifically for that service.

Rich service-specific CLI UX may be added separately.

### 15. P12 requires a non-core conformance service

The architecture is not proven by documentation or by exposing only Compute/Network/Volume.

P12 must include a deliberately small, non-production conformance service. The preferred reference is:

```text
service namespace: database
resource type:     database:instance
actions:           database:CreateInstance, database:ReadInstance, database:DeleteInstance
```

The reference controller should compose at least representative Compute, Network, and Volume resources where existing capabilities permit a truthful end-to-end proof.

Acceptance criterion:

> The example service is added without Database-specific business logic in `o3k-kernel`, is discovered generically, inherits common platform contracts, and composes existing resources through canonical application APIs.

If implementation requires hard-coded Database branches in the kernel or generic CLI, P12 extension architecture has failed and must be corrected before completion.

### 16. Extension security is fail-closed

Required security evidence includes:

- namespace/resource/action hijacking rejection;
- forged service identity rejection;
- controller certificate/identity mismatch rejection;
- delegation escalation rejection;
- cross-project resource access rejection;
- stale/replayed controller session rejection;
- operation/idempotency scope isolation;
- manifest input bounds and schema validation;
- dependency-cycle/conflict handling where applicable;
- secret redaction and bounded diagnostics;
- safe disable/remove behavior.

## Consequences

### Positive

- O3K can become a platform for cloud services rather than a fixed set of services.
- New services reuse common cloud plumbing.
- OpenStack compatibility no longer contaminates native service identity.
- External services gain failure/process isolation and language independence.
- Namespace ownership provides a clear security and governance boundary.
- The conformance service gives executable evidence for the Cloud OS claim.

### Negative

- Registry, controller identity, delegation, and lifecycle become security-critical platform components.
- A versioned controller protocol introduces compatibility and upgrade obligations.
- Generic discovery/schema handling increases API and CLI complexity.
- Composition across services requires careful durable compensation rather than simple synchronous call chaining.
- Service removal/upgrades are harder than static compile-time composition and require explicit safety rules.

## Rejected alternatives

### Keep a static hard-coded registry forever

Rejected because every new service would require core edits and would preserve the extensibility bottleneck.

### Put OpenStack `service_type` and Keystone endpoint fields in every native manifest

Rejected because OpenStack compatibility is optional projection metadata, not native service identity.

### Load arbitrary Rust dynamic libraries into `o3kd`

Rejected because of ABI instability, privilege/crash coupling, trust exposure, and language lock-in.

### Let higher-level services call providers directly

Rejected because it bypasses canonical resource authority, policy, audit, compensation, scheduling, and provider ownership boundaries.

### Give service principals broad admin credentials

Rejected because service identity is not equivalent to tenant/system administrator authority; delegation must be bounded to the accepted workflow.

## Required follow-up

- accept or reject this ADR through human architecture/security review;
- apply SPEC-0031 only after acceptance;
- add validated manifest and compatibility-projection types without breaking the current accepted runtime registry;
- define/version the first controller protobuf and mTLS identity binding only after the architecture gate;
- implement the minimal Rust service SDK;
- implement the non-core conformance service and generic discovery proof;
- add extension security/conformance gates before any ecosystem/support claim;
- do not claim “third-party plugin ecosystem” or production extension support until installation, upgrade, removal, compatibility, security, and evidence profiles are separately proven.
