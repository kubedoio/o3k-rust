# Issue #904 — Metering evidence ledger (DRAFT)

Status: draft; placeholders marked `[pending]` are filled only after the
converged head is frozen, so that no post-approval commit invalidates the
exact-head CI and human-approval gate.

## Scope

| Item | Value |
| --- | --- |
| Issue | #904 — authoritative metering definitions and bounded usage aggregation |
| Program | #907 — production northbound contract closure for Araf P2 |
| Decision | ADR-0183 |
| Specification | SPEC-0046 |
| Contract | `contracts/native-metering-v1.schema.json` |
| Base main SHA | `17fed2e7db01a309824fcf4052677e79c7c3d321` |
| Branch | `planning/native-metering` |
| PR | #915 (draft) |
| Candidate head SHA | `[pending]` |
| GitHub CI | `[pending]` |

## Implemented meters

| Key | Service | Unit | Aggregation | Authoritative source | Known limitation |
| --- | --- | --- | --- | --- | --- |
| `compute:instance_seconds` | compute | `instance_second` | `integral` | Canonical compute lifecycle projection (`resources.observed_state` CAS for kind `compute_instance`, projected from the reconciler and the compute service) | A resource whose first durable O3K transition is not a create is metered from that transition only; earlier consumption is absent, never invented |
| `volume:allocated_byte_seconds` | volume | `byte_second` | `integral` | Canonical native volume authority (`native_volumes` create/delete plus startup recovery) | Volume resize does not exist in v1, so size is constant across an interval; if resize is added it must open a new interval |

A meter is advertised only while the active composition can produce it: the
definitions endpoint returns the producible subset and a usage request for a
non-producible meter is rejected as an unknown meter (`400`) rather than
answered with a fabricated zero. The volume meter is producible only when a
native storage provider is configured.

Deliberately **not** advertised: CPU utilisation/cycles, network bytes, storage
IOPS/bandwidth, object storage, floating-IP allocation time, request counts,
provider telemetry, energy, cost, price.

## Authority map

| Diagnostic surface | Authority |
| --- | --- |
| Meter catalog | `o3k_kernel::metering::METER_CATALOG` (static, code-owned) |
| Observations | `LifecycleMeteringObserver` implemented by `bins/o3kd` `MeteringAdapter` |
| Intervals + aggregates | `metering_intervals` / `metering_aggregates` (SQLite and PostgreSQL) |
| Start of authority | `metering_authority` (single row, anchored once) |
| Clock | `o3k_kernel::metering::Clock`; `SystemClock` in production, injectable in tests |

## Interval, aggregation and time semantics

- One open interval per `(meter, scope, resource)` series, enforced by a partial
  unique index.
- Closed intervals are split at one-hour ingest buckets and folded additively
  into per-series aggregates; the end instant is exclusive.
- All arithmetic is checked and fails closed on overflow; an interval spanning
  more than 96 000 ingest buckets fails closed.
- Instants are Unix milliseconds internally and RFC3339 UTC with `Z` publicly.
  Quantities are decimal strings with three fractional digits.

## Completeness

| Status | Meaning |
| --- | --- |
| `complete` | Whole requested period is at or after the authority anchor and at or before the evaluation instant |
| `partial` | Period starts before the anchor, or extends beyond the evaluation instant |
| `unavailable` | Period is wholly before the authority anchor; no usage is reported or inferable |

`stale` is deliberately not part of the vocabulary: recording is synchronous
with lifecycle authority and open intervals are evaluated live, so there is no
observation lag to report.

## Security boundary

| Requirement | Mechanism |
| --- | --- |
| Tenant reads only its own scope | Effective scope derived from `AuthContext`; `scope` parameter denied for non-system callers |
| Cross-scope reads | Durable `System` scope **and** `operator` role |
| Authorization before capability disclosure | `metering:ReadUsage` / `metering:ReadDefinitions` authorized before the reader-presence check |
| No secret leakage | DTO review + contract tests; no provider/endpoint/path/credential fields exist in the DTOs |
| Enumeration resistance | `resource_id` filters inside the authorized scope only; a foreign id yields no rows and no distinguishing error |
| DoS resistance | Hard bounds on range, meters, buckets, series and open intervals, enforced by rejection |

## Validation checklist

Focused tests added by this change:

