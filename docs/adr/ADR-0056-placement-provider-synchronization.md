# ADR-0056 — Durable placement provider synchronization

Status: Accepted for the placement ledger boundary.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, placement, governance

## Context

The placement ledger already supported allocations and restart recovery, but a
capability refresh had no safe operation that could create or update a provider
without trusting an agent-reported `used` value. That made it easy for future
runtime publication code to erase durable reservations or double-count them.

## Decision

`PlacementLedger::sync_provider` creates or updates a stable provider, applies
the reported inventory totals and state, and recomputes `used` exclusively from
the ledger's durable allocations. Capacity reductions therefore retain existing
allocations and naturally make the provider unavailable for new allocations.

This is a library boundary only. The daemon still needs an explicit capability
publication loop and a declared disk-capacity policy before a scheduler can be
wired into production startup.

## Consequences

Provider refreshes are idempotent with respect to allocation ownership and
survive reopen. Unavailable providers retain allocations while scheduler
eligibility excludes them. Agent-reported usage cannot silently release or
duplicate a reservation.
