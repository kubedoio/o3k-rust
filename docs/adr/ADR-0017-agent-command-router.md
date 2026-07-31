# ADR-0017 — Authenticated compute-agent command router

## Status

Accepted as the transport bridge for the alpha control plane.

## Context

The mTLS control stream could register agents and receive commands, but the
control plane had no way to enqueue a command onto a registered stream or
observe the agent acknowledgements and operation updates. This made the
compute-agent path unusable for a caller even though the wire protocol was
implemented.

## Decision

`NodeRegistry` owns one bounded command sender per registered agent epoch.
Commands are validated for identity, action, protocol version, and deadline;
dispatch requires an available, enabled node and an exact epoch match. The
registry exposes a broadcast event subscription for `CommandAccepted`,
`OperationUpdate`, `Observation`, and protocol errors received from agents.
Connection attach/detach is epoch-safe, so an old stream cannot unregister a
newer connection.

## Consequences

The authenticated transport boundary is now callable and observable by a
future durable operation reconciler. This slice deliberately does not select
hosts, persist Placement allocations, or construct commands from Nova
requests; those integrations must consume this router rather than bypassing
it. Command payloads still reference resource IDs and remain free of secrets
and arbitrary host paths.
