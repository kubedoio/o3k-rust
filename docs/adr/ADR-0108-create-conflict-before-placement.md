# ADR-0108 — Check durable create conflicts before Placement allocation

Status: Accepted for the repository-side issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, placement, governance

#82 lifecycle slice.

## Context

`ComputeService::create_server` previously reserved Placement capacity before
checking whether the deterministic server ID already existed. A conflicting
durable create intent could therefore return `Conflict` after acquiring a new
allocation, leaving capacity reserved without a journaled operation.

## Decision

Resolve the existing-resource/idempotency check before scheduling. Existing
requests are compared after ignoring Placement binding fields, since those
fields are scheduling results rather than caller intent. Pre-journal duplicate
name and store-error paths continue to release any allocation acquired by the
current request.

## Consequences

The bounded create path no longer allocates for a known durable conflict. This
does not add agent inventory wiring, real Placement integration, restart
recovery, or real-host acceptance.
