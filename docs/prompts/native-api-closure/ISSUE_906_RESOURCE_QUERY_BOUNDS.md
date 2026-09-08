# Implementation Prompt — Issue #906

## Mission

Implement **#906 — Push resource pagination/filtering to bounded repository queries**. This closes a production scalability defect: bounding the JSON page after loading the full collection is not acceptable.

## First actions

Refresh from current authoritative `main`; read #906; inspect generic native `ListQuery`/cursor and `ResourceApplication::list`, concrete compute/network/volume pagination, all O3kStore list methods/indexes, external-controller resource projection, Araf bounded-search requirements and current native resource envelope/schema authority.

## Required implementation

Define a service-neutral bounded collection query contract carrying only accepted semantics such as:

- server-clamped limit;
- continuation/cursor key;
- deterministic small ordering vocabulary;
- common safe/indexed filters;
- resource-specific filters only when explicitly declared/versioned by authoritative schema/contract.

Bind cursor/token to effective scope, resource type, query version, filter set and order. Tampered/mismatched replay fails closed.

Push limit/continuation/filter/order down through service/repository/provider boundaries. Add SQLite/PostgreSQL indexes and query ports. For external controllers, define a bounded pagination capability/contract; never silently load an unlimited external collection.

Unknown filters/sorts must be explicit unsupported/validation errors, not silently ignored. Do not make arbitrary `spec.*` or provider-private fields queryable by default.

## Security / correctness evidence

Prove:

- Project A query/cursor cannot enumerate Project B;
- cursor cannot be reused with broader scope/filter/order;
- filtering cannot infer private provider/backend topology;
- per-resource ownership remains enforced;
- malformed/oversized query/cursor is bounded;
- SQL/provider errors remain private;
- generic/concrete native routes over the same canonical resource converge;
- concurrent inserts/deletes follow documented cursor behavior;
- SQLite/PostgreSQL parity;
- representative external/provider path is bounded.

## Performance evidence

Use a sufficiently large deterministic dataset and instrumentation/query plans to prove a page of N does not require API/application materialization of all M matching rows. Demonstrate first/subsequent page and indexed filter behavior. Avoid environment-specific performance marketing claims; record reproducible structural evidence.

## Contracts / validation

Update SPEC-0030 or a dedicated collection-query spec, public OpenAPI/schema/query docs and drift tests. Run full Rust gates, migrations/store conformance, PostgreSQL ignored tests, large-data pagination tests, concurrency/security negatives and production `o3kd` process tests.

## Stop condition

Only report `BLOCKED` for a genuine external dependency. Required service/store/controller query-port changes are part of #906.

Finish only with `#906 COMPLETE` after the issue exit criteria and bounded-query evidence are proven.