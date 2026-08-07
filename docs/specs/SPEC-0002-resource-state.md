# SPEC-0002 — Resource and Operation State

Status: Draft

## Resource identity

O3K resource IDs are UUIDs owned by O3K. Provider IDs are separate opaque values. Mapping is persisted.

## Desired and observed state

A resource stores desired state, observed state, generation, observed generation, provider reference, and latest operation/error summary.

## Operation states

```text
Pending -> Running -> Succeeded
                  -> Retryable
                  -> UnknownOutcome
                  -> Failed
```

`UnknownOutcome` requires observation before reissuing an external mutation.

## Initial server lifecycle

```text
Requested -> Building -> Active
Active -> Stopping -> Stopped -> Starting -> Active
Active -> Rebooting -> Active
Stopped -> Rebooting
Any state except Deleted and Error -> Error
Any state except Deleted and Deleting -> Deleting -> Deleted
```

`Deleted` is the only terminal state; `Error` retains only the delete
transition. Transitions are validated in the domain. Public OpenStack status
values are projections and may not be identical to internal states.

## Idempotency

- client retry of the same accepted request must resolve to the same operation/resource when an idempotency identity is available;
- reconciler retries must not create duplicate external resources;
- delete of an already absent provider resource converges to deleted;
- a timeout cannot be treated as proof of failure.

## Concurrency

State updates use version/generation checks. Lost updates are rejected and retried through application logic.
