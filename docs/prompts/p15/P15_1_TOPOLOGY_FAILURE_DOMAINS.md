# P15.1 — Topology and Failure Domains

Issue: #931. Fetch protected `origin/main`; verify the baseline is
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928) or a later protected merge;
do not start from stale branches or worktrees. Verify P15.0 (#930) is merged
and read the umbrella issue #929 before writing code.

Pre-flight: record the Required agent plan fields before any code change.

```text
Required agent plan:
- Issue: #931
- Deployment/evidence profile: TestLab/portable + production-oriented
  (SQLite now, PostgreSQL parity); small edge shapes represented in tests
- Canonical service/domain: Cloud Kernel topology (LocationRegistry)
- OpenStack compatibility adapter: Keystone region/AZ projection only
- Authority mode: o3k-implemented (canonical O3K topology)
- Files expected to change: crates/o3k-kernel/src/location.rs,
  crates/o3k-kernel/src/manifest.rs (seed_core), crates/o3k-identity/src/lib.rs
  (catalog region/AZ projection), discovery surface in crates/o3k-native-api
- Contracts/specs affected: ADR-0181, SPEC-0038, ADR-0184, SPEC-0047
- Provenance: ADR-0181, SPEC-0038, ADR-0184, SPEC-0047, p15-e2d-gap-register.md
- Operations/actions/resources: topology/failure-domain CRUD under IAM scope;
  discovery read actions
- Database assumptions: new store area must have SQLite + PostgreSQL parity
- Cross-service dependencies/compensation: none (leaf authority); consumers are
  projections/adapters only
- Evidence tier: domain/store tests + process-level discovery tests
- Tests first: hierarchy validation, acyclicity, projection derivation,
  seed_core regression
- Known uncertainties: exact failure-domain kind catalog beyond edge/datacenter
  shapes; resolved by ADR-0184/SPEC-0047 text, not invention
- Explicit non-goals: scheduling, multi-region operation, cells
```

P15.1 is authorized by the human architecture approval recorded on PR #938
(ADR-0184/SPEC-0047 Accepted, 2026-09-12), effective once P15.0 (#930) is
merged; the PR must record `P15.1 implementation authorized: YES`.

## Objective

Complete the canonical topology beyond Region/AZ. Extend `LocationRegistry`
(`crates/o3k-kernel/src/location.rs`, ADR-0181/SPEC-0038) with a generic
failure-domain model under Region/AZ: typed kinds, a validated acyclic
hierarchy, provider-neutral semantics, and shapes capable of representing both
edge and datacenter deployments. Bind failure domains to ResourceProviders,
hosts, fabric, and storage domains by reference. Fix `seed_core` empty regions
(`crates/o3k-kernel/src/manifest.rs:1612`). Derive the Keystone region/AZ
projection from canonical topology, replacing the hard-coded `RegionOne` path
(`crates/o3k-identity/src/lib.rs:1538-1553`). Expose the topology on the native
discovery surface; Araf consumes it.

## Authoritative dependencies

Read before editing: ADR-0184 (Accepted), SPEC-0047 (Accepted),
`docs/architecture/p15-e2d-gap-register.md`,
`docs/architecture/p15-0-post-araf-current-state-audit.md`, ADR-0181,
SPEC-0038, ADR-0182, SPEC-0039. Mandatory per AGENTS.md: `README.md`,
`docs/PROJECT_CHARTER.md`, `docs/CLEAN_IMPLEMENTATION.md`,
`docs/ARCHITECTURE.md`, `docs/NORMATIVE_SOURCES.md`, `docs/TEST_STRATEGY.md`,
ADR-0165/0166/0167/0160/0162/0163, SPEC-0020/0021/0022/0024/0025,
`compatibility/product-profiles.yaml`, `contracts/execution-boundaries.md`,
`contracts/core-architecture-boundaries.toml`.

## Architecture boundaries and guardrails

Guardrails are normative:

- NO second topology authority. The extended `LocationRegistry` is the single
  canonical topology store.
- NO provider-specific topology in public Cloud Kernel semantics. Libvirt,
  switch fabric, Ceph, or vendor rack constructs stay at provider edges.
- NO hard-coded rack/row model unless required by an accepted contract.
- NO scheduler inside topology. Topology stores and validates structure and
  references; it never filters or selects candidates.
- NO Araf-owned topology. Araf consumes topology through discovery; it does not
  keep its own copy.

## In scope

- Typed failure-domain kinds with a validated, acyclic parent/child hierarchy
  under Region/AZ.
- Reference bindings from failure domains to ResourceProviders, hosts, fabric
  domains, and storage domains; bindings are references, never embedded copies.
- Fix `seed_core` so regions are no longer empty
  (`crates/o3k-kernel/src/manifest.rs:1612`).
- Keystone region/AZ projection derived from canonical topology, replacing
  hard-coded `RegionOne` (`crates/o3k-identity/src/lib.rs:1538-1553`).
- Native discovery surface for topology; Araf-readable.

## Out of scope

- Scheduling or filtering behavior of any kind (P15.3).
- Multi-region operation.
- Cells (in any form).
- Provider-specific topology models in kernel semantics.

## Authority model

O3K Cloud Kernel owns canonical topology under `o3k-implemented` authority.
OpenStack region/AZ is a derived projection from canonical state, never a
second source. Execution providers and fabric/storage subsystems reference
failure domains; they do not define them.

## Security requirements

All topology mutations are protected operations expressed as
Principal × Action × Resource × Context with the shared typed `AuthContext`.
Topology reads on the discovery surface must not reveal cross-tenant
existence. Audit topology mutations with the canonical audit identity. Never
log secrets or provider payloads. Provider/agent-facing paths MUST NOT create
or rebind failure-domain membership: failure-domain membership writes are
operator-scope only and must fail closed for provider/agent principals.

## Database implications

Any new topology store area must have SQLite (supported minimal default) and
PostgreSQL (production target) parity: same schema semantics, same migrations,
same invariants. Persist hierarchy and bindings durably before they are
advertised on discovery.

## OpenStack compatibility implications

The Keystone region/AZ projection changes derivation: region and AZ values come
from canonical topology instead of hard-coded `RegionOne`. This is a
compatibility-adapter change; every affected Keystone response shape keeps its
profile record and tests updated. Do not advertise broader compatibility than
the profile records show.

## Araf implications

Araf consumes topology through the discovery surface. Araf must not own,
cache-authoritatively, or mutate topology. Record the discovery contract Araf
relies on in SPEC-0047 traceability.

## Failure and restart behavior

Topology and bindings survive control-plane restart by construction (durable
store). A restart mid-mutation must not leave a partially validated hierarchy:
persist intent/phase before side effects, validate on write, and reject cycles
atomically.

## Idempotency and concurrency requirements

Topology mutations use deterministic operation identity; retried identical
mutations converge. Concurrent hierarchy writes must serialize so acyclicity
and uniqueness invariants always hold. Treat timeout as unknown outcome:
observe before retrying.

## Required tests

- Hierarchy validation: typed kinds, acyclicity rejection, parent/child rules.
- Reference bindings to providers/hosts/fabric/storage by reference.
- `seed_core` regression: regions no longer empty.
- Keystone projection derivation from canonical topology (fail-before shows
  hard-coded `RegionOne`).
- Discovery surface shape and Araf-consumable read contract.
- Malformed/cross-tenant discovery reads.
- SQLite and PostgreSQL parity for the new store area.

## Required real-process evidence

Process-level test with real `o3kd` + real auth showing topology CRUD,
restart survival, and the derived Keystone projection in the compatibility
adapter. No fake provider at this boundary.

## Claim limitations

Do not claim multi-region support, cells, scheduling awareness, or scale
ceilings. Do not claim production topology HA.

## Validation ladder

Run package-level `cargo check -p o3k-kernel -p o3k-identity -p o3k-native-api`,
then `cargo test -p o3k-kernel -p o3k-identity -p o3k-native-api --all-features`,
then `cargo clippy` on the same packages with `--all-targets --all-features --
-D warnings`. Before completion run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace --all-features`, plus
`scripts/check-architecture-boundaries.py`,
`scripts/check-maintainability-guards.py`, and
`scripts/validate-kernel-actions.py`.

## Acceptance criteria

Issue #931 acceptance criteria met; guardrails honored; projections derived, not
duplicated; SQLite/PostgreSQL parity demonstrated; Araf discovery contract
recorded.

## Completion

```text
Failure-domain hierarchy validated: PASS
Reference bindings by reference: PASS
seed_core empty regions fixed: PASS
Keystone region/AZ derived from canonical topology: PASS
No second topology authority: CONFIRMED
P15.1 implementation authorized: YES (human approval recorded on PR #938; requires P15.0 #930 merged)
Required CI/governance: PASS
Exact HEAD reviewed: YES
```
