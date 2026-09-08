# Implementation Prompt — Issue #898

## Mission

Implement **#898 — Add bounded scope-safe canonical Operation collection** on this branch as production infrastructure.

## First actions

1. Rebase/refresh from current authoritative `o3kio/o3k` `main`.
2. Read issue #898 in full and inspect the current canonical Operation model, store records, migrations/indexes, `OperationReaderAdapter`, native cursor machinery and all mutation paths that create Operations.
3. Inspect current SQLite/PostgreSQL query capabilities and repository conventions before designing the public list API.
4. Preserve current show-by-ID concealment and public Operation projection unless an explicit versioned improvement is required.

Do not treat an issue-suggested URL/filter list as pre-decided architecture; make the smallest versioned contract consistent with current authority.

## Required implementation

Implement a native Operation collection backed directly by durable O3K authority. It must:

- apply canonical AuthContext/server-side authorization;
- bind tenant queries to the effective scope with no arbitrary scope override;
- support explicitly authorized system/operator queries without making global mode implicit;
- use strict page-size limits and opaque tamper-resistant cursors;
- bind cursors to scope + query/filter/order/version;
- use deterministic stable ordering;
- support only bounded/indexable filters that are semantically defined;
- push filtering/order/continuation/limit to repository/database boundaries;
- never load the whole Operation table and filter/page in memory;
- expose only canonical public fields, not provider/store-private fields or raw secret-bearing errors.

Add/adjust repository query ports and indexes for both SQLite and PostgreSQL.

## Consistency requirements

Every supported canonical native mutation must produce Operations that can be listed and shown consistently. Historical rows missing required canonical public metadata must fail closed or be handled by an explicit migration; do not fabricate actor/scope/action identity.

## Required tests/evidence

Prove:

- Project A list/show cannot reveal Project B Operations;
- tenant cannot select another owner scope;
- tenant cursor cannot be reused in another scope or system mode;
- tampered/mismatched cursors fail closed;
- system/global query requires explicit system authorization;
- state/service/action/resource/resource-id/time/correlation filters implemented are indexed/bounded and correct;
- multi-page traversal is stable;
- concurrent inserts/deletes follow documented cursor semantics;
- SQLite/PostgreSQL parity;
- large synthetic data evidence shows a small page does not materialize the whole table;
- mutation response Operation IDs resolve through show and collection;
- restart preserves activity truth.

Add query-plan/instrumentation evidence where useful, but do not make unverifiable performance claims.

## Contracts and documentation

Update ADR/SPEC/public schema/OpenAPI artifacts so pagination, ordering, filter vocabulary, cursor binding, tenant/system scope semantics and error/concealment behavior are explicit. Add drift tests where appropriate.

## Validation

Run all repository-required Rust formatting/Clippy/check/test gates, relevant ignored PostgreSQL tests, migration/store conformance, native API process tests, two-tenant negatives and any stronger current-main production gates. Do not mark ready until production `o3kd` composition exposes and proves the contract.

## Forbidden shortcuts

Do not:

- build the Operations Center from Araf/browser history;
- introduce a second Operation database;
- expose provider-native operation payloads;
- page after `list_all()`;
- silently ignore unsupported filters;
- authorize global queries from a frontend role/name.

## Stop condition

Only report `BLOCKED` for a genuine external dependency outside this repository, with reproducible evidence. Cross-crate/store/schema/API changes needed to satisfy #898 are part of the work.

Finish only with `#898 COMPLETE` after the issue exit criteria and production evidence are actually satisfied.