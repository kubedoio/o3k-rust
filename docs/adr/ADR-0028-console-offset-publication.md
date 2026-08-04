# ADR-0028 — Bounded console offset reads

Status: Accepted for the durable console-service slice.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

Console output was durably bounded and restart-safe, but every consumer had to
read the whole retained buffer. The compute-agent protocol already models
offsets, truncation, and continuation, while the local service had no
equivalent bounded read contract. Writes also shared one temporary pathname.

## Decision

Console writes use unique temporary filenames and remove temporary output on
write or rename failure. `ConsoleService::read_from` returns a bounded
`ConsoleChunk` with the effective offset, next offset, and truncation flag;
offsets beyond the retained buffer clamp to its end. The retained buffer and
existing cleanup semantics are unchanged.

This slice does not fabricate guest console bytes or wire the agent's
`ConsoleLogCommand` to libvirt. Guest boot output and host console transport
remain evidence-gated follow-up work.

## Consequences

API/control-plane consumers can page through durable retained output without
unbounded reads, and concurrent publication no longer shares a temporary path.
The source of guest output remains explicitly unresolved.
