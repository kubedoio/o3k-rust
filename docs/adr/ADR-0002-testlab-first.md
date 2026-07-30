# ADR-0002 — TestLab first

Status: Accepted

## Decision

The first product is a single-node TestLab, not a production OpenStack replacement.

## Rationale

A complete small workflow provides earlier user value, faster compatibility feedback, and a measurable base for edge and SMB profiles.

## Consequences

- SQLite, local image storage, flat networking, and stub/CellHV providers first;
- endpoint breadth is deferred;
- production claims require separate evidence and ADRs.
