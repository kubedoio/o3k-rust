# ADR-0024 — Atomic config-drive publication

## Status

Accepted for the portable config-drive slice.

## Context

Config-drive generation already wrote new content in a temporary directory,
but then removed the previous instance directory before renaming the new one
into place. A failed replacement could therefore lose the last valid guest
configuration.

## Decision

Generation moves an existing instance directory to a managed hidden backup,
publishes the fully written temporary directory with a rename, restores the
backup if publication fails, and removes the backup only after successful
publication. The generated directory and fingerprint remain deterministic for
the same inputs; temporary and backup names remain unique per attempt.

This is filesystem publication safety only. It does not attach the directory
as a libvirt disk or claim guest cloud-init evidence; those remain host-backed
release-gate work.

## Consequences

A failed regeneration preserves the previous usable config drive, while a
successful retry converges on one instance directory. Recovery cleanup is
bounded to the managed root and no partially written directory is published.
