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

The duplicate-name pre-check now runs before scheduling as well as after it.
The latter remains a race fence; the former avoids a reservation/release cycle
for a conflict already visible in durable control-plane state.

The `o3kd` daemon now opens a durable Placement ledger below its configured
data directory and attaches one scheduler plus the authenticated-agent
registry when the compute-agent control plane is configured. Production
agent-backed create requests therefore use the same agent-eligibility and
Placement path as the tested compute service, while the local fake profile
without an agent control plane retains its standalone contract.

The daemon also periodically projects authenticated agent capability snapshots
into that ledger. VCPU, MEMORY_MB, and an explicit operator-declared
`max_disk_gb` become provider inventory; incomplete capacity and unavailable
or disabled agents are published fail-closed, while draining providers retain
existing allocations. This is inventory publication only and does not claim
agent lifecycle dispatch or real-host evidence.

## Evidence

- `o3k-placement` regression coverage forces the final publication rename to
  fail and verifies that the allocation, usage, and generation remain
  unchanged.
- `o3k-compute` regression coverage forces Placement publication to fail after
  provider deletion, then verifies a later idempotent delete releases the
  allocation.
- `o3k-compute` regression coverage verifies a conflicting durable create does
  not acquire an allocation before returning `Conflict`.
- `o3k-compute` regression coverage verifies repeated duplicate-name conflicts
  leave the provider generation and allocation set unchanged.
- The normal workspace tests continue to cover idempotency, stale-generation
  fencing, rollback, restart persistence, and reported-usage reconciliation.
- `cargo check -p o3kd` verifies the daemon wiring and dependency boundary.
- `o3k-compute` regression coverage verifies explicit disk-capacity mapping,
  durable allocation retention, and draining-state projection.
- No real OpenStack Placement service, agent-backed Nova lifecycle, or
  real-host acceptance is claimed.

## Remaining blockers

- issue #78's agent-backed provider path;
- server create/delete dispatch through the real agent provider;
- real guest scheduling/allocation evidence and restart recovery;
- trusted real-host artifact with `status: passed`.
