# PR55 / O3K Rust current-status report

**Report date:** 2026-08-01  
**Repository:** `kubedoio/o3k-rust`  
**Current main analyzed:** [`08e586e`](https://github.com/kubedoio/o3k-rust/commit/08e586e3abc0b342ad030cff7379c71483f76f60)
**Release target:** `v0.2.0-alpha.1` libvirt TestLab  

## Executive summary

The repository contains a large amount of repository-side implementation, safety hardening, test-contract work, and release-evidence validation. The open release program is still **not real-host verified** and no alpha release claim is justified.

## Latest protected-run evidence

Run [`30717871057`](https://github.com/kubedoio/o3k-rust/actions/runs/30717871057)
executed from `main` at source commit `08e586e` on runner `runner-2404`.
Bootstrap authenticated with the generated ephemeral credential; the runner
capabilities artifact reported `passed`, the pre-run inventory guard was ready,
cleanup passed, and all redacted artifacts uploaded successfully. The keypair
import/list/show/delete portion completed and the lifecycle advanced to flavor
discovery. The next failure was:

```text
GET /v2.1/bootstrap-project/flavors -> HTTP 405 Method Not Allowed
```

The redacted `openstack-cli-result.json` and `libvirt-result.json` reported
`status: failed`; `real-host-workflow-result.json` and
`resource-leak-result.json` reported failed lifecycle evidence with no foreign
state change and no remaining managed resources. This is the authoritative next
blocker; run `30715574164` is superseded for keypair diagnosis.

Issues #280 and #282 delivered the focused Nova keypair compatibility slice.
Issue #86 remains open because its required full real-host artifact is not
`passed`.

The immediate blocker is now Nova flavor collection. The runner, KVM, libvirt,
bootstrap, authentication, keypair lifecycle, and cleanup paths have all been
exercised; no release or full CirrOS acceptance claim is made.

```text
(Line: 78, Col: 11): 'OS_PASSWORD' is already defined
```

The prior workflow parse failure is superseded by protected execution.


## Current repository state

- No open pull request was found at the time of this report update.
- Issues #76 through #94 remain open.
- Repository-side slices for the compute-agent protocol, Placement, image cache, config-drive safety, network ownership, lifecycle observation, console storage, recovery, packaging, evidence schemas, and release guards have been merged.
- The repository has extensive fail-closed evidence validation, but this is not a substitute for executing the protected workflow on the QEMU/KVM server.
- README/release status must remain pre-alpha until real-host and human-review gates pass.

## Immediate CI root cause

File:

```text
.github/workflows/real-host-validation.yml
```

Affected step:

```yaml
- name: Run public real-host lifecycle
  env:
    ...
    OS_PASSWORD: ${{ secrets.O3K_TESTLAB_OS_PASSWORD }}
    ...
    OS_PASSWORD: ${{ secrets.O3K_TESTLAB_OS_PASSWORD }}
```

YAML mappings cannot contain the same key twice for GitHub Actions workflow validation. This is a workflow-definition failure, not an authentication failure and not a runner failure.

The existing `tests/ci-workflow.sh` validates only `.github/workflows/ci.yml` through text assertions. It does not parse every workflow and therefore did not detect the duplicate key in `real-host-validation.yml`.

## Required corrective phase — workflow recovery

Before resuming issue #76 or any later issue:

1. Create a dedicated branch for the workflow fix.
2. Remove the duplicate `OS_PASSWORD` entry and keep exactly one secret mapping in each step that needs it.
3. Add `actionlint` or an equivalent real GitHub Actions workflow parser to portable CI.
4. Validate every file under `.github/workflows/*.yml` and `.github/workflows/*.yaml`.
5. Add a regression test proving duplicate YAML keys make CI fail.
6. Ensure no secret value is printed during validation or runtime.
7. Open one focused PR, run independent review, fix findings, and merge only with green CI.
8. Manually dispatch the protected workflow on the labeled self-hosted runner.
9. Upload and inspect machine-readable runner/workflow artifacts.
10. Close #76/#77 only if their required artifacts report `passed`.

Do not begin speculative repository hardening while the workflow cannot be parsed.

## Open issue dependency status

### Blocked at workflow layer

- **#76 — runner provisioning:** implementation may exist, but no accepted runner-capabilities artifact can be generated while the workflow is invalid.
- **#77 — protected workflow:** not complete; the workflow currently cannot be loaded by GitHub Actions.

### Blocked behind #77 real execution

- **#78:** complete real `o3kd → o3k-compute` mTLS command/observation execution.
- **#79:** authenticated image transfer and real compute-host qcow2 evidence.
- **#80:** real config-drive media, attachment, and guest consumption.
- **#81:** production TAP/bridge/libvirt NIC/dnsmasq orchestration and guest fixed IP.
- **#82:** real agent-backed scheduling, allocation lifecycle, and restart evidence.
- **#83:** complete real domain assembly and observed Nova lifecycle.
- **#84:** actual CirrOS serial output through the Nova console-log API.
- **#86:** complete public OpenStack CLI CirrOS acceptance workflow.
- **#87:** protected real-host failure injection.
- **#88:** independent real-host leak and foreign-state verification.
- **#89/#90:** clean Ubuntu and Debian installation evidence.
- **#91:** measured real-libvirt benchmark evidence.
- **#92:** independent non-LLM human architecture/security approval.
- **#93:** signed and verified alpha release after every gate passes.
- **#94:** authoritative program tracker; must remain fail-closed.

## Correct execution order from the current state

```text
Workflow syntax and validation fix
  ↓
#76 runner capabilities evidence
  ↓
#77 protected workflow evidence
  ↓
#78 real compute-agent command path
  ↓
(#79 image, #81 network, #82 Placement)
  ↓
#80 config-drive
  ↓
#83 complete libvirt guest lifecycle
  ↓
#84 real console
  ↓
#86 public CLI E2E
  ↓
(#87 recovery, #88 leak guard, #91 measurements)
  ↓
(#89 Ubuntu, #90 Debian)
  ↓
#92 independent human review
  ↓
#93 release
```

## Rules for the next LLM goal

The next goal must be recovery-oriented rather than open-ended.

- Fix the workflow parser failure first and verify it with a real parser.
- Work on only the first unblocked dependency.
- Do not create dozens of speculative safety PRs when the current gate cannot execute.
- Use a dedicated branch and focused PR for each issue.
- Use implementation and review subagents, but do not treat an LLM review as the required human approval for #92.
- Never duplicate environment keys or define the same credential in multiple places in one step.
- Centralize repeated non-secret OpenStack values at job level when appropriate; keep secrets at the narrowest required step scope.
- A merged script is not evidence. Real-host issues close only with `passed`, benchmarks with `measured`, and #92 with independent human `approved` evidence.
- After each merge, re-read the current workflow and tracker before selecting the next issue.
- Stop and report the exact blocker when required infrastructure, secret, runner label, or human review is unavailable; do not invent a passing artifact.

## Release truth

At this point:

```text
repository implementation: substantial
portable CI contracts: substantial
workflow parser status: failed
protected runner execution: not reached
real CirrOS boot evidence: missing
clean-install evidence: missing
human approval: missing
release status: blocked
```

No `v0.2.0-alpha.1` tag should be created until the workflow parses, all real-host artifacts pass, and the independent human review is approved.

## Report provenance

This report was updated from the current GitHub repository state, open issues #76–#94, the current workflow source, merged PR history, and the existing release tracker. It makes no claim of host acceptance, human approval, signing, or release readiness.
