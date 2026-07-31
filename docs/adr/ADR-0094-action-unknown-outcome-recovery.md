# ADR-0094 — Recover lifecycle actions from observed unknown outcomes

## Status

Accepted

## Context

The fake compute provider could inject an unknown outcome for create and delete,
but lifecycle actions always returned success and did not look up an existing
idempotency record before applying a transition. The journal also left an
unknown action unresolved after observing the provider operation, even when the
instance had already reached the requested state. A retry could therefore
either repeat a provider mutation or remain stuck without a durable success.

## Decision

Lifecycle actions use a fingerprinted idempotency record keyed by the operation
identity. The fake provider's deterministic timeout injection applies the
transition once, records an `UNKNOWN_OUTCOME` operation, and replays that same
operation for duplicate delivery. The journal persists unknown operation
responses and, on the next reconciliation pass, observes the provider
operation and instance. It marks the action successful only when observation
proves the requested state; otherwise it remains `UNKNOWN_OUTCOME` and does not
issue an unproven duplicate action.

## Consequences

- Repository tests cover timeout-after-action, duplicate delivery, and
  observation-based convergence.
- Unknown actions are fail-closed when observation does not prove convergence;
  a later policy may create a fresh operation identity after an explicit
  recovery decision.
- This does not claim real-host fault injection, process crash testing, or
  distributed operation leases.

## Provenance

This is an independently authored repository decision based on SPEC-0003,
SPEC-0008, the public provider contract, and issue #87. No private source or
implementation was used.
