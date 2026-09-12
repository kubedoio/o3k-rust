# P15.2 — Service Registry Convergence

Issue: #932. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930) is merged
and read the umbrella issue #929 before writing code. P15.2 may proceed before,
after, or in parallel with P15.1 (#931); it does not depend on P15.1.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #932
- Deployment/evidence profile: TestLab/portable + production-oriented;
  convergence must hold for SQLite and PostgreSQL deployments
- Canonical service/domain: Cloud Kernel service/manifest authority
- OpenStack compatibility adapter: Keystone catalog projection only
- Authority mode: o3k-implemented (canonical registry authority)
- Files expected to change: crates/o3k-kernel/src/registry.rs (:207-546),
  crates/o3k-kernel/src/lib.rs (re-exports KernelRegistry),
  crates/o3k-kernel/src/manifest.rs (ManifestRegistry; also rewrite the stale
  "not the runtime authority until the P12 migration is proven" comment at
  ~lines 1201-1203 as part of this phase),
  bins/o3kd/src/composition/mod.rs (constructs/seeds ManifestRegistry, wires
  readiness), crates/o3k-identity/src/lib.rs (catalog projection),
  crates/o3k-native-api/src/lib.rs (discovery surface),
  scripts/validate-kernel-actions.py
- Contracts/specs affected: ADR-0184, SPEC-0047, ADR-0182, SPEC-0039
- Provenance: ADR-0184, SPEC-0047, p15-0-post-araf-current-state-audit.md,
  p15-e2d-gap-register.md
- Operations/actions/resources: service/manifest registration and readiness
  under IAM scope; discovery read actions
- Database assumptions: ManifestRegistry dynamic state durable; static
  KernelRegistry deprecated with a safe migration path
- Cross-service dependencies/compensation: identity catalog projection and
  native discovery become read-only projections of the converged authority
- Evidence tier: domain/store tests + process-level discovery tests
- Tests first: singular-authority invariants, catalog projection equivalence,
  readiness gating regression
- Consumer inventory (pre-flight deliverable): enumerate every consumer of
  the current dual registry — Keystone/catalog projection
  (crates/o3k-identity), native discovery (/o3k/v1/services), o3kd readiness
  wiring (composition root), scripts/validate-kernel-actions.py, kernel
  re-exports, and the existing regression tests — and map each consumer to
  its post-convergence source before implementation starts;
- Known uncertainties: exact deprecation timeline for static registry
  consumers; keep both paths honest until SPEC-0047 retires one
- Explicit non-goals: CloudProfile desired composition, runtime
  install/deploy automation
```

This phase is authorized under Accepted ADR-0184/SPEC-0047 once its listed
dependency phases are merged and its review passes per REVIEW_AND_MERGE.md;
the PR must record the authorization line with its actual state.

## Objective

Converge `KernelRegistry` (static, `crates/o3k-kernel/src/registry.rs:207-546`,
still projects the Keystone catalog, `for_profile` ignores its profile argument
at `:544`) and `ManifestRegistry` (dynamic, `crates/o3k-kernel/src/manifest.rs`,
drives `/o3k/v1/services` + readiness) into one authoritative native
service/manifest state. Catalog and native discovery become projections of that
authority. Provide a safe migration/deprecation path for static registry
consumers, keep `scripts/validate-kernel-actions.py` semantics (extended to
cover the converged authority), and preserve the #928 advertised-implies-
executable readiness gating (regression test
`discovery_advertises_only_reachable_lifecycle_operations`).

## Authoritative dependencies

Read before editing: ADR-0184 (Accepted), SPEC-0047 (Accepted),
`docs/architecture/p15-e2d-gap-register.md`,
`docs/architecture/p15-0-post-araf-current-state-audit.md`, ADR-0182,
SPEC-0039. Mandatory per AGENTS.md: `README.md`,
`docs/PROJECT_CHARTER.md`, `docs/CLEAN_IMPLEMENTATION.md`,
`docs/ARCHITECTURE.md`, `docs/NORMATIVE_SOURCES.md`, `docs/TEST_STRATEGY.md`,
ADR-0165/0166/0167/0160/0162/0163, SPEC-0020/0021/0022/0024/0025,
`compatibility/product-profiles.yaml`, `contracts/execution-boundaries.md`,
`contracts/core-architecture-boundaries.toml`.

## Architecture boundaries and guardrails

Guardrails are normative:

- Singular authority: exactly one authoritative native service/manifest state
  after convergence. No split-brain between static and dynamic registries.
- Compatibility metadata is never merged back into kernel authority. Keystone
  catalog shapes stay at the projection edge.
- Readiness is not desired composition: a service being registered/Ready
  (P15.2 concern) is semantically distinct from a CloudProfile declaring a
  service SHALL be provided (P15.4). Do not collapse them.

## In scope

- One authoritative native service/manifest state merging static descriptors
  and dynamic manifests, with `for_profile` honoring its profile argument.
- Keystone catalog and `/o3k/v1/services` discovery as read-only projections.
- Safe migration/deprecation path for static registry consumers (no silent
  breakage; documented transition).
- Extend `scripts/validate-kernel-actions.py` semantics to cover the converged
  authority.
- Preserve the #928 readiness gating regression
  `discovery_advertises_only_reachable_lifecycle_operations`: advertised
  implies executable.

## Out of scope

- CloudProfile desired-composition contract (P15.4).
- Runtime install/deploy automation.
- Readiness reinterpretation as composition intent.

## Authority model

O3K Cloud Kernel owns service/manifest authority under `o3k-implemented`
authority. The Keystone catalog is a consumable projection; native discovery is
a native projection; neither is authoritative. External-hosted services remain
external (SPEC-0023 boundary) and are represented, not absorbed.

## Security requirements

Registration and readiness mutation are protected operations under the shared
typed `AuthContext` with canonical ownership. Discovery reads must not reveal
cross-tenant existence or disabled/internal surfaces beyond their profile.
Audit registration/removal with canonical audit identity. Never log tokens or
connection info.

## Database implications

Dynamic manifest state is durable (SQLite default, PostgreSQL parity). The
static registry remains code-delivered during deprecation; the converged
authority must reconstruct cleanly after restart from durable state plus
versioned static input.

## OpenStack compatibility implications

The Keystone catalog projection changes source (converged authority instead of
`KernelRegistry::standard` static projection). Catalog contents and shapes must
stay within existing profile records; update the profile records where
derivation changed and keep Tempest/client-facing behavior convergent.

## Araf implications

Araf consumes service discovery through the same projection. Araf must not
re-derive service authority. Record the discovery contract in SPEC-0047
traceability.

## Failure and restart behavior

Converged registry state survives control-plane restart. A restart mid-migration
must not leave consumers bound to a half-deprecated static path: stage
migration, persist phase, and make the active authority unambiguous at any
observable point.

## Idempotency and concurrency requirements

Registration uses deterministic operation identity; retried registration
converges. Concurrent registration/readiness updates serialize so projections
never observe partial authority state. Treat timeout as unknown outcome and
observe before retrying.

## Required tests

- Singular-authority invariant: no divergence between static and dynamic views.
- `for_profile` honors profile argument.
- Catalog projection equivalence with the converged authority.
- Readiness gating regression `discovery_advertises_only_reachable_lifecycle_operations`.
- `scripts/validate-kernel-actions.py` extended coverage over the converged
  authority.
- Migration/deprecation path: old consumers keep working during transition.
- Restart reconstruction of converged authority.

## Required real-process evidence

Process-level test with real `o3kd` + real auth showing converged authority,
projections, and readiness gating. No fake provider at this boundary.

## Claim limitations

Do not claim CloudProfile composition, automated install/deploy, or that
readiness equals desired composition. Do not claim the catalog is authoritative.

## Validation ladder

Run package-level `cargo check -p o3k-kernel -p o3k-identity -p o3k-native-api`,
then tests, then clippy on the same packages. Run
`python3 scripts/validate-kernel-actions.py` (or the repository convention) and
extend it in-repo. Before completion run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace --all-features`, plus
`scripts/check-architecture-boundaries.py`,
`scripts/check-maintainability-guards.py`.

## Acceptance criteria

Issue #932 acceptance criteria met; singular authority demonstrated; migration
path safe; readiness gating regression green; guardrails honored.

## Completion

```text
Converged native service/manifest authority: PASS
Catalog + discovery are projections only: PASS
for_profile honors profile argument: PASS
Migration/deprecation path safe: PASS
Readiness gating regression green: PASS
P15.2 implementation authorized: YES under Accepted ADR-0184, contingent on listed dependencies merged (verify per Execution prerequisites)
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
