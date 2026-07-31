# ADR-0024 — Atomic config-drive publication

## Status

Accepted for the portable config-drive slice.

## Context

Config-drive generation already wrote new content in a temporary directory,
but then removed the previous instance directory before renaming the new one
into place. A failed replacement could therefore lose the last valid guest
configuration.

Cleanup also previously accepted any non-hidden directory path, which could
remove data that was never created by the config-drive subsystem.

## Decision

`ConfigDriveStore` binds operations to one configured root and writes a
versioned ownership manifest containing only schema, manager, instance, and
fingerprint fields. Generation rejects symlinks and unowned existing instance
directories, moves an owned directory to a managed hidden backup,
publishes the fully written temporary directory with a rename, restores the
backup if publication fails, and removes the backup only after successful
publication. The generated directory and fingerprint remain deterministic for
the same inputs; temporary and backup names remain unique per attempt.

Cleanup is idempotent for absent paths but fails closed for unowned or
symlinked paths.

This is filesystem publication safety only. It does not attach the directory
as a libvirt disk or claim guest cloud-init evidence; those remain host-backed
release-gate work.

## Consequences

A failed regeneration preserves the previous usable config drive, while a
successful retry converges on one instance directory. Cleanup cannot remove a
directory without the subsystem's ownership manifest, and no partially written
directory is published.
