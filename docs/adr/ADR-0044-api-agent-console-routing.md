# ADR-0044 — Route console queries through the fenced agent

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, placement, identity, governance

## Context

Console observations were durable, but the API still read only its local cache.
For servers bound to an authenticated compute agent, that could return stale
or empty output even when the agent had current bytes.

## Decision

When a project-owned server has a persisted Placement agent and the daemon has
an authenticated `NodeRegistry`, the console action resolves the agent epoch,
builds a bounded protocol query, dispatches it through the fenced control
stream, and waits for the matching observation. The response is returned from
the observation and best-effort persisted; a nonsequential cache chunk must not
turn a successful remote query into a server error. Servers without an agent
binding retain the local-cache behavior.

## Consequences

Agent identity and epoch fencing are enforced by the existing registry. Query
timeouts and unavailable agents produce `503`, while invalid request bounds
produce `400`. A real libvirt console source and production Placement
inventory publication remain separate acceptance requirements.
