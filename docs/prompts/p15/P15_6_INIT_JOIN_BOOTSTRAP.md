# P15.6 — Init, Join, and Bootstrap

Issue: #936. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930), P15.4 (#934,
CloudProfile), and P15.5 (#935, BuildingBlock lifecycle) are merged; P15.6
reuses their machinery. Read the umbrella issue #929 before writing code.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #936
- Deployment/evidence profile: production-oriented target with TestLab
  coverage; real execution path evidence required (no fake provider)
- Canonical service/domain: Cloud Kernel bootstrap (o3k init / o3k join)
- OpenStack compatibility adapter: none
- Authority mode: o3k-implemented; authenticated enrollment only
- Files expected to change: bins/o3k (CLI init/join), crates/o3k-kernel
  bootstrap domain, certificate issuance integration, client/Araf config
  output
- Contracts/specs affected: ADR-0184, SPEC-0047
- Provenance: ADR-0184, SPEC-0047, p15-e2d-gap-register.md
- Operations/actions/resources: init, join/enroll, certificate issuance,
  provider/agent registration, placement publication, profile reconciliation,
  readiness operations under IAM scope
- Database assumptions: enrolled state durable and surviving control-plane
  restart; SQLite + PostgreSQL parity
- Cross-service dependencies/compensation: reuses P15.4 profile machinery and
  P15.5 enrollment/lifecycle; no parallel bootstrap truth; compensation per
  SPEC-0021
- Evidence tier: process tests + real-process evidence on the real execution
  path
- Tests first: interrupted-join resumption, idempotent enrollment, restart
  survival, authenticated-enrollment rejection
- Known uncertainties: certificate issuance plumbing details; reuse existing
  PKI machinery, do not invent a second CA
- Explicit non-goals: absorbing pre-provisioned switch/Ceph/hardware/DNS/BGP/
  external-DB work into O3K bootstrap; presenting the TestLab installer as
  this flow
```

This phase is authorized under Accepted ADR-0184/SPEC-0047 once its listed
dependency phases are merged and its review passes per REVIEW_AND_MERGE.md;
the PR must record the authorization line with its actual state.

## Objective

Implement the production-oriented `o3k init` + CloudProfile selection +
`o3k join` flow: authenticated enrollment, capability discovery,
failure-domain assignment, certificate issuance, provider/agent registration,
Placement publication, profile reconciliation, service readiness, and
client/Araf configuration. Reuse P15.4/P15.5 machinery — no parallel bootstrap
truth. An interrupted join is resumable/idempotent, and enrolled state survives
control-plane restart.

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

- Authenticated enrollment only: unauthenticated join requests are rejected.
- No parallel bootstrap truth: init/join reuses CloudProfile (P15.4) and
  BuildingBlock enrollment/lifecycle (P15.5) machinery and identities.
- Benchmark evidence is required for any timing claim.
- Never include pre-provisioned switch/Ceph/hardware/DNS/BGP/external-DB work
  in O3K bootstrap timing unless O3K actually performs it.
- The TestLab installer must not be presented as this flow.

## In scope

- `o3k init`: initialize the Cloud Kernel and select a CloudProfile.
- `o3k join`: authenticated enrollment, capability discovery, failure-domain
  assignment, certificate issuance, provider/agent registration, Placement
  publication, profile reconciliation, service readiness, and client/Araf
  configuration.
- Idempotent, resumable interrupted join; enrolled state survives
  control-plane restart.

## Out of scope

- Performing external provisioning (switch, Ceph, hardware, DNS, BGP,
  external database) — O3K consumes those as prerequisites, it does not own
  them.
- Rebranding or extending the TestLab disposable installer as the production
  flow.
- Any timing claim without benchmark evidence.

## Authority model

O3K Cloud Kernel owns bootstrap authority (`o3k-implemented`). Joining nodes
prove authenticated execution identity (mTLS enrollment) and receive bounded
certificates; the control plane remains the source of canonical identity,
topology binding, and placement publication.

## Security requirements

Enrollment is authenticated only; bootstrap tokens/secrets are single-purpose,
short-lived where applicable, never logged, and referenced by handle in stored
configuration. Certificate issuance reuses existing PKI machinery with
auditable issuance records. Client/Araf configuration output contains no
plaintext secrets.

## Database implications

Enrolled node state, issued identity bindings, and bootstrap operation phases
are durable with SQLite + PostgreSQL parity. Enrolled state must survive
control-plane restart without re-enrollment.

## OpenStack compatibility implications

None directly: no compatibility surface changes. Client configuration output
that feeds compatibility projections must not alter advertised profiles.

## Araf implications

Araf configuration produced by init/join points at the same discovery truth;
Araf consumes bootstrap results, it does not drive enrollment.

## Failure and restart behavior

Interrupted join resumes idempotently from durable phase state. Control-plane
restart after enrollment preserves enrolled nodes, issued identities, and
placement publication. Node-side retry uses deterministic enrollment identity;
observe-before-retry on unknown outcome.

## Idempotency and concurrency requirements

Enrollment and each bootstrap step use deterministic operation identity;
retries converge to the same canonical state. Concurrent joins serialize
identity issuance so no duplicate agent inventory or certificate identity
appears. Re-join of an existing node reconciles instead of duplicating.

## Required tests

- Authenticated enrollment only (unauthenticated and replayed enrollment
  rejected).
- Interrupted join resumption and idempotent retry.
- Enrolled state survives control-plane restart.
- Capability discovery, failure-domain assignment, certificate issuance,
  provider/agent registration, placement publication, profile reconciliation,
  readiness, and client/Araf configuration each verified.
- No-parallel-truth checks: bootstrap state lives in P15.4/P15.5 machinery.
- SQLite + PostgreSQL parity.

## Required real-process evidence

Real execution path evidence (not a fake provider): real `o3kd` +
production composition router + real auth + at least one real execution
boundary completing init → join → enrollment → placement publication.

## Claim limitations

Do not claim timing without benchmark evidence. Do not claim O3K performs
switch/Ceph/hardware/DNS/BGP/external-DB provisioning. Do not present the
TestLab installer as this flow. Do not claim scale ceilings.

## Validation ladder

Run package-level `cargo check -p o3k -p o3k-kernel`, then tests, then clippy
on the same packages. Before completion run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace --all-features`, plus
`scripts/check-architecture-boundaries.py`,
`scripts/check-maintainability-guards.py`, and
`scripts/validate-kernel-actions.py`.

## Acceptance criteria

Issue #936 acceptance criteria met; init/join complete on the real execution
path; interrupted join resumable; enrolled state restart-safe; guardrails
honored.

## Completion

```text
o3k init + profile selection working: PASS
o3k join authenticated enrollment: PASS
Join: capability discovery, FD assignment, cert issuance, registration,
  placement publication, profile reconciliation, readiness: PASS
Client/Araf configuration emitted: PASS
Interrupted join resumable/idempotent: PASS
Enrolled state survives control-plane restart: PASS
Timing claims backed by benchmark evidence: YES/NA
P15.6 implementation authorized: YES under Accepted ADR-0184, contingent on listed dependencies merged (verify per Execution prerequisites)
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
