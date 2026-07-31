# Issue #82 — Placement lifecycle safety boundary

Issue #82 remains open and depends on issue #78. Its full acceptance requires
real agent capability registration, Nova create/delete wiring, guest lifecycle
evidence, and a trusted real-host scheduling artifact.

## Bounded repository slice

The persistent Placement ledger now treats state publication as a transaction
from the caller's perspective. Registration, refresh, synchronization,
allocation, release, and provider-state mutations restore the prior in-memory
state if JSON serialization or atomic file publication fails. This prevents a
failed write from leaking a reservation or generation into later scheduling
before a process restart.

The compute delete path now retries a recorded Placement release when provider
deletion already projected the server to `DELETED` but the first release
publication failed. This prevents the idempotent delete fast path from making
that allocation permanently unreachable within the running process.

The compute create path now checks the deterministic durable resource conflict
before scheduling. Known conflicts therefore do not acquire a new Placement
allocation; duplicate-name and pre-journal store-error paths release any
allocation acquired by the current request.

## Evidence

- `o3k-placement` regression coverage forces the final publication rename to
  fail and verifies that the allocation, usage, and generation remain
  unchanged.
- `o3k-compute` regression coverage forces Placement publication to fail after
  provider deletion, then verifies a later idempotent delete releases the
  allocation.
- `o3k-compute` regression coverage verifies a conflicting durable create does
  not acquire an allocation before returning `Conflict`.
- The normal workspace tests continue to cover idempotency, stale-generation
  fencing, rollback, restart persistence, and reported-usage reconciliation.
- No real OpenStack Placement service, agent-backed Nova lifecycle, or
  real-host acceptance is claimed.

## Remaining blockers

- issue #78's agent-backed provider path;
- real agent inventory publication and server create/delete dispatch;
- real guest scheduling/allocation evidence and restart recovery;
- trusted real-host artifact with `status: passed`.
