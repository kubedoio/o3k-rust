# Implementation Prompt — Issue #902

## Mission

Implement **#902 — Make audit durable and expose a bounded native audit API** as a real Cloud Kernel production subsystem. A MemoryAuditSink, Noop sink, stdout log or best-effort UI feed is not completion.

## Mandatory architecture pass

Refresh from current `main`, read #902 fully, inspect every production `AuditSink` construction path, canonical AuditEvent, mutation/Operation transaction boundaries, SQLite/PostgreSQL stores, delegated service identity, request/audit/operation IDs and current production readiness/evidence contracts.

Before claiming production behavior, add/accept an ADR/SPEC that defines:

- event classes and required durability;
- mutation-vs-audit atomicity/outbox semantics;
- behavior when durable audit storage is unavailable;
- bounded backpressure/timeouts;
- retention/compaction;
- redaction/secret invariants;
- tenant/system query authorization.

If the current synchronous infallible `AuditSink` cannot express the accepted semantics, evolve the port deliberately across production services instead of masking failure.

## Required implementation

Implement durable SQLite/PostgreSQL audit authority preserving canonical safe fields and indexes for bounded investigation. Wire the supported production composition to the durable path.

Implement native bounded audit list/show APIs with strict page/time/filter limits, opaque query-bound cursors and canonical authorization. Tenant scope can see only authorized project events; global/system queries require explicit durable system authority.

Audit remains distinct from Operation but must preserve Operation/request/audit correlation.

## Security / reliability proof

Prove:

- forbidden secret classes cannot be represented/stored in public audit DTOs;
- tenant cannot enumerate foreign events;
- scope/query cursors cannot be replayed to broaden authority;
- system query requires explicit operator/system authorization;
- denied authorization events preserve concealment and accepted evidence semantics;
- service-principal/delegated actions record correct actor/service identity;
- required audit durability behavior is correct during audit DB failure/degradation;
- no unbounded control-plane stall under audit backpressure;
- restart/recovery preserves durable events;
- concurrent writers and replay do not create unintended duplicates;
- retention/compaction is bounded and safe;
- SQLite/PostgreSQL parity;
- high-volume pages do not materialize the full history;
- Operation correlation is correct without conflating the two models.

Expose secret-safe health/metrics for audit lag/failure and make mandatory-audit unavailability affect readiness/degraded state according to the accepted policy.

## Validation

Run full current-main Rust gates, migrations/store conformance, PostgreSQL ignored tests, service-level audit tests, two-tenant/system negative tests, failure injection, restart/concurrency/retention tests and real production `o3kd` process evidence.

## Forbidden shortcuts

Do not call log files the durable audit repository. Do not swallow sink failures if the accepted policy requires durability. Do not store arbitrary request/response bodies or raw provider exceptions. Do not expose an unbounded global audit query.

## Stop condition

Only report `BLOCKED` for a genuine dependency outside this repository. Required changes to AuditSink, services, store schema, router, readiness or tests are in scope.

Finish only with `#902 COMPLETE` after all issue exit criteria and durability/failure evidence are proven.