# ADR-0110 — Clean owned console artifacts after successful Nova deletion

## Status

Accepted

## Context

Nova server deletion completed through `ComputeService`, but the API route did
not remove the durable console artifact associated with the deleted server.
The console store already provides per-UUID, idempotent cleanup. Cleanup must
not run when provider deletion fails, because the artifact remains useful for
retry and diagnosis, and a project must not be able to target another
project's artifact.

## Decision

The authenticated Nova delete route first completes the existing
project-scoped `ComputeService::delete_server` operation. Only on success does
it call the configured `ConsoleService::cleanup` with that same server UUID.
The cleanup operation is idempotent, so repeated successful deletes are safe.
If compute deletion fails, the route returns the existing compute error and
leaves the console artifact untouched. If post-delete console cleanup itself
fails, the route returns an internal error so a caller can retry the cleanup;
no host or libvirt artifact is inferred or touched.

## Consequences

Successful API deletion removes only the O3K console file derived from the
owned server UUID. Failed deletion preserves it. This closes the repository
cleanup boundary without claiming real guest output, host cleanup, or
protected real-host acceptance; those remain issue #84 evidence work.
