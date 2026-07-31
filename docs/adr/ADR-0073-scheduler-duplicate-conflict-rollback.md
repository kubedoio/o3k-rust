# ADR-0073 — Roll back allocations on duplicate-name conflicts

## Status

Accepted for the alpha scheduler integration.

## Context

`ComputeService::create_server` reserves Placement capacity before checking
whether the requested project/name is already in use. A request with a new
idempotency key can therefore reach the duplicate-name check after acquiring a
new allocation. Returning `Conflict` without releasing that allocation leaks
capacity and can turn later valid requests into `NoValidHost` errors.

## Decision

When the duplicate project/name check rejects a request, release the
allocation created for that request through `Scheduler::release_terminal`
before returning `ComputeError::Conflict`. If releasing the allocation fails,
surface the scheduler error instead of claiming that the conflict was safely
handled.

The ordering remains schedule, validate durable/name conflicts, then persist
the create intent. Moving the name check before scheduling would reduce this
rollback case, but would not replace Placement's atomic capacity reservation
or protect against races between concurrent callers. The rollback is therefore
the required terminal cleanup at this boundary.

## Consequences

Repeated conflicting requests no longer consume Placement capacity. Unknown
outcomes after intent persistence retain their allocations for reconciliation,
while terminal provider failures and deletes continue using their existing
release paths. The regression test uses a two-capacity provider so each
duplicate request can reserve one transient allocation; it proves every
request returns `Conflict` and leaves only the original allocation.
