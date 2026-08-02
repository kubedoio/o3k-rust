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

The repository now implements durable command replay, command-id storage,
agent dispatch, and compute-agent lifecycle execution through the `agent`
provider. The reserved/package `libvirt` profile remains rejected so the daemon
cannot bypass that boundary. Host capability evidence and a passing protected
real-run artifact remain required for issue closure.
