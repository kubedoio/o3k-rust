# SPEC-0038 — Canonical location discovery v1
Status: Accepted
Related decision: [ADR-0181](../adr/ADR-0181-canonical-location-identity.md) (human architecture/security approval 2026-09-08 in task instruction for PR #889 / issue #887; this spec derives acceptance from that decision)
Related issue: [#887](https://github.com/o3kio/o3k/issues/887)
Related contract: [native location discovery v1](../../contracts/native-location-discovery-v1.schema.json)
Related normative sources:
- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0173](../adr/ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [ADR-0174](../adr/ADR-0174-service-manifest-and-resource-provider-controller.md)
- [SPEC-0030](SPEC-0030-native-o3k-resource-api-v1.md)
- [SPEC-0031](SPEC-0031-service-extension-controller-v1.md)
This specification derives acceptance from ADR-0181. It defines the v1 canonical location-discovery contract and the evidence required before that contract may be advertised.

## 1. Purpose

O3K is the single authority for region and availability-domain identity. This spec defines how a generic client discovers, authoritatively and provider-neutrally:

- which O3K regions exist;
- the availability domains belonging to each region;
- stable, provider-neutral location IDs;
- resource/service placement scope (global vs regional);
- whether availability-domain selection is supported, and whether it is optional or required where O3K can truthfully declare it;
- valid location choices where O3K has authoritative knowledge.

No Araf, Terraform, SDK, CLI, provider, or frontend configuration may invent location topology.

## 2. Canonical authority

The kernel `LocationRegistry` is the only location authority. It is seeded by the deployment composition root (`o3kd`) from `O3K_LOCATIONS` and is validated deterministically. It is read-only after construction.

Service manifests (`regions`, `availability_domains`) are placement **filters** over canonical IDs. The composition root runs `LocationRegistry::validate_manifest_registry` over every registered manifest and fails closed on any reference to an unknown location.

Rules:

- A region is not a compute provider, host, hypervisor, cluster, Ceph pool, storage backend, or network controller.
- Backend/provider identity is never exposed as tenant-facing location identity.
- Replacing an implementation provider never changes public region identity.

## 3. Location identity

Region and availability-domain IDs use a stable, human-safe, provider-neutral alphabet (`^[a-z0-9][a-z0-9_-]*$`, 1..=128).

Validation rejects deterministically:

- empty IDs;
- malformed IDs (forbidden characters, uppercase, whitespace);
- duplicate region IDs;
- duplicate availability-domain IDs within a region;
- the same availability-domain ID appearing in more than one region (an ambiguous mapping);
- unknown region/AZ references from service manifests.

Because topology is nested (AZ inside region), an AZ can never reference an unknown region.

## 4. Endpoint: `GET /o3k/v1/regions`

Response contract: [`native-location-discovery-v1.schema.json`](../../contracts/native-location-discovery-v1.schema.json).

```json
{
  "regions": [
    {
      "id": "region-a",
      "availability_domains": [{"id": "az-1"}, {"id": "az-2"}]
    }
  ],
  "count": 1
}
```

Semantics:

- `regions` is sorted by `id`; each region's `availability_domains` is sorted by `id` (deterministic serialization).
- When no locations are configured the response is `{"regions": [], "count": 0}` — absent, never fabricated.
- The endpoint is mounted under the native prefix and in production `o3k_api::router_with_state`, so actual `o3kd` exposes it.

## 5. Resource placement on `GET /o3k/v1/resource-types`

Each resource type exposes derived placement fields:

- `placement`: `"global"` or `"regional"`.
- `regions`: canonical region IDs (present only when `regional`); only canonical IDs are disclosed (fail closed on unknown).
- `availability_domain_selection`: `"unsupported"`, `"optional"`, or `"required"`.

| Manifest regions | Manifest AZs | placement | availability_domain_selection |
|---|---|---|---|
| empty | empty | global | unsupported |
| non-empty | empty | regional | unsupported |
| non-empty | non-empty | regional | optional |
| empty | non-empty | global | required |

Placement is derived from the single manifest declaration, so no second placement authority exists and contradictory global/regional declarations are impossible by construction.

A global resource type never advertises regional placement; `regions` is absent/empty for globals.

## 6. Configuration

`O3K_LOCATIONS` (JSON array of `RegionDeclaration`) is read by the `o3kd` composition root. Invalid or unknown-referencing configuration fails startup (fail closed). Unset/empty produces an empty registry.

Example:

```json
[{"id":"region-a","availability_domains":[{"id":"az-1"},{"id":"az-2"}]},
 {"id":"region-b","availability_domains":[{"id":"az-3"}]}]
```

Production ships no hard-coded demo/Araf regions; region topology is deployment configuration.

## 7. Regencies and readiness

Publication reflects configured topology, not liveness. A region is not declared healthy merely because one controller/provider process is alive. No aggregate regional health is invented here.

## 8. Non-goals

Provider/host/node inventory for tenants, scheduler rewrite, billing/marketplace geography, Araf UI metadata, and OpenStack compatibility rewrite are out of scope. OpenStack compat region/AZ remains a separate projection; any future mapping must be derived explicitly from canonical O3K identity.
