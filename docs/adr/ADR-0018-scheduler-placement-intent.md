# ADR-0018 — Persist scheduler and Placement bindings in create intent

## Status

Accepted for the alpha control-plane scheduling slice.

## Context

Placement and the first-fit scheduler existed as isolated, tested libraries,
but `ComputeService::create_server` did not call them. A server could be
created without a selected host or a durable allocation, and delete could not
release capacity.

## Decision

`ComputeService` can be supplied a `Scheduler`. When present, create derives a
deterministic allocation ID from the server ID, schedules the flavor, and
persists the selected provider and allocation IDs inside the durable
`CreateInstanceRequest` intent. Repeated creates reuse the idempotent
allocation. Successful delete releases the recorded allocation; unknown
outcomes retain it for reconciliation. Terminal create failure releases the
allocation.

The provider contract maps placement bindings as control-plane intent metadata;
the provider itself does not trust those fields for authorization or capacity.
The default constructor remains unscheduled for compatibility with deployments
that have not configured a Placement ledger; production wiring must opt in with
an explicitly populated scheduler.

## Consequences

Host selection, allocation persistence, and terminal cleanup now have a real
durable path and regression coverage. Dynamic agent capability registration,
command construction from the selected binding, and full dependent network,
image, and config-drive realization remain follow-up work.
