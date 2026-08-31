# Prompt — P13.5C: Native Drift Detection and Canonical Re-Convergence

## Task

Implement **P13.5C** for issue #750 after P13.5B is independently reviewed and
merged.

P13.5C proves the one-authority model end to end: a resource created through
OpenTofu/OpenStack compatibility remains a canonical O3K resource; when that
same canonical resource is mutated or deleted through the native O3K path, the
unmodified upstream provider must observe the resulting state accurately, and
OpenTofu must propose and execute the correct convergence action.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before editing:

1. Read `AGENTS.md`.
2. Fetch current protected `origin/main`.
3. Verify P13.5A and P13.5B are merged and their contracts/evidence are
   reachable from main.
4. Create a new branch from current `origin/main`.
5. Record the exact starting SHA.

Suggested branch: `p13-5c-native-drift`

## Mandatory reading

Read:

- `docs/prompts/p13-5/README.md`
- merged P13.5A convergence matrix
- merged P13.5B import/read evidence
- ADR-0175
- SPEC-0032
- `contracts/iac-openstack-profile-v1.yaml`
- native O3K API contracts for the canonical resources being mutated
- compatibility adapters for the same resources
- existing P13.2/P13.3/P13.4 provider evidence
- issue #750

Do not assume a Terraform field is drift-testable merely because it exists in
the provider schema. Use the bounded P13 attribute set and mutable/replacement
classification frozen in P13.5A.

## Objective

Prove two distinct drift classes:

1. **Native mutable drift**: out-of-band mutation of a canonical resource is
   observed by the provider as an exact in-place difference and re-apply
   restores Terraform desired state without changing canonical identity.
2. **Native deletion drift**: out-of-band canonical deletion is observed as
   remote absence; OpenTofu proposes recreation where the upstream provider
   semantics require it, and re-apply creates exactly one correct new canonical
   resource/relation.

## Scenario C1 — mutable drift

For each resource/attribute classified as mutable in P13.5A:

1. Create/manage the resource through OpenTofu.
2. Capture canonical ID `A`, owner/project, relevant generation and canonical
   desired/observed state.
3. Verify a normal plan is no-op.
4. Mutate the same resource through the native O3K API/application path using a
   value intentionally different from Terraform configuration.
5. Verify canonical ID remains `A`.
6. Run `tofu plan -refresh-only` and capture JSON.
7. Run a normal `tofu plan` and capture JSON.
8. Assert the plan shows the exact bounded drift and no unrelated changes.
9. Run `tofu apply`.
10. Verify canonical ID is still `A` and the canonical value matches Terraform
    desired state.
11. Require a final normal plan to be no-op.

The test must distinguish between:

- provider refresh/state update;
- Terraform proposed desired-state convergence;
- canonical O3K mutation.

A refresh-only operation MUST NOT mutate O3K desired state or provider/host
state.

## Scenario C2 — native deletion drift

For every independently managed resource for which remote deletion/recreation
is applicable:

1. Create with OpenTofu and capture canonical ID `A`.
2. Verify no-op plan.
3. Delete through native O3K API/application path.
4. Verify canonical absence through the authoritative native/domain path.
5. Run refresh-only plan and normal plan.
6. Assert the provider reports absence using its expected resource semantics.
7. Assert normal plan proposes the upstream-correct recreate action.
8. Apply.
9. Capture new canonical ID `B` and assert `B != A` for true independent
   resource recreation.
10. Prove old canonical state/provider ownership is absent and only one new
    resource exists.
11. Require final no-op plan.

For relationship resources, native detach/removal semantics may preserve both
parents. Those deeper graph semantics are finalized in P13.5D; P13.5C may prove
absence detection without forcing destructive replacement behavior.

## Exact-drift rule

A successful drift test does not merely mean `tofu plan` returns exit code 2.
Machine-readable plan assertions must prove that:

- the intended resource address is the one changing;
- the intended bounded attribute/action is present;
- unrelated resources do not change;
- no false replacement is proposed for a mutable field;
- no stale compatibility object remains after canonical deletion.

## High-risk resource cases

Pay special attention to:

### Server

Native rename/power-state drift must not confuse desired Terraform configuration
with transient observed execution state. Do not make provider-observed power
state a new canonical Terraform authority.

### Port / Security Group attachment

Native attachment or fixed-IP-related drift must respect the bounded profile,
IPAM identity and relationship authority. Do not reallocate IP/MAC merely
because provider state refreshes.

### Router/Floating IP

Canonical L3Gateway/PublicAddress authority must remain independent of Neutron
compatibility rows. A native change must be reflected through projection, not
by mutating a second Neutron authority.

### Volume

Native name/description/metadata drift supported by the bounded profile must be
observable without changing Volume identity or provider-local device state.

### VolumeAttachment

A native detach must become provider-visible relationship absence while Server
and Volume survive. P13.5D will prove full replacement/parent-retention
semantics.

## Implementation rule

First reproduce with the real provider. Fix the smallest owner of incorrect
behavior.

Examples:

- stale compatibility response -> fix projection/read path;
- canonical read returns stale reconstructed value -> fix canonical service or
  durable reconstruction;
- provider plan shows unrelated fields because response defaults oscillate ->
  normalize at the compatibility boundary if consistent with frozen contract;
- remote absence not cleared from provider state -> return the exact upstream
  expected not-found behavior.

Do not add a Terraform-specific desired-state overlay.

## Evidence

Extend the P13.5 harness. For each scenario record:

```text
resource
scenario: native-mutable-drift | native-delete-drift
terraform_address
canonical_id_before
canonical_id_after_native_mutation
canonical_id_after_reapply
native_change
refresh_only_actions
normal_plan_actions
unrelated_changes_count
old_resource_absent
new_resource_count
final_plan_noop
backend
head_sha
toolchain provenance
```

For mutable drift, canonical ID before/after reapply should remain equal unless
the P13.5A contract explicitly classified that change as replacement instead.

## Safety invariants

P13.5C MUST NOT weaken:

- project ownership/non-disclosure;
- canonical generation/CAS fencing;
- Operation/idempotency identity;
- IPAM uniqueness;
- attachment parent ownership;
- unknown-outcome observation rules;
- cleanup/compensation;
- R7 SQL and host-execution boundaries.

## Explicit non-scope

Do NOT yet implement:

- forced `-replace` matrices beyond what is needed to classify a defect
  (P13.5D);
- HTTP fault injection / retry-loss behavior (P13.5E);
- full SQLite/PostgreSQL closure matrix (P13.5F);
- P13.6 controller restart or multi-project matrix;
- new resource breadth.

## Validation

Run permanent workspace/architecture gates plus:

- all P13.5A/B regression tests;
- focused native-drift tests for every changed resource group;
- existing P13 lifecycle tests for affected resources;
- relevant SQLite/PostgreSQL conformance when durable paths change;
- protected real-host/provider gates only where required by the affected claim.

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
P13.5C branch:
P13.5C HEAD:
Starting main SHA:
Mutable drift scenarios passed: <resource.attribute list>
Native deletion scenarios passed: <resource list>
False replacements found/fixed: <list>
Ghost/stale compatibility resources found/fixed: <list>
Canonical identity preserved for in-place convergence: YES / NO
Refresh-only caused cloud mutation: NO / finding
Unrelated plan changes: 0 / findings
Provider modified: NO
Authority model preserved: YES / NO
Production semantic changes: <exact list, or NONE>
SQLite evidence:
PostgreSQL evidence:
Known limitations:
Recommended next slice: P13.5D / STOP
```

Do not merge. Leave ready for independent review.
