# ADR-0112 — Reject symlink components in clean-install paths

## Status

Accepted for the repository-side portion of issue #89.

## Context

The installer rejected a requested path when the path itself was a symlink,
but an existing symlink in a parent component could redirect publication into
an unintended directory. That weakens the clean-install ownership boundary
even when the final path does not yet exist.

## Decision

Before any installer mutation, inspect every component of each configured
absolute path and reject the path if any component is a symlink. Keep the
validation lexical and deterministic; the installer still rejects relative,
root, non-directory-compatible, and populated unowned paths as before.

## Non-goals

- no Ubuntu package installation or host execution;
- no real libvirt, systemd, reset/reinstall, uninstall, purge, or lifecycle
  evidence;
- no general installer transaction or rollback mechanism.

## Verification

`tests/packaging-safety.sh` uses a temporary symlink parent and verifies that
the installer rejects the path before creating state under the symlink target.

## Provenance

This is an independently authored repository decision based on issue #89 and
the existing clean-install path-safety contract. No private source or
implementation was used.
