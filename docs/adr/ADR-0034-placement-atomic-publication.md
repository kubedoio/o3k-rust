# ADR-0034 — Use unique temporary files for Placement publication

## Status

Accepted

## Context

The Placement ledger published JSON through a fixed process-ID temporary path.
Concurrent writers could overwrite one another's temporary bytes, and a
failed write could leave stale temporary output in the allocation root.

## Decision

Serialize the complete state before publication, write it to a unique UUIDv7
temporary path, remove that path on write or rename failure, and publish only
through the final atomic rename. Allocation and generation semantics are
unchanged.

## Consequences

Concurrent Placement writers no longer share a temporary pathname, and failed
publication does not leave stale temporary files for a later process to
mistake for current state. Cross-process locking and real Placement service
integration remain out of scope for this slice.
