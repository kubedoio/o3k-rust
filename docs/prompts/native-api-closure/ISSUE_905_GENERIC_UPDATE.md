# Implementation Prompt — Issue #905

## Mission

Implement **#905 — Complete generic lifecycle update mutation with canonical concurrency and Operations** on this branch.

## First actions

Refresh from current authoritative `main`; read #905 and #888; inspect ResourceDescriptor lifecycle mappings, ResourceApplication/controller ports, existing create/delete native mutations, generation/If-Match semantics, canonical mutation/idempotency journal, mutable Compute/Network/Volume/Image operations and external-controller protocol.

#888 owns authoritative update schemas/ActionIds. #897 owns non-CRUD actions. This issue owns lifecycle `update` execution.

## Required implementation

Choose/document a versioned update request method/contract consistent with O3K semantics. It must:

- exist only when the descriptor declares update;
- validate the authoritative update schema;
- authorize canonical Update ActionId against AuthContext + durable ownership;
- prevent caller mutation of id/owner_scope/provider identity/canonical generation/private fields;
- require sufficient optimistic concurrency/precondition semantics to prevent lost updates;
- use idempotency/replay conflict detection;
- establish/follow canonical Operation semantics when asynchronous/reconciling;
- truthfully return unsupported/not-ready/conflict/accepted/completed states;
- work through a service-neutral application/controller port for in-process and external-controller resources.

Do not adopt arbitrary JSON Merge Patch merely for convenience unless architecture review proves it safe for all declared resources.

## Evidence

Prove real/contract-level genericity across at least two resource types where possible, including:

- valid update;
- stale generation -> conflict and zero forbidden provider side effect;
- equivalent idempotent replay;
- conflicting replay;
- Project A -> Project B concealment;
- undeclared update rejection;
- invalid/oversized payload rejection;
- immutable/private field mass-assignment rejection;
- restart preserves asynchronous accepted update/Operation;
- native/OpenStack convergence for overlapping mutable canonical resources;
- external-controller protocol/version compatibility if changed;
- SQLite/PostgreSQL durable parity where applicable.

## Contracts / validation

Update ADR/SPEC/schema/OpenAPI and drift tests. Run full Rust gates, controller protocol tests, store/PostgreSQL tests, process/restart/race/two-tenant negatives and compatibility convergence.

## Stop condition

Only report `BLOCKED` for a genuine dependency outside this repository. Cross-crate protocol/application/store/API changes needed for #905 are part of the implementation.

Finish only with `#905 COMPLETE` when every issue exit criterion is proven.