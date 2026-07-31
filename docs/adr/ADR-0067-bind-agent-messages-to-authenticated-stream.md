# ADR-0067 — Bind agent messages to the authenticated stream

## Context

The compute-agent control stream authenticates the certificate and registration
message, but identity-bearing heartbeat, state-ack, and observation messages
were subsequently validated only against registry state. A connected agent
could therefore attribute those messages to another registered agent.

## Decision

Capture the registered agent ID and epoch as stream authority. Reject and close
the stream when a later heartbeat, administrative-state acknowledgement, or
observation carries a different identity. Operation and command events without
identity fields remain scoped to the authenticated stream that delivered them.

## Consequences

Certificate authorization now fences all identity-bearing messages for the
stream lifetime. Reconnects must register with the new epoch before sending
heartbeats or observations, and spoofed attribution is rejected before it
reaches registry consumers.
