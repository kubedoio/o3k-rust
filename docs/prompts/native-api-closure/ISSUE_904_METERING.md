# Implementation Prompt — Issue #904

## Mission

Implement **#904 — Add authoritative metering definitions and bounded usage
aggregation** as the minimum production O3K metering subsystem. This is usage
authority, not billing.

## Baseline (all merged and authoritative)

This workstream builds on already-merged canonical contracts. Do **not**
recreate, fork or bypass any of them:

- #887 — canonical region and availability-domain identity;
- #898 — canonical bounded Operation collection;
- #900 — canonical quota dimensions, limits and usage;
- #901 — canonical IAM governance / operator authority and ActionId conventions;
- #902 — durable Audit repository and native audit query API;
- #903 — system-scoped operator diagnostics, status vocabulary and freshness
  semantics.

Quota, Audit, Diagnostics and Metering are **distinct** authorities. Metering
may share infrastructure patterns with them but must not conflate semantics:
quota answers "how much is allocated against a limit now", Audit records
administrative facts, Diagnostics reports health/freshness, Metering reports
usage over time.

## Architecture first

Refresh from current `main`, read #904 fully, and inspect, in the current code
(not the historical prototype):

- canonical resource lifecycle records, Operations and reconciliation state;
- compute server create/running/stopped/deleted authority, durable timestamps,
  flavor/vCPU/RAM association and placement association;
- volume size/state/resize lifecycle authority;
- public/floating address allocation lifecycle where authoritative;
- provider observation timestamps and their durability;
- durable store/migration/indexing architecture for SQLite **and** PostgreSQL;
- P13/P14 replay/import/migration and reconstruction behavior;
- #902 durable event/outbox patterns where reusable;
- #903 status/freshness vocabulary that may be reused for completeness.

Add an accepted ADR/SPEC defining meter identity, units, aggregation vocabulary,
clock/time boundaries, interval semantics, idempotent event identity,
late/out-of-order handling, start-of-authority for imported resources,
retention/compaction, completeness/lag semantics and query bounds **before**
production claims.

## Required implementation

Create a service-neutral versioned `MeterDefinition` and durable
metering/interval/aggregate authority with SQLite/PostgreSQL parity.

Advertise only meters whose source can be proven in current O3K. Implement at
least one representative compute time-based meter and one storage/allocation
meter where technically sound. Candidate examples are compute runtime duration,
vCPU-time/RAM-time, and volume allocated byte-time — but **verify actual
authority first**: only expose vCPU/RAM duration if flavor/resource association
is durable across the whole interval, and never compute historical byte-time
from a volume's *current* size without modelling resizes as interval
boundaries. Leave unprovable meters absent/unavailable rather than estimating
them.

Implement bounded meter discovery and usage query APIs with:

- effective tenant scope derived from `AuthContext` (never from request JSON);
- explicit start/end;
- supported granularity/aggregation within limits;
- explicit units in every result;
- strict maximum time-range/bucket/series/meter limits that **reject** rather
  than silently truncate;
- optional authorized resource filter;
- freshness/completeness/lag state (e.g. `observed_through` watermark);
- explicit system/operator cross-scope authorization requiring durable
  canonical authority.

Do not expose cost unless a separate authoritative pricing contract exists.
Pricing, discounts, tax, invoice/payment/credits/accounting are non-goals.

## Time semantics

Metering is fundamentally time-dependent. Freeze the canonical clock source,
event time versus ingestion time, interval boundaries and endpoint inclusion
rules, time precision, bucket boundaries, UTC representation, start/stop
semantics, deletion closure, and behavior for late, out-of-order, duplicate,
future and clock-regressed events. Client timezone must never affect usage
computation.

Prefer a small injectable clock abstraction at the correct architecture
boundary if one does not already exist — real UTC clock in production,
deterministic clock in tests — so interval arithmetic is provable without
`sleep`.

## Correctness proof

Prove:

- retry/replay cannot double-meter (deterministic event identity from canonical
  Operation/resource-generation/transition identity, not a random per-replay id);
- compute `create -> run -> stop -> start -> delete` opens/closes intervals
  correctly, with reboot and failed start/stop semantics explicit;
- volume size lifecycle is accounted correctly for the supported initial
  semantics;