- `crates/o3k-kernel/src/metering.rs` (14): catalog integrity, no pricing or
  formula text, bucket splitting, end-instant exclusivity, `MAX_INGEST_BUCKETS`
  rejection, bucket-end overflow fail-closed, contribution overflow
  fail-closed, accumulator merging, quantity formatting, query bound and
  alignment rejection, observation validation, full-range single-series
  admission, aggregate-row and distinct-series bounds.
- `crates/o3k-store/tests/metering_repository.rs` (33, SQLite): exact interval
  arithmetic, stopped intervals not accruing, idempotent replay of open and of
  close, replay of an open whose interval already closed, duplicate consume,
  close-without-open, zero-duration close, clock regression, pre-anchor
  observation refused, overlapping out-of-order open refused, quantity change,
  hour-boundary split, day-granularity multi-day split, full-range single-series
  day query, long interval, live open contribution, authority status, authority
  anchor immutability and watermark monotonicity, scope isolation,
  unrelated-resource filter, series and open-interval bound rejection,
  aggregate-row bound rejection, aggregate overflow, restart durability,
  colon-bearing series identity, future-end partiality, snapshot mechanism,
  concurrent reads, two writers folding once, cancelled write keeps the pool
  usable, dropped write rolls back.
The deterministic crash-window pins are
  `mid_fold_rollback_keeps_interval_open_and_converges` (SQLite) and
  `postgres_mid_fold_rollback_keeps_interval_open_and_converges` (PostgreSQL),
  and the reconciler safety nets are
  `observation_close_failure_is_healed_by_surfaced_lifecycle_close` and
  `surfaced_lifecycle_close_retries_until_the_interval_closes`.

- `crates/o3k-store/tests/postgres_metering.rs` (22, real PostgreSQL): the
  PostgreSQL parity of the above, plus concurrent release folding exactly once.
- `crates/o3k-store/tests/metering_migration_upgrade.rs` (1): a pre-metering
  schema upgrades with usable metering tables and the open-series index.
- `crates/o3k-native-api/src/metering.rs` (13): definition page bounds and
  cursor, catalog fidelity, exact JSON key sets, RFC3339 rendering, malformed
  instant rejection, status vocabulary, error mapping, unknown-parameter
  rejection, alignment rejection, contract-schema validation.
- `crates/o3k-api/tests/native_metering_routes.rs` (16): authentication,
  definitions visibility and limit bounds, own-scope reading, same-scope
  `scope=` accepted, foreign `scope=` denied for a project-scoped caller,
  system scope without the operator role denied, system operator allowed,
  `ReadUsageAll` discoverable in the authorization inventory, unknown parameter
  rejected, multiple meters, producible-set default, meter-count bound,
  alignment and unknown-meter rejection, `resource_id` narrowing.
- `crates/o3k-api/tests/native_volume_metering_repair.rs` (4): a lost volume
  open is repaired by the next read, the state→consuming mapping is pinned, a
  read during `Deleting` does not close, and create replay derives consuming
  from the durable row.
- `bins/o3kd/src/native_adapters/metering.rs` (8): state-to-consuming mapping
  (re-exported from the reconciler as the single definition), unknown state
  corrupt, non-metered kind no-op, definition paging, allocation close, exact
  lifecycle accrual, producible-meter filtering.
- `bins/o3kd/tests/native_metering_process.rs` (3): real HTTP
  definitions/authority, usage journey replay safety, undecodable-state refusal.
- `bins/o3kd/tests/native_metering_lifecycle_process.rs` (6): lifecycle-driven
  compute journey with exact totals across stop/start/delete and restart,
  volume allocation exactness and replay safety, Cinder-compatible volume
  create/delete metering, recovery closing an interrupted volume delete,
  recovery deferring the mutation when the projection fails, capability hiding.
- `crates/o3k-reconciler` (11): ordered lifecycle projection, replay does not
  duplicate the projected sequence, a failed projection surfaces and leaves the
  step retriable, a losing compare-and-set records no observation, a failing
  observer does not drop an agent observation, a duplicate observation repairs a
  lost projection, a dispatch-rejected create accrues no compute metering,
  a retried create terminal failure accrues no compute metering, and retry
  exhaustion accrues no compute metering — each through the real durable
  authority with restart and replay checks; a lost best-effort observation
  close is healed by the surfaced `finish_lifecycle` projection retried until
  terminalization, in both the heal-on-first-drive and the retry-until-close
  shapes.
