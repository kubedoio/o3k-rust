# v0.2.0-alpha.1 release tracker

<!-- tracker-contract
owner_issue: 94
release_issue: 93
program_status: blocked
closure_decision: pending
-->

This file is the source-controlled status record for issue #54. A merged PR
means the scoped implementation and repository gates passed; it does not mean
the real-libvirt acceptance evidence exists. The tracker contract is checked
by `packaging/validate-program-tracker.sh`; it must remain blocked and pending
until the independent evidence and decisions listed below exist. The validator
also requires a row for every issue in #94's closure chain (#76–#84 and
#86–#94); PR #85 is tracked as a prerequisite in the issue, not as a closure
row. This is a source-document completeness check, not host evidence.

Closure-chain rows keep their closure-evidence marker as `pending` until
program closure: individual issue closures and their evidence artifacts are
recorded in each row's evidence prose and in the issues themselves, while the
marker flips only when the program closes.

| Issue | Scope | Repository state | Real-host evidence |
|---:|---|---|---|
| #36 | libvirt/KVM direction | merged | pending |
| #37 | compute-agent protocol | merged | accepted contract; protocol tests passed; runtime and host security evidence remain pending |
| #38 | secure registration/heartbeat | merged | durable validated agent administrative state, registration-before-heartbeat sequencing, and desired-state acknowledgements added; host deployment and process-level restart evidence pending |
| #39 | local libvirt adapter | merged | provider-backed readiness and vCPU/KVM capability projection added; host libvirt pending |
| #40 | domain XML/ownership/discovery | merged | deterministic ownership discovery now quarantines all duplicate IDs, rejects unsafe source inputs, and validates ownership metadata before managed-domain listing; host discovery pending |
| #41 | image cache/overlays | merged | atomic publication, cache-hit checksum revalidation, and verified project-scoped artifact resolution added; host qemu-img evidence pending |
| #42 | config-drive | merged | atomic replacement publication, bounded network/vendor payloads, and optional read-only libvirt attachment added; Nova wiring and guest cloud-init evidence pending |
| #43 | Placement | merged | unique atomic state publication, allocation-preserving provider synchronization, refresh and re-registration usage reconciliation added; daemon capability publication and disk-capacity policy pending |
| #44 | scheduler | merged | agent-targeted scheduling and registry eligibility gate added; duplicate-name conflicts now roll back newly reserved Placement allocations; production inventory wiring and integration dispatch pending |
| #45 | bridge/TAP | merged | TAP reuse ownership fencing, existing bridge/uplink validation, ownership-checked deletion, unique network metadata publication, and validated libvirt TAP attachment added; host execution/guest network evidence pending |
| #46 | DHCP/fixed IP | merged | atomic publication, owned dnsmasq supervision, and gateway/binding conflict rejection added; TAP/dnsmasq/guest IP evidence pending |
| #47 | libvirt lifecycle backend | merged | distinct CellHV provider selection, durable journal, command router, scheduler binding, canonical create builder, durable agent-event reconciliation, live control-plane event consumer, fake command realization, typed resolved create inputs, lifecycle ownership fencing, and fail-closed libvirt-provider validation for unsupported network inputs added; full network/TAP, artifact, config-drive, agent-backed create realization, and host evidence remain coupled follow-ups |
| #48 | console log | merged | bounded API reads, fenced agent routing, durable observations, serialized per-instance writes, atomic writes, serial-console XML, and libvirt stream reads added; guest boot evidence pending |
| #49 | real-libvirt harness | merged | prerequisite validation invokes the public OpenStack lifecycle workflow with artifact validation; isolated guest/failure scenarios and trusted-host evidence remain pending |
| #50 | OpenStack CLI workflow | merged | local guest-image upload, waited lifecycle transitions, bounded console polling, failure cleanup, validated show/list identity, verified deletion, and redacted evidence with discarded raw CLI errors; stronger script evidence does not replace a trusted real CLI/libvirt run |
| #51 | clean-host packaging | merged | release bundles now install their bundled `bin/o3kd`/`bin/o3k-compute` binaries without Cargo; uninstall removes the complete O3K helper set while retaining state by default and restricts systemd cleanup to the exact default layout, libvirt profile selects libvirt, and reset stops both control-plane and compute services; Ubuntu/Debian clean installs pending |
| #52 | measurements / release-evidence integrity | merged | fake control-plane measurement now preflights the selected port, verifies launched-PID ownership through readiness/token/RSS checkpoints, emits redacted diagnostics for port/child failures, and binds summary to canonical raw JSON; fake artifacts are explicitly non-release evidence; guest metrics pending |
| #53 | release gate | merged | benchmark and cleanup evidence are required; positive, non-future, seven-day freshness checks now reject stale artifacts; gate remains blocked by real-host rows above |
| #54 | program tracker | merged; protected run 30717871057 recorded against main 08e586e | keypair lifecycle passes on runner-2404; next blocker is `GET /v2.1/bootstrap-project/flavors -> HTTP 405`; full host acceptance remains open |
| #94 | program closure and provenance tracker | repository contract guarded; ADR-0100; compatibility evidence follow-up #346 is published in draft PR #347 with all 34 required baseline fixtures and explicit non-claiming protected evidence | blocked until every required real-host artifact and independent human decision is recorded; no program closure is claimed; closure evidence: pending |
| #77 | protected real-host validation | repository implementation complete; ADR-0084/0103, protected workflow guards, 14-day redacted artifact retention, and portable tests added | host-gated: protected environment, exact labeled runner, configured TestLab/libvirt host, credentials, image, and a passing manual run remain required; closure evidence: pending |
| #76 | protected runner capability probe | repository implementation complete; ADR-0085/0102, redacted atomically published artifact fenced to workflow run/attempt/source commit, workflow preflight, and portable fake-command tests added | host-gated: dedicated non-root labeled runner must produce `status: passed`; repository work does not claim host acceptance; closure evidence: pending |
| #78 | durable o3kd-to-compute-agent command and observation path | repository runtime now selects an authenticated agent, persists command/operation identity before dispatch, fences agent epochs, journals accepted commands, replays safely after restart, enforces retry budgets, and projects observations; ADR-0114/0141/0148 and regression coverage preserve stream identity, operation progress, duplicate-delivery idempotency, and unknown-outcome recovery; normal CI also runs `cargo test --workspace --all-features` via ADR-0133 | host-gated: a protected current-commit run must still report `status: passed` with real mTLS command transitions; guest image/config-drive/network realization and full guest acceptance remain later issues; closure evidence: pending |
| #79 | image cache and overlay safety boundary | repository safety boundary plus managed-directory fencing, exact committed-artifact identity lookup, deterministic kind-aware transfer IDs, agent-local verified base publication, instance-owned qcow2 overlay materialization, durable overlay ownership/reference records, restart/epoch fencing, and qemu-img regression coverage complete in draft PR #342; ADR-0087/0104/0115/0135/0142/0147/0155 | blocked until the complete control-plane-to-agent create wiring, protected Glance upload, real-host qemu-img backing-chain evidence, and cleanup artifact exist; no host evidence claimed; closure evidence: pending |
| #80 | config-drive attachment | repository failure cleanup, manifest integrity, digest-bound libvirt attachment, and explicit API rejection of unsupported `config_drive: true` requests complete; ADR-0088/0105/0055/0116/0149 and regression coverage remove unpublished temporaries, reject altered or ambiguous artifacts before XML generation, and prevent silent API omission | blocked until deterministic ISO/VFAT media, Nova/agent artifact wiring, guest cloud-init consumption, reboot evidence, and trusted real-host evidence exist; closure evidence: pending |
| #81 | Neutron/TAP/bridge/DHCP lifecycle | repository link-kind, bounded rollback, authoritative deterministic port-MAC binding, explicit managed dnsmasq lease path, duplicate-MAC fencing, and the promoted durable host-ownership/TAP/DHCP reconciliation primitives are merged; ADR-0089/0106/0117/0140, migration/API coverage, deterministic failure-injection coverage, and reverse-order cleanup of O3K-created bridge/TAP resources remain covered | blocked until agent-backed create, real TAP/bridge/libvirt/DHCP orchestration, guest fixed-IP evidence, cleanup/restart evidence, and trusted real-host artifact exist; no host acceptance claimed; closure evidence: pending |
| #82 | Placement scheduling and allocations | draft PR #344 adds a typed durable allocation-intent journal with idempotent commit/retry, scheduler intent fencing, and restart-safe orphan reconciliation; repository publication rollback, post-delete release-retry, pre-journal create-conflict boundaries, pre-reservation duplicate-name fencing, daemon Placement/scheduler/agent-registry wiring, and authenticated agent inventory publication remain covered | blocked until the intent boundary is wired into compute lifecycle, agent-backed create/delete dispatch, real guest allocation evidence, restart recovery, and trusted real-host scheduling artifact exist; no host acceptance claimed; closure evidence: pending |
| #83 | libvirt lifecycle and observed Nova state | repository observed-state projection and agent observation-state propagation now also enforce live agent epoch plus durable per-resource observation sequence ordering; ADR-0091/0119/0144 and regression coverage reject delayed state regressions while successful command observations carry explicit resource state | blocked until agent-backed lifecycle dispatch, real Nova/guest lifecycle evidence, restart/failure recovery, and trusted real-host artifact exist; no host acceptance claimed; closure evidence: pending |
| #84 | libvirt serial console and Nova console-log | repository console ownership fence, private bounded storage, explicit oversized/non-regular rejection, successful-delete console cleanup, registered-agent durable-cache fallback, and direct durable-cache routing for nonzero offsets complete; ADR-0092/0110/0126/0047/0143/0145 and regression coverage require matching O3K domain metadata, preserve artifacts on failed deletion, make repeated cleanup safe, and retain bounded console output across agent-stream loss | blocked until actual CirrOS output, bounded restart persistence on a real host, cross-project CLI isolation, deletion evidence, and trusted real-host console artifact exist; no host acceptance claimed; closure evidence: pending |
| #86 | complete real CirrOS OpenStack CLI acceptance | repository-owned keypair import/list/show/delete and cleanup passed in protected run 30717871057 on runner-2404; the harness still requires redacted initial and post-reboot ACTIVE/fixed-IP/config-drive evidence plus a console marker and uses direct `OS_*` configuration without generated credential YAML; ADR-0093 and stateful no-op/special-character regression coverage added | blocked until flavor discovery and subsequent real CirrOS ACTIVE/config-drive/console/restart/leak evidence pass; no host acceptance claimed; closure evidence: pending |
| #87 | real-host failure injection and unknown-outcome recovery | repository action-recovery boundary complete; ADR-0094, deterministic timeout and partial-create recovery, fingerprinted duplicate replay, durable accepted/running and terminal-failure recovery transitions, observation-based lifecycle convergence tests, and per-scenario evidence-shape validation added | executed on the trusted KVM/libvirt host (nkudo-vm1) against 8e532ea: all required crash, timeout, duplicate, partial-completion, corruption, disk-full, cleanup, and aggregate scenarios executed with injections observed and recovery verified, 19/19 gate scenarios passed (issue #87 closed); aggregate failure-recovery.json reports status: passed; closure evidence: pending |
| #88 | real-host resource leak and foreign-state guard | repository race-safe inventory boundary complete; ADR-0095, stable two-read snapshots, atomic redacted inventory publication, public-CLI keypair inventory, validated O3K-owned network-link inventory, foreign-state digests, and `resource-leak-result.json` output added | executed and closed at 519517e5bd217b08ba1b5e6957f996040fd8fbac on the trusted KVM/libvirt host (nkudo-vm1): the independent verifier (inventory schema v3, compare/negative/aggregate, ADR-0164) ran around the normal CirrOS E2E and the complete #87 failure-injection suite (19 gate scenarios + supplementary artifact-replay + agent-kill variant, 22/22 verdicts passed, zero owned leaks / inconsistencies / foreign changes); stale-artifact and foreign-mutation negatives detected as expected; aggregate target/real-host-workflow-artifacts/resource-leak-result.json reports status: passed (issue #88 closed); closure evidence: pending |
| #89 | clean Ubuntu install and full TestLab lifecycle | repository clean-install tooling merged (provider selection, config-dir traversal, compute disk capacity, polkit rule, install/reset/uninstall/purge paths); portable packaging tests cover path and ownership safety | executed on the trusted KVM/libvirt host (nkudo-vm1) from a clean Ubuntu 24.04: install from release-candidate instructions, bootstrap, full real CirrOS E2E, failure/recovery smoke, reset, reinstall, E2E rerun, uninstall, purge; target/real-host-workflow-artifacts/clean-ubuntu/clean-ubuntu-install.json reports status: passed (issue #89 closed); closure evidence: pending |
| #90 | clean Debian install and full TestLab lifecycle | same clean-install tooling as #89 | executed on the trusted KVM/libvirt host (nkudo-vm1) from a clean Debian 12 (bookworm): install from release-candidate instructions, bootstrap, full real CirrOS E2E, failure/recovery smoke, reset, reinstall, E2E rerun, uninstall, purge; target/real-host-workflow-artifacts/clean-debian/clean-debian-install.json reports status: passed (issue #90 closed); closure evidence: pending |
| #91 | measure real libvirt TestLab footprint and lifecycle latency | repository benchmark tooling merged with summary-to-raw canonical binding (raw_sha256) and release-gate eligibility checks | executed on the trusted KVM/libvirt host (nkudo-vm1): binary/bundle size, startup/readiness, RSS/CPU, token/API latency, upload/cache/overlay, scheduling/dispatch, guest boot, restart/reconciliation, lifecycle, cleanup, and repeated growth/leak behavior measured; target/real-host-workflow-artifacts/benchmark/real-libvirt-benchmark.json reports status: measured with release_eligible: true (issue #91 closed); closure evidence: pending |
| #92 | human architecture and security review of the libvirt alpha | review package contract and validator merged (ADR-0099, packaging/validate-human-review.sh); candidate package regenerated for the frozen release candidate commit | blocked until a real non-LLM reviewer approves the candidate package (reviewer.is_implementing_agent must be false; reviewed_commit must equal the frozen candidate commit; findings carry severity and disposition; destructive-cleanup and foreign-state safeguards require explicit approval); package: target/real-host-workflow-artifacts/human-review/HUMAN-REVIEW-PACKAGE.md; closure evidence: pending |
| #93 | pass the release gate and publish v0.2.0-alpha.1 | release gate tooling merged (packaging/release-gate.sh with schema-bound E2E evidence and the human-review gate); verdicts recorded at target/real-host-workflow-artifacts/release-gate.json | blocked: release gate reports blocked with the human review (issue #92) as the only missing input; no release-ready claim is made; tag creation remains an explicit operator action after status: ready; closure evidence: pending |

