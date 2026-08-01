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

The journal now applies an ownership invariant during both command completion
and unknown-outcome recovery: the provider operation's embedded O3K operation
ID must equal the durable journal operation ID. A mismatched provider record is
rejected without changing the operation or resource state, preventing stale
or cross-wired provider observations from being accepted as convergence.

When recovery observes an `accepted` or `running` provider operation, it now
persists that stronger state before returning. When it observes a provider
terminal failure, it persists a failed journal operation rather than returning
an ambiguous retry result. Portable tests exercise both transitions, including
an action that was applied before the original response was lost.

Partial create completion is also observation-gated. The provider returns a
non-terminal running operation and stable resource ID while the instance is
`CREATING`; the journal persists that reference and `BUILD` state, then
finishes the same operation only after a later observation reports `RUNNING`.
The provider's idempotency record ensures recovery does not issue a duplicate
create. Provider `ERROR` observation is recorded as a failed operation rather
than an `ACTIVE` resource.

The release gate now also rejects an incomplete or failed aggregate recovery
artifact: all required crash, restart, interruption, timeout, duplicate,
image, host-tool, and cleanup scenario keys must be present with a passed
machine-readable result. This validates evidence shape only and does not claim
that any real-host scenario has run.

The gate validates the required scenario key set, each result's `status`, and
the presence of an artifact identifier plus non-empty checks for every
scenario. It does not inspect those referenced artifacts; richer host evidence
remains the responsibility of the failure-injection harness and is not
inferred from these fields.

## Explicit non-goals

- no real libvirt, agent, daemon, network, disk, or process failure injection;
- no distributed leases or durable retry scheduler;
- no claim that issue #87's required host scenarios ran;
- no automatic re-dispatch after an unknown action has been observed but not
  proven converged.

Decision: [ADR-0094](../adr/ADR-0094-action-unknown-outcome-recovery.md).
