# Prompt — P13.5E: Retry, Replay, and Unknown-Outcome Convergence

## Task

Implement **P13.5E** for issue #750 after P13.5D is independently reviewed and
merged.

P13.5E proves bounded retry/replay safety between the unmodified upstream
OpenStack provider and O3K's durable Operation model without falsely claiming
client-level exactly-once creation where the OpenStack protocol supplies no
durable client token/resource ID after a lost create response.

## Repository and branch discipline

Repository: `kubedoio/o3k-rust`

Before editing:

1. Read `AGENTS.md`.
2. Fetch current protected `origin/main`.
3. Verify P13.5A–D are merged and reachable.
4. Create a new branch from current `origin/main`.
5. Record exact starting main SHA.

Suggested branch: `p13-5e-retry-replay`

## Mandatory reading

Read:

- `docs/prompts/p13-5/README.md`
- merged P13.5A retry matrix
- P13.5B/C/D evidence
- ADR-0175 / SPEC-0032
- SPEC-0021 cross-service workflows and compensation
- durable Operation/idempotency/replay/fencing contracts and tests from P12
- P13.4 storage workflow unknown-outcome/observation-before-retry evidence
- upstream provider 3.4.0 retry behavior and Gophercloud behavior relevant to
  the frozen scenarios
- issue #750
- P13.6 scope in #744 / implementation plan, especially the explicit
  lost-create-response ambiguity

## Objective

Prove that safe upstream-client retries and repeated reads/updates/deletes
converge without duplicating one already-accepted O3K Operation's provider side
effects.

Maintain this strict distinction:

```text
O3K internal Operation idempotency
!=
client-level exactly-once HTTP create
```

P13.5E MUST NOT claim that a create whose successful HTTP response is lost
before Terraform learns the new resource ID is exactly-once safe. That explicit
ambiguous case remains P13.6 evidence/documentation unless an accepted protocol
change provides a real durable client identity.

## Fault-injection architecture

Prefer a **test-only HTTP fault proxy** between provider and O3K:

```text
OpenTofu
  -> terraform-provider-openstack 3.4.0 (unmodified)
  -> test-only deterministic fault proxy
  -> o3kd
```

The proxy should be owned by tests/scripts, not production service code, unless
there is already an accepted generic test transport seam that can express the
same behavior without semantic risk.

Do not scatter production environment-variable failpoints through compatibility
handlers merely to make this test easy.

The fault proxy should support deterministic one-shot or counted faults such as:

- 503 before request forwarding;
- connection reset before forwarding;
- delayed response/timeout;
- successful GET forwarded but response dropped;
- successful idempotent/read operation response dropped;
- successful update response dropped after O3K commits;
- successful delete response dropped after O3K commits.

Record exactly whether the request reached O3K and whether O3K committed before
the client fault was injected.

## Scenario E1 — read retry

For representative list/show/read routes across Compute, Network and Storage:

1. Establish canonical resource.
2. Drop/fail one provider read response according to the frozen upstream retry
   behavior.
3. Let the upstream provider retry or rerun the operation as appropriate.
4. Verify no canonical mutation occurred.
5. Verify final Terraform state is correct and plan is no-op.

Reads must remain side-effect free.

## Scenario E2 — update rejected before commit

Inject a failure before O3K receives/accepts the update.

Prove:

- canonical desired state remains unchanged after failed attempt;
- retry applies the update once;
- one correct canonical resource remains;
- final plan is no-op.

## Scenario E3 — update committed, response lost

For representative mutable resources:

1. Forward update to O3K.
2. Verify O3K accepts/commits canonical Operation/state transition.
3. Drop the successful HTTP response.
4. Exercise the actual upstream-provider/client retry/read behavior.
5. Verify the same canonical resource converges.
6. Verify no duplicate provider/host side effect for the same accepted O3K
   Operation.
7. Final normal plan must be no-op.

Where a retried HTTP request legitimately creates a *new* O3K Operation because
OpenStack carries no stable request token, do not blur that into internal
exactly-once evidence. The acceptance claim is that observation/current-state
semantics prevent unsafe duplicate provider effects where the existing O3K
workflow guarantees that property.

## Scenario E4 — delete committed, response lost

For representative independently managed resources and relationship removals:

1. Forward DELETE/detach and allow canonical deletion/finalization to commit.
2. Drop response.
3. Exercise provider retry/read behavior.
4. Provider must converge on remote absence using accepted 404/not-found/delete
   semantics.
