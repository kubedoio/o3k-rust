# ADR-0022 — Atomic image-overlay publication

## Status

Accepted for the portable image-cache slice.

## Context

The image cache validated base images, but `qemu-img` wrote a new overlay
directly to its final path. A failed or interrupted host command could leave a
partial overlay that later lifecycle code treated as usable. Repeated requests
also needed deterministic behavior.

## Decision

Overlay creation is serialized within the cache process, returns an existing
final overlay unchanged, writes new overlays to a per-process temporary path,
removes failed temporary output, and publishes the completed file with an
atomic rename. A cross-process race that finds a final overlay after creation
is treated as idempotent and discards the redundant temporary file.

The operation still requires the host `qemu-img` executable and does not claim
that a real guest image was created when the executable is unavailable. Host
execution remains covered by the real-libvirt release evidence gate.

## Consequences

Failed overlay creation no longer leaves the normal overlay pathname occupied
by partial output, and retries converge on one deterministic per-instance
path. Cleanup remains idempotent and path validation continues to prevent
escaping the managed overlay directory.
