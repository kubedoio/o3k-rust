# ADR-0120 — Fence uninstall paths before removing files

## Status

Accepted for the repository-side preparation of issue #90. This ADR does not
claim that a clean Debian host passed installation or the TestLab lifecycle.

## Context

`packaging/uninstall.sh` protected purge data, configuration, and log
directories with absolute-path and ownership checks, but did not reject dot or
symlink components. It also did not validate `--prefix` at all. These gaps
could redirect removal or ownership checks to a location outside the intended
installation layout.

## Decision

Validate `--prefix` before any service or filesystem action, and validate each
purge target before ownership checks or service mutation. Every path must be
absolute and non-root, must not contain lexical `.` or `..` components, and no
existing component may be a symlink. This mirrors the install path boundary and
keeps uninstall targets deterministic. The existing explicit helper inventory
and default-layout systemd fence remain unchanged.

## Consequences

Unsafe prefixes and purge targets fail closed, including for a non-purge
uninstall. A valid custom prefix continues to remove only the known O3K
binaries and helper files, while foreign files remain untouched. The portable
packaging safety test verifies dot- and symlink-component rejection before
removal.

## Non-goals

- no Debian package installation or host execution;
- no real libvirt, CirrOS, systemd, reset/reinstall, or lifecycle evidence;
- no change to purge ownership or default-layout service policy.

## Provenance

This is an independently authored repository decision based on issue #90, the
existing installer path contract, and ADR-0097. No private source or
implementation was used.
