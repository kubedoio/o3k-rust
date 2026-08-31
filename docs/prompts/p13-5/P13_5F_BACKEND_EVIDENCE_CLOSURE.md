# Prompt — P13.5F: Backend Parity, Full Convergence Evidence, and P13.5 Closure

## Task

Implement **P13.5F** for issue #750 after P13.5E is independently reviewed and
merged.

P13.5F is the P13.5 closure slice. It does not introduce new feature scope. It
runs the complete accepted convergence matrix against the supported persistence
profiles/evidence tiers, closes any narrowly evidenced parity defects, updates
compatibility/product status from executed evidence, and prepares issue #750 for
closure.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before editing:

1. Read `AGENTS.md`.
2. Fetch current protected `origin/main`.
3. Verify P13.5A–E are merged and reachable.
4. Create a fresh branch from current `origin/main`.
5. Record exact starting main SHA.

Suggested branch: `p13-5f-evidence-closure`

## Mandatory reading

Read:

- `docs/prompts/p13-5/README.md`
- all merged P13.5A–E contracts/evidence
- ADR-0175 / SPEC-0032
- `contracts/iac-openstack-profile-v1.yaml`
- `compatibility/product-profiles.yaml`
- `docs/compatibility/matrix.yaml`
- `docs/compatibility/backlog-inventory.yaml`
- `docs/status/current-state.yaml`
- `docs/P13_IMPLEMENTATION_PLAN.md`
- issue #750 and parent #744
- P13.6 scope so closure does not overclaim failure/security evidence not yet
  implemented

## Objective

Produce one trustworthy P13.5 acceptance record proving the bounded IaC state
convergence profile across the applicable SQLite and PostgreSQL paths, with
exact toolchain provenance and no claims beyond executed evidence.

The final P13.5 result should support this bounded statement only if all required
gates pass:

> For the declared `p13-iac-compatibility-v1` resources and attribute subsets,
> OpenTofu 1.12.6 with the unmodified upstream OpenStack provider 3.4.0 can
> observe/import supported canonical O3K resources, detect supported native
> drift/deletion, converge desired state, perform bounded replacement safely,
> and exercise the accepted retry/replay semantics while canonical O3K remains
> the sole cloud authority.

Do not upgrade this into a general OpenStack, general Terraform, HA, SLA or
production-readiness claim.

## Full accepted P13.5 matrix

Execute the complete matrix frozen by P13.5A and incrementally proven by
P13.5B–E.

At minimum aggregate these scenario classes:

```text
stable repeated read / no perpetual diff
provider-supported import
native mutable drift detection
native mutable drift re-convergence
native deletion detection
recreation after remote absence where applicable
explicit/provider-required replacement
relationship replacement/parent retention
read retry
pre-commit retry
post-commit update response loss
post-commit delete response loss
same accepted O3K Operation replay
relationship replay
explicit ambiguous create-response-loss classification (not a PASS)
```

Every resource/scenario cell must end in one of a controlled vocabulary such as:

- `passed`
- `not_applicable`
- `upstream_provider_unsupported`
- `deferred_p13_6`
- `blocked`

Do not use vague `done`, `works`, or `supported` without evidence binding.

## SQLite evidence

Run the complete portable P13.5 matrix against the supported SQLite TestLab
path unless a specific scenario is explicitly protected/privileged only.

The SQLite result must prove:

- deterministic canonical identity/ownership;
- stable read/import/drift/replacement behavior;
- no duplicate canonical resources;
- correct relationship cardinality;
- operation/replay semantics required by P13.5;
- final no-op plan after each convergence journey;
- cleanup between cases.

## PostgreSQL evidence

Run PostgreSQL evidence sufficient to prove externally equivalent P13.5
semantics for every state class that can differ because of persistence,
transaction, concurrency, reconstruction or replay behavior.

At minimum PostgreSQL must cover representative:

- import/read reconstruction;
- mutable drift and re-convergence;
- remote deletion/absence;
- independent replacement;
- RouterInterface or equivalent relationship lifecycle;
- VolumeAttachment relationship lifecycle;
- accepted Operation replay/unknown-outcome path where persistence matters.

Prefer running the same black-box P13.5 harness with backend configuration
changed rather than maintaining a second semantic test implementation.

If full matrix execution is practical, run it. If a cell is intentionally not
rerun on PostgreSQL, the final evidence must state why the selected PostgreSQL
coverage is sufficient for the bounded parity claim.

Any externally observable SQLite/PostgreSQL semantic difference is a P13.5
blocker unless already explicitly accepted by the product profile.

## Restart boundary

P13.6 owns the larger controller failure matrix. P13.5F may perform a clean
process restart between established state and later provider observation where
needed to prove **durable reconstruction parity**, but do not turn this into the
P13.6 crash-injection campaign.

