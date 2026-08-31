# Prompt — P13.5A: Convergence Contract, Discovery Matrix, and Harness Baseline

## Task

Implement **P13.5A** for issue #750. This is the contract/discovery and harness
baseline slice for P13.5. It MUST NOT intentionally change production runtime
semantics.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before doing anything:

1. Read `AGENTS.md` and follow it as mandatory policy.
2. Fetch the current protected `origin/main`.
3. Confirm all R7 work is already merged/reachable and issue #769 is closed or
   otherwise explicitly recorded complete by its final independent audit.
4. Create a fresh implementation branch from the **then-current** `origin/main`.
   Do not implement from the historical prompt-set baseline SHA.
5. Record the exact starting `main` SHA in the issue/PR and evidence.

Suggested branch: `p13-5a-convergence-contract`

## Mandatory reading

Read at minimum:

- `AGENTS.md`
- `docs/PROJECT_CHARTER.md`
- `docs/CLEAN_IMPLEMENTATION.md`
- `docs/ARCHITECTURE.md`
- `docs/NORMATIVE_SOURCES.md`
- `docs/TEST_STRATEGY.md`
- `docs/LLM_DEVELOPMENT.md`
- `docs/P13_IMPLEMENTATION_PLAN.md`
- `docs/adr/ADR-0175-openstack-ecosystem-and-iac-compatibility-boundary.md`
- `docs/specs/SPEC-0032-openstack-terraform-opentofu-compatibility-profile-v1.md`
- `contracts/iac-openstack-profile-v1.yaml`
- `compatibility/product-profiles.yaml`
- the P13.1 provider-contract evidence relevant to every managed resource
- P13.2/P13.3/P13.4 lifecycle tests and committed evidence
- issue #750 and parent #744
- `docs/prompts/p13-5/README.md`

Also inspect the exact upstream `terraform-provider-openstack/openstack` 3.4.0
public behavior needed to determine import identifiers, Read/Refresh behavior,
ForceNew/replacement semantics, polling/retry behavior, and provider state
requirements. Public upstream sources may be used; record provenance. Do not
modify the provider.

## Objective

Freeze a reviewable, machine-readable **P13.5 convergence contract** before any
semantic fix is attempted.

P13.5A must answer, for each bounded managed resource:

- Is import supported by the upstream provider?
- What exact import identifier syntax does the provider accept?
- What is the first read after import?
- Which declared P13 attributes are provider-read state?
- Which declared attributes are mutable in place?
- Which are replacement/ForceNew semantics?
- What does remote absence look like to the provider?
- Which fields are computed/defaulted/normalized and therefore susceptible to
  perpetual diff?
- Which resource types are relationships rather than independent parents?
- Which retry/read patterns are expected by the provider?
- Which scenarios belong to P13.5 and which remain P13.6?

Do not guess. Discover from the pinned provider and existing frozen P13 traces.
Where a new black-box probe is required, run the real unmodified provider.

## Required P13.5 resource matrix

Create a committed machine-readable contract/evidence document under an
appropriate `docs/compatibility/p13-5/` location. Use the repository's existing
compatibility/evidence conventions.

For each of these resources:

- `openstack_compute_keypair_v2`
- `openstack_networking_network_v2`
- `openstack_networking_subnet_v2`
- `openstack_networking_port_v2`
- `openstack_compute_instance_v2`
- `openstack_networking_secgroup_v2`
- `openstack_networking_secgroup_rule_v2`
- `openstack_networking_router_v2`
- `openstack_networking_router_interface_v2`
- `openstack_networking_floatingip_v2`
- `openstack_blockstorage_volume_v3`
- `openstack_compute_volume_attach_v2`

record at least:

```text
resource
canonical_o3k_mapping
import: required | supported | unsupported | not_applicable
import_identifier_shape
first_import_read
refresh/read route(s)
remote_absence_behavior
mutable_attributes in the bounded profile
replacement_attributes in the bounded profile
relationship_parents, if applicable
native_drift_cases to prove in P13.5C
replacement cases to prove in P13.5D
retry cases to prove in P13.5E
backend evidence requirement
known bounded deviations
provenance/evidence source
```

An unsupported provider capability is not automatically an O3K defect. Record
it honestly and do not add a custom O3K/provider extension merely to force the
matrix to be symmetric.

## Existing behavior baseline

Before introducing the P13.5 harness, execute the existing applicable P13
provider gates on this post-R7 main using:

- OpenTofu 1.12.6
- upstream unmodified provider 3.4.0

The baseline must cover the already-advertised P13.2, P13.3 and P13.4 bounded
managed-resource lifecycles as far as the local/protected environment permits.
Do not falsely mark unavailable protected/privileged evidence as passed.

If an existing accepted P13 lifecycle fails on the current main, STOP P13.5A
feature work. Classify it as a regression, open/update the appropriate issue,
and fix it separately before freezing the new convergence baseline.

## Harness architecture

Introduce the smallest reusable P13.5 harness skeleton. The implementation plan
expects `tests/p13_5_state_convergence.sh`; it may orchestrate smaller helpers.

Prefer:

- existing P13 tool verification/mirror setup;
- real OpenTofu and real provider;
- fresh disposable state/data directories;
- machine-readable plan output;
- a small Python validator for `tofu show -json` if needed;
- no production failpoints in P13.5A;
- no fake Terraform provider;
- no manual state-file edits.

The harness in P13.5A may prove discovery/baseline scenarios but MUST NOT claim
later P13.5B–F acceptance before those slices run.

## Refresh evidence rule

Use refresh-only planning as the primary observation artifact:

```bash
tofu plan -refresh-only -out=refresh.tfplan
tofu show -json refresh.tfplan > refresh.json
```

A direct `tofu refresh` command may be smoke-tested for compatibility but must
not be the sole acceptance artifact.

## Architectural non-goals

Do NOT:

- reopen R7 structural refactoring;
- change public/native API behavior;
- change OpenStack compatibility semantics;
- add schemas or migrations;
- modify SQL/transaction/lock behavior;
- add a Terraform-specific database/table/resource;
- add `terraform-provider-o3k`;
- modify/fork the upstream OpenStack provider;
- implement P13.5B/C/D/E semantic fixes preemptively;
- absorb P13.6 multi-project/failure-matrix work;
- claim general Terraform or general OpenStack compatibility.

## Validation

At minimum run:

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

Plus the existing P13 provider gates required to establish the post-R7 baseline
and any new P13.5A harness self-tests.

## Required final report

Report exactly:

```text
P13.5A branch:
P13.5A HEAD:
Starting protected main SHA:
OpenTofu version/hash:
Provider version/hash:
Provider modified: NO
Existing P13 baseline: PASS / BLOCKED (with exact blockers)
Convergence matrix frozen: YES / NO
Resources classified: <count>
Import identifier discovery complete: YES / NO
Mutable vs replacement classification complete: YES / NO
P13.5/P13.6 boundary preserved: YES / NO
Production semantic changes: NONE / list exact deviations
Architecture guard changes: NONE / list
Tests/evidence:
Known uncertainties:
Recommended next slice: P13.5B / STOP
```

Do not merge. Leave the PR ready for independent review using
`docs/prompts/p13-5/REVIEW_AND_MERGE.md`.
