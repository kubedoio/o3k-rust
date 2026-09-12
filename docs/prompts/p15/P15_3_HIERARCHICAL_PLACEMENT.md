# P15.3 — Hierarchical Placement

Issue: #933. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930) and P15.1
(#931, topology/failure domains) are merged; P15.3 consumes P15.1 failure-domain
references. Read the umbrella issue #929 before writing code.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #933
- Deployment/evidence profile: TestLab/portable + production-oriented;
  placement store on SQLite and PostgreSQL
- Canonical service/domain: Cloud Kernel placement/capacity
- OpenStack compatibility adapter: none (placement is internal kernel domain;
  any Placement-like compatibility surface stays a projection)
- Authority mode: o3k-implemented (kernel owns capacity/scheduling authority)
- Files expected to change: crates/o3k-placement/src/lib.rs and siblings
  (currently flat host-shaped ResourceProvider with VCPU/MEMORY_MB/DISK_GB only)
- Contracts/specs affected: ADR-0184, SPEC-0047
- Provenance: ADR-0184, SPEC-0047, p15-e2d-gap-register.md,
  ADR-0181/SPEC-0038 (failure-domain references consumed from P15.1)
- Operations/actions/resources: provider/inventory publication, allocation
  intent begin/commit/abandon under IAM scope
- Database assumptions: durable allocations and intents; SQLite + PostgreSQL
  parity for any new store area
- Cross-service dependencies/compensation: consumes canonical topology (P15.1)
  by reference; consumers (compute execution) compensate per SPEC-0021
- Evidence tier: domain/store tests + provider conformance fakes + process
  tests
- Tests first: hierarchy/capability candidate selection, fencing, drain
  exclusion, deterministic order
- Known uncertainties: trait/catalog naming; follow SPEC-0047, do not invent a
  second capability taxonomy
- Explicit non-goals: scheduler replacement, performance/scale ceilings,
  building blocks
```

Until ADR-0184 is Accepted by human review, implementation under it proceeds as
Proposed-architecture work and the PR must record
`P15.3 implementation authorized: NO — awaiting human architecture approval`
if the human has not approved. Do not self-approve.

## Objective

Extend `crates/o3k-placement` (currently a flat, host-shaped ResourceProvider
model with only VCPU/MEMORY_MB/DISK_GB inventory) with parent/child provider
topology, capabilities/traits, and failure-domain references consumed from
P15.1. Implement candidate selection by quantity, required/forbidden
capabilities, location constraints, failure-domain constraints, and locality.
Preserve every existing placement invariant: durable allocations, deterministic
candidate order, idempotency, generation fencing (`StaleGeneration`),
`AllocationIntent` begin/commit/abandon, unknown-outcome semantics,
observe-before-retry, and P11 drain semantics (`ProviderState::Draining`
excludes the provider from placement with honest blockers). Stay
provider-neutral.

## Authoritative dependencies

Read before editing: ADR-0184 (Proposed), SPEC-0047 (proposed),
`docs/architecture/p15-e2d-gap-register.md`,
`docs/architecture/p15-0-post-araf-current-state-audit.md`, ADR-0181,
SPEC-0038 (P15.1 output). Mandatory per AGENTS.md: `README.md`,
`docs/PROJECT_CHARTER.md`, `docs/CLEAN_IMPLEMENTATION.md`,
`docs/ARCHITECTURE.md`, `docs/NORMATIVE_SOURCES.md`, `docs/TEST_STRATEGY.md`,
ADR-0165/0166/0167/0160/0162/0163, SPEC-0020/0021/0022/0024/0025,
`compatibility/product-profiles.yaml`, `contracts/execution-boundaries.md`,
`contracts/core-architecture-boundaries.toml`.

## Architecture boundaries and guardrails

Guardrails are normative:

- NO cells and NO sharding. Do not add partitioning or cell-style fanout;
  measure first — P18 owns measurements.
- Placement remains provider-neutral: no libvirt/KVM/fabric-specific semantics
  in candidate selection.
- Placement consumes topology by reference from the P15.1 authority; it never
  redefines topology.
- Keep the placement crate an internal kernel domain; any OpenStack
  Placement-compatible surface is a projection, not this work.

## In scope

- Parent/child ResourceProvider topology within placement.
- Capabilities/traits on providers, with required/forbidden selection.
- Failure-domain references (from P15.1) usable as selection constraints.
- Candidate selection: quantity, required/forbidden capabilities, location
  constraints, failure-domain constraints, locality.
- Preservation of durable allocations, deterministic candidate order,
  idempotency, generation fencing (`StaleGeneration`), `AllocationIntent`
  begin/commit/abandon, unknown-outcome semantics, observe-before-retry, and
  P11 drain exclusion with honest blockers.

## Out of scope

- Replacing or renaming the scheduler role of placement in the architecture.
- Performance or scale ceilings (P18 owns measurements).
- Building Block lifecycle (P15.5).

## Authority model

O3K Cloud Kernel owns capacity and scheduling authority (`o3k-implemented`).
Execution providers publish inventory and honor allocations; they never
authorize callers or invent public O3K IDs. Failure domains remain P15.1
authority, referenced by ID.

## Security requirements

Allocation intents and inventory publication are protected operations under the
shared typed `AuthContext` with canonical ownership scopes. Candidate lists and
allocation results must not leak cross-tenant existence. Audit allocation
lifecycle transitions with canonical audit identity.

## Database implications

Allocations, intents, provider topology, and trait bindings are durable.
Any new store area has SQLite + PostgreSQL parity: same invariants, same
migrations. Persist intent/phase before external side effects where recovery
requires it.

## OpenStack compatibility implications

None directly: no new OpenStack-facing endpoint is added. If an existing
compatibility surface projects placement state, keep it convergent with the new
model and update its profile record; do not add unprofiled compatibility.

## Araf implications

Araf may observe placement outcomes through discovery/diagnostics only after
P15.5 exposes them; Araf does not own placement state.

## Failure and restart behavior

Durable allocations and intents survive restart and reconcile against provider
generations. Unknown-outcome semantics are preserved: a timed-out commit is
unknown, observed before retry, never assumed failed or succeeded. Draining
providers stay excluded across restarts until drain completes or is reversed
honestly.

## Idempotency and concurrency requirements

`AllocationIntent` begin/commit/abandon uses deterministic identity; retries
converge. Generation fencing (`StaleGeneration`) rejects decisions made against
stale inventory. Concurrent selections serialize so deterministic candidate
order holds for identical requests.

## Required tests

- Hierarchy, capabilities/traits, and failure-domain constraint selection.
- Deterministic candidate order for identical requests.
- Generation fencing (`StaleGeneration`) on stale inventory.
- AllocationIntent begin/commit/abandon, idempotent replay, unknown-outcome
  observe-before-retry.
- P11 drain exclusion with honest blockers (`ProviderState::Draining`).
- SQLite + PostgreSQL parity for new store areas.

## Required real-process evidence

Process-level placement tests with real `o3kd` + real auth demonstrating
constraint selection and drain behavior. Provider-side effects covered by
conformance fakes plus the real execution boundary where the evidence tier
requires it.

## Claim limitations

Do not claim cells, sharding, scale ceilings, or scheduler replacement. Do not
claim performance numbers without P18 measurements. Do not claim building-block
semantics (P15.5).

## Validation ladder

Run package-level `cargo check -p o3k-placement -p o3k-kernel`, then tests,
then clippy on the same packages. Before completion run
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace --all-features`, plus
`scripts/check-architecture-boundaries.py`,
`scripts/check-maintainability-guards.py`, and
`scripts/validate-kernel-actions.py`.

## Acceptance criteria

Issue #933 acceptance criteria met; all preserved invariants green; P15.1
failure-domain references consumed by reference; guardrails honored.

## Completion

```text
Hierarchical providers + traits + FD constraints: PASS
Candidate selection semantics complete: PASS
Allocations durable + deterministic order: PASS
StaleGeneration fencing: PASS
Drain exclusion with honest blockers: PASS
No cells/sharding added: CONFIRMED
P15.3 implementation authorized: NO — awaiting human architecture approval
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
