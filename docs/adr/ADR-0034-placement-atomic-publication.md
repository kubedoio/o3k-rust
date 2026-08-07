# ADR-0034 — Use unique temporary files for Placement publication

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: placement, governance

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

## Status note — superseded in part by issue #523 (SPEC-0025 step 5)

The whole-file JSON publication mechanism this ADR describes was replaced by
the durable `PlacementRepository` port on the SQLite adapter (migration
0017): the ledger no longer publishes JSON snapshots, so the
unique-temporary-path and atomic-rename mechanics no longer apply. Atomicity
and crash safety are now provided by the adapter (BEGIN IMMEDIATE
transactions, WAL, busy_timeout, optimistic generation guards), and the
legacy journals are imported once and never read again. This decision is
retained as the historical record of the file-backed era.
