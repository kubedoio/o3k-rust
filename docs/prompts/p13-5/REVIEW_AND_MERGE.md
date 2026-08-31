# Prompt — P13.5 Independent Review, Protected Merge, and Next-Slice Handoff

Use this control prompt after **every** P13.5 implementation slice. Do not merge
an implementation agent's work merely because its own report is green.

## Inputs

Provide to the review agent:

- repository: `kubedoio/o3k-rust`;
- issue #750 and parent #744;
- the exact P13.5 slice prompt from `docs/prompts/p13-5/`;
- PR number;
- reviewed HEAD SHA;
- base/main SHA used by the implementation;
- implementation final report;
- committed evidence paths;
- CI/protected evidence available for that HEAD.

## Part 1 — independent review prompt

```text
You are the independent reviewer for one bounded P13.5 slice in
kubedoio/o3k-rust.

Do not implement changes during the first review pass. Review the actual PR diff,
issue #750, the exact slice prompt under docs/prompts/p13-5/, AGENTS.md, ADR-0175,
SPEC-0032, contracts/iac-openstack-profile-v1.yaml, relevant canonical
resource ADRs/SPECs, existing P13 evidence, and the new committed evidence.

Treat the unmodified upstream terraform-provider-openstack 3.4.0 and OpenTofu
1.12.6 black-box behavior as the IaC compatibility client contract within the
accepted P13 profile. Canonical O3K state must remain the sole cloud authority.

Review for correctness, not stylistic preference.

Explicitly inspect:

1. Scope
   - Does the PR implement only the assigned P13.5 slice?
   - Did it silently absorb P13.6/P13.7 or new resource breadth?
   - Did it reopen R7 without a feature-driven necessity?

2. Authority
   - Did Terraform state, an OpenStack compatibility row, provider-local state,
     or a new mapping become a second canonical authority?
   - Are canonical O3K IDs/owner scope/desired state still authoritative?
   - Was any Terraform-specific table/resource/idempotency authority added?

3. Upstream-provider integrity
   - Provider is exactly upstream 3.4.0 and unmodified?
   - Import/read/replacement/retry behavior is derived from real upstream
     behavior, not assumptions or a fake provider?
   - No manual terraform.tfstate surgery is being called support?

4. State convergence
   - Stable reads really end in no-op plans?
   - Machine-readable plan assertions prove exact actions rather than only exit
     codes/log strings?
   - Native drift is observed through the compatibility projection from
     canonical state?
   - Remote absence clears provider state correctly?
   - Re-apply converges the intended canonical object only?

5. Identity and relationships
   - In-place changes preserve canonical identity?
   - True replacement uses correct old/new identity?
   - RouterInterface replacement/removal preserves Router/L3Gateway and
     Subnet/AddressRealm parents?
   - VolumeAttachment replacement/detach preserves Server and Volume parents?
   - No duplicate relationships or stale compatibility/provider objects?

6. Retry/replay, when applicable
   - Internal O3K Operation idempotency is not confused with client HTTP
     exactly-once semantics?
   - Same accepted Operation cannot duplicate provider side effects?
   - Unknown outcome is observed before unsafe retry where required?
   - Lost create response without learned resource ID is NOT falsely called a
     safety pass?
   - Fault injection is test-only and deterministic unless an accepted seam
     already existed?

7. Persistence/backends
   - SQLite/PostgreSQL externally preserve equivalent supported semantics?
   - Any SQL/query/bind/transaction/lock change is actually justified by a
     reproduced P13.5 defect and independently tested?
   - Restart reconstruction does not create a second authority?

8. Security
   - Owner-scope authorization happens before mutation?
   - Import/read does not disclose cross-project existence differently?
   - IDOR/BOLA/non-disclosure behavior remains intact?
   - No secrets appear in evidence, traces or logs?

9. Recovery/cleanup
   - Replacement/deletion removes old owned state?
   - Parent resources are retained where required?
   - No stale Operations/allocations/IPs/device attachments?
   - Foreign/unowned state remains untouched at the evidence tier claimed?

10. Architecture
    - R7 SQL, host-execution, dependency and unsafe-code guards remain intact?
    - Fixes live at the narrowest correct canonical/application/compatibility
      boundary?
    - No new generic framework/ORM/CRUD/plugin/command-template system?

11. Evidence honesty
    - Exact HEAD/toolchain/backend bound?
    - Protected/real-host evidence is called passed only if it actually ran?
    - Unsupported/not-applicable/deferred cases are not collapsed into PASS?
    - Product/compatibility status does not exceed executed evidence?

Run or verify all required validation for the touched slice. Inspect failures;
do not accept a green summary without the underlying relevant evidence.

Return findings ordered by severity:
BLOCKER / HIGH / MEDIUM / LOW / NIT.

Then return this verdict block exactly:

P13.5 slice:
Reviewed HEAD:
Review verdict: APPROVE / CHANGES REQUIRED
Scope preserved: YES / NO
Canonical authority preserved: YES / NO
Upstream provider unmodified: YES / NO
Terraform-specific authority introduced: NO / YES
State-convergence semantics proven: YES / NO / N/A
Relationship parent retention proven: YES / NO / N/A
Retry/replay boundary honest: YES / NO / N/A
Authorization/non-disclosure preserved: YES / NO
SQLite/PostgreSQL parity preserved: YES / NO / N/A
R7 architecture boundaries preserved: YES / NO
Evidence claims bounded and honest: YES / NO
P13.6/P13.7 scope kept separate: YES / NO
Merge authorized: YES / NO

Do not merge during the review pass.
```

