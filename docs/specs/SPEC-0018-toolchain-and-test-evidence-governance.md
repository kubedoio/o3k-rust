# SPEC-0018 — Toolchain and test-evidence governance

Status: Proposed. This specification becomes normative only after human
acceptance through [ADR-0154](../adr/ADR-0154-engineering-governance-lifecycle.md).

## Scope and non-claims

This specification defines how reproducibility and evidence are recorded. It
does not claim that every proposed CI lane, nextest profile, coverage report,
fuzz run, Miri run, protected-host run, or release artifact currently exists.
An absent artifact is `missing`; a skipped or unavailable run is not `passed`.
The compatibility manifest and its tests are deliberately outside this
specification's change scope.

## Toolchain policy

- `rust-toolchain.toml` is the single source of the selected Rust channel and
  required components. Cargo metadata, CI actions, release scripts, and docs
  consume it or verify it; they must not silently select another channel.
- The exact target is the accepted value in
  [ADR-0153](../adr/ADR-0153-static-rust-and-openstack-release-policy.md). A
  toolchain result records the complete `rustc --version`, Cargo version,
  target triple, profile, lockfile state, and command.
- Locked checks are mandatory for reproducibility. A successful non-locked
  build does not prove a release or dependency-reproducibility requirement.
- Toolchain upgrades are separate reviewed changes. The upgrade record names
  the old/new versions, release notes or official source, MSRV impact,
  compiler diagnostics requiring edits, lockfile changes, and rollback point.
- Release evidence must be built from a clean checkout and must bind artifacts
  to a commit, toolchain, target, and dependency lockfile. A local version
  string or a passing unit test is not release evidence.

## Evidence vocabulary

Evidence records use exactly one state per claim:

`missing`, `partial`, `implemented`, `portable-contract-verified`,
`CLI-verified`, `protected-runner-verified`, `measured`, `human-approved`, or
`released`.

States are ordered only when the claim requires it; `implemented` never implies
`portable-contract-verified`, and `protected-runner-verified` never implies
`measured`. A record must include:

- stable subject/requirement ID and claim text;
- repository commit and, for external software, pinned version/revision;
- environment identity and target profile;
- exact command or workflow run;
- start/end time and result;
- artifact path plus digest, when an artifact exists;
- redaction declaration and retained logs;
- reviewer identity and approval timestamp when the state is
  `human-approved` or `released`.

Evidence must be source-bound and reproducible. Agents and CI may produce
results, but only the required human role may provide human approval. No
record may promote `skipped`, `unavailable`, `failed`, or stale output to a
passing state.

## Evidence tiers

The project distinguishes these gates. A tier is a required policy boundary,
not a statement that the repository currently runs every item in that tier.

### Fast pull-request tier

Workflow validation, formatting, locked compile/check, warnings-denied Clippy,
the canonical unit/integration test profile, doctests, contract/schema checks,
generated-file drift, and dependency policy checks. Privileged host tests are
excluded.

### Deep portable tier

All-feature process and store tests, contract/differential tests where the
reference is available, migration conformance, coverage artifacts, bounded
fuzz smoke, targeted Miri where supported, SBOM, and provenance generation.
Retries must not turn a flaky failure into a green result; a retry is recorded
as a failure requiring triage.

### Protected real-host tier

Source-bound KVM/libvirt lifecycle, failure/recovery, cleanup, leak, and
foreign-state evidence. The record identifies the protected runner and
redacts credentials and unrelated host state. Portable tests cannot satisfy a
protected-host requirement.

### Release tier

Clean locked build, exact toolchain, checksums/signatures, SBOM/provenance,
supported-profile results, and required human approvals. A release gate is
closed when any required artifact is missing or failed.

## Test-profile rules

The intended canonical test runner is cargo-nextest `0.9.131`, configured in
`.config/nextest.toml`. The checked-in PR and deep profiles specify
deterministic timeouts, JUnit output, zero retries, and failing flaky results;
doctests run separately through cargo test. Property, store-conformance,
protocol, provider, restart/recovery, concurrency, fuzz, native-boundary, and
real-host cases are selected by risk; a global coverage percentage is not a
substitute for a missing behavioral category.

Fuzz and model-checking smoke lanes may be bounded and scheduled. A crash must
produce a minimized reproducer and regression test before the associated
claim can become verified. Any future unsafe or FFI boundary requires the
dedicated safety/security review specified by the repository rules.

## Required acceptance checks

The governance implementation is accepted only when reviewers can verify:

- one toolchain source of truth and a failing drift check for duplicate or
  conflicting declarations;
- a locked clean build records exact toolchain and lockfile metadata;
- every release-required requirement maps to a contract, executable test, and
  evidence state, or records `not-applicable` with rationale;
- evidence records reject missing fields, unknown states, stale commits, and
  promotion of failed/skipped/unavailable results;
- each CI tier has an explicit workflow/job boundary and fails closed on
  missing required artifacts;
- JUnit, compatibility, coverage, SBOM, and provenance artifacts are retained
  without secrets where the applicable tier requires them;
- protected-host and human-approval states cannot be produced by portable CI
  alone;
- toolchain upgrade records and rollback points are reviewable.

Until those checks and artifacts exist, this specification must report the
affected requirements as `missing` or `partial`, not as verified.

## Sources

- [ADR-0153 static Rust/OpenStack target](../adr/ADR-0153-static-rust-and-openstack-release-policy.md)
- [Repository test strategy](../TEST_STRATEGY.md)
- [Repository agent contract](../../AGENTS.md)
- [Issue #332](https://github.com/kubedoio/o3k-rust/issues/332)
