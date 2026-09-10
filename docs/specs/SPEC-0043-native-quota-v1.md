# SPEC-0043 — Native quota contract v1

The native quota projection is served by `o3kd` at `/quota` and
`/quota/{namespace}/{dimension}`. Responses are versioned as `v1` and use the
canonical `LimitKey`, `LimitValue`, and committed `Usage` authority. Reserved
amounts are deliberately omitted from the public projection. Tenant scope is
always taken from `AuthContext`; the operator collection at
`/operator/quotas/{project}` requires the canonical `quota:ReadQuota` action in
system/operator scope. Limit writes use `quota:ManageQuota`, reject unknown
dimensions and invalid values, and publish a durable `AuditEvent` with the
quota dimension and owner scope. `generation` is an in-process optimistic
concurrency generation for the mounted control-plane adapter; stale
`expected_generation` values are rejected before durable mutation. Durable
limits and usage remain authoritative across restart; a restarted adapter
begins a fresh generation sequence.

The response schema is [native-quota-v1.schema.json](../../contracts/native-quota-v1.schema.json).
