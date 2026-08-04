# ADR-0107 — Retry Placement release after provider deletion

Status: Accepted for the repository-side issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: placement, governance

#82 lifecycle slice.

## Context

Server deletion marks the resource `DELETED` after the provider confirms
deletion, then releases its recorded Placement allocation. If Placement
publication fails at that point, the provider resource is already gone but the
allocation remains in the in-memory ledger. The next idempotent delete request
currently returns before attempting the release again, so capacity can remain
reserved indefinitely in that process.

## Decision

An idempotent delete of an already-`DELETED` resource re-reads its durable
create intent and retries the recorded Placement release when a scheduler and
binding are configured. Release remains idempotent; a failed retry returns the
Placement error so callers and reconciliation can retry later.

## Consequences

This closes the local post-delete allocation retry gap and does not introduce
agent inventory publication, cross-process locking, a real Placement service,
or real-host acceptance. Those remain issue #82 blockers.
