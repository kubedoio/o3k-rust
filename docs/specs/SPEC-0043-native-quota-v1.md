# SPEC-0043 — Native quota contract v1

The native quota projection is served by `o3kd` at `/quota` and
`/quota/{namespace}/{dimension}`. Responses are versioned as `v1` and use the
canonical `LimitKey`, `LimitValue`, and committed `Usage` authority. Reserved
amounts are deliberately omitted from the public projection. Tenant scope is
always taken from `AuthContext`; the operator collection at
`/operator/quotas/{project}` requires the canonical `quota:ReadQuota` action in
system/operator scope. Limit writes use `quota:ManageQuota`, reject unknown
dimensions and invalid values, and publish a durable `AuditEvent` with the
quota dimension and owner scope. `generation` is a durable,
per-(scope,namespace,dimension) version. An absent override reads as
`Unlimited` at generation `0`; every accepted replacement, including clear or
reset to the explicit unlimited value, atomically advances the generation by
one. Mutations require `expected_generation` and use a database compare-and-set,
so stale writers receive a conflict across restarts and independent processes.
Quota state and its required audit record commit in the same SQLite/PostgreSQL
transaction. Reserved amounts remain internal; `usage` is committed
consumption only. Clear/reset is not an unconditional delete and therefore
requires the same generation precondition as a finite limit. Effective tenant
scope is always taken from `AuthContext`; a project path is accepted only for
an authorized system/operator action.

The response schema is [native-quota-v1.schema.json](../../contracts/native-quota-v1.schema.json).
