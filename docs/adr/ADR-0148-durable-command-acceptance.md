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
live stream remains responsible for connection fencing, while durable command
replay, command-id storage, and full agent-backed lifecycle dispatch remain
separate follow-up work.

## Consequences

The API/control-plane operation view reflects authenticated command acceptance
before execution completes. This slice does not claim restart replay or
real-agent lifecycle acceptance.
