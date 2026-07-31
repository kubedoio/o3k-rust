# ADR-0013 — Lifecycle safety boundaries for the libvirt provider

## Status

Accepted for the portable lifecycle foundation; real guest/network validation remains environment-gated.

## Decision

The compute create intent carries the project and requested network identifiers
through the provider contract. The libvirt provider writes the project identity
into O3K domain metadata and must successfully parse that metadata before
inspecting or mutating a domain. A same-prefix name is not ownership evidence;
foreign or malformed domains are treated as not found.

Nova actions observe the provider after the mutation and persist the observed
state instead of assuming the requested state succeeded.

This change deliberately does not claim that the network identifiers are
already translated into TAP/bridge devices, config-drive media, or a resolved
image overlay. Those integrations require the agent command path, scheduler,
and host-backed TestLab evidence tracked by issue #47 and the release gate.

## Consequences

- Provider requests retain enough identity for later network/config-drive
  translation without silently discarding Nova input.
- Destructive libvirt operations fail closed for foreign and malformed domains.
- A successful API mutation does not hide a divergent provider state.
- Full restart reconciliation and durable provider operation idempotency remain
  separate follow-up work; the release gate must continue to reject skipped
  real-libvirt evidence.
