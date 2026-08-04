# ADR-0021 — Live agent-event consumer boundary

Status: Accepted for the alpha control-plane integration slice.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, identity, governance

## Context

The authenticated compute-agent server and durable reconciler were separate:
the server published operation updates, but the production binary did not
consume them. A successful agent operation could therefore remain invisible to
the API process until another reconciliation path observed it.

## Decision

`o3kd` creates one `NodeRegistry`, shares it with `ControlPlaneServer`, and
starts the `ComputeService` event consumer against that same registry. The
consumer forwards only `AgentEvent::Operation` values through the narrow
`ComputeService::apply_agent_update` API; all other event types are ignored.
Receiver lag and closure are handled explicitly, and the task is aborted and
joined during shutdown.

The broadcast stream is a live-update accelerator, not the recovery authority.
It is in-memory and lossy, and protocol sequence numbers are not yet durable.
Startup/restart recovery still requires the reconciler's durable observation
loop. Create dispatch is intentionally not enabled by this ADR because agent
selection is not yet bound to Placement and the current libvirt executor does
not realize create commands.

## Consequences

Authenticated operation results now reach the durable journal in the running
control plane. The shared registry removes the previous split-brain wiring,
while the documented limitations prevent this slice from being presented as
restart-safe end-to-end orchestration.
