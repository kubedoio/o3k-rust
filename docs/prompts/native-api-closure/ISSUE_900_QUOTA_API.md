# Implementation Prompt — Issue #900

## Mission

Implement **#900 — Expose authoritative quota dimensions, limits and usage** on this branch.

## First actions

Refresh from current authoritative `main`; read #900 fully; inspect canonical quota types/repositories/enforcement, ServiceManifest quota dimensions, current IAM/system authorization and selected compatibility quota paths. Establish exact semantics for missing/default/unlimited limits and units before exposing a public API.

## Required implementation

Create versioned native quota contracts backed by canonical O3K authority. Tenant reads must be bound to effective AuthContext scope. Explicitly authorized system/operator callers may inspect other scopes and mutate limit overrides according to accepted policy.

The API must expose only authoritative:

- quota dimension identity;
- unit/scope semantics;
- effective limit with explicit absent/default/unlimited behavior;
- committed/current usage;
- freshness/generation/version when necessary.

Do not make reservation/hold internals part of the normal tenant contract unless architecture review proves a public requirement. Clients may never write usage directly.

Administrative limit updates need validation, privilege checks and lost-update/concurrency protection. Unknown dimensions, negative/overflow values and caller scope spoofing fail before durable mutation.

## Correctness/security evidence

Prove:

- Project A cannot read Project B quota;
- tenant cannot mutate limits;
- system/operator authorization is explicit and durable;
- successful allocation/enforcement/deletion converges with displayed usage;
- quota exceeded blocks forbidden provider side effects;
- replay/retry cannot double-account;
- restart preserves limits/usage;
- concurrent administrative edits are deterministic and protected from lost update;
- SQLite/PostgreSQL parity;
- quota responses expose no reservation/provider/private state;
- quota mutations correlate with canonical Audit when #902 is available.

Quota reads must not enumerate all resources per request. Use canonical counters/repositories and bounded/indexed queries.

## Contracts

Add/adjust ADR/SPEC/public schema/OpenAPI artifacts and drift tests. Keep ServiceManifest quota-dimension authority converged with runtime enforcement; do not create dashboard-only keys.

## Validation

Run full current-main Rust gates, store/migration conformance, relevant PostgreSQL ignored tests, real service quota enforcement/replay/process tests, two-tenant authorization negatives and compatibility convergence where applicable.

## Stop condition

Only report `BLOCKED` for a genuine external dependency. Any required kernel/store/API/schema/index changes in this repository are part of #900.

Finish only with `#900 COMPLETE` when every issue exit criterion is proven.