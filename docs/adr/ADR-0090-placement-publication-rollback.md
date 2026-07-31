# ADR-0090 — Roll back in-memory Placement mutations on publication failure

## Status

Accepted for the portable issue #82 Placement lifecycle slice.

## Context

The Placement ledger changes its mutex-protected state and then publishes a
new JSON snapshot through a unique temporary file and atomic rename. If that
publication fails, returning an error without restoring the in-memory state
leaves the current process observing an allocation, usage value, provider
state, or generation that was never durably committed. A later scheduling
request in the same process could then make an incorrect capacity decision,
and the result would differ before and after restart.

## Decision

Every mutating ledger operation snapshots the prior provider map before
applying its change. If serialization, temporary-file writing, or atomic
publication fails, the operation restores that snapshot before returning the
storage error. Successful publication retains the mutation. The existing
idempotent allocation and unique atomic publication rules are unchanged.

## Consequences

Publication failures are transactionally visible to callers: failed changes
cannot leak into subsequent in-process scheduling or reconciliation. The
ledger still cannot claim cross-process locking or real OpenStack Placement
service integration; those remain outside this repository-side slice.
