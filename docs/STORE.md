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

## Repository ports

Application services depend on narrow repository ports defined in this crate,
not on the concrete `SqliteStore` adapter:

- `IdentityRepository` — the Keystone-compatible record surface used by
  identity bootstrap seeding and snapshot load;
- `KeypairRepository` and `VolumeAttachmentRepository` — the keypair and
  attachment-record surfaces used by compute;
- `ComputeRepository: DurableStore + KeypairRepository + VolumeAttachmentRepository`
  with recovery listing — the compute use-case surface (the reconciler's
  `OperationJournal` already consumes `DurableStore`);
- `DurableStore` — the resource/operation/agent-command/artifact-transfer/
  image-overlay/provider-reference surface.

`SqliteStore` implements every port. The composition root (`o3kd`) opens the
SQLite adapter and injects it through the ports. Each extracted port has an
adapter-agnostic conformance routine
(`run_identity_repository_conformance`, `run_keypair_repository_conformance`,
`run_volume_attachment_repository_conformance`) exercised against the SQLite
adapter, so a future adapter passes the same behavioral suite. The
`testkit` module exists so application test code can construct the SQLite
adapter without naming the concrete type in application sources.
