# Issue #888 — Versioned resource and action schemas

Repository: `kubedoio/o3k-rust`

Issue: `#888 — [Native API] Publish versioned resource and action schemas`

## Goal

Publish authoritative, versioned, machine-readable native O3K schemas for resource representations and resource actions so generic clients can render/validate workflows without inventing production semantics.

This must build on the existing O3K resource/service model rather than replacing it.

The contract is for Araf, CLI/SDKs, Terraform, automation, and future agentic clients. It must remain UI-framework neutral.

## Starting point

Current `main` already exposes important authoritative discovery:

- `/o3k/v1/resource-types`;
- `ResourceDescriptor.resource_type`;
- `collection`;
- `schema_version`;
- `scope`;
- `lifecycle_actions` mapped to canonical `ActionId`s;
- owning service and readiness.

Therefore do NOT claim O3K has no action discovery. Lifecycle action identity already exists.

The missing production contract is:

- canonical resource representation schema projection;
- canonical action input schema projection;
- canonical output/result schema where applicable;
- richer generic action metadata such as target and asynchronous/Operation behavior where that is authoritative.

## Before coding

Read `AGENTS.md` and inspect current `main`, especially:

- `crates/o3k-native-api/src/resource.rs`;
- `/o3k/v1/resource-types` implementation;
- `crates/o3k-kernel/src/manifest.rs`;
- `contracts/service-manifest-v1.schema.json`;
- native envelope JSON Schema / OpenAPI contracts;
- generic `CreateRequest` and `ResourceApplication` boundary;
- concrete compute/network/storage native adapters and request validation;
- `ActionId`, authorization, and `ResourceTarget` semantics;
- canonical `Operation` behavior;
- public contract validation scripts;
- P13/P14 tests that consume `lifecycle_actions` and schema versions.

Search accepted ADR/specs before defining a new schema registry or naming model.

If the schema/evolution semantics are not already frozen, write/update a bounded ADR/spec first.

## Architectural invariants

1. O3K publishes cloud API semantics, not UI presentation.
2. Published schemas must derive from authoritative service/resource/application contracts.
3. No second schema authority may drift independently from runtime validation.
4. Existing canonical `ActionId` and server-side authorization remain authoritative.
5. A published schema never grants permission; every request remains authorized server-side.
6. Tenant schemas remain provider-neutral and must not expose backend secrets/topology.
7. `schema_version` should be reused/evolved deliberately rather than inventing unrelated parallel versioning.
8. Unsupported actions are explicit absence/unsupported semantics, never inferred by clients.
9. Standalone native API and actual `o3kd` production composition must expose the same contract.

## Semantic ownership boundary

### O3K owns

- resource representation types/constraints;
- required vs optional fields;
- enums/formats/ranges when canonical;
- create/update/action request semantics;
- canonical action identity;
- collection vs instance target where meaningful;
- output/result schema where useful;
- whether the action creates/follows a canonical Operation when this is a stable API property;
- resource schema version/evolution rules.

### O3K does NOT own

- React/Cloudscape widgets;
- page sections/layout;
- visual field ordering unless order is semantically meaningful;
- CSS/theme information;
- Araf help-panel structure;
- arbitrary executable visibility expressions;
- frontend-only conditional logic.

Araf may later layer bounded presentation metadata over the machine contract.

## Contract design requirements

The exact wire shape is not pre-decided.

Design the smallest public contract allowing a generic client to resolve deterministically:

1. `resource type + schema_version -> representation schema`;
2. lifecycle/domain action -> canonical ActionId;
3. action -> target semantics (`collection` / `instance` or equivalent) where useful;
4. action -> input schema when input exists;
5. action -> output/result schema when defined;
6. action -> asynchronous / canonical Operation behavior where that is an actual contract;
7. stable schema identifiers/URLs/version references suitable for caching and automation.

Prefer JSON Schema / OpenAPI-compatible contracts and repository-standard validation tooling. Do not invent an embedded O3K expression language unless unavoidable and explicitly justified.

Possible designs include schema references embedded in `/resource-types` plus a dedicated schema endpoint/registry. Choose based on current architecture and evolution needs; do not copy this suggestion blindly.

## Lifecycle and domain actions

Preserve current lifecycle operations:

- list;
- show;
- create;
- update;
- delete.

Do not force future domain actions into CRUD.

The schema/action contract must be extensible for future authoritative actions such as:

- start/stop/reboot/resize;
- snapshot;
- backup/restore;
- rotate;

but do not invent or implement such domain actions in this issue unless they already exist canonically in current O3K.

## Runtime-schema convergence

This is the hardest requirement.

Do not publish decorative JSON Schema that is merely similar to Rust request structs.

Create a maintainable authority path and drift tests so:

- a payload accepted by the published schema is consistent with canonical application/handler validation;
- a payload rejected by canonical validation cannot remain falsely advertised as valid;
- required fields/enums/ranges cannot silently diverge;
- schema version changes are deliberate and tested.

