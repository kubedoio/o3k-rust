# ADR-0118 — Reject deterministic name conflicts before Placement reservation

Status: Accepted for the repository-side portion of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, placement, governance

#82.

## Context

Nova server names are unique within a project in the current control-plane
model. The create path previously discovered that conflict only after it had
reserved Placement capacity, relying on rollback. Rollback prevents a leak,
but it still creates unnecessary ledger mutations and makes a rejected
request observable as a reservation/release cycle.

## Decision

Check the durable server list for an active same-name resource before calling
the scheduler. Keep the existing check after scheduling as a race fence for a
concurrent create. The pre-check is advisory with respect to concurrency; the
post-check remains authoritative for the request that won the race.

## Consequences

Known duplicate-name requests do not mutate Placement state. This is tested by
asserting that the provider generation and allocation set remain unchanged.
The decision does not claim real Nova, Placement, agent, guest, or host
evidence; the remaining integration gates stay open in issue #82.
