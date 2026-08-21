# ADR-0173 — Native O3K Resource API and Resource Model

Status: Proposed
Date: 2026-08-21
Human-approval: pending
Supersedes: none
Superseded-by: none
Affected-services: governance, cloud-kernel, identity, compute, network, image, placement, volume, api, cli, future-services

Related issue: [#727](https://github.com/kubedoio/o3k-rust/issues/727)

Related decisions and specifications:

- [ADR-0165 — O3K Cloud Operating System and shared Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility boundary](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169 — Native persistent storage and O3K storage boundary](ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [ADR-0171 — AddressRealm-encapsulated edge fabric](ADR-0171-addressrealm-encapsulated-edge-fabric.md)
- [ADR-0174 — O3K Service Manifest and Resource Provider/Controller Architecture](ADR-0174-service-manifest-and-resource-provider-controller.md)
- [SPEC-0030 — Native O3K Resource API v1](../specs/SPEC-0030-native-o3k-resource-api-v1.md)
- [SPEC-0031 — O3K Service Extension and Controller v1](../specs/SPEC-0031-service-extension-controller-v1.md)
- [native resource envelope contract](../../contracts/native-resource-envelope-v1.schema.json)

This ADR changes public-contract architecture and security-sensitive scope handling. It MUST remain `Proposed` until explicit human architecture/security approval is recorded under ADR-0154.

## Context

ADR-0165 made O3K's long-term product identity explicit: O3K is a Cloud Operating System whose canonical resource authority is independent from OpenStack wire models. OpenStack remains a strategically important compatibility surface, but Nova, Neutron, Keystone, Glance, Placement, and Cinder request/response shapes must not become the internal product model.

P9-P11 established mature native semantics for networking, storage, and multi-hypervisor operation. P12 is therefore the point at which O3K can expose those semantics as a first-class native contract instead of forcing every user-facing capability through historical OpenStack shapes.

A narrow REST rewrite would be insufficient. The native API must preserve the architectural possibility for future first-class services—database, DNS, Kubernetes, AI/ML, object services, and others—to expose namespaced resources through the same Cloud Kernel without adding service-specific business logic to the kernel.

## Decision

### 1. P12 is the Native O3K Resource API and Service Framework milestone

P12 is not only a CLI/REST convenience milestone. Its architecture target is:

> A first-class O3K-native resource API and CLI over the canonical Cloud Kernel model, designed so future services can add namespaced resource types without redefining IAM, authorization, ownership, operations, audit, quotas, or public-resource identity.

ADR-0174 defines the extensible service/controller half of this target.

### 2. Native API paths are service-namespaced

The native HTTP root is:

```text
/o3k/v1/{service-namespace}/{resource}
```

Examples:

```text
/o3k/v1/compute/servers
/o3k/v1/network/address-realms
/o3k/v1/network/endpoints
/o3k/v1/storage/volumes
/o3k/v1/database/instances
```

The namespace is the canonical O3K service namespace, not an OpenStack service name copied for compatibility.

A future breaking native API generation may use `/o3k/v2/...`. OpenStack microversion negotiation does not become the native versioning model merely because compatibility adapters use it.

### 3. Canonical action identifiers remain service-namespaced

Native and OpenStack-compatible adapters invoke the same canonical application actions, for example:

```text
compute:CreateServer
network:CreateEndpoint
volume:CreateVolume
database:CreateInstance
```

O3K MUST NOT create parallel `openstack:*` and `o3k:*` authorization vocabularies for the same semantic operation. Protocol-only compatibility actions may exist only when they genuinely cannot map to a canonical application action.

### 4. Public resource identity is opaque and independent from display/natural keys

Existing P0-P11 resource IDs are preserved. P12 MUST NOT re-key existing resources.

New canonical resource IDs MUST NOT be deterministically derived from mutable or reusable human keys such as name, project display name, IP, hostname, or other natural key.

Rules:

- display names are metadata, never identity;
- delete-and-recreate with the same human name produces a distinct resource identity;
- opaque generated IDs are preferred;
- UUIDv7 is the preferred UUID form for newly introduced canonical resource classes where a UUID is required;
- deterministic UUIDv5 remains appropriate only when deterministic identity is itself the accepted semantic contract, such as bounded idempotency identity or explicit compatibility/provider projections.

The existing generic `ResourceId` concept remains valid and does not require every service to expose a UUID on the wire.

### 5. O3K defines a common resource envelope, not a giant common business schema

A native resource has common metadata with semantics equivalent to:

```text
ResourceEnvelope {
    api_version
    kind / resource_type
    id
    owner_scope
    generation
    region / availability-domain when applicable
    created_at
    updated_at
    labels / annotations when selected
    spec
    status
}
```

The exact wire contract is SPEC-0030 and `contracts/native-resource-envelope-v1.schema.json`.

The Cloud Kernel owns common identity/ownership/generation semantics. Each service owns the schema and validation of its `spec` and `status`. The kernel MUST NOT grow a universal enum containing every future service's business fields.

### 6. Native resources follow canonical O3K domain semantics

The native API MUST NOT expose a resource merely because an OpenStack API has a similarly named object.

Examples:

- accepted AddressRealm/Endpoint/NetworkPolicy/PublicAddress semantics may become native Network resources;
- a Neutron `subnet` does not automatically become a canonical native resource if the accepted O3K model does not require it;
- Geneve VNIs, WireGuard peers, Linux bridges, namespaces, nftables handles, provider tunnel IDs, and similar execution details remain provider state;
- Cinder/Nova compatibility objects remain projections when they do not correspond one-to-one with native authority.

Provider-native identity is never promoted to public O3K identity merely for API convenience.

### 7. Native authentication uses O3K IAM directly

Native API requests use bearer authentication over O3K-issued/validated credentials and produce the same typed `AuthContext` consumed by first-class O3K services.

Conceptually:

```text
Keystone-compatible API ----\
                             > O3K IAM -> AuthContext
Native identity API --------/
```

The native API MUST NOT permanently depend on Keystone request envelopes for login. P12 defines a native identity/context surface such as:

```text
POST /o3k/v1/identity/tokens
GET  /o3k/v1/identity/me
```

This is another adapter over O3K IAM, not a second identity system.

### 8. Ownership scope comes from authenticated authority, not arbitrary path selection

For ordinary tenant operations, the target ownership/security scope is derived from the authenticated `AuthContext` and the referenced resource's durable ownership.

The initial native API MUST NOT treat a caller-controlled `/projects/{id}` path segment as sufficient authority to act cross-project.

Cross-project/system/domain administration requires a separately authorized target-scope contract with explicit action, actor, target scope, and audit semantics. It fails closed when such authority is absent.

### 9. Operations are first-class and service-neutral

The existing reconciler and operation journal provide valuable proven semantics, including durable intent, unknown outcomes, observation-before-retry, fencing, and compensation. They are not themselves the universal public API because current implementations contain Compute/provider-specific behavior.

P12 therefore defines a service-neutral operation contract with semantics equivalent to:

```text
Operation {
    id
    service
    action
    actor / service-principal context
    owner_scope
    target_resource
    state
    attempt
    created_at
    started_at
    finished_at
    error
    result/reference
    request/audit correlation
}
```

Exact states map to accepted durable store semantics and MUST NOT create a second incompatible operation state machine.

HTTP behavior follows completion state:

- `200`/`204` for completed synchronous mutation;
- `201 Created` for synchronously completed creation;
- `202 Accepted` only when accepted processing remains incomplete.

A `202` response makes both the operation and intended target resource discoverable where a target identity already exists.

### 10. Errors use Problem Details semantics

The native API uses RFC 9457-compatible HTTP Problem Details. O3K stable machine codes are extension members.

The error contract MUST NOT expose SQL errors, secrets, provider credentials, internal paths, cryptographic material, or cross-tenant resource existence.

### 11. Pagination cursors are opaque

Collection APIs use bounded cursor pagination where required. Public cursors are opaque tokens. Their internal representation may bind sort position, resource ID, filter hash, owner scope, and cursor schema version.

Clients MUST NOT depend on database offsets or `cursor=<resource-id>` semantics. Cursor evaluation MUST preserve authorization/filter scope and work across supported SQLite/PostgreSQL persistence profiles.

### 12. Optimistic concurrency is generation-based

Where mutation races matter, the native API exposes the canonical resource generation and requires explicit compare-and-set/precondition behavior defined by SPEC-0030.

Provider observations do not silently overwrite newer desired-state generations.

### 13. Native API and OpenStack compatibility share application services

The intended dependency direction is:

```text
Native O3K API ----\
                    > application services -> Cloud Kernel/domain
OpenStack API -----/
```

The native adapter MUST NOT duplicate Compute/Network/Volume business logic merely to produce different JSON.

Internal refactoring of `o3k-api` is allowed when necessary to converge adapters on shared application boundaries, provided compatibility evidence proves no OpenStack regression.

### 14. CLI provides both ergonomic core commands and generic resource discovery

The `o3k` CLI may add rich first-class commands for core resources, for example `o3k server list` or `o3k volume create`.

It also MUST support generic discovery/operation sufficient for a newly installed service to become usable without a CLI release specifically hard-coding that service. The model includes:

```text
o3k service list
o3k resource-type list
o3k resource list <namespace:type>
o3k resource show <namespace:type> <id>
o3k resource create <namespace:type> --file <file>
```

Exact command syntax is specified in SPEC-0030.

### 15. Native API security is a new attack surface

The native API reuses canonical IAM, authorization, ownership, operation, quota, and audit contracts, but it still creates a new externally reachable protocol surface.

Required security evidence includes at least:

- cross-project isolation;
- IDOR/BOLA resistance;
- caller-controlled scope injection;
- unauthorized cross-resource references;
- operation visibility isolation;
- idempotency scope isolation;
- cursor/filter leakage prevention;
- oversized/malformed input rejection;
- secret-safe errors/logging.

Every mutation authorizes before provider or external-service side effects.

## Consequences

### Positive

- O3K gains a first-class API for its own resource semantics.
- OpenStack wire models remain protocol-edge concerns.
- Native and compatibility clients can operate the same canonical resources.
- Future services have a stable namespaced API model rather than requiring new global conventions.
- Opaque IDs and cursors preserve evolution freedom.
- Common operations/errors/concurrency semantics reduce per-service reinvention.

### Negative

- P12 requires architectural refactoring before broad endpoint implementation.
- Native and compatibility APIs may expose different shapes over the same authority and require explicit translation tests.
- Generic resource discovery creates schema/versioning/security responsibilities that a fixed CLI would avoid.
- Service-neutral operations require careful convergence from currently domain-specific implementations.

## Rejected alternatives

### Make the native API a cleaned-up copy of Nova/Neutron/Cinder

Rejected because it preserves OpenStack as the canonical product model and blocks future non-OpenStack-native services.

### Flat global `/o3k/v1/{resource}` namespace

Rejected because future independently developed services would compete for global resource names and service ownership would be harder to enforce.

### Natural-key-derived UUIDv5 resource IDs

Rejected because names and natural keys can be mutable/reusable and would collapse delete/recreate history into the same durable identity.

### Make every mutation `202 Accepted`

Rejected because synchronous completed work should report synchronous completion; `202` represents accepted but incomplete processing.

### Put a project ID in every native resource path

Rejected for the initial tenant API because routing input is not authorization. Cross-project administration requires an explicit security model.

## Required follow-up

- accept or reject this ADR through human architecture/security review;
- apply SPEC-0030 only after this ADR is accepted;
- define/accept ADR-0174 and SPEC-0031 before claiming extension-framework support;
- add `crates/o3k-native-api` only after the architecture gate;
- add native resource/discovery conformance tests for SQLite and PostgreSQL where applicable;
- prove representative OpenStack/native round-trip convergence without expanding compatibility claims;
- keep broad endpoint count secondary to contract correctness and evidence.
