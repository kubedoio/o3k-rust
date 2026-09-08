# ADR-0181 — Canonical location identity: regions and availability domains

Status: Accepted
Date: 2026-09-08
Human-approval: project-requester (2026-09-08, explicit architecture/security approval recorded in task instruction)
Supersedes: none
Superseded-by: none
Affected-services: governance, cloud-kernel, api, compute, network, volume, image, identity, future-services

Related issue: [#887](https://github.com/o3kio/o3k/issues/887)

Related decisions and specifications:

- [ADR-0165 — O3K Cloud Operating System and shared Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility boundary](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0173 — Native O3K Resource API and Resource Model](ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [ADR-0174 — O3K Service Manifest and Resource Provider/Controller Architecture](ADR-0174-service-manifest-and-resource-provider-controller.md)
- [SPEC-0030 — Native O3K Resource API v1](../specs/SPEC-0030-native-o3k-resource-api-v1.md)
- [SPEC-0031 — O3K Service Extension and Controller v1](../specs/SPEC-0031-service-extension-controller-v1.md)
- [SPEC-0038 — Canonical location discovery v1](../specs/SPEC-0038-canonical-location-discovery-v1.md)

This ADR defines a new public-contract location-identity architecture. Human architecture/security approval was recorded on 2026-09-08 by project-requester in the task instruction for PR #889 / issue #887.

## Context

Araf and generic O3K clients, Terraform, SDKs, CLI automation, and future service integrations need to discover, authoritatively and provider-neutrally, which O3K regions and availability domains exist, how they relate, and what placement scope resources and services support.

Today the native surface exposes no canonical location topology:

- `ServiceManifest` carries flat `regions` and `availability_domains` string lists (ADR-0174 / SPEC-0031), all seeded empty, with **no region-to-availability-domain relationship** and **no relationship validation**.
- `o3k-placement` and `o3k-scheduler` are node/provider-inventory and allocation abstractions with **no region, availability-domain, or host-aggregate concept**.
- Execution providers (`o3k-provider`, `o3k-storage`, `o3k-network`) advertise capability labels, never location: replacing a provider must never change tenant-visible location identity.
- The only live region/AZ plumbing is Cinder volume `availability_zone` pass-through and the fixed OpenStack-compat Keystone catalog `RegionOne` — unrelated to native location truth.

No single authority represents "which regions/AZs exist". That is the gap this ADR closes.

## Decision

### 1. One canonical location authority: `LocationRegistry`

Create a single canonical, read-only `LocationRegistry` in the O3K Cloud Kernel namespace that owns the **global region-to-availability-domain topology** for a deployment:

```text
LocationRegistry
  └─ Region       (stable, provider-neutral id)
       └─ AvailabilityDomain   (stable, provider-neutral id, scoped to its region)
```

- It is seeded by the deployment composition root from **configuration** (`O3K_LOCATIONS`), never derived from compute providers, hosts, hypervisors, clusters, Ceph pools, storage backends, or network controllers.
- Region and availability-domain identity are the **declared ID strings only**. No provider/backend/host identity participates.
- The registry validates deterministically (empty/malformed/duplicate IDs; an AZ declared in more than one region is an ambiguous mapping and is rejected).
- The registry exposes only sorted topology, so discovery is deterministic.

This is the single authority. There is no second independent source of location truth.

### 2. Manifests are placement filters, not a second authority

`ServiceManifest.regions` and `ServiceManifest.availability_domains` remain the per-service placement declaration, but they are now interpreted as **filters/references onto canonical `LocationRegistry` IDs**:

- Every referenced region/AZ must already exist as canonical O3K location identity (validated by the composition root; unknown references fail closed).
- Because the manifest can only reference canonical IDs, it cannot invent location truth or drift from the registry.

This satisfies the authority-model rule: manifests reference/filter canonical locations; they do not create a competing authority.

### 3. Placement semantics are derived, not independently declared

Resource/service placement scope is **derived from the single manifest region/AZ declaration**, with no separate placement field that could diverge:

| Manifest regions | Manifest AZs | Derived scope | Derived AZ selection |
|---|---|---|---|
| empty | empty | `global` | `unsupported` |
| non-empty | empty | `regional` | `unsupported` |
| non-empty | non-empty | `regional` | `optional` |
| empty | non-empty | `global` | `required` |

Derivation makes a mutually-contradictory **scope** declaration impossible by construction: `global` and `regional` are mutually exclusive functions of the same manifest `regions` field, so a manifest cannot simultaneously be global and regional. Availability-domain selection is orthogonal placement metadata, not scope: the combination of a `global`-scoped resource with `required` availability-domain selection is legal and means "place in any canonical region, but a concrete availability domain must be selected".

Only canonical region IDs are disclosed to clients; declared-but-unknown regions are filtered out (fail closed). Provider/host/backend identity is never exposed.

### 4. Public discovery endpoints

- `GET /o3k/v1/regions` — the canonical, sorted region topology and the AZs of each region.
- `GET /o3k/v1/resource-types` carries placement metadata (derived scope, canonical regions, AZ-selection capability) per resource type.

Both are registered in the production composition `o3k_api::router_with_state` so actual `o3kd` exposes them.

### 5. Readiness conservatism

The topology/publication reflects **configuration**, not per-controller liveness. A region is not declared "healthy" merely because one provider/controller process is alive. No aggregate regional health is invented here; richer health is deferred to a later operator contract.

## Consequences

- Public region/AZ IDs are stable and provider-neutral; replacing an implementation provider does not alter public location identity.
- A deployment with no configured locations truthfully reports no regions (fail closed, nothing fabricated).
- `ServiceManifest.validate()` still performs structural checks; canonical-existence cross-checks happen in the composition root against the `LocationRegistry`.
- No scheduler/placement rewrite; `o3k-placement` remains node/provider-scoped.

## Non-goals

- No billing/pricing or marketplace geography.
- No provider/host/node inventory APIs for tenants.
- No scheduler rewrite, no host-aggregate model.
- No Araf menu/widget/UI metadata.
- No OpenStack compatibility rewrite; native location identity is authoritative and any future mapping to compatibility surfaces must be explicit and derived from canonical O3K identity.