- `crates/o3k-compute` (4): delete projects `DELETED`, a failing observer
  surfaces on the mutation path, a read-path projection failure does not fail
  `GET /servers/{id}`, and the read-path repair only opens/refreshing consuming
  states (it skips idle states).

Repository gates (each command was exercised green on the current head while
converging; the `[pending]` marks are placeholders that are filled only after
the converged head is frozen, so that no post-approval commit invalidates the
exact-head CI and human-approval gate):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | `[pending]` |
| `cargo check --workspace --all-targets --all-features` | `[pending]` |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `[pending]` |
| `cargo test --workspace --all-features` | `[pending]` |
| `git diff --check` | `[pending]` |
| `tests/adr-governance.sh` | `[pending]` |
| `tests/architecture-boundaries.sh` | `[pending]` |
| `scripts/check-architecture-boundaries.py` | `[pending]` |
| `scripts/check-maintainability-guards.py` | `[pending]` |

## Review convergence

Adversarial review round 1 (independent store-correctness and
API/integration/privacy reviews) produced findings that were all fixed with
regression tests: PostgreSQL replay-after-close divergence, out-of-order
overlapping open, zero-duration close leaving an interval open, unchecked
bucket-end arithmetic, non-injective interval identity, cancellation-unsafe raw
read transactions, the Cinder-compatible volume path not metering, recovery
swallowing a projection failure and then mutating the row, cross-scope
authority not expressible in the authorization inventory, silently ignored
unknown query parameters, lost read-path projections, projection before a
losing compare-and-set, unenforced producibility, and several coverage gaps.

Round 1 findings: BLOCKER 1, HIGH 3, MEDIUM 5, LOW 10. All fixed.

Review round 2 (two independent reviewers, fresh orders) produced:
BLOCKER 0, HIGH 1, MEDIUM 4, LOW 3. All fixed:

- read-path repair could close a still-consuming `Deleting` interval — repairs
  now only open/refresh consuming intervals; closing belongs to the
  authoritative transition paths (regression: a read during `Deleting` leaves
  the interval open and totals unchanged);
- `MAX_USAGE_SERIES` bounded ingest rows rather than distinct series, spuriously
  rejecting legitimate full-range queries — split into
  `MAX_USAGE_AGGREGATE_ROWS = 25000` (work bound) and `MAX_USAGE_SERIES = 500`
  (distinct-series cardinality bound), plus a full-range single-series test;
- the SQLite write path was not cancellation-safe (raw `BEGIN IMMEDIATE`) —
  moved to `pool.begin_with("BEGIN IMMEDIATE")` with rollback-on-drop;
- the open-interval read had no covering index — added
  `idx_metering_intervals_open_series` in both migrations;
- no crash/failure-injection test existed between the CAS close and the fold —
  added aborted-write rollback tests proving the interval stays open, aggregates
  unchanged, and a replayed close converges;
- no test pinned the disclosed failed-create residual (`0.000` usage, no open
  interval, restart-stable) — added dispatch-rejection, retried-terminal-failure
  and retry-exhaustion tests;
- the native create-replay observation was state-independent and could reopen a
  `Deleting` interval — it now derives consuming from the durable row;
- doc wording implying full repair was made forward-only precise.

Two consecutive independent clean convergence passes at one unchanged head are
`[pending]` and are not claimed here.

## PostgreSQL evidence

`crates/o3k-store/tests/postgres_metering.rs` runs against a disposable
`postgres:16.4` container with `O3K_DATABASE_URL` set, one isolated database
per test. Each test creates its own database, runs, then terminates every
remaining backend for that database and drops it with `WITH (FORCE)` and a
bounded retry, so the suite is robust under `cargo test`'s in-process
parallelism and `cargo nextest`'s per-test process isolation alike — the
fixture's teardown never depends on a process-level mutex. Verified by running
the suite both ways against a real container. `O3K_DATABASE_URL` being absent is
a skip, not a blocker.

## Honest non-claims

- No automatic compaction/retention job exists in v1; aggregates are additive
  and bounded by series count and the ingest window, and closed intervals are the
  durable reconciliation record. There is no raw event history to compact.
- No operation or audit correlation identifier is embedded in usage responses;
  a bucket aggregates many transitions, so correlation is by resource identity.
- Global cross-scope aggregation is not offered; an operator reads explicit
  scopes.
- Cost and price are not part of the contract.
