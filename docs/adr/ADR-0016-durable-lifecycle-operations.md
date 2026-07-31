# ADR-0016 — Durable lifecycle action and delete operations

## Status

Accepted for the alpha lifecycle slice.

## Context

Create operations were journaled, but Nova start, stop, reboot, and delete
called the provider directly. A provider timeout could therefore leave the
API showing a successful mutation with no durable operation to observe after a
process restart. Delete also marked the resource deleted immediately after a
single provider call.

## Decision

Persist lifecycle operations with an explicit operation kind in the store.
Operation IDs are deterministic for the project, resource, action target, and
resource generation, so retries after an unknown result reuse the same durable
identity while a later generation can issue a new action. The journal routes
provider calls, records accepted/running/unknown/failed states, and observes
unknown deletes by checking whether the provider resource has disappeared.

The ComputeService action and delete APIs now use this journal. An operation
that is not durably successful is surfaced as a conflict by the current
synchronous API; reconciliation can subsequently finish it from the stored
operation record.

## Consequences

Lifecycle mutations are restart-observable and idempotent at the control-plane
boundary. Provider references and dependent network, overlay, config-drive,
DHCP, and Placement cleanup remain separate integrations and are not claimed
by this decision.
