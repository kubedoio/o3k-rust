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

- `crates/o3k-kernel/src/metering.rs` (13): catalog integrity, no pricing or
  formula text, bucket splitting, end-instant exclusivity, `MAX_INGEST_BUCKETS`
  rejection, bucket-end overflow fail-closed, contribution overflow
  fail-closed, accumulator merging, quantity formatting, query bound and
  alignment rejection, observation validation.
- `crates/o3k-store/tests/metering_repository.rs` (27, SQLite): exact interval
  arithmetic, stopped intervals not accruing, idempotent replay of open and of
  close, replay of an open whose interval already closed, duplicate consume,
  close-without-open, zero-duration close, clock regression, pre-anchor
  observation refused, overlapping out-of-order open refused, quantity change,
  hour-boundary split, day-granularity multi-day split, long interval, live open
  contribution, authority status, authority anchor immutability and watermark
  monotonicity, scope isolation, unrelated-resource filter, series and
  open-interval bound rejection, aggregate overflow, restart durability,
  colon-bearing series identity, future-end partiality, snapshot mechanism,
  concurrent reads, two writers folding once.
- `crates/o3k-store/tests/postgres_metering.rs` (17, real PostgreSQL): the
  PostgreSQL parity of the above, plus concurrent release folding exactly once.
- `crates/o3k-store/tests/metering_migration_upgrade.rs` (1): a pre-metering
  schema upgrades with usable metering tables.
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
- `bins/o3kd/src/native_adapters/metering.rs` (8): state-to-consuming mapping,
  unknown state corrupt, non-metered kind no-op, definition paging, allocation
  close, exact lifecycle accrual, producible-meter filtering.
- `bins/o3kd/tests/native_metering_process.rs` (3): real HTTP
  definitions/authority, usage journey replay safety, undecodable-state refusal.
- `bins/o3kd/tests/native_metering_lifecycle_process.rs` (6): lifecycle-driven
  compute journey with exact totals across stop/start/delete and restart,
  volume allocation exactness and replay safety, Cinder-compatible volume
  create/delete metering, recovery closing an interrupted volume delete,
  recovery deferring the mutation when the projection fails, capability hiding.
- `crates/o3k-reconciler` (4) and `crates/o3k-compute` (4): ordered lifecycle
  projection, replay does not duplicate the projected sequence, a failed
  projection surfaces and leaves the step retriable, a losing compare-and-set
  records no observation, a read-path projection failure does not fail
  `GET /servers/{id}`, and a lost read-path projection is repaired on a later
  drive.

Repository gates:

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

Two consecutive independent clean convergence passes at one unchanged head are
`[pending]` and are not claimed here.

## PostgreSQL evidence

`crates/o3k-store/tests/postgres_metering.rs` runs against a disposable
`postgres:16.4` container with `O3K_DATABASE_URL` set, one isolated database per
test, executed with `--test-threads=1`. `O3K_DATABASE_URL` being absent is a
skip, not a blocker.

## Honest non-claims

- No automatic compaction/retention job exists in v1; aggregates are additive
  and bounded by series count and the ingest window, and closed intervals are the
  durable reconciliation record. There is no raw event history to compact.
- No operation or audit correlation identifier is embedded in usage responses;
  a bucket aggregates many transitions, so correlation is by resource identity.
- Global cross-scope aggregation is not offered; an operator reads explicit
  scopes.
- Cost and price are not part of the contract.
