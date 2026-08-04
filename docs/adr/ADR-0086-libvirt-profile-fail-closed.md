# ADR-0086 — Reject the unimplemented direct libvirt daemon path

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The `o3kd` binary previously selected `LibvirtProvider` directly for the
`libvirt` configuration value. That provider owns a local libvirt adapter and
could open `qemu:///system` from the control-plane process. This contradicts
the provider boundary: only `o3k-compute` may access host libvirt, and the
agent-backed provider path from `o3kd` does not yet exist.

Starting the real-libvirt profile in this state would therefore silently use
an unsafe implementation path and could expose a listener before the missing
execution boundary was apparent.

## Decision

`o3k-config` rejects `provider = "libvirt"` with an actionable
`DirectLibvirtProviderUnavailable` error. Configuration is loaded before
logging, storage, provider construction, or API listener binding, so the
failure is deterministic and occurs before `o3kd` can open `qemu:///system` or
serve requests. The direct `LibvirtProvider` construction and dependency are
removed from `o3kd`.

The `fake` and configured `cellhv` provider paths remain unchanged. This ADR
does not invent agent dispatch, host evidence, or a replacement libvirt
implementation. A future issue must add and test the agent-backed path before
the reserved `libvirt` profile value can be enabled.

## Consequences

The packaged real-libvirt profile is intentionally blocked at daemon startup
until the provider boundary is implemented. Existing fake and CellHV users
retain their behavior. The configuration test proves the guard and its
actionable error without requiring libvirt, QEMU, a listener, or host
capabilities.

## Public sources

- O3K [architecture](../ARCHITECTURE.md), accessed 2026-07-31.
- Public libvirt architecture documentation:
  <https://libvirt.org/architecture.html>, accessed 2026-07-31.
