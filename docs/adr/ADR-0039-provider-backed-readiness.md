# ADR-0039 — Report provider-backed daemon readiness

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, governance

## Context

The daemon marked `/readyz` successful after constructing the compute service,
even when the configured libvirt adapter could not connect. Operators and
orchestrators could therefore send traffic to a process that could not serve
compute requests.

## Decision

At startup, ask the configured compute provider for its capabilities with a
five-second timeout and set the daemon readiness flag only when that probe
succeeds. Keep liveness and process startup independent: the daemon remains
available for diagnostics and can shut down cleanly while readiness reports
the provider failure.

## Consequences

Fake providers remain ready, while an unavailable or slow libvirt endpoint
produces a machine-visible `503` from `/readyz` after the bounded startup
probe. This is a startup probe; reconnect and runtime provider-health
monitoring remain separate work.
