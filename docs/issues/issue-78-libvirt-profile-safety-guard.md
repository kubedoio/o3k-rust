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

## Explicit boundary

This issue does not implement agent dispatch, compute-agent lifecycle
execution, host capability evidence, or a new libvirt provider path. The
reserved/package `libvirt` profile remains blocked until a separately scoped
issue supplies and tests the agent-backed wiring.
