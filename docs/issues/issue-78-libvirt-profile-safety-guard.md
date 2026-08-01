# Issue #78 — Fail closed for the unimplemented direct libvirt path

## Repository-side completion

Implemented on the repository branch:

- `o3kd` no longer constructs a direct local `LibvirtProvider`;
- `o3k-config` rejects `provider = "libvirt"` before runtime startup with a
  clear remediation message;
- deterministic configuration coverage proves the rejection without opening a
  listener or requiring libvirt/QEMU;
- fake and configured CellHV provider selection remain unchanged;
- [ADR-0086](../adr/ADR-0086-libvirt-profile-fail-closed.md).
- authenticated `CommandAccepted` events now durably transition pending
  operations to `running`, with duplicate acceptance idempotency coverage;
- [ADR-0148](../adr/ADR-0148-durable-command-acceptance.md).

## Explicit boundary

This slice does not implement durable command replay, command-id storage,
agent dispatch, compute-agent lifecycle execution, host capability evidence,
or a new libvirt provider path. The reserved/package `libvirt` profile remains
blocked until separately scoped work supplies and tests the agent-backed
wiring.
