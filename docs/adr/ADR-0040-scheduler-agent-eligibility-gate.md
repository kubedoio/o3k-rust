# ADR-0040 — Gate scheduled placement by authenticated agent eligibility

## Status

Accepted

## Context

Placement tracks provider capacity and its own administrative state, while the
authenticated `NodeRegistry` tracks heartbeat availability and the control
plane's desired administrative state. Scheduling through Placement alone can
select a provider whose agent is unavailable, draining, or disabled.

## Decision

`ComputeService` may be configured with the live `NodeRegistry`. When both the
registry and scheduler are configured, each create builds an allow-list from
agents that are registered, available, and administratively enabled, then
passes that list to the scheduler. An empty list fails closed. Existing
provider-only scheduling remains unchanged when no registry is configured.

## Consequences

Placement allocation remains authoritative for capacity, generation, and
atomic reservation, while agent liveness and administrative state are honored
at scheduling time. The production daemon still needs a Placement inventory
publisher and scheduler wiring for authenticated-agent deployments; this
slice deliberately does not invent inventory from incomplete capability data.
