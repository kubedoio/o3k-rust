# Issue #83 — Libvirt lifecycle and observed-state safety

Issue #83 remains open. Full acceptance requires agent-backed domain lifecycle
dispatch, real Nova create/show/action/delete behavior, guest evidence, and a
trusted real-libvirt host run.

## Bounded repository slice

`LibvirtProvider::get_instance` now projects domain observations explicitly.
Only an active `running` domain is reported as `Running`, and only an
inactive `shutdown` or `shutoff` domain is reported as `Stopped`. Paused,
blocked, crashed, suspended, unknown, and inconsistent observations report
`Error` instead of being mistaken for a healthy lifecycle state.

## Evidence

- `o3k-libvirt` regression coverage exercises every fail-closed state and both
  active-bit inconsistencies.
- The provider retains the readable libvirt state in `observed_message`.
- No real libvirt daemon, guest lifecycle, Nova integration, or host evidence
  is claimed.

## Remaining blockers

- issue #78's agent-backed provider path;
- real domain create and action dispatch through `o3k-compute`;
- guest boot, restart, and failure-recovery evidence;
- trusted real-host lifecycle artifact.

The decision is recorded in [ADR-0091](../adr/ADR-0091-libvirt-observed-state-projection.md).
