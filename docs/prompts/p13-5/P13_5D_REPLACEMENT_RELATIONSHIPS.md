# Prompt — P13.5D: Replacement Semantics and Relationship Convergence

## Task

Implement **P13.5D** for issue #750 after P13.5C is independently reviewed and
merged.

P13.5D proves that Terraform replacement semantics map cleanly onto canonical
O3K lifecycle semantics, with exact cleanup and parent-retention guarantees for
relationship resources.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before editing:

1. Read `AGENTS.md`.
2. Fetch current protected `origin/main`.
3. Verify P13.5A/B/C are merged and reachable.
4. Create a fresh branch from current `origin/main`.
5. Record exact starting SHA.

Suggested branch: `p13-5d-replacement-relations`

## Mandatory reading

Read:

- `docs/prompts/p13-5/README.md`
- merged P13.5A matrix, especially mutable vs replacement/ForceNew fields
- P13.5B/C evidence
- ADR-0175 / SPEC-0032
- accepted NetworkPolicy/L3Gateway/Volume architecture ADRs/SPECs used by P13.3/P13.4
- `contracts/iac-openstack-profile-v1.yaml`
- existing replacement/delete/compensation/recovery tests for Compute, Network
  and Storage
- issue #750
- upstream provider 3.4.0 replacement semantics for each applicable resource

## Objective

Prove that explicit or provider-required replacement:

- deletes/replaces only the intended canonical resource or relationship;
- produces correct old/new canonical identity;
- leaves no old provider-owned state or compatibility ghost;
- preserves independent parent resources;
- converges to a final no-op plan;
- preserves O3K operation/recovery/compensation semantics.

Use OpenTofu's supported replacement flow, e.g.:

```bash
tofu plan -replace='RESOURCE_ADDRESS' -out=replace.tfplan
tofu show -json replace.tfplan > replace.json
tofu apply replace.tfplan
```

Do not rely on legacy state surgery/taint as the primary mechanism.

## Scenario D1 — independent resource replacement

For every applicable independently managed resource classified for replacement
coverage by P13.5A:

1. Create with OpenTofu and capture canonical ID `A`.
2. Require no-op plan.
3. Request explicit replacement or change a bounded ForceNew attribute as
   appropriate to the upstream provider contract.
4. Assert plan actions are exactly the expected replacement action for the
   intended address.
5. Apply.
6. Capture canonical ID `B` and require `B != A` for true replacement.
7. Verify old canonical resource `A` is absent.
8. Verify no old provider-local owned resources/allocations/bindings remain.
9. Verify exactly one new canonical resource exists at the Terraform address.
10. Require final no-op plan.

Record lifecycle order when it matters. If the upstream provider uses
create-before-destroy or destroy-before-create, O3K must honor the actual
provider request sequence safely; do not invent different Terraform semantics.

## Scenario D2 — relationship replacement and parent retention

Relationship resources are high-risk and require explicit independent-parent
proof.

### Router Interface

Canonical model is an independent L3Gateway and AddressRealm connected through
the accepted canonical attachment/relation model.

Prove replacement/remove-readd semantics such that:

- Router/L3Gateway parent survives with the same canonical ID;
- Subnet/AddressRealm parent survives with the same canonical ID;
- old relationship/attachment is absent after replacement;
- exactly one intended new/current relationship exists;
- route/provider realization for the old relationship is cleaned;
- unrelated gateway attachments remain unchanged;
- final Terraform plan is no-op.

A Router Interface replacement MUST NOT delete/recreate the Router or Subnet
merely because Terraform models the relationship as a resource.

### VolumeAttachment

Canonical model is an independent Server and Volume connected through
VolumeAttachment.

Prove detach/replacement/re-attach semantics such that:

- Server survives with identical canonical ID;
- Volume survives with identical canonical ID;
- Volume data/resource identity is not recreated by attachment replacement;
- old VolumeAttachment relation/provider device association is removed;
- exactly one intended current VolumeAttachment exists;
- guest/provider device cleanup is correct at the claimed evidence tier;
- final Terraform plan is no-op.

Do not promote provider-local `/dev/*` paths to canonical state merely to make
Terraform state stable.

### Other relationship-like cases

Use the P13.5A matrix to decide whether Port↔SecurityGroup, FloatingIP binding,
or other bounded relationships require replacement-specific coverage. Preserve
canonical parent ownership and cardinality.

## Scenario D3 — dependency graph replacement

For representative bounded graphs, verify replacing one child does not cause
unrelated graph churn.

At minimum consider:

```text
Network -> Subnet -> Port -> Server
SecurityGroup -> Port attachment
L3Gateway -> RouterInterface -> AddressRealm
Server -> VolumeAttachment -> Volume
```

The machine-readable plan must identify the intended replacement set. Reject
unexpected replacement cascades unless they are explicitly required by the
frozen upstream provider contract and accepted P13 resource semantics.

## Cleanup and leak proof

Replacement is not complete because Terraform reports success. Verify:

- old canonical records absent/finalized correctly;
- old durable relationships absent;
- allocations/IPAM state consistent;
- provider execution state cleaned where the existing profile can inspect it;
- no stale Operation remains incorrectly active;
- foreign/unowned state untouched where existing real-host/leak-verifier
  evidence applies.

Do not claim privileged/guest-level cleanup without running the required tier.

## Implementation rule

Tests first. If replacement exposes a defect, fix the smallest owning boundary.

Do not:

- redesign canonical resource identity;
- make compatibility IDs authoritative;
- add generic relationship frameworks;
- weaken delete/compensation fencing;
- add cascade deletion merely to satisfy Terraform;
- introduce Terraform-specific parent retention flags.

## Evidence

For each replacement scenario capture:

```text
resource/relationship
scenario
terraform_address
plan_actions
canonical_id_before
canonical_id_after
old_absent
new_count
parent_ids_before
parent_ids_after
parents_preserved
unrelated_plan_changes
owned_cleanup_result
foreign_state_result where available
final_plan_noop
backend
head_sha
toolchain provenance
```

## Explicit non-scope

Do NOT yet implement:

- HTTP transport fault/retry injection (P13.5E);
- P13.6 controller crash matrix;
- multi-project isolation expansion;
- boot-from-volume, multi-attach or online resize;
- Neutron HA/DVR/router breadth;
- new provider/resource types.

## Validation

Run permanent guards and all P13.5A/B/C regressions plus:

- replacement-focused provider harness scenarios;
- relationship lifecycle tests;
- affected Compute/Network/Storage recovery and compensation tests;
- SQLite/PostgreSQL conformance for touched durable paths;
- real-host LVM/libvirt/network cleanup gates where a claim depends on them.

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
P13.5D branch:
P13.5D HEAD:
Starting main SHA:
Independent replacement scenarios passed: <list>
Relationship replacement scenarios passed: <list>
Router/L3Gateway preserved: YES / NO / N/A
Subnet/AddressRealm preserved: YES / NO / N/A
Server preserved across VolumeAttachment replacement: YES / NO / N/A
Volume preserved across VolumeAttachment replacement: YES / NO / N/A
Unexpected replacement cascades: NONE / list
Old canonical/provider-owned leaks: 0 / findings
Foreign state changed: NO / finding / not executed
Final no-op plans: PASS / failures
Provider modified: NO
Authority/recovery/compensation semantics preserved: YES / NO
Production semantic changes: <exact list, or NONE>
Protected/real-host evidence:
Known limitations:
Recommended next slice: P13.5E / STOP
```

Do not merge. Leave ready for independent review.
