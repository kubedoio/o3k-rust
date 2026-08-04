# ADR-0043 — Persist sequential agent console observations

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

Compute agents now emit bounded console observations, but the daemon's event
consumers only handled lifecycle operation updates. Console bytes therefore
could not survive an agent event handoff or be served by the existing durable
console API.

## Decision

Run a daemon-local observation consumer alongside lifecycle reconciliation. It
accepts only UUID resource IDs and sequential or exact replayed chunks, then
atomically persists them through `ConsoleService`. Out-of-order or conflicting
chunks are rejected and logged; a lagged broadcast remains recoverable through
the existing durable control-plane model.

## Consequences

Agent observations are now durable and available to the bounded API reader.
Command routing from the API to a selected agent and a real libvirt console
source remain separate follow-up work.
