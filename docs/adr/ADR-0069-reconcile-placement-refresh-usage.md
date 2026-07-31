# ADR-0069 — Reconcile usage during placement inventory refresh

## Context

Placement allocations are durable, while inventory reports are refresh input.
The legacy `refresh_inventory` path trusted the reported `used` value and could
make allocated resources appear available after a refresh.

## Decision

Recompute `Inventory.used` from the provider's durable allocation map whenever
inventory is refreshed. Reported usage is ignored for reservation accounting,
matching the already fail-closed `sync_provider` path.

## Consequences

Capacity refreshes cannot erase reservations or allow over-allocation. Totals,
reserved capacity, allocation ratios, generation fencing, and provider state
remain controlled by the existing ledger semantics.
