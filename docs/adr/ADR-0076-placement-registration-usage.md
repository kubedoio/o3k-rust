# ADR-0076 — Reconcile usage during provider registration

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, placement, governance

## Context

Placement allocations are durable, but provider registration receives a fresh
inventory snapshot. Replacing an existing provider's inventories previously
trusted its reported `used` values. A re-registration could therefore erase
reservations or overstate them, allowing a later allocation to exceed the
capacity represented by the durable allocation map.

## Decision

When `register_provider` updates an existing provider, it recomputes
`Inventory.used` from that provider's durable allocations. Reported `used`
values are ignored. Usage is added only for resource keys present in the new
inventory snapshot; allocations remain durable even when a refreshed snapshot
does not report one of their resource keys.

The same reconciliation helper is used by registration, refresh, and provider
synchronization so all inventory replacement paths share the same accounting
rule. Totals, reserved capacity, allocation ratios, provider state, generation
fencing, and allocation ownership remain governed by the existing ledger
semantics.

## Consequences

Re-registering a provider cannot make an existing reservation available twice,
and the result remains correct after reopening the ledger. A capacity decrease
naturally leaves existing allocations recorded while rejecting new allocations
that exceed the refreshed inventory. Provider capability publication and the
policy for mapping host disk capacity to `DISK_GB` remain separate follow-ups.
