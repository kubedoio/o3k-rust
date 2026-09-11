# ADR-0183 — Authoritative Metering and Bounded Usage Aggregation

Status: Accepted
Date: 2026-09-11
Human-approval: protected review-and-merge loop for issue #904 (approved plan recorded on PR #915)
Supersedes: none
Superseded-by: none
Affected-services: cloud-kernel, compute, storage, operations

Related issue: #904
Normative specification: [SPEC-0046](../specs/SPEC-0046-native-metering-v1.md)
Related decision: [ADR-0165](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)

## Context

Araf P2.7 needs truthful usage data with explicit units and time ranges. O3K
currently exposes quota (a point-in-time allocation counter) and audit (an
administrative fact log), but neither answers "how much did this scope consume
over this interval".

Without a metering authority, a consumer must either count current resources
client-side, sample compatibility APIs, or infer consumption from provider
telemetry. All three fabricate consumption that O3K cannot prove, and none can
survive replay, restart or deletion.

Authoritative lifecycle inspection established the following facts about
current `main`:

- the canonical compute `Server` domain type carries **no timestamps**; the only
  durable transition instants are the canonical operation's `started_at` /
  `finished_at`, which exist only for canonical mutation paths;
- the durable compute row (`resources`, kind `compute_instance`) carries the
  flavor/vCPU/RAM snapshot in `desired_state`, immutable for the lifecycle, and
  is tombstoned (`observed_state = DELETED`) rather than hard-deleted;
- the durable volume row (`native_volumes`) carries an immutable `created_at`
  and an immutable `size_bytes`; volume deletion hard-deletes that row;
- there is no volume resize, so size never changes within an allocation;
- floating-address allocation has no durable allocation/release instant, so its
  allocation duration is not authoritative today.

## Decision

1. **Metering is a first-class Cloud Kernel authority, distinct from quota,
   audit, diagnostics and billing.** A meter describes a quantity aggregated
   over time. Quota remains a current-allocation counter and must never be read
   as historical consumption.

2. **Usage is recorded from canonical O3K lifecycle authority, not derived by
   enumerating current resources.** The control plane projects each durable
   lifecycle state change it already applies into a deterministic, idempotent
   metering observation. Usage is never reconstructed by scanning resources.

3. **Usage is represented as durable intervals folded into durable aggregates.**
   An observation opens, refreshes or closes exactly one interval per
   (meter, scope, resource) series. A closed interval is split into fixed
   one-hour ingest buckets and folded additively into per-series aggregates.
   Bounded queries read aggregates plus live open intervals; they never scan raw
   history.

4. **Observation identity is deterministic and applied at-least-once.** Interval
   identity derives from the meter, scope, resource and open instant. Closing is
   guarded by a compare-and-set on `ended_at_ms IS NULL`, so a replayed or
   concurrently applied transition produces exactly one durable effect. On a
   mutation path a metering failure is surfaced to the caller, which retries the
   idempotent step, and the projection is applied before the step is
   terminalized so a crash window is repaired rather than lost. Where a
   projection cannot be retried behind the transition that already committed —
   an agent observation whose compare-and-set already landed, or a native volume
   whose row is durably created — the authority repairs instead: it re-projects
   the resource's **durable** state, which is idempotent and derived from truth
   rather than from an unapplied input, and the next observation or read of that
   same resource makes good on a projection that was lost. The read path also
   uses this durable-state repair and is best-effort, because a metering hiccup
   must never turn a read into an error. Metering is never silently dropped in
   any of these cases.

5. **Start of O3K authority is explicit and durable.** Metering authority begins
   at an anchored instant and is never backdated. Periods before that instant
   are reported `unavailable` (wholly) or `partial` (partly) and may never be
   presented as complete. Imported or migrated resources therefore never inherit
   consumption O3K did not observe.

6. **A meter is only advertised when O3K can prove it.** The v1 catalog is
   compute instance-seconds and allocated volume byte-seconds. CPU utilisation,
   network bytes, storage I/O, request counts, provider telemetry and cost are
   deliberately absent rather than estimated.

7. **Query bounds are enforced, not truncated.** Range, meter count, bucket
   count, series count and open-interval count all have hard maxima; a request
   exceeding any of them fails instead of returning a silently partial
   authoritative number.

8. **Units and precision stay explicit.** Every result names its unit and
   renders quantities as decimal strings with millisecond precision, so large
   byte-second totals never lose precision through floating point.

9. **Ownership scope is always derived from `AuthContext`.** A tenant may read
   only its own effective scope; cross-scope reads require durable system or
   operator authority. No query parameter broadens a caller's authority.

10. **Pricing is out of scope.** The contract emits usage only. Cost, currency,
    discounts, tax, invoices, payments and ledgers belong to a separate
    authoritative contract that does not exist yet.

## Consequences

- The Cloud Kernel gains a metering module, a durable metering repository with
  SQLite/PostgreSQL parity, two versioned native endpoints and a new
  `LifecycleMeteringObserver` port.
- Compute and volume lifecycle paths gain an optional observer call. Because
  metering failures propagate, a metering outage can fail the mutation step; the
  retry is safe because every observation is idempotent.
- The `storage` service is listed as affected because volume allocation is
  metered through the native volume authority.
- Future meters must be added to the catalog and SPEC-0046 together, with proof
  of their authoritative source; the catalog is not a roadmap.

## Alternatives considered

- **Derive usage from quota usage counters.** Rejected: quota is point-in-time
  allocation, carries no time semantics and cannot reconstruct intervals.
- **Derive usage by periodically enumerating resources.** Rejected: it cannot
  recover history it did not observe, and it violates the boundedness and
  "no unbounded scan" invariants.
- **Reuse audit events as metering authority.** Rejected: audit is a
  best-effort, retention-bounded administrative record with different query and
  retention requirements, and it is not written on every lifecycle projection.
- **Provider telemetry meters (CPU, network, storage I/O).** Rejected for v1:
  no authoritative integrated measurement source exists, so advertising them
  would fabricate precision.