- restart/recovery does not duplicate or lose open intervals;
- deletion closes usage and deleted resources remain historically queryable;
- failed/compensated creation does not create false usage;
- imported/migrated resources have explicit start-of-O3K-authority semantics
  and no invented historical consumption (survives restart/reconciliation);
- late/out-of-order observations follow one deterministic documented rule;
- SQLite/PostgreSQL parity verified against real PostgreSQL, not mocks;
- bounded aggregation avoids full raw-history scans per dashboard query
  (aggregate/rollup at the repository/SQL boundary, not "load all in Rust");
- concurrent writers (two `o3kd` processes) produce exactly one effect via
  durable uniqueness/CAS/transactions, not process-local mutexes;
- checked/wider arithmetic cannot silently overflow duration × quantity;
- tenant cannot see foreign usage and cannot enumerate foreign resources;
- global queries require system authority; project-admin alone is insufficient;
- high-cardinality/private provider metadata is not exposed as meter labels.

Adjacent buckets must neither double-count nor omit boundary instants, and a
resource crossing many buckets must distribute usage correctly.

## Retention, completeness and observability

Define explicit retention for raw events, durable aggregates and
closed-resource history. If raw events may be compacted, prove aggregates
remain authoritative afterwards; compaction must be idempotent, restart-safe,
replay-safe and bounded. Never delete the only durable source required for
reconciliation.

Expose a machine-readable completeness/freshness status and an authoritative
`observed_through` watermark so a client can display "usage available through
T" instead of silently showing an incomplete number. Expose secret-safe
metering health/lag indicators; integrate with #903 diagnostics only where
architecturally appropriate and bounded.

## Security / privacy

- tenant queries cannot access foreign project/resource usage;
- operator global queries require durable system authority;
- meter labels/dimensions cannot contain provider secrets, internal endpoints,
  agent epochs, database details, host paths or arbitrary provider metadata;
- raw provider telemetry is not exposed merely to implement a meter;
- query bounds prevent denial-of-service through huge time ranges/bucket counts;
- errors are secret-safe.

## Validation

Run full current-main Rust gates:

`cargo fmt --all -- --check`

`cargo check --workspace --all-targets --all-features`

`cargo clippy --workspace --all-targets --all-features -- -D warnings`

`cargo test --workspace --all-features`

plus focused metering store conformance, SQLite **and** PostgreSQL metering,
migration tests, event idempotency, concurrency, replay, restart, deletion,
failure/compensation, import/start-of-authority, time-boundary and
late/out-of-order tests, bounded-query tests, tenant-isolation and
operator-authorization negatives, secret-safety, contract/schema drift, a real
`o3kd` HTTP metering process journey, and existing architecture/maintainability/
ADR gates. GitHub CI must be green on the exact final HEAD.

`O3K_DATABASE_URL` being absent is **not** a blocker: provision disposable
PostgreSQL locally for parity evidence.

## Forbidden shortcuts

Do not derive historical usage from browser resource counts. Do not call quota
counters metering without time semantics. Do not fabricate network bytes, CPU
utilization, egress, request counts or prices. Do not silently lose usage
events under replay/restart. Do not implement SSH, shell, provider
administration, billing, pricing or Araf UI.

## Stop condition

Only report `BLOCKED` for a genuine external dependency. Required
kernel/store/aggregation/API changes belong in #904.

Only report `BLOCKED` for a genuine external dependency. Required
kernel/store/aggregation/API changes belong in #904.

Additional feeds-freezing constraints that the accepted contract already
carries, and which must be honoured rather than reinvented:

- The meter catalog is compiled into the kernel but a meter is advertised only
  while the active composition can produce it: `definitions` returns the
  producible subset and a usage request for a non-producible meter is rejected
  as an unknown meter rather than answered with a fabricated zero.
- Cross-scope usage reads are a durable kernel capability (`metering:ReadUsage`
  for the caller's effective scope, `metering:ReadUsageAll` for durable system
  scope plus the `operator` role), registered and discoverable in the
  authorization inventory — not ad-hoc handler logic.
- Unrecognized query parameters are rejected, so a typo cannot silently return
  a different scope's usage.
- `last_observed_at` is authority-wide observability, not per-scope
  completeness evidence.

Finish only with `#904 COMPLETE` when issue exit criteria and authoritative
usage evidence are proven.
