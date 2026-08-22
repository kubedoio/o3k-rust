# P12.4 implementation notes

P12.4 reuses the existing durable operation journal and keeps the provider-
neutral `o3k_kernel::Operation` separate from `o3k_store::OperationRecord`.
Store state conversion is explicit and provider execution metadata remains
internal to the store/reconciler layers.

Idempotency reservations are durable and unique on `(owner_scope, action,
idempotency_key)`. The bounded request constructor fingerprints the action,
resource type, target, and recursively key-ordered semantic JSON with
SHA-256. Equivalent reservations return the existing operation ID; conflicting
reuse fails closed; different ownership scopes cannot collide.

Generation compare-and-set continues to use the existing atomic store update
primitives; `sqlite_generation_cas_allows_only_one_concurrent_writer` proves
one winner and one stale writer. Native mutation HTTP wiring and `If-Match`
route integration remain deferred to #731. Recovery and `UnknownOutcome`
behavior remain owned by the existing reconciler journal and are not duplicated
here.

`GET /o3k/v1/operations/{id}` is an ownership-safe native read boundary. It
returns the canonical Kernel operation and maps foreign and missing operations
to the same non-disclosure response.

SQLite evidence includes the concurrent equivalent, conflict, and scope/action
races in `sqlite_idempotency_concurrent_*`; losing proposals are removed before
commit. PostgreSQL uses the mandatory CI-provisioned PostgreSQL 16.4 service and
`tests/postgres_p12_4.rs` for persistence/reconnect, canonical conversion,
equivalent replay, conflict, cross-scope isolation, and stale-generation
checks. The test fails closed when `O3K_DATABASE_URL` is absent.

Historical rows without `canonical_operation_metadata` remain usable by legacy
OperationJournal paths but cannot be reconstructed as a public Kernel
Operation; conversion requires complete typed action, scope, resource, actor,
and timestamp metadata. Provider-only fields are never serialized by the native
operation response.
