# ADR-0112 — Reject unsafe components in clean-install paths

Status: Accepted for the repository-side portion of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, governance

#89.

## Context

The installer rejected a requested path when the path itself was a symlink,
but an existing symlink in a parent component could redirect publication into
an unintended directory. Lexical `.` and `..` components were also accepted;
they can make a path that is textually non-root resolve to `/` or another
unexpected publication target. Both cases weaken the clean-install ownership
boundary even when the final path does not yet exist.

## Decision

Before any installer mutation, inspect every component of each configured
absolute path and reject the path if any component is a symlink or is `.`/`..`.
Keep the validation lexical and deterministic; the installer still rejects
relative, root, non-directory-compatible, and populated unowned paths as
before.

## Non-goals

- no Ubuntu package installation or host execution;
- no real libvirt, systemd, reset/reinstall, uninstall, purge, or lifecycle
  evidence;
- no general installer transaction or rollback mechanism.

## Verification

`tests/packaging-safety.sh` uses temporary symlink-parent and dot-component
inputs and verifies that the installer rejects each path before creating state
under a redirected or resolved target.

## Provenance

This is an independently authored repository decision based on issue #89 and
the existing clean-install path-safety contract. No private source or
implementation was used.
