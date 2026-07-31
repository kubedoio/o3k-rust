# ADR-0058 — Ownership-checked TAP deletion

## Status

Accepted for host-network cleanup safety.

## Context

TAP names are deterministic, but names alone are not ownership proof. A
foreign interface could occupy the same derived name after restart or partial
cleanup. Deleting by port ID alone could therefore mutate host state owned by
another process.

## Decision

TAP deletion requires the original instance, port, and MAC specification. If
the derived interface exists, the manager must observe both the expected MAC
and the configured bridge before issuing `ip link del`; otherwise it returns a
foreign-interface error. Missing interfaces remain idempotently absent-safe.

## Consequences

Failure cleanup can be retried without deleting foreign interfaces. Callers
must retain the same validated TAP specification used for creation, and full
durable restart discovery remains a separate integration task.
