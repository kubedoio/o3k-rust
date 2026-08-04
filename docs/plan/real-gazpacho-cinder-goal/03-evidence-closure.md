# Goal Part 3/3 — Tempest, Evidence, Closure

Goal: Execute and Prove the Real Gazpacho Cinder Service-Testbed Profile.
This is file 3 of 3: `03-evidence-closure.md` (sections N–T). See `01-goal-and-audit.md` (A–D) and `02-protected-runner-and-execution.md` (E–M).

## N. Phase 12 — Execute real Tempest evidence

The current Tempest script only writes `NOT_READY`; replace this behavior with a real pinned execution path.

Pin:

```text
cinder-tempest-plugin 1.21.0
```

and a compatible pinned Tempest revision. Create an explicit allowlist of test IDs matching only the accepted service-testbed profile.

Run the selected tests against real O3K Identity, real O3K Nova attachment API, real external Cinder, and the real compute attachment path where applicable.

Produce:

- JUnit XML;
- exact Tempest revision;
- exact plugin revision;
- exact selected test IDs;
- passed/failed/skipped counts;
- skip reasons;
- profile ID;
- O3K source commit;
- Cinder version;
- redacted logs.

Every skip must map to an explicit unsupported operation. Do not make the suite green using broad regex exclusions or expected failures.

## O. Phase 13 — Evidence artifacts

Upload separate machine-readable artifacts:

```text
real-cinder-environment.json
keystone-hosted-service-result.json
real-volume-lifecycle.json
nova-cinder-attachment-result.json
compute-block-device-result.json
guest-device-observation.json
attachment-restart-recovery.json
real-cinder-cleanup-result.json
foreign-state-result.json
tempest-cinder-results.xml
tempest-cinder-summary.json
real-cinder-workflow-result.json
```

The aggregate workflow may report `passed` only when all required real-service and real-compute artifacts pass.

Keep evidence classifications separate:

```text
portable
component-mock
real-service
real-compute
tempest
```

## P. Phase 14 — Issue closure

After successful protected evidence:

Close #420 only when real Cinder service authentication works; real Cinder validates O3K tokens; the durable catalog works; restart persistence is demonstrated; isolation and redaction pass.

Close #421 only when public Nova attach/detach works; the typed Cinder client is used; the real compute-agent and libvirt path is used; restart and idempotency tests pass; no attachment/device leak remains.

Close #424 only when an actual pinned Tempest subset runs; JUnit and machine-readable evidence exist; skip mapping is honest.

Close #429 only when real Gazpacho Cinder starts; a real volume lifecycle passes; attachment occurs through O3K Nova and compute; guest or accepted host observation proves the device; detach/delete cleanup passes; foreign state remains unchanged.

Update #432 with:

```yaml
issue:
implementation:
portable_evidence:
component_evidence:
real_service_evidence:
real_compute_evidence:
tempest_evidence:
closure:
artifact_links:
```

Do not close an issue because its implementation exists without the required evidence.

## Q. Required repository checks

Run at minimum:

```bash
actionlint
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features --profile pr
cargo test --workspace --all-features
cargo test --workspace --doc --all-features
bash tests/adr-governance.sh
bash tests/compatibility-target.sh
bash tests/ci-workflow.sh
bash scripts/component-cinder-mock.sh
```

Also run focused: attachment-orchestrator tests; Cinder client tests; identity restart tests; agent block-device tests; libvirt disk tests; workflow guard tests; secret scans.

## R. PR strategy

Use a small number of coherent PRs:

1. Gazpacho version/profile correction and protected workflow hardening.
2. Real Nova-to-compute attachment runner integration.
3. Defects discovered by the first protected run, grouped by one actual root cause.
4. Real Tempest execution and final evidence/closure.

Do not put unrelated API, native Cinder, edge-cloud, PostgreSQL, or general release work into these PRs. Use independent implementation, OpenStack compatibility, libvirt/storage, security, workflow, and evidence-review subagents.

## S. Stop conditions

Stop with a precise blocker report when:

- the protected runner lacks required KVM/libvirt/LVM/iSCSI capabilities;
- Cinder 28.0.0 cannot be installed using the accepted reproducible method;
- the runner cannot safely isolate Cinder's host dependencies;
- a Cinder API behavior conflicts with the frozen O3K profile;
- the real Cinder dependency is unavailable;
- host cleanup cannot be proven safe.

Do not silently fall back to Cinder 24.2.0, mock Cinder, direct Cinder attachment calls, fake connectors, fake Nova servers, host-only database records, or `NOT_READY` Tempest evidence.

Blocker report format:

```yaml
phase:
expected:
observed:
first_failing_command:
service_boundary:
logs:
owned_resources_remaining:
foreign_state_changed:
recommended_next_action:
```

## T. Completion condition

This goal is complete only when the evidence-backed answer to the following question is yes:

> Can a real Gazpacho Cinder deployment authenticate through O3K, discover its endpoint, create a real volume, attach it through O3K's public Nova API to an O3K-managed libvirt guest using the real compute agent, prove the device exists, detach it, delete all resources, preserve foreign state, and pass the selected Tempest subset?

The required answer is `Yes`.
