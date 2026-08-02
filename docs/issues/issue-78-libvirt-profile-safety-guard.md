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
- the protected real-host workflow records its selected provider mode and
  refuses lifecycle evidence unless `O3K_PROVIDER=agent`; the former
  fake-provider path is explicitly non-evidence.
- the compute boundary now exposes a read-only inspect operation that validates
  an existing Placement allocation without reserving capacity again and lets
  the agent provider dispatch a fenced Inspect command over mTLS.

## Explicit boundary

The repository now implements durable command replay, command-id storage,
agent dispatch, and compute-agent lifecycle execution through the `agent`
provider. The reserved/package `libvirt` profile remains rejected so the daemon
cannot bypass that boundary. Host capability evidence and a passing protected
real-run artifact remain required for issue closure. The protected workflow is
now configured for `agent`; until the agent create/artifact path is implemented,
it must fail closed rather than report a fake-provider lifecycle as real
compute evidence. The protected probe still needs to be migrated to a seeded
service-mediated server record before it can be promoted to acceptance
evidence.
