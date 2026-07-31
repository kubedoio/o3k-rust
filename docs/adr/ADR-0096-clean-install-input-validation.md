# ADR-0096 — Validate clean-install inputs before mutation

## Status

Accepted

## Context

Issue #89 requires a clean Ubuntu installation to be reproducible and not
depend on manual repair after an invalid profile or path input. The installer
previously accepted relative paths and could begin creating a prefix and
owned state directories before discovering incomplete libvirt TLS inputs.
That produced a partial installation even though the requested profile could
not start.

## Decision

`packaging/install.sh` validates every installation path as absolute,
non-root, non-symlink, and directory-compatible before performing filesystem
mutations. For the libvirt profile it also validates the TLS directory,
required regular credential files, and the 64-character agent fingerprint
before creating the installation prefix or state directories.

The installer remains intentionally non-transactional for failures occurring
after successful input validation, such as an unexpected permission error
during publication. Full rollback is a separate packaging issue and is not
claimed here.

## Consequences

- Invalid clean-install inputs fail deterministically with no partial state
  from the input-validation phase.
- Release-bundle and packaging safety tests cover relative paths, symlink
  paths, and missing libvirt TLS inputs without requiring Ubuntu or libvirt.
- A real clean Ubuntu installation and lifecycle run remain required to close
  issue #89.

## Provenance

This is an independently authored repository decision based on issue #89,
the project packaging contract, and the existing ownership-marker policy. No
private source or implementation was used.
