# ADR-0125 — Release only the losing placement decision in a create race

Status: Accepted

## Context

Placement is reserved before the durable create intent is inserted. Two
idempotent callers can therefore reserve different hosts before one caller
wins the unique resource insertion. The losing caller must not leak its
reservation, but it also must not release the placement decision persisted by
the winner. The transactional store path must expose this duplicate as the
same `ResourceAlreadyExists` condition as its non-transactional insert path.

## Decision

When the durable insert loses a create race, compare the locally selected
provider and allocation IDs with the persisted intent. Release the local
decision only when it is not the persisted decision. Normalize unique-resource
violations from the atomic resource/operation insert to `ResourceAlreadyExists`
so the compute layer can apply that rule.

## Consequences

Concurrent idempotent creates retain exactly one Placement allocation and
converge through the existing durable resource path. A failed loser cleanup is
surfaced as a scheduler error rather than silently leaving capacity reserved.
