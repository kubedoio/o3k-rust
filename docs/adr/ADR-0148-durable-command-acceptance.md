# ADR-0148 — Persist authenticated command acceptance

## Status

Accepted for the repository-side issue #78 operation-journal slice.

## Context

The compute-agent stream sends `CommandAccepted` before execution, but the
control-plane event consumer previously discarded that event. A command could
therefore be executing while its durable operation still reported `pending`.
Duplicate acceptance messages are expected during reconnect and must not
advance a terminal operation backward.

## Decision

The reconciler validates the accepted operation identity, persists its state as
`running`, and emits the existing provider-started journal event. Repeated
acceptances are idempotent; terminal durable operations remain terminal. The
live stream remains responsible for connection fencing. Durable command
replay, command-id storage, and agent-backed lifecycle dispatch are now part of
the repository implementation; protected real-host execution remains a
separate evidence requirement.

## Consequences

The API/control-plane operation view reflects authenticated command acceptance
before execution completes. Repository tests cover restart replay and real-agent
protocol lifecycle behavior; this ADR does not claim a passing protected
real-host run.