5. No deleted resource may resurrect.
6. Parent retention for RouterInterface/VolumeAttachment cases must remain as
   proven in P13.5D.
7. No second destructive provider side effect may target unrelated resources.

## Scenario E5 — same accepted internal Operation replay

Using the existing canonical O3K operation/replay interfaces and tests, prove
for representative Compute, Network and Storage mutations that replay of the
same already-accepted internal Operation identity results in:

```text
one accepted Operation
-> at most one intended provider mutation
-> observation/replay returns canonical outcome
```

Cover unknown-outcome observation before retry where the relevant workflow
supports it.

Do not weaken generation/controller/agent/session fencing to make replay pass.

## Scenario E6 — relationship replay

At minimum cover one high-risk relationship operation:

- RouterInterface add/remove or equivalent canonical gateway attachment; and/or
- VolumeAttachment attach/detach.

Verify repeated/replayed accepted relationship Operation does not duplicate the
relation, duplicate provider attachment, or delete/recreate parents.

## Explicitly ambiguous create-response-loss case

P13.5E may build the proxy support needed for later use, but it MUST classify
this case honestly:

```text
POST create reaches O3K
O3K successfully creates canonical resource
success response is lost
Terraform/provider never learns resource ID
```

Unless the existing upstream OpenStack request has a durable identifier that
O3K can legitimately use for canonical create correlation, this case is NOT a
P13.5 exactly-once pass.

Record it as `AMBIGUOUS_CLIENT_CREATE_RESPONSE_LOSS` and leave the explicit
failure-matrix treatment to P13.6 as already planned.

Do not add a hidden Terraform-specific idempotency token/table to “solve” it.

## Retry budgets and error semantics

Do not hide defects behind broad retries.

Preserve:

- existing status/error mapping required by upstream provider/Gophercloud;
- bounded retry behavior;
- conflict/not-found classification;
- timeout and unknown-outcome distinction;
- authorization before mutation;
- audit/correlation identity;
- non-disclosure.

If the upstream provider does not automatically retry a particular mutating
request, the harness may reproduce a second client attempt only if that
behavior is clearly distinguished from provider-native automatic retry.

## Evidence

For each fault scenario record:

```text
resource
scenario
fault_location: before_forward | after_commit_before_response | read_response_drop | ...
request_reached_o3k
canonical_operation_id(s)
canonical_resource_id(s)
provider_mutation_count where observable
client_attempt_count
observations_before_retry
final_resource_count
parent_ids_before/after for relations
final_plan_noop
result: PASS | EXPECTED_AMBIGUOUS | FAIL
backend
head_sha
toolchain provenance
```

The evidence must not label `EXPECTED_AMBIGUOUS` as PASS.

## Explicit non-scope

Do NOT implement:

- a new public idempotency extension/header for Terraform;
- a provider fork;
- a Terraform-specific canonical store;
- P13.6 full controller crash/restart matrix;
- P13.6 two-project isolation;
- network/storage/compute feature breadth;
- HA/SLA claims.

## Validation

Run permanent guards plus:

- all P13.5A–D regressions;
- fault-proxy unit/self-tests proving each fault occurs at the intended point;
- representative retry/replay integration scenarios;
- existing Operation/recovery/idempotency tests for touched subsystems;
- SQLite/PostgreSQL conformance when persistence/operation semantics change;
- applicable protected real-host evidence if a provider-side-effect count/cleanup
  claim requires it.

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
P13.5E branch:
P13.5E HEAD:
Starting main SHA:
Fault proxy implemented test-only: YES / NO
Read retry safety: PASS / findings
Pre-commit update retry: PASS / findings
Post-commit update response-loss convergence: PASS / findings
Post-commit delete response-loss convergence: PASS / findings
Same accepted Operation duplicate provider mutations: 0 / findings
Relationship replay: PASS / findings
Client-lost-create-response exactly-once claimed: NO
Ambiguous create-response-loss classification preserved: YES / NO
Provider modified: NO
Terraform-specific authority introduced: NO
Retry/error semantics changed: <exact list, or NONE>
SQLite evidence:
PostgreSQL evidence:
Protected/real-host evidence:
Known limitations:
Recommended next slice: P13.5F / STOP
```

Do not merge. Leave ready for independent review.