If findings require code changes, return the PR to implementation, require a new
HEAD, rerun required gates, and independently review the changed HEAD. Never
reuse approval from an earlier SHA.

## Part 2 — protected merge prompt

Use only after the independent review says `Merge authorized: YES` and required
checks for the exact reviewed HEAD are green.

```text
Merge the reviewed P13.5 slice safely.

Repository: kubedoio/o3k-rust
PR: <PR_NUMBER>
Expected reviewed HEAD: <HEAD_SHA>

Rules:
1. Re-fetch PR metadata and confirm HEAD is exactly <HEAD_SHA>.
2. Confirm the independent review applies to this exact HEAD.
3. Confirm required protected checks/evidence for this slice are green or have
   the exact accepted environment-specific status documented by the reviewer.
4. Do not make implementation edits in the merge step.
5. Do not bypass repository rules, required checks, review requirements or
   merge-freshness protection.
6. If the branch is stale and protected policy requires update/rebase, use the
   repository's normal protected mechanism, then STOP because the HEAD changed;
   require validation/re-review of the new HEAD before merge.
7. Merge using the repository's accepted protected method.
8. Record merge SHA.
9. Fetch current origin/main and verify the merge is reachable.
10. Do not start the next slice from the old branch.

Return:
PR:
Reviewed HEAD:
Merge SHA:
Protected checks: PASS / exact status
origin/main after merge:
Issue #750 status updated if appropriate: YES / NO
Next authorized slice: <next slice / final closure review>
```

## Part 3 — next-slice handoff prompt

After the merge, the next implementation agent must begin cleanly:

```text
Proceed to the next P13.5 slice using the committed prompt under
`docs/prompts/p13-5/`.

First:
- git fetch origin
- checkout current protected main
- pull/fast-forward to exact origin/main
- verify the prerequisite P13.5 merge is reachable
- read AGENTS.md and the next slice prompt
- create a fresh branch from current main

Do not reuse the previous implementation branch or assume its base SHA is still
current.

Before editing, report:
- current main SHA
- prerequisite merge SHA
- next P13.5 slice
- proposed branch name
- exact contracts/evidence you will read
- tests you will add/run first

Then implement only that slice.
```

## Final P13.5 closure review

After P13.5F passes independent review and is merged, perform a final read-only
closure review of protected `main` before closing #750:

```text
Review current protected main against the full P13.5 prompt set and issue #750.
Do not implement during this pass.

Verify:
- A–F merged and reachable;
- exact aggregate P13.5 evidence is current-main bound or explicitly run-bound;
- all accepted matrix cells have honest states;
- upstream provider 3.4.0 remained unmodified;
- OpenTofu 1.12.6 mandatory target passed;
- no Terraform-specific canonical authority exists;
- stable read/import/native drift/deletion/replacement/retry/replay claims are
  supported by executable evidence;
- RouterInterface and VolumeAttachment parent-retention claims are supported;
- SQLite/PostgreSQL parity claim is supported;
- lost-create-response ambiguity is not falsely marked exactly-once;
- P13.6 multi-project/failure work remains open/unproven;
- P13.7 full-stack/product-profile closure remains open/unproven;
- compatibility/status docs match evidence;
- R7 architecture guards remain green.

Return findings by severity and then:

P13.5 final main SHA:
P13.5 aggregate verdict: PASS / BLOCKED
Issue #750 closure authorized: YES / NO
P13 umbrella may proceed to P13.6: YES / NO
Remaining blockers/deviations:
```

Close #750 only after this final closure review authorizes it.
