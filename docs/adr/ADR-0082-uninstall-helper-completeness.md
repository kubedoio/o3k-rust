# ADR-0082 — Remove the complete installed helper set

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: placement, governance

## Context

The installer copies O3K service and helper scripts into
`prefix/share/o3k`. Uninstall removed only some of them, leaving reset and
uninstall scripts behind. A broad directory cleanup would risk deleting files
owned by an operator or another package, while removing the running script
requires deliberate ordering.

## Decision

Uninstall maintains an explicit inventory matching the installer's O3K-owned
files under `prefix/share/o3k` and removes only those paths. It removes the
running `uninstall.sh` entry last. The helper cleanup is independent of purge:
state and credentials remain by default, and `--purge --yes` continues to be
required for owned data, configuration, and logs.

## Consequences

Installed helper files no longer survive an uninstall, foreign files in the
shared directory are retained, and invoking the installed uninstall script is
safe. Adding a new installed helper requires updating the explicit inventory
and its packaging regression assertions.