A useful bounded proof is:

```text
create/converge resource
stop o3kd cleanly
start new runtime on same durable backend
provider refresh/read
final normal plan no-op
```

Use only where it validates P13.5 durable state semantics.

## Aggregate machine-readable evidence

Create/update a committed P13.5 aggregate evidence artifact under the existing
compatibility/evidence conventions. It should include at least:

```text
artifact_type/schema_version
phase: P13.5
source_head_sha
starting_main_sha
toolchain
provider_modified: false
profile
backend results
resource/scenario matrix
canonical authority statement
import results
native drift results
replacement/relationship results
retry/replay results
ambiguous/deferred cases
cleanup/leak results
protected evidence references
known deviations
final verdict
```

The aggregate must reference underlying scenario artifacts rather than silently
collapsing failures/unsupported cases into PASS.

## Status/documentation updates

Only after the evidence is complete and green for the accepted matrix, update
applicable:

- `docs/status/current-state.yaml`
- `docs/compatibility/matrix.yaml`
- `docs/compatibility/backlog-inventory.yaml`
- `compatibility/product-profiles.yaml`
- `docs/P13_IMPLEMENTATION_PLAN.md`
- other P13 status/index files required by existing validation

The updates must state P13.5's bounded evidence honestly and keep P13.6/P13.7
open/unproven.

Do not mark:

- multi-project isolation passed unless P13.6 ran;
- controller crash matrix passed unless P13.6 ran;
- full real-host profile passed unless P13.7 ran;
- production readiness/HA/SLA;
- general OpenStack/Terraform compatibility.

## Issue #750 closure preparation

Prepare a final issue/PR report that contains:

- exact merged prerequisite SHAs;
- current HEAD;
- exact OpenTofu/provider provenance;
- supported resource matrix;
- upstream-provider unsupported/not-applicable import cases;
- SQLite/PostgreSQL evidence summary;
- retry/replay claim boundary;
- explicit statement that lost create response without learned resource ID is
  not claimed exactly-once and remains P13.6 evidence;
- all known deviations;
- P13.6 as next phase if no blockers remain.

Do not close #750 from the implementation agent before independent review and
protected merge. The reviewer/merge step decides closure readiness.

## Semantic change rule

P13.5F should ideally be evidence/status only. If the full matrix finds a real
semantic parity defect:

1. reproduce it deterministically;
2. do not hide it in evidence tooling;
3. make the smallest owning runtime/persistence correction in this PR only if
   it remains one coherent closure defect and is independently reviewable;
4. otherwise split it into a dedicated blocker fix before closing P13.5F;
5. rerun affected P13.5 and historical regressions.

Never weaken an assertion to obtain a green aggregate.

## Validation

Run all permanent repository validation plus the complete P13.5 acceptance
matrix and all historical P13 regressions affected by P13.5 changes.

At minimum:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
python3 scripts/check-architecture-boundaries.py
python3 scripts/check-maintainability-guards.py
bash tests/maintainability-guards.sh
cargo nextest run --workspace --all-features --profile pr
cargo test --workspace --all-features
```

Plus:

- complete P13.5 SQLite harness;
- required PostgreSQL P13.5 parity/conformance;
- mandatory historical PostgreSQL suites for touched persistence domains;
- existing P13.2/P13.3/P13.4 provider regressions;
- applicable protected/real-host gates referenced by final claims;
- compatibility/profile/status validators;
- `git diff --check`.

## Required final report

```text
P13.5F branch:
P13.5F HEAD:
Starting main SHA:
OpenTofu: 1.12.6 / hash
Provider: terraform-provider-openstack/openstack 3.4.0 / hash
Provider modified: NO
P13.5 aggregate verdict: PASS / BLOCKED
Resources in bounded profile: <list/count>
Stable-read convergence: PASS / findings
Import convergence: PASS / supported/unsupported matrix
Native drift/re-convergence: PASS / findings
Native deletion/recreation: PASS / findings
Replacement: PASS / findings
Relationship parent-retention: PASS / findings
Retry/replay: PASS / findings
Client-lost-create-response exactly-once claimed: NO
SQLite matrix: PASS / findings
PostgreSQL parity: PASS / findings
Restart reconstruction evidence: PASS / N/A / findings
Canonical authority preserved: YES / NO
Terraform-specific authority introduced: NO
Architecture guard regression: NONE / findings
Compatibility/product status updated from executed evidence: YES / NO
P13.6/P13.7 left unproven: YES / NO
Known deviations:
Issue #750 ready for independent closure review: YES / NO
Recommended next phase: P13.6 / STOP
```

Do not merge or close #750 yourself. Leave ready for the independent final
review and protected merge process in `REVIEW_AND_MERGE.md`.
