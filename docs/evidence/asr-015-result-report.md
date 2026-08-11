# ASR-015 Result Report

## Result

ASR-015 is **not closed**. Portable epoch/reconnect protection passes, but the
required real-host crash-window proof was not completed.

## Source and ownership

- Repository: `kubedoio/o3k-rust`
- Source SHA: `633f8cb49f225394968bc90c8b2124257f28ffad`
- Owning issue: #83
- Related closed issues: #87 and #575
- Product profile: `native-rust-testlab`
- Runtime code changed: no
- PR created: no

## Corrected host finding

The initial report incorrectly said that KVM was unavailable. That check ran in
the restricted agent namespace, where `/dev/kvm` was not visible.

The host-level check later confirmed:

- `/dev/kvm` exists with mode `0660`;
- `qemu:///system` is reachable;
- libvirt version: `10.0.0`;
- QEMU version: `8.2.2`;
- kernel: `6.8.0-110-generic`;
- passwordless root access is available.

Therefore, host prerequisites passed. The KVM result was a reporting error, not
a host limitation.

## Portable implementation result

The existing implementation and tests cover:

- durable `AgentCommandRecord` identity;
- stable `agent_id` with per-connection `agent_epoch`;
- operation, resource, command, and idempotency identity binding;
- payload fingerprint preservation;
- Pending/recoverable command selection;
- current-epoch replay after agent re-registration;
- stale old-epoch evidence rejection;
- observation sequence fencing and replay idempotency;
- duplicate command/effective mutation suppression.

Focused tests passed, including:

- `agent_replay_after_reregistration_applies_unknown_outcome`;
- `agent_evidence_from_dead_epoch_is_rejected_after_reregistration`;
- `inspect_after_agent_reregistration_dispatches_against_current_epoch`;
- `command_observation_wait_accepts_current_epoch_replay_after_reconnect`.

The full compute-agent suite passed: 107 tests. The full workspace suite also
passed when local socket binding was permitted.

## Real-host attempt

The disposable agent/libvirt TestLab bootstrap completed successfully on the
current source. The host preflight passed and the real `o3kd` and
`o3k-compute` binaries were built and launched.

The required ASR-015 lifecycle was not completed because the execution wrapper
reaped detached TestLab daemons when the bootstrap command session ended. As a
result, the following values were not legitimately observed:

- old agent epoch E1;
- new agent epoch E2;
- Pending command immediately before the crash;
- durable recovery and replay under E2;
- explicit stale-E1 probe on the real host;
- provider/domain count after recovery;
- lifecycle mutation reconnect proof;
- final real-host cleanup and foreign-canary comparison.

No host pass is claimed from this attempt.

## Evidence files

- Machine-readable artifact: `docs/evidence/asr-015-reconnect-host-633f8cb.json`
- This report: `docs/evidence/asr-015-result-report.md`
- Status matrix: `docs/security-remediation-status.md`

The machine-readable artifact is redacted and records the corrected host
preflight result plus the fact that the lifecycle gate remained unobserved.

## Validation

Passed:

```text
python3 scripts/check-architecture-boundaries.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
bash tests/real-host-leak-verifier.sh
bash tests/real-host-workflow-guards.sh
```

## Closure decision

ASR-015 remains `implemented-portable`. It may be closed only after a
protected real-host run proves the command-before-acceptance crash window,
same-agent/new-epoch recovery, stale-epoch fencing, at-most-once effective
mutation, lifecycle reconnect behavior, cleanup, and unchanged foreign state.

## Next exact ASR item

The next item in the current matrix is **ASR-016 — concurrent durable
operation/evidence monotonicity on the real host**. It should not be started as
part of this ASR-015 report.
