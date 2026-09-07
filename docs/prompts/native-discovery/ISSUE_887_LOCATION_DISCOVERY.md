# Issue #887 — Canonical region and availability-domain discovery

Repository: `kubedoio/o3k-rust`

Issue: `#887 — [Native API] Add canonical region and availability-domain discovery`

## Goal

Implement a provider-neutral, authoritative native O3K location-discovery contract so API clients can discover regions and availability domains without inventing production truth.

This is a Cloud OS contract, not an Araf-specific endpoint.

The result must be usable by Araf, CLI/SDKs, Terraform, automation, and future service integrations.

## Starting point

Current authoritative `main` already has:

- `ServiceManifest.regions`;
- `ServiceManifest.availability_domains`;
- native `/o3k/v1/resource-types` discovery;
- provider-neutral resource descriptors;
- placement/provider internals elsewhere in the system.

However current built-in/native manifests expose empty region/availability-domain lists and there is no sufficient northbound cloud-location topology for production clients.

Do not solve this by hard-coding sample regions or by exposing provider/host identities as public cloud locations.

## Before coding

Read `AGENTS.md` and inspect current `main` completely around:

- `crates/o3k-kernel/src/manifest.rs`;
- `contracts/service-manifest-v1.schema.json`;
- `crates/o3k-native-api` discovery/resource code;
- `crates/o3k-api` production route composition;
- `bins/o3kd` composition/configuration;
- compute/network/storage/provider location or placement concepts;
- OpenStack compatibility region/AZ semantics where already supported;
- Terraform/P13 convergence assumptions;
- P14 deployment/profile configuration.

Search for existing ADR/spec language before inventing terminology.

If the public semantics are not already frozen, create or update a bounded ADR/spec before implementing the public API.

## Architectural invariants

1. O3K is authoritative for location identity.
2. Region and availability-domain identity are provider-neutral.
3. A region is not a compute provider, host, cluster, hypervisor, Ceph pool, network controller, or storage backend.
4. Public location IDs must be stable enough for persisted automation/Terraform state.
5. A provider replacement must not silently change the public region identity.
6. Unsupported/unknown placement must fail closed or be omitted honestly.
7. Tenant-visible discovery must not expose provider credentials or implementation topology.
8. The browser/UI must not be the source of location truth.
9. The native API, public contract, and actual `o3kd` production composition must agree.

## Required design outcome

A client must be able to determine authoritatively:

- which regions exist;
- which regions are available for supported cloud services/resources;
- which availability domains belong to a region;
- stable IDs and human-safe display labels if O3K has an authoritative label concept;
- whether a resource type is global, regional, or availability-domain aware;
- whether availability-domain selection is unsupported, optional, or required where O3K can truthfully define it;
- which location choices are valid where the control plane has authoritative data.

The exact wire shape is not pre-decided.

A candidate is a bounded native endpoint such as:

`GET /o3k/v1/regions`

with resource-type placement metadata referenced from `/o3k/v1/resource-types`, but choose the smallest architecture-consistent contract after inspecting current code/specs.

Do not blindly copy AWS/Azure/OpenStack terminology if O3K already has better canonical concepts.

## Authority model

Prefer one canonical location registry/configuration authority in the control plane.

ServiceManifest `regions` and `availability_domains` should either:

- reference/filter canonical locations; or
- be explicitly documented as the canonical service-placement declaration if that remains correct.

Do not create two independent location authorities that can drift.

If manifests currently cannot represent region→availability-domain relationships safely, evolve the contract carefully and version it.

## Resource discovery integration

Extend resource/service discovery only as necessary to express placement semantics generically.

Examples of semantic concepts that may be required:

- `global`;
- `regional`;
- availability-domain aware;
- availability-domain selection optional/required/unsupported.

Do not add Araf layout/widget metadata.

Do not make the resource descriptor contain provider-specific placement internals.

## Readiness / health

Be conservative.

If O3K can authoritatively distinguish configured vs ready locations, expose that with precise semantics.

Do not claim a region is healthy merely because one provider process is alive.

If health aggregation is not defined well enough, expose topology/configuration only and leave richer health for a later operator contract.

## API and contract requirements

Any new public route/schema must have:

- OpenAPI/JSON Schema or the repository-standard public contract;
- deterministic serialization/order where useful for clients/tests;
- bounded response semantics;
- production `o3k-api::router_with_state` registration;
- standalone/native and production composition convergence tests;
- version/evolution documentation.

Do not create a route that is tested only in `o3k-native-api` but missing from `o3kd` production composition.

## Validation semantics

Reject invalid configuration deterministically, including as applicable:

- empty IDs;
- duplicate region IDs;
- duplicate availability-domain IDs within invalid scopes;
- availability domain referring to an unknown region;
- ambiguous mappings;
- malformed identifiers;
- contradictory global/regional declarations.

Use existing O3K validation/value-object patterns.

## Required representative proof

Build focused tests proving at least:

1. a deployment with more than one configured region is discoverable;
2. at least one region has multiple availability domains;
3. stable/deterministic returned identity/order;
4. a global resource type does not falsely advertise regional placement;
5. a regional resource type advertises only authoritative region support;
6. an availability-domain-aware resource describes that capability without provider leakage;
7. invalid/duplicate topology is rejected;
8. provider implementation replacement does not alter public region identity in the model;
9. real production router exposes the contract;
10. clients need no hard-coded `eu-de-1`-style fixture in production.

Use real current service/resource examples where possible instead of test-only fictional semantics, but do not expand the issue into broad placement implementation.

## Compatibility considerations

Review OpenStack region/AZ compatibility behavior.

Do not rewrite compatibility architecture in this issue.

If native locations need a mapping to existing compatibility surfaces, keep the mapping explicit and derived from O3K canonical identity.

Native semantics remain authoritative.

## Non-goals

Do not implement:

- billing/pricing geography;
- service-plan/marketplace geography;
- arbitrary provider inventory APIs;
- host/node discovery for tenants;
- scheduler rewrite;
- Araf-specific menus/widgets/forms;
- complex regional health aggregation unless already architecturally defined;
- unrelated P14 work.

## Repository/PR discipline

Use a dedicated implementation branch derived from latest `origin/main`, not the planning branch as an unreviewed code base if `main` has moved materially.

Suggested implementation branch:

`native/issue-887-location-discovery`

Keep one focused PR that closes #887.

Do not mix issue #888 schema/action work into this PR except for the minimal placement metadata reference needed by #887.

## Required gates

Run all repository-required validation. At minimum:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Also run/add:

- public contract validation;
- focused native discovery tests;
- `o3k-api::router_with_state` composition tests;
- relevant PostgreSQL/profile/process gates if affected;
- architecture/maintainability guards.

Do not weaken/ignore tests to obtain green CI.

## Required final report

Report:

### Contract
Exact endpoints/types/fields and semantic meaning.

### Authority
Where region and availability-domain truth lives and why there is no second authority.

### Resource placement
How global/regional/availability-domain semantics are represented.

### Provider neutrality
Evidence that provider/host/backend details do not leak into tenant location identity.

### Production composition
Proof that actual `o3kd` exposes the contract.

### Validation
Exact commands and results.

### Findings
- BLOCKER
- HIGH
- MEDIUM
- LOW/NIT

### Git
- branch
- commit SHA
- PR
- issue #887

### Verdict
Exactly one:

`O3K NATIVE LOCATION DISCOVERY: PASS — READY FOR REVIEW`

or

`O3K NATIVE LOCATION DISCOVERY: BLOCKED`

Do not modify Araf in this work.