Prefer deriving schema from canonical Rust/public contract types where practical, or otherwise mechanically validating equivalence.

If current generic `serde_json::Value` boundaries make exact schema convergence impossible for a resource, fix the smallest authoritative layer needed rather than encoding Araf assumptions.

## Representation schema

Define what a resource representation schema means.

It should describe the canonical native resource payload/envelope/spec/status fields relevant to generic clients without exposing internal persistence/provider structs.

Be explicit whether the schema applies to:

- the entire native envelope;
- resource `spec`;
- resource `status`;
- or distinct versioned components.

Do not leave this ambiguous.

## Action schemas

For each declared action included in scope, define authoritative input/output behavior.

Representative proof must include multiple different resource types and at least one mutation.

For no-input actions, do not fabricate meaningless object fields; declare no input or a precise empty contract.

For async actions, if metadata claims canonical Operation behavior, prove the actual handler returns/links the canonical Operation as declared.

## Authorization / security

Schema discovery may be public/authenticated according to existing native discovery policy, but must not leak privileged-only fields/actions.

If effective action visibility depends on caller capabilities, decide explicitly whether discovery publishes:

- globally supported actions with server authorization remaining separate; or
- capability-filtered action availability.

Do not accidentally turn UI hiding into authorization.

Keep the chosen semantics deterministic and documented.

Never project:

- provider credentials;
- secret material;
- host-only fields;
- private controller topology;
- internal database fields;

into tenant schemas.

## Public contract and production composition

Any new route/schema must have repository-standard public API artifacts and tests.

Prove:

- native lower-level router exposure;
- `o3k-api::router_with_state` production registration;
- real `o3kd` composition if a process harness exists;
- API root/discovery catalog convergence where applicable.

Avoid repeating the route-drift defect fixed by issue #880.

## Required representative tests

At minimum prove:

1. at least three meaningfully different resource types resolve through one generic schema-discovery path;
2. returned schema version equals manifest/resource descriptor authority;
3. lifecycle ActionIds equal manifest authority;
4. valid create/action input passes published schema and reaches canonical application validation successfully;
5. invalid input rejected by schema is also invalid canonically;
6. undeclared action has no fabricated schema;
7. partial lifecycle support remains accurately represented;
8. no-input action semantics are correct;
9. async/Operation metadata, if exposed, matches runtime behavior;
10. provider-specific internal fields do not appear in tenant schema;
11. deterministic schema IDs/references survive restart;
12. contract drift tests fail if implementation and schema diverge;
13. actual production router exposes the contract.

## Evolution/versioning

Document rules for compatible vs incompatible schema evolution.

At minimum define:

- what changes require `schema_version` bump;
- stability expectations for schema IDs/references;
- whether old schema versions remain retrievable and for how long (if supported);
- how clients should handle unknown newer fields/actions.

Keep the first implementation bounded; do not build a full historical schema archive unless required by existing API compatibility guarantees.

## Coordination with issue #887

Issue #887 owns canonical region/availability-domain discovery.

Do not implement that topology here.

This issue may expose a generic placement field/reference in resource/action schemas only if it consumes the canonical location contract from #887 or an already-existing authoritative contract.

If #887 is not merged yet, keep schema work independent and avoid hard-coded region enums.

## Non-goals

Do not implement:

- UI widgets/layout;
- pricing/billing;
- Terraform HCL generation;
- CLI flag generation;
- marketplace/service plan metadata;
- arbitrary executable descriptor scripts;
- broad service catalog rewrite;
- provider-specific tenant APIs;
- unrelated P14 changes.

## Repository/PR discipline

Use a dedicated implementation branch derived from latest `origin/main`.

Suggested implementation branch:

`native/issue-888-resource-action-schemas`

Keep one focused PR that closes #888.

Do not stack it on an unmerged #887 implementation unless a concrete contract dependency requires it. Prefer independent development with a small rebase/integration after #887 if needed.

## Required gates

Run all repository-required validation. At minimum:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Also run/add:

- OpenAPI/JSON Schema contract validation;
- focused native resource discovery/schema tests;
- production router composition tests;
- P13 lifecycle-discovery compatibility tests;
- architecture/maintainability guards;
- relevant PostgreSQL/process gates if touched.

Do not weaken tests to get green CI.

## Required final report

Report:

### Schema contract
Exact discovery/schema routes, identifiers, versions, and semantics.

### Authority/convergence
How schemas are kept aligned with runtime validation and ServiceManifest/ResourceDescriptor authority.

### Action model
Lifecycle ActionId, target, input/output, and Operation semantics.

### Security
Evidence that schemas do not grant authorization or leak provider/private fields.

### Genericity proof
At least three resource types using the same schema-discovery path.

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
- issue #888

### Verdict
Exactly one:

`O3K NATIVE RESOURCE/ACTION SCHEMAS: PASS — READY FOR REVIEW`

or

`O3K NATIVE RESOURCE/ACTION SCHEMAS: BLOCKED`

Do not modify Araf in this work.
