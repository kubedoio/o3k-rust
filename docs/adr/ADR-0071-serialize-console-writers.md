# ADR-0071 — Serialize per-instance console mutations

## Context

Console writes use atomic replacement, but append and sequential observation
updates are read–modify–write operations. Concurrent API and agent writers can
therefore both read the same buffer and let the last rename discard a chunk.

## Decision

Keep a service-local mutex keyed by instance UUID and hold that lock across the
entire mutation. `write`, `append`, `write_chunk`, and cleanup share the same
per-instance lock; internal unlocked helpers avoid recursive locking.

## Consequences

Concurrent writers preserve both updates while retaining atomic file
replacement and the existing 64 KiB bound. The lock is process-local; durable
cross-process coordination remains outside this library's single-service
ownership contract.
