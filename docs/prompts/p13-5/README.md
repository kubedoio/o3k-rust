# P13.5 — IaC State Convergence Prompt Set

Issue: #750 — P13.5 — IaC state convergence
Parent: #744 — P13 — Ecosystem Compatibility & Infrastructure as Code

Authoritative prompt-set baseline when this branch was created:

`main@e3b9c6e1e68c0c2372fb8710d3d1ed060927bb89`

The R7 structural-convergence program is complete. Its final independent audit
found no category-C structural blocker and explicitly moved future refactoring
to feature-driven work. P13.5 MUST NOT reopen R7 or perform speculative
structural cleanup.

## Objective

Prove that the unmodified upstream OpenStack Terraform provider and the native
O3K APIs converge on one canonical O3K resource state under refresh/read,
import, out-of-band/native drift, deletion, replacement, retry and replay.

Terraform/OpenTofu and the OpenStack compatibility APIs remain clients and
projections. They MUST NOT become a second source of resource authority.

## Toolchain baseline

- OpenTofu: `1.12.6`
- `terraform-provider-openstack/openstack`: `3.4.0`, upstream and unmodified
- O3K profile: `p13-iac-compatibility-v1`

## Execution order

Execute these prompts strictly in order. Every slice starts from the then-current
protected `origin/main` after all prerequisite slices are independently reviewed
and merged.

1. [`P13_5A_CONVERGENCE_CONTRACT.md`](P13_5A_CONVERGENCE_CONTRACT.md)
2. [`P13_5B_REFRESH_IMPORT.md`](P13_5B_REFRESH_IMPORT.md)
3. [`P13_5C_NATIVE_DRIFT.md`](P13_5C_NATIVE_DRIFT.md)
4. [`P13_5D_REPLACEMENT_RELATIONSHIPS.md`](P13_5D_REPLACEMENT_RELATIONSHIPS.md)
5. [`P13_5E_RETRY_REPLAY.md`](P13_5E_RETRY_REPLAY.md)
6. [`P13_5F_BACKEND_EVIDENCE_CLOSURE.md`](P13_5F_BACKEND_EVIDENCE_CLOSURE.md)

After every implementation slice, use
[`REVIEW_AND_MERGE.md`](REVIEW_AND_MERGE.md) before starting the next slice.

## Global architecture invariants

The following are absolute throughout P13.5:

- Canonical O3K resources remain the sole desired-state authority.
- Terraform/OpenTofu state is client-side bookkeeping only.
- OpenStack compatibility rows/projections are not canonical resources.
- Provider-local state is execution/observation state only.
- Existing O3K resource IDs, owner scope, desired state, Operation identity,
  generation/fencing, recovery and compensation remain authoritative.
- The upstream provider MUST remain unmodified.
- No `terraform-provider-o3k` is introduced.
- No Terraform-specific canonical database/table/resource is introduced.
- No manual `terraform.tfstate` editing is accepted as lifecycle support.
- No compatibility endpoint breadth is added unless a frozen P13 provider
  contract proves it is required for one declared P13 resource.
- A state-convergence defect is fixed at the narrowest correct canonical or
  compatibility boundary; do not add a parallel authority to satisfy Terraform.
- R7 architecture boundaries and permanent guards MUST remain at least as strict.

## Global resource scope

P13.5 is limited to the bounded P13 IaC profile already introduced by P13.2–P13.4:

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

Read-only image/flavor data sources are regression inputs where needed, not new
managed-resource scope.

## P13.5 vs P13.6 boundary

P13.5 owns state convergence and bounded retry/replay behavior.

P13.6 still owns:

- two-project/multi-project isolation;
- the larger controller restart/failure matrix;
- cross-project negative evidence;
- the explicit client-lost-create-response ambiguity where the client never
  receives a newly-created resource ID.

Do not silently absorb P13.6 into P13.5.

## Evidence discipline

Prefer machine-readable evidence. For OpenTofu plans use:

```bash
tofu plan -out=plan.tfplan
tofu show -json plan.tfplan > plan.json
```

For observation-only state reconciliation prefer refresh-only planning rather
than relying on deprecated `tofu refresh` as the primary evidence mechanism:

```bash
tofu plan -refresh-only -out=refresh.tfplan
tofu show -json refresh.tfplan > refresh.json
```

A legacy `tofu refresh` smoke may be retained if useful, but it is not the
primary acceptance artifact.

Every committed evidence record must bind at least:

- exact O3K HEAD SHA;
- OpenTofu version;
- provider version and provider binary/archive hash where the existing P13
  harness records them;
- `provider_modified: false`;
- resource and scenario;
- precondition/canonical identity;
- observed plan actions;
- final convergence result;
- backend/profile;
- cleanup/leak result where applicable.

## Stop conditions

Stop the current slice and report the blocker instead of hiding it if:

- an accepted ADR/SPEC conflicts with observed upstream-provider behavior;
- supporting the scenario appears to require a second canonical authority;
- import requires state-file surgery not supported by the upstream provider;
- a relationship resource cannot be represented without changing parent
  resource ownership semantics;
- retry safety would require claiming client-level exactly-once create without
  a durable upstream client token;
- SQLite/PostgreSQL externally disagree in canonical semantics;
- a required protected/real-host evidence tier is unavailable and the claim
  depends on it.

## Definition of P13.5 done

P13.5 is complete only when the accepted matrix proves, for every applicable
bounded resource/scenario:

- stable provider reads / no perpetual diff;
- supported import converges without manual state surgery;
- native mutable drift is detected exactly;
- native deletion becomes provider-visible absence;
- re-apply converges canonical desired state;
- replacement produces correct identity/cleanup semantics;
- relationship replacement/detach preserves independent parents;
- bounded retry/replay does not duplicate one accepted O3K Operation's provider
  side effects;
- SQLite and PostgreSQL expose equivalent supported behavior;
- final normal plan is no-op after convergence;
- compatibility/product claims are updated only from executed evidence.
