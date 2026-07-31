# ADR-0020 — Durable agent-event reconciliation

## Status

Accepted for the alpha command-routing slice.

## Context

The authenticated compute-agent router could deliver operation updates, but
those updates were not connected to the durable operation and resource records.
Without that mapping, a control-plane restart could lose the agent's result or
persist provider-specific failure details.

## Decision

`OperationJournal::apply_agent_update` validates operation and resource identity,
maps protocol states into durable states, and applies terminal success, failure,
and unknown-outcome updates to the existing journal. Successful create and
delete updates update the resource observation and attach an idempotent
`compute-agent` provider reference. Repeated terminal updates are no-ops.
Failure persistence stores only a stable error category and generic message;
the protocol's redacted message is never written to the durable store.

The durable schema does not yet persist the protocol sequence number. Terminal
state fencing prevents late updates from regressing a completed operation; a
future multi-process event consumer must add durable sequence fencing before
multiple consumers are enabled.

## Consequences

Agent results now survive control-plane reconciliation through the same store
used by provider-backed operations. Full control-plane command construction,
dispatch subscription, and create realization remain integration work, as do
real-libvirt and clean-host validation.
