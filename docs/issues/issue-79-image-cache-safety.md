# Issue #79 — Image cache and overlay safety boundary

## Repository-side completion

This bounded repository change hardens the existing image-cache boundary:

- cached bases must be regular files and are inspected without following
  symlinks;
- overlay bases must be regular files inside the managed `base` directory;
- existing overlay destinations must be regular files before they are returned;
- symlink, directory, and other non-regular destinations fail with
  `InvalidPath` without touching an outside target;
- atomic temporary publication remains unchanged for new base and overlay
  files;
- regression coverage proves symlinked bases, symlinked overlays, and an
  outside target are rejected.

See [ADR-0087](../adr/ADR-0087-image-cache-node-safety.md).

## Explicit boundary

This does not claim completion of the full issue. Glance-backed image
resolution, compute-agent dispatch, libvirt image realization, and trusted
real-host evidence remain separate follow-ups.