## Decision log

- ADR-0100 (program closure and provenance tracker) establishes this tracker
  and its closure chain.
- ADR-0099 (human architecture/security review evidence package) requires a
  real non-LLM reviewer with `is_implementing_agent: false`.
- ADR-0164 (independent real-host leak and foreign-state verifier) provides
  the #88 resource-leak evidence.
- issue #93 owns release-gate execution and publication; readiness is decided
  by `packaging/release-gate.sh`, not by this document.
- Closure-chain rows keep the marker `closure evidence: pending` until program
  closure; issue closures and their evidence artifacts are recorded in each
  row's evidence prose.

## Evidence required to close the program

- real CirrOS libvirt E2E:
  `target/real-host-workflow-artifacts/leak-final-e2e/openstack-cli-result.json`
  (accepted by the release gate).
- failure/recovery matrix:
  `target/real-host-workflow-artifacts/failure-recovery.json` (status: passed).
- resource-leak/foreign-state verifier:
  `target/real-host-workflow-artifacts/resource-leak-result.json`
  (status: passed).
- clean Ubuntu install:
  `target/real-host-workflow-artifacts/clean-ubuntu/clean-ubuntu-install.json`
  (status: passed).
- clean Debian install:
  `target/real-host-workflow-artifacts/clean-debian/clean-debian-install.json`
  (status: passed).
- benchmark:
  `target/real-host-workflow-artifacts/benchmark/real-libvirt-benchmark.json`
  plus canonical raw (status: measured).
- human architecture/security review: pending (issue #92).