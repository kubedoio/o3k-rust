# Issue #80 — Config-drive attachment boundary

## Repository-side completion

This bounded change closes one portable safety gap in the existing
config-drive publisher:

- failed generation after temporary-directory creation removes the
  unpublished `.instance-*-tmp-*` directory;
- rejected replacement of an unowned instance directory leaves that directory
  untouched;
- published ownership validation recomputes the manifest fingerprint and
  rejects symlinked, missing, or unexpected content before cleanup or replace;
- regression coverage proves no temporary artifact remains after the failure.
- libvirt config-drive attachment now accepts a typed path-plus-SHA-256
  artifact and refuses altered, non-regular, or ambiguous host files before
  XML generation.

See [ADR-0088](../adr/ADR-0088-config-drive-failed-generation-cleanup.md) and
[ADR-0105](../adr/ADR-0105-config-drive-manifest-integrity.md).

## Explicit boundary

Issue #80 remains open for deterministic ISO/VFAT media, real libvirt
attachment, Nova/compute-agent wiring, guest cloud-init consumption, reboot
preservation, and trusted real-host evidence. This repository change makes no
claim about those behaviors or about media generation from the directory
publisher.
