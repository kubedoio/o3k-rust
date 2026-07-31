# ADR-0105 — Verify config-drive manifest integrity before destructive use

## Status

Accepted for the portable config-drive slice.

## Context

The ownership manifest identified O3K-created directories, but validation only
checked its fields and did not compare its fingerprint with the files on disk.
An altered or structurally foreign directory could therefore still pass the
ownership check and be removed or replaced.

## Decision

Before cleanup or replacement, validate the complete published layout: the
manifest, `openstack/latest` directories, and the three required files must be
regular non-symlink entries; an optional vendor file is allowed; unexpected
entries are rejected. Recompute the existing fingerprint in the same canonical
file order used during publication and require an exact match with the manifest.

This protects destructive operations from treating modified or foreign content
as O3K-owned. It does not claim protection from races after validation, create
ISO/VFAT media, attach libvirt devices, or provide guest cloud-init evidence.

## Consequences

Tampered, incomplete, symlinked, or extended published directories remain in
place and fail closed. Valid generated directories continue to replace and
clean up atomically. Future layout changes require a manifest schema/design
update rather than silently widening the destructive ownership boundary.

## Public sources

- Rust standard library filesystem metadata, directory traversal, and hashing
  behavior, accessed 2026-07-31.
- O3K [atomic config-drive publication](ADR-0024-atomic-config-drive-publication.md).
