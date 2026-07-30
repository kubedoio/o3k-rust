# Durable store

Issue #3 introduces `o3k-store`, a domain-facing durable-store boundary with a
SQLite adapter. SQLx types and row mapping stay inside the adapter; callers use
typed records, UUIDs, operation states, and typed store errors.

## Persistence model

- `resources` stores O3K-owned identity, project scope, desired/observed state,
  generation, observed generation, and an opaque provider ID.
- `operations` stores the operation state, provider operation identity, and
  redacted error fields. `unknown_outcome` is persisted as a first-class state.
- `provider_refs` stores the O3K resource ID to provider name/resource ID
  mapping with uniqueness constraints in both directions.

Migrations are embedded in the crate and run on every open. SQLite foreign keys
are enabled on connections and a bounded busy timeout is configured. File-based
stores create only their requested parent directory; no database is silently
repaired or replaced.

## Concurrency and recovery

Resource updates require the caller's expected generation. A stale update is
reported as `StaleGeneration`, so application logic can retry without silently
overwriting newer intent. Persisting a resource and operation uses one SQLite
transaction and rolls back all writes on constraint failure.

`readiness_check` runs an integrity check and verifies the required schema. A
failed open, migration, integrity check, or schema check is an error that the
runtime must expose as not ready; it is never converted into an empty database.

The public `run_conformance` routine defines the reusable baseline for future
store adapters: persistence, stale-write rejection, unknown-outcome retention,
operation updates, and provider-reference mapping. Additional SQLite tests
cover duplicate IDs, atomic rollback, file restart, and corrupt database input.
