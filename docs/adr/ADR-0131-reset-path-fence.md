# ADR-0131 — Fence reset paths before service or filesystem mutation

Status: Accepted for the repository-side implementation of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

#90. This does not
claim a clean Debian installation or host lifecycle result.

## Decision

`packaging/reset.sh` applies the same lexical and symlink-component boundary as
installation and uninstall paths. Both `--data-dir` and `--log-dir` must be
absolute, non-root paths without `.`/`..` components or symlink components.
Validation occurs before ownership checks, systemd stop commands, or `find`/
`rm` cleanup.

## Rationale

Reset is destructive even though it preserves credentials. An unsafe path could
redirect cleanup into a foreign directory or cause service state changes before
the command is rejected. Failing closed before side effects preserves the
ownership and service-boundary contract.

## Verification

`tests/packaging-safety.sh` covers lexical-dot, symlink-parent, and final
symlink reset paths, checks that sentinels remain untouched, and verifies no
fake systemd stop command is issued. The existing valid reset test remains.
