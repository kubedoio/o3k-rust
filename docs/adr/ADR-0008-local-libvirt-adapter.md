# ADR-0008 — Local libvirt adapter boundary

Status: Accepted

## Decision

Add `o3k-libvirt` as a typed adapter around the `virt` 0.4.3 bindings. The
public API is async, but every FFI operation runs in `spawn_blocking`; each
operation opens `qemu:///system` afresh so a lost connection is recoverable on
the next operation without restarting the agent. The adapter exposes
capability discovery and the minimum managed-domain lifecycle operations.

The binding is optional. Builds without the `libvirt` feature return a typed
unavailable error, allowing CI and non-libvirt hosts to compile while making
compute readiness fail with an operator-facing reason. Production Linux
images enable `o3k-compute --features libvirt` and provide the system libvirt
development/runtime libraries.

## Consequences

- Only the local Unix-socket URI `qemu:///system` is accepted.
- Domain listing is prefix-scoped to `o3k-` by default, preventing accidental
  management of unrelated domains.
- Raw libvirt errors and credentials are not exposed; errors use typed
  categories and stable messages.
- The `virt` crate is LGPL-2.1 and has an explicit dependency-policy exception
  because it is the maintained Rust FFI boundary required for this issue.
- Real lifecycle validation requires a Linux host with libvirt and KVM; this
  repository's default CI profile exercises unavailable-host and type/URI
  validation, while a dedicated libvirt profile is the integration gate.
