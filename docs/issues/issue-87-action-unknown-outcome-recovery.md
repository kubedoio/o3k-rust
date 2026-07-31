# Issue #87 — Lifecycle action unknown-outcome recovery

Issue #87 requires real-host failure injection and a passing aggregate
`failure-recovery.json`. That acceptance remains host-gated and is not claimed
by this repository change.

## Bounded repository implementation

The stateful fake provider now supports deterministic timeout injection for
start/stop/reboot actions. It records the operation and applies the transition
once, then replays the same operation for duplicate delivery using a
fingerprinted idempotency key. The operation journal persists unknown action
responses and observes both the provider operation and instance state before
finishing. It only records success when the requested state is observable;
otherwise it remains unknown and does not repeat an unproven mutation.

Regression tests cover timeout-after-stop, duplicate action delivery, durable
unknown state, and observation-based completion.

The release gate now also rejects an incomplete or failed aggregate recovery
artifact: all required crash, restart, interruption, timeout, duplicate,
image, host-tool, and cleanup scenario keys must be present with a passed
machine-readable result. This validates evidence shape only and does not claim
that any real-host scenario has run.

## Explicit non-goals

- no real libvirt, agent, daemon, network, disk, or process failure injection;
- no distributed leases or durable retry scheduler;
- no claim that issue #87's required host scenarios ran;
- no automatic re-dispatch after an unknown action has been observed but not
  proven converged.

Decision: [ADR-0094](../adr/ADR-0094-action-unknown-outcome-recovery.md).
