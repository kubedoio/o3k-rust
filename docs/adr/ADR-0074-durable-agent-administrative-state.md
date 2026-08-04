# ADR-0074 — Durably apply compute-agent administrative state

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The compute agent previously initialized every connection as `ENABLED`, sent
heartbeats before consuming `RegisterResponse`, and ignored
`DesiredAgentState`. A process restart or reconnect could therefore advertise
an unsafe transient state, and a state transition could be lost before its
acknowledgement. SPEC-0015 requires registration before heartbeats and durable
administrative-state application for `ENABLED`, `DRAINING`, and `DISABLED`.

## Decision

Store the validated applied administrative state in a small state file beside
the stable identity file. A missing file means `ENABLED`; an unreadable,
corrupt, or unspecified value fails safely. State updates are written to a
temporary file and atomically renamed into place.

The agent loads and validates the state before each connection attempt. It
then waits for a matching, version-validated `RegisterResponse` before
starting the heartbeat loop. The registration response is authoritative for
the new fenced epoch, is persisted before the first heartbeat, and supplies
that heartbeat's state. Heartbeat responses and `DesiredAgentState` envelopes
are enum-validated, persisted before acknowledgement, and the latter is
acknowledged with its transition sequence.

The control-plane registry now dispatches desired-state envelopes to the
currently attached, epoch-scoped stream. Explicit `AgentStateAck` messages
remain the authority for the registry's applied state; heartbeat reports are
retained only as observations for verification and diagnostics.

## Consequences

- Restart and reconnect cannot silently begin with an invalid or fabricated
  administrative state.
- No heartbeat is emitted before registration identity, protocol version, and
  desired state are validated.
- A desired-state transition has an explicit durable apply-and-ack path.
- State files contain only a bounded enum value and no secrets.
- The state file is process-local durable input; cross-host replication and
  control-plane persistence remain outside issue #38.

## Provenance

This decision implements the public O3K contract in
[`SPEC-0015`](../specs/SPEC-0015-compute-agent.md), section 5, using the
repository's existing atomic-publication pattern. No private implementation,
schema, or test was used.
