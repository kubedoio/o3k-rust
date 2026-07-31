# Issue #79 — Image cache and overlay safety boundary

## Repository-side completion

This bounded repository change hardens the existing image-cache boundary:

- cached bases must be regular files and are inspected without following
  symlinks;
- overlay bases must be regular files inside the managed `base` directory;
- existing overlay destinations must be regular files before they are returned;
- newly-created overlays must pass `qemu-img info --output=json` verification
  before publication: exactly `qcow2` with a backing path resolving to the
  managed base file;
- malformed, wrong-format, missing-backing, and foreign-backed temporary
  outputs are removed and never published;
- symlink, directory, and other non-regular destinations fail with
  `InvalidPath` without touching an outside target;
- atomic temporary publication remains unchanged for new base and overlay
  files;
- regression coverage proves symlinked bases, symlinked overlays, and an
  outside target are rejected.
- verified `ImageService` artifacts now have an explicit local
  `ImageCache::cache_artifact` bridge with size revalidation and idempotent
  publication coverage.

See [ADR-0087](../adr/ADR-0087-image-cache-node-safety.md) and
[ADR-0104](../adr/ADR-0104-image-overlay-backing-verification.md).

## Explicit boundary

This does not claim completion of the full issue. Glance-backed image
resolution, authenticated transfer to a selected compute host,
compute-agent dispatch, durable image/overlay ownership, libvirt image
realization, and trusted real-host evidence remain separate follow-ups.
