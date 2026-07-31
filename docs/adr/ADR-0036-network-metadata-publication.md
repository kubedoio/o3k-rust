# ADR-0036 — Isolate network metadata publication temporary files

## Status

Accepted

## Context

The network metadata ledger used a process-ID temporary filename for its
atomic JSON publication. Concurrent writers could share that pathname, and a
failed write could leave stale temporary bytes in the network state root.

## Decision

Use a UUIDv7 temporary filename for each metadata publication, remove it on
write or rename failure, and retain the final atomic rename. Allocation,
project isolation, and TAP host-operation behavior are unchanged.

## Consequences

Network state publication is isolated across writers and failed writes do not
leave stale candidates. Multi-process locking and privileged host evidence
remain outside this portable metadata slice.
