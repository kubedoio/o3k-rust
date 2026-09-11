# SPEC-0046 — Native Metering Definitions and Bounded Usage Aggregation v1

Status: Accepted

Related issue: [#904](https://github.com/o3kio/o3k/issues/904)
Related decision: [ADR-0183](../adr/ADR-0183-authoritative-metering-and-bounded-usage-aggregation.md)
Related contract: [native-metering-v1.schema.json](../../contracts/native-metering-v1.schema.json)
Related normative sources: [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md),
[ADR-0166](../adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md),
[SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md),
[SPEC-0045](SPEC-0045-native-operator-diagnostics-v1.md)

## 1. Purpose

This specification freezes the v1 O3K metering contract: what a meter is, which
meters O3K advertises, how usage is recorded and aggregated, what a bounded
usage query returns, and what it must never claim.

Metering is **usage over time**. It is not quota (current allocation against a
limit), not audit (administrative facts), not diagnostics (health/freshness) and
not billing/pricing.

## 2. Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/o3k/v1/metering/definitions` | Bounded, stable-ordered meter catalog |
| `GET` | `/o3k/v1/metering/usage` | Bounded usage aggregation |

Both are mounted only when the native API is configured. Responses conform to
`contracts/native-metering-v1.schema.json`.

### 2.1 Definitions

Query parameters: `limit` (1–200, default 50), `cursor` (opaque, base64url of
the last returned key). Ordering is by ascending meter key and is stable. The
page reports `has_more` and an opaque `next_cursor`.

### 2.2 Usage

Query parameters:

| Parameter | Required | Meaning |
| --- | --- | --- |
| `meter` | no (repeatable) | Meter key. Defaults to the advertised catalog in catalog order. At most 8. |
| `start` | yes | RFC3339 inclusive start, aligned to the requested granularity |
| `end` | yes | RFC3339 exclusive end, aligned to the requested granularity |
| `granularity` | no | `hour` (default) or `day`; both UTC |
| `resource_id` | no | Single series filter within the caller's authorized scope |
| `scope` | no | Optional target scope. Honored when it equals the caller's effective scope; otherwise it requires the `metering:ReadUsageAll` capability (durable system scope plus the `operator` role). |

Unrecognized query parameters are rejected with `400` rather than ignored, so a
typo cannot silently return a different scope's usage than the caller asked for.

The response is a JSON array with exactly one element per requested meter, in
request order. When `meter` is omitted the requested set is the **producible**
catalog reported by the definitions endpoint, evaluated in the same order. Each
element carries `scope`, `meter_key`, `unit`,
`aggregation`, `granularity`, `start`, `end`, `observed_through`,
`authority_started_at`, `last_observed_at`, `status`, `buckets` and `total`.

All instants are RFC3339 UTC with `Z`. All quantities are decimal strings with
exactly three fractional digits in the meter's unit; raw floating point is never
emitted.

## 3. Meter definitions

A meter definition carries a stable namespaced key, owning service, unit,
aggregation kind, canonical resource type, supported granularities, tenant and
operator visibility, a secret-free description and a version.

v1 advertises exactly:

| Key | Service | Unit | Aggregation | Resource | Granularities |
| --- | --- | --- | --- | --- | --- |
| `compute:instance_seconds` | compute | `instance_second` | `integral` | `compute_instance` | `hour`, `day` |
| `volume:allocated_byte_seconds` | volume | `byte_second` | `integral` | `volume` | `hour`, `day` |

The catalog is compiled into the kernel, but a meter is **advertised only while
the active composition can produce it**. The definitions endpoint returns the
producible subset, and a usage request for a meter the composition cannot
produce is rejected as an unknown meter (`400`) rather than answered with a
fabricated zero. Concretely, the volume meter is producible only when a native
storage provider is configured.

**Explicitly not advertised in v1:** CPU utilisation, CPU cycles, network
ingress/egress bytes, storage IOPS or bandwidth, object storage, floating-IP
allocation time, request counts, provider telemetry, energy, cost and price.
Absence is correct; fabricated precision is not. Definitions never contain
pricing, layout or executable formulas, and a meter is advertised only while it
is genuinely producible for the active profile.

## 4. Source authority map

| Meter | Authoritative source | Recorded when |
| --- | --- | --- |
| `compute:instance_seconds` | Canonical compute lifecycle projection (`update_resource` / `update_resource_from_observation` on kind `compute_instance`) | The control plane durably applies an observed state |
| `volume:allocated_byte_seconds` | Canonical native volume authority (`native_volumes` insert/delete and startup recovery) | The volume row is durably created or removed |

Consuming states for `compute_instance` are `ACTIVE`, `STARTING`, `STOPPING` and
`REBOOTING`. `REQUESTED`, `BUILD`, `SHUTOFF`, `DELETED` and `ERROR` are idle. An
undecodable state is a corrupt observation and fails closed; it is never
treated as idle or consuming.

For `volume`, "consuming" means the volume exists: the interval opens when the
row is durably created and closes when it is durably removed. Volume size is
immutable in v1 (there is no resize), so the interval quantity is constant.

## 5. Interval and aggregation semantics

- Usage is `Σ quantity × duration`, recorded as durable intervals per
  `(meter, scope, resource)` series.
- At most one interval per series is open at any instant, guaranteed by a
  partial unique index on `(meter_key, scope, resource_id) WHERE ended_at_ms IS
  NULL`.
- A closed interval `[start, end)` is split at fixed one-hour ingest bucket
  boundaries. The end instant is exclusive; adjacent buckets neither overlap nor
  omit a boundary instant.
- Contributions are folded additively into per-series, per-ingest-bucket
  aggregates. Aggregates are only ever produced by closed intervals.
- All arithmetic is checked. A contribution or aggregate that would exceed the
  signed 64-bit range fails closed instead of wrapping or saturating.
- An interval spanning more than `MAX_INGEST_BUCKETS` (96 000 hourly buckets)
  fails closed rather than truncating.

## 6. Observation and idempotency semantics

An observation is `(meter_key, scope, resource_id, quantity, consuming,
observed_at_ms, authority)`.

- `consuming = true` with an open interval for the series is an idempotent
  refresh: it never opens a second interval.
- `consuming = true` without an open interval opens one at `observed_at_ms`.
- `consuming = false` without an open interval is an idempotent no-op.
- `consuming = false` with an open interval closes it and folds usage exactly
  once, guarded by a compare-and-set on `ended_at_ms IS NULL`. A concurrent or
  replayed close that loses the compare-and-set does not fold again. A close at
  the same instant as the open closes with zero usage; it is not an error.
- An observation earlier than the open interval's start is a clock regression
  and fails closed.
- An observation earlier than the durable start-of-authority anchor is refused;
  it predates O3K metering authority and must not be folded.
- A new interval may not start before the series' latest closed interval end; an
  overlapping out-of-order open fails closed instead of double-counting a period
  that is already folded. An open exactly at that end is adjacent and allowed.
- A quantity change within an open interval fails closed. v1 supports only
  constant-quantity intervals.
- Interval identity is deterministic (`uuid5` over meter, scope, resource and
  open instant), so replay never creates a second interval for the same open
  event.

Two independent writers therefore produce exactly one durable effect per logical
transition. Correctness rests on durable uniqueness, compare-and-set and
transactions, never on process-local locks.

On a mutation path a projection failure is surfaced to the caller and the step is
retried. Where a projection cannot be retried behind a transition that already
committed, the projection is repaired by re-projecting the resource's durable
state — which is idempotent and derived from truth — on the next observation or
read of that same resource. A repair never emits a consuming=`false` close:
repairs only open or refresh consuming intervals, and closing belongs to the
authoritative transition paths (`remove_native_volume`, provider-absence
recovery, and the surfaced mutation projections), because a close must carry
the transition's own instant rather than a later read instant. The read-path
repair projection is best-effort: it logs, because a metering hiccup must not
fail a read.

## 7. Time semantics

- The canonical clock is UTC. All persisted and reported instants are Unix
  milliseconds or RFC3339 UTC with `Z`.
- Event time is the instant the control plane applies the durable transition.
  There is no separate ingestion time in v1 because recording is synchronous
  with the transition.
- Bucket boundaries are floor-aligned to the ingest width (one hour) and to UTC
  for `day` granularity. `start` and `end` must be exactly aligned to the
  requested granularity; the API rejects unaligned requests instead of silently
  aligning them.
- Client timezone never influences computation.
- Deletion closes the open interval at the deletion instant.

## 8. Start of authority

`authority_started_at_ms` is anchored once, the first time metering authority is
initialized, and is never backdated or rewritten.

- A requested period wholly before the anchor returns `status: unavailable`
  with no buckets and `0.000` total.
- A requested period that starts before and ends after the anchor returns
  `status: partial`; only the covered part is counted.
- A requested period wholly at or after the anchor returns `status: complete`.

Imported or migrated resources therefore never inherit consumption O3K did not
observe. A resource whose first durable O3K transition is a state change rather
than a create is metered from that transition onwards; earlier consumption is
silently *absent*, not fabricated.

## 9. Completeness and freshness

Every response carries `observed_through` (never later than the evaluation
instant used by the metering authority), `authority_started_at` and
`last_observed_at`. The metering reader re-stamps the evaluation instant from
its own clock, so observation and evaluation share one time source; in
production that clock is the system clock.

`last_observed_at` is **authority-wide observability, not per-scope
completeness evidence**. It is the most recent durable observation processed by
the whole metering authority, so whichever scope was observed most recently sets
it: a quiet scope can therefore appear fresher than it is, and `last_observed_at`
must not be read as evidence that this scope is current. Per-scope completeness
meaning comes only from `authority_started_at` and `observed_through`.

Because recording is synchronous with the lifecycle authority, and because open
intervals are evaluated live at query time rather than folded, a returned series
is current as of `observed_through`. A requested period that extends beyond the
evaluation instant is not yet fully observed and is therefore `partial`, never
`complete`.

`stale` is deliberately absent from the vocabulary rather than synthesized:
metering has no asynchronous processing pipeline, so there is no observation lag
that could be stale, and reporting one would be a fabrication. One caveat is
honest and documented instead: a projection that is lost (an observation whose
commit landed after the durable transition, or an outage during a volume-open
observe) is repaired **forward only** by the durable-state repair described in
§6 — it never backdates, so the segment between the durable transition and the
repair is absent, exactly as an imported resource's pre-authority segment is
absent. `complete`, `partial` and `unavailable` are the only truthful states.

## 10. Boundedness

Hard maxima, enforced by rejection and never by truncation:

| Bound | Value |
| --- | --- |
| Query range | 366 days |
| Output buckets per meter | 1100 |
| Meters per query | 8 |
| Aggregate rows read (resource × ingest bucket) | 25000 |
| Distinct resource series read | 500 |
| Open intervals read | 20000 |
| Definitions page size | 200 |

These bounds are independent: a request that satisfies any of them but exceeds
another is rejected, so a maximal range plus a maximal series count is not
claimable. Aggregate reads are indexed by scope, meter and ingest bucket, and
open intervals by a partial index over open series only, so a query never scans
closed history to find open intervals.

Aggregates are read at the repository/SQL boundary with the scope as the leading
predicate. A usage request never enumerates resources, volumes, networks or
tenants, and never performs a provider discovery fan-out.

## 11. Authorization

| Action | Resource | Principal | Ownership |
| --- | --- | --- | --- |
| `metering:ReadDefinitions` | `metering:definitions` | User | not required |
| `metering:ReadUsage` | `metering:usage` | User | required (caller's effective scope) |
| `metering:ReadUsageAll` | `metering:usage` | User | not required; `System` scope **and** the `operator` role |

- Authorization runs before the reader is consulted, so an unauthorized caller
  cannot distinguish an unavailable subsystem from a forbidden one.
- A tenant reads only its effective `AuthContext` scope. Supplying `scope` equal
  to its own effective scope is accepted; supplying any other scope requires
  `metering:ReadUsageAll` and is otherwise denied, never silently ignored.
- `metering:ReadUsageAll` requires durable system authority: a project-scoped
  caller carrying a role named `operator` cannot satisfy it.
- `resource_id` narrows within the caller's authorized scope and can never
  broaden it or reveal foreign existence beyond accepted policy.

## 12. Privacy

Responses carry only meter keys, units, bucket instants, decimal quantities,
the caller's authorized scope and definition metadata. They never carry
provider credentials, endpoints, host paths, node or agent identities, epochs,
database details, connection strings, raw errors or arbitrary provider labels.
Meter dimensions are fixed by the catalog; no caller- or provider-supplied label
becomes a dimension, so high-cardinality metadata cannot enter the contract.

## 13. Errors

| Condition | Result |
| --- | --- |
| Missing or invalid credentials | `401` |
| Caller lacks authority, or requests a scope it cannot read | `403` |
| Malformed request, unrecognized query parameter, unaligned instants, unknown or non-producible meter, exceeded bound | `400` |
| Metering authority unavailable | `503` |
| Corrupt or inconsistent metering state | `500` |

A meter with no usage in the requested period is **data**: an empty bucket list
with `0.000`, not an HTTP error. Degradation is reported as data or as the
status field, never by failing an otherwise valid request.

## 14. Retention and compaction

v1 defines no automatic compaction. Aggregates are additive and bounded by the
number of series and the ingest window; closed intervals remain as the durable
reconciliation record. Raw per-observation events are not stored separately, so
there is no raw history to compact and no risk of deleting the only durable
source. Closed-resource history remains queryable for as long as its aggregates
are retained; operators prune by retention policy outside this contract.

## 15. Correlation

Usage correlates with the canonical resource identity (`resource_id`) that
appears in Operations and Audit. v1 does **not** embed operation or audit
identifiers in usage responses: a usage bucket is an aggregate over many
transitions, so attaching a single operation would be misleading. Correlation is
performed by the consumer joining on resource identity.

## 16. Non-goals

Explicitly out of scope for #904 and this specification: SSH, shell, kubectl,
provider command execution, provider credential or configuration exposure,
arbitrary RPC invocation, remediation actions, capacity override, quota
mutation, Ceilometer/OpenStack telemetry compatibility, pricing, discounts, tax,
invoices, payments, credits, accounting ledgers, cost allocation, Araf UI, and
global cross-scope aggregation (an operator reads explicit scopes; there is no
`scope=*`).
