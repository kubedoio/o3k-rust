# P15.5 — Building Block Lifecycle

Issue: #935. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930), P15.1 (#931,
topology), P15.3 (#933, placement), and P15.4 (#934, CloudProfile) are merged;
P15.5 links to all of them by reference. Read the umbrella issue #929 before
writing code.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #935
- Deployment/evidence profile: TestLab/portable + production-oriented +
  small edge; evidence per profile recorded in tests
- Canonical service/domain: Cloud Kernel building-block lifecycle
- OpenStack compatibility adapter: none
- Authority mode: o3k-implemented; execution identities are authenticated
  mTLS agent identities
- Files expected to change: crates/o3k-kernel (BuildingBlock domain),
  crates/o3k-native-api (discovery/diagnostics surface), discovery/diagnostics
  integration
- Contracts/specs affected: ADR-0184, SPEC-0047, ADR-0182, SPEC-0039,
  ADR-0181/SPEC-0038 (topology references)
- Provenance: ADR-0184, SPEC-0047, p15-e2d-gap-register.md
- Operations/actions/resources: BuildingBlock enroll/state/removal
  operations under IAM scope; drain with blockers
- Database assumptions: durable BuildingBlock identity and state; SQLite +
  PostgreSQL parity
- Cross-service dependencies/compensation: capacity DERIVED from Placement
  (P15.3); topology membership by reference (P15.1); profile membership by
  reference (P15.4); drain blockers reported honestly, never blind evacuation
- Evidence tier: domain/store tests + process-level lifecycle tests
- Tests first: lifecycle state machine, honest drain blockers, derived
  capacity, reference integrity, cross-tenant concealment
- Known uncertainties: exact diagnostics payload shape; follow SPEC-0047 and
  native diagnostics v1 conventions
- Explicit non-goals: workload mobility/evacuation (P16), zero-downtime
  maintenance claims, scale ceilings
```

This phase is authorized under Accepted ADR-0184/SPEC-0047 once its listed
dependency phases are merged and its review passes per REVIEW_AND_MERGE.md;
the PR must record the authorization line with its actual state.

## Objective

Implement the first-class BuildingBlock lifecycle: enrollment, identity,
capability publication, failure-domain membership, Ready/Unavailable/Draining
states, removal/replacement, and honest drain blockers (resident workloads,
local storage, attachments — never blind evacuation). Capacity projection is
DERIVED from Placement, not stored separately. Give operators and Araf
visibility through discovery/diagnostics. Link by reference to canonical
ResourceProviders, execution identities (mTLS agent identities), topology
(P15.1), and profile membership (P15.4).

## Authoritative dependencies

Read before editing: ADR-0184 (Accepted), SPEC-0047 (Accepted),
`docs/architecture/p15-e2d-gap-register.md`,
`docs/architecture/p15-0-post-araf-current-state-audit.md`, ADR-0182,
SPEC-0039, ADR-0181, SPEC-0038. Mandatory per AGENTS.md: `README.md`,
`docs/PROJECT_CHARTER.md`, `docs/CLEAN_IMPLEMENTATION.md`,
`docs/ARCHITECTURE.md`, `docs/NORMATIVE_SOURCES.md`, `docs/TEST_STRATEGY.md`,
ADR-0165/0166/0167/0160/0162/0163, SPEC-0020/0021/0022/0024/0025,
`compatibility/product-profiles.yaml`, `contracts/execution-boundaries.md`,
`contracts/core-architecture-boundaries.toml`.

## Architecture boundaries and guardrails

Guardrails are normative:

- NO second scheduler: drain and placement decisions route through P15.3
  placement authority.
- NO second capacity database: capacity shown for a BuildingBlock is derived
  from Placement state, never a parallel inventory.
- NO second topology authority: failure-domain membership references P15.1
  canonical topology by ID.
- NO duplicate agent inventory: enrollment binds to existing authenticated
  execution identities; do not re-register agents under a parallel registry.
- Cross-tenant concealment is preserved on every discovery/diagnostics surface.

## In scope

- BuildingBlock lifecycle state machine: enrollment, identity, capability
  publication, failure-domain membership, Ready/Unavailable/Draining,
  removal/replacement.
- Honest drain blockers: resident workloads, local storage, and attachments
  block drain with visible reasons; never blind evacuation.
- Capacity projection derived from Placement (P15.3).
- Operator/Araf visibility via discovery/diagnostics.
- Reference links: canonical ResourceProviders, mTLS execution identities,
  P15.1 topology, P15.4 profile membership.

## Out of scope

- Workload mobility/evacuation (P16).
- Zero-downtime maintenance claims.
- Scale ceilings (P18 owns measurements).

## Authority model

O3K Cloud Kernel owns BuildingBlock identity and lifecycle under
`o3k-implemented` authority. Execution agents own bounded mutation/observation
only and authenticate with mTLS agent identities; they never authorize callers
or invent public O3K IDs.

## Security requirements

Enrollment and lifecycle mutations are protected operations under the shared
typed `AuthContext`. Only authenticated execution identities may enroll.
Discovery/diagnostics must preserve cross-tenant concealment: blockers and
capacity are visible within authorized operator scope only, never as a
cross-tenant existence oracle. Audit lifecycle transitions and drain decisions.
Provider/agent-facing paths MUST NOT create or rebind Building Block topology
bindings: binding writes are operator-scope only and must fail closed for
provider/agent principals.

## Database implications

BuildingBlock identity, state, membership references, and lifecycle operations
are durable with SQLite + PostgreSQL parity. Capacity is derived at read time
from Placement and is not independently persisted.

## OpenStack compatibility implications

None directly: no new OpenStack-facing surface. Diagnostics must not leak
internal building-block topology through compatibility projections beyond
existing profile records.

## Araf implications

Araf gains building-block visibility through discovery/diagnostics and uses it
for operator UX. Araf does not own or mutate building-block state.

## Failure and restart behavior

BuildingBlock state survives control-plane restart. Agents disconnecting
move blocks to Unavailable honestly; restart must not fabricate Ready state.
An interrupted drain resumes from durable operation phase and re-evaluates
blockers, never skipping them.

## Idempotency and concurrency requirements

Enrollment uses deterministic operation identity and authenticated identity
binding; retried enrollment converges without duplicate inventory.
Concurrent lifecycle transitions serialize through validated state transitions;
stale-generation transitions are rejected. Drain re-checks blockers at each
step (observe-before-retry).

## Required tests

- Lifecycle state machine with invalid-transition rejection.
- Honest drain blockers: resident workloads, local storage, attachments.
- Capacity projection derived from Placement (no second capacity store).
- Reference integrity: providers, mTLS identities, topology, profile
  membership by ID.
- Cross-tenant concealment on discovery/diagnostics.
- Enrollment idempotency and authenticated-identity enforcement.
- Restart survival of lifecycle state.
- SQLite + PostgreSQL parity.

## Required real-process evidence

Process-level test with real `o3kd` + real auth showing enrollment, state
transitions, drain blockers, and derived capacity visibility.

## Claim limitations

Do not claim workload evacuation (P16), zero-downtime maintenance, or scale
ceilings. Do not claim capacity independent of Placement. Do not claim the
block is tenant-visible beyond authorized operator scope.

## Validation ladder

Run package-level `cargo check -p o3k-kernel -p o3k-native-api`, then tests,
then clippy on the same packages. Before completion run
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace --all-features`, plus
`scripts/check-architecture-boundaries.py`,
`scripts/check-maintainability-guards.py`, and
`scripts/validate-kernel-actions.py`.

## Acceptance criteria

Issue #935 acceptance criteria met; guardrails (no second scheduler, capacity
database, topology authority, or agent inventory) demonstrated by architecture
checks and tests; drain blockers honest; cross-tenant concealment preserved.

## Completion

```text
BuildingBlock lifecycle state machine: PASS
Honest drain blockers (no blind evacuation): PASS
Capacity derived from Placement only: PASS
References by ID to P15.1/P15.3/P15.4: PASS
Cross-tenant concealment preserved: PASS
No duplicate agent inventory: CONFIRMED
P15.5 implementation authorized: YES under Accepted ADR-0184, contingent on listed dependencies merged (verify per Execution prerequisites)
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
