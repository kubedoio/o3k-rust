# P15.4 — CloudProfile Composition

Issue: #934. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930) is merged and
read the umbrella issue #929 before writing code. P15.4 builds on the
converged registry authority from P15.2 (#932); verify it is merged.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #934
- Deployment/evidence profile: all P15 profiles; small edge profiles must
  remain simple (single implicit default profile acceptable)
- Canonical service/domain: Cloud Kernel composition (CloudProfile)
- OpenStack compatibility adapter: none (CloudProfile is native kernel domain)
- Authority mode: o3k-implemented; external-hosted selections retain their own
  lifecycle (SPEC-0023 boundary)
- Files expected to change: crates/o3k-kernel (new CloudProfile domain),
  crates/o3k-native-api (surface), operations/audit integration points
- Contracts/specs affected: ADR-0184, SPEC-0047, SPEC-0023, SPEC-0021
- Provenance: ADR-0184, SPEC-0047, p15-e2d-gap-register.md
- Operations/actions/resources: CloudProfile CRUD, validation, reconciliation
  operations under IAM scope
- Database assumptions: durable CloudProfile objects + desired-vs-observed
  state; SQLite + PostgreSQL parity
- Cross-service dependencies/compensation: drift surfaced via canonical
  Operations; compensation per SPEC-0021 where reconciliation acts
- Evidence tier: domain/store tests + process-level reconciliation tests
- Tests first: profile validation, ownership/versioning, drift surfacing,
  semantic separation (SHALL provide vs Ready vs consumable)
- Known uncertainties: upgrade-ordering expressiveness limits; keep the v1
  contract minimal per SPEC-0047
- Explicit non-goals: Building Block lifecycle (P15.5), bootstrap automation
  (P15.6), package management
```

This phase is authorized under Accepted ADR-0184/SPEC-0047 once its listed
dependency phases are merged and its review passes per REVIEW_AND_MERGE.md;
the PR must record the authorization line with its actual state.

## Objective

Define and implement the declarative CloudProfile desired-composition contract:
selected services, ownership mode, versions, dependencies, required
capabilities, placement/locality constraints, configuration references, and
upgrade ordering. CloudProfile is a first-class canonical object with ownership,
versioning, validation, and IAM scope. Desired-vs-observed reconciliation is
surfaced through canonical Operations (drift is visible, never a silent
auto-install). Enforce the semantic separation: CloudProfile = SHALL provide;
registry = exists/Ready; catalog = consumable projection. External-hosted
services retain their own lifecycle (SPEC-0023 boundary): a profile
selects/expects them, never absorbs them. Small edge profiles remain simple —
a single implicit default profile is acceptable.

## Authoritative dependencies

Read before editing: ADR-0184 (Accepted), SPEC-0047 (Accepted),
`docs/architecture/p15-e2d-gap-register.md`,
`docs/architecture/p15-0-post-araf-current-state-audit.md`, ADR-0182,
SPEC-0039, SPEC-0023, SPEC-0021. Mandatory per AGENTS.md: `README.md`,
`docs/PROJECT_CHARTER.md`, `docs/CLEAN_IMPLEMENTATION.md`,
`docs/ARCHITECTURE.md`, `docs/NORMATIVE_SOURCES.md`, `docs/TEST_STRATEGY.md`,
ADR-0165/0166/0167/0160/0162/0163, SPEC-0020/0022/0024/0025,
`compatibility/product-profiles.yaml`, `contracts/execution-boundaries.md`,
`contracts/core-architecture-boundaries.toml`.

## Architecture boundaries and guardrails

Guardrails are normative:

- Singular composition semantics: exactly one meaning for "SHALL provide".
  Never collapse CloudProfile desire, registry existence/Ready, and catalog
  consumability into one flag.
- CloudProfile holds no tenant resource state. It is control-plane desired
  composition, not a tenant object.
- External-hosted services keep their own DB, lifecycle, migrations, upgrades,
  and operations (SPEC-0023). The profile references/selects; it does not
  absorb, import, or mutate external lifecycle state.
- Drift is surfaced via canonical Operations with durable identity; never
  silently auto-install or auto-remove services to match a profile.

## In scope

- CloudProfile schema: selected services, ownership mode, versions,
  dependencies, required capabilities, placement/locality constraints,
  configuration references, upgrade ordering.
- First-class canonical object semantics: ownership, versioning, validation,
  IAM scope.
- Desired-vs-observed reconciliation surfaced through canonical Operations.
- The three-semantics separation (SHALL provide / exists-Ready / consumable)
  enforced in code and tests.
- Small-edge simplicity: a single implicit default profile remains valid.

## Out of scope

- Building Block lifecycle (P15.5).
- Bootstrap automation (`o3k init`/`o3k join`, P15.6).
- Package management of any kind.

## Authority model

O3K Cloud Kernel owns CloudProfile under `o3k-implemented` authority.
External-hosted service selections are `external-hosted` authority: the profile
records expectation and dependency, while the external system remains the
source of truth for its own state.

## Security requirements

CloudProfile CRUD and reconciliation operations are protected operations under
the shared typed `AuthContext`. Configuration references must never embed
secrets inline; reference secret stores by handle. Profile reads must not leak
cross-tenant or internal-only composition data beyond scope. Audit every
profile mutation and reconciliation operation with canonical audit identity.

## Database implications

CloudProfile objects, versions, and desired-vs-observed reconciliation state
are durable with SQLite + PostgreSQL parity. Persist operation intent/phase
before any side effect so restart recovers reconciliation honestly.

## OpenStack compatibility implications

None directly: CloudProfile is a native kernel domain with no Keystone-shaped
surface. Do not add a compatibility endpoint for it. Catalog projections stay
limited to consumable services per existing profile records.

## Araf implications

Araf can read composition state (what the cloud SHALL provide) through the
native surface and correlate it with drift operations. Araf does not own or
mutate CloudProfile.

## Failure and restart behavior

Profiles and reconciliation state survive restart. An interrupted
reconciliation resumes from durable operation phase; it never restarts as a
duplicate operation and never hides drift.

## Idempotency and concurrency requirements

Profile mutations and reconciliation operations use deterministic operation
identity; retries converge. Concurrent edits serialize per profile version;
stale-version writes are rejected with a typed conflict, not silently merged.

## Required tests

- Schema validation: versions, dependencies, capabilities, constraints,
  upgrade ordering rules.
- Ownership/versioning/IAM-scope enforcement, including cross-scope denial.
- Desired-vs-observed drift surfaced as canonical Operations (no silent
  auto-install).
- Semantic-separation tests: SHALL provide vs exists/Ready vs consumable.
- External-hosted selection: expectation recorded, external lifecycle untouched.
- No tenant resource state in CloudProfile (negative test).
- Default implicit profile for small edge.
- SQLite + PostgreSQL parity.

## Required real-process evidence

Process-level test with real `o3kd` + real auth showing profile lifecycle,
drift surfacing, and restart survival of profiles and operations.

## Claim limitations

Do not claim automated installation, building-block lifecycle, package
management, or that a selected service is running merely because a profile
selects it. Do not claim edge/production equivalence beyond tested profiles.

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

Issue #934 acceptance criteria met; three-semantics separation enforced in
code and tests; external-hosted boundary honored; drift visible via canonical
Operations; guardrails honored.

## Completion

```text
CloudProfile contract implemented + validated: PASS
Ownership/versioning/IAM scope enforced: PASS
Drift surfaced via canonical Operations: PASS
SHALL-provide vs Ready vs consumable separated: PASS
External-hosted lifecycle untouched: PASS
P15.4 implementation authorized: YES under Accepted ADR-0184, contingent on listed dependencies merged (verify per Execution prerequisites)
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
