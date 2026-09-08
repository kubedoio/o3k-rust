# SPEC-0040 — Native resource and action schemas

The native discovery contract resolves a manifest-declared resource and its
`schema_version` to a stable schema identity:

`GET /o3k/v1/resource-schemas/{namespace}/{collection}/{version}`

The response is the common native resource envelope composed with the
resource-specific `spec` contract. Built-in contracts are Rust public DTOs
with `serde(deny_unknown_fields)`; the same DTO is deserialized at the native
create boundary and projected with `schemars`. A declared resource without a
registered contract is not represented by `spec: object` and schema resolution
fails closed.

`schema_version` versions the public representation and operation input
semantics. Adding optional response fields is compatible; removing fields,
changing types, narrowing enums, making fields required, or changing input
meaning requires a new version. `$id` is deterministic from namespace,
collection, and version. Only current versions are served; unknown versions
return not found. Clients must ignore unknown response fields, reject or
explicitly negotiate newer input versions, and never infer undeclared actions.

Schemas describe structural validation only. Authorization, ownership, quota,
referenced-resource existence, and other dynamic semantics remain runtime
checks. Schema discovery grants no authorization. Region and availability-zone
identity remains owned solely by the canonical location discovery contract
from PR #889; schemas do not copy a provider region enum.
