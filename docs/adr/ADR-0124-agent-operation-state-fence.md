# ADR-0124 — Keep agent operation updates separate from resource observations

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, identity, governance

## Context

An authenticated compute-agent `OperationUpdate` reports command progress and
ownership identifiers, but it does not report the provider's live resource
state. Treating a successful create update as `active` (or a successful
delete update as `DELETED`) lets a command acknowledgement outrun the actual
libvirt observation.

## Decision

Successful agent operation updates may persist operation completion and a
validated provider reference, but must preserve the resource's existing
`observed_state`. Only a successful, explicit agent `Observation` may change
that state. Lifecycle reconciliation that directly queries the provider keeps
its separate observation path.

## Consequences

Resources remain `unknown` or otherwise unchanged until the agent supplies
authoritative state. This prevents Nova projections from claiming a guest is
active or deleted solely because a command was acknowledged, while retaining
idempotent operation and ownership recovery.
