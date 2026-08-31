# Prompt — P13.5B: Refresh/Read Stability and Import Convergence

## Task

Implement **P13.5B** for issue #750 after P13.5A has been independently
reviewed and merged.

P13.5B proves that the upstream provider can repeatedly observe canonical O3K
resources without perpetual diff, and that provider-supported existing O3K
resources can be imported into OpenTofu state without creating a second cloud
authority.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before editing:

1. Read `AGENTS.md`.
2. Fetch the then-current protected `origin/main`.
3. Verify the reviewed/merged P13.5A convergence contract and evidence are
   reachable from main.
4. Create a fresh branch from current `origin/main`.
5. Record the exact starting main SHA.

Suggested branch: `p13-5b-refresh-import`

Do not base implementation on the prompt-set branch or on an older P13.5A
implementation branch.

## Mandatory reading

Read at minimum:

- `docs/prompts/p13-5/README.md`
- the merged P13.5A convergence contract/evidence
- `docs/P13_IMPLEMENTATION_PLAN.md`
- ADR-0175
- SPEC-0032
- `contracts/iac-openstack-profile-v1.yaml`
- existing P13.2/P13.3/P13.4 provider lifecycle tests/evidence
- issue #750
- the exact upstream provider 3.4.0 implementation/docs relevant to each
  import/read path identified by P13.5A

The P13.5A matrix is authoritative for which resource/import cases are
supported, required, unsupported or not applicable. Do not broaden it without
new evidence and review.

## Objective

For each applicable bounded managed resource, prove two classes of convergence:

1. **Read/refresh stability**: a normally managed resource repeatedly read by
   the provider reaches a no-op normal plan with no projection oscillation.
2. **Import convergence**: an existing canonical O3K resource imported through
   the upstream provider reaches a no-op plan without manual Terraform state
   surgery or duplicate canonical resources.

## Scenario B1 — stable normal read

For each applicable resource:

```text
tofu apply
  -> canonical O3K resource exists
  -> provider read/refresh
  -> tofu plan
  -> NO-OP
  -> second provider read/refresh
  -> tofu plan
  -> NO-OP
```

The test must detect perpetual diffs caused by, at minimum:

- omitted vs empty vs null values;
- provider defaults;
- computed fields;
- list/set ordering;
- normalized names/status values;
- fixed-IP/network block ordering;
- security-group rule ordering;
- attachment/device projection;
- status/power-state projections;
- router/FIP compatibility fields;
- Cinder/Nova volume/attachment response normalization.

Do not paper over a perpetual diff with `ignore_changes` unless the accepted P13
profile already explicitly declares that field unmanaged. Do not mutate the
provider schema.

## Scenario B2 — provider-supported import

For every resource classified by P13.5A as supported/required for import:

1. Create the resource through the native O3K API or canonical O3K application
   path, not through Terraform.
2. Capture the canonical resource ID and owner scope.
3. Configure the corresponding Terraform resource with the exact bounded
   desired attributes.
4. Run the upstream provider's supported `tofu import` form using the exact
   identifier shape frozen by P13.5A.
5. Capture the provider's first read after import.
6. Assert the imported Terraform object resolves to the intended canonical O3K
   resource/relation.
7. Run a normal plan and require NO-OP after any provider-defined normalization.
8. Destroy/cleanup through the normal supported lifecycle and verify canonical
   absence/retention semantics as applicable.

Absolute rule: **no manual editing of `terraform.tfstate` or plan JSON**.

## Identity requirements

Where the compatibility projection is one-to-one, the provider-visible resource
identity must remain the canonical O3K identity already established by P13.

Where P13.5A proves that the upstream provider requires a composite relationship
import identifier, preserve that provider contract, but the underlying O3K
parent/relationship canonical identities must remain authoritative and be
validated explicitly.

Import MUST NOT:

- create a new canonical resource;
- reassign owner/project scope;
- create a Terraform-owned duplicate mapping authority;
- mutate provider/host state merely because the resource was read;
- rewrite unrelated relationships;
- expose a cross-project resource through a guessed import ID.

## Security and non-disclosure

Import/read paths are still protected OpenStack compatibility operations.
Preserve project ownership and non-disclosure semantics. A resource ID supplied
through import must not bypass owner-scope authorization or reveal foreign
resource existence differently from the accepted compatibility behavior.

P13.6 owns the full two-project matrix, but P13.5B must not weaken the existing
single-project authorization boundary.

## Resource prioritization

Use the P13.5A matrix. Existing P13.2 already contains some import evidence for
core resources; do not duplicate tests blindly. Reuse/refactor test helpers only
where it reduces duplication without changing accepted tests.

Pay special attention to currently weaker convergence areas:

- Security Group / Rule where provider normalization may reorder fields;
- Router Interface relationship identity/import form;
- Floating IP binding fields;
- native Volume;
- VolumeAttachment relationship/import form.

If upstream provider 3.4.0 does not support import for one relationship type,
record that as an upstream/provider limitation exactly as P13.5A classified it;
do not invent a custom O3K import API.

## Implementation rule

Tests drive fixes. First reproduce any failure with the real upstream provider.
Then make the smallest correct change at the owning boundary:

- canonical application read if canonical state is wrong;
- compatibility projection if response shape/normalization is wrong;
- persistence only if durable canonical reconstruction is wrong.

Do not proactively redesign APIs, storage, services or adapters.

## Evidence

Extend `tests/p13_5_state_convergence.sh` and/or bounded helpers.

For each scenario capture machine-readable plan evidence using `tofu show -json`.
Record at least:

```text
resource
scenario: stable-read | import
canonical_id
provider_import_id
first_read_route
plan_actions
final_plan_noop
canonical_duplicate_count
cleanup_result
backend
head_sha
toolchain provenance
```

## Explicit non-scope

Do NOT implement yet:

- native out-of-band mutable drift/re-apply (P13.5C);
- forced replacement graph semantics (P13.5D);
- HTTP fault proxy/retry injection (P13.5E);
- broad backend closure (P13.5F);
- P13.6 multi-project/controller-failure matrix;
- new managed resource types;
- general OpenStack/Terraform compatibility claims.

## Validation

Run all permanent repository guards plus:

- P13.5A contract/harness validation;
- focused P13.5B refresh/import tests;
- all existing provider lifecycle regressions for touched resource groups;
- SQLite tests for touched canonical persistence paths;
- PostgreSQL conformance for any persistence location touched;
- protected/privileged gates only where the change actually affects their
  claimed behavior.

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

## Required final report

```text
P13.5B branch:
P13.5B HEAD:
Starting main SHA:
Resources with stable repeated reads: <list>
Resources import-verified: <list>
Upstream import unsupported/not-applicable: <list + reason>
Perpetual-diff defects found/fixed: <list>
Canonical duplicate resources after import: 0 / findings
Manual Terraform state edits required: NO
Provider modified: NO
Authority model preserved: YES / NO
Authorization/non-disclosure preserved: YES / NO
Production semantic changes: <exact list, or NONE>
SQLite evidence:
PostgreSQL evidence:
Existing P13 regressions:
Known limitations:
Recommended next slice: P13.5C / STOP
```

Do not merge. Leave the PR ready for independent review.
