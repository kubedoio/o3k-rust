# P12.4 implementation notes

P12.4 reuses the existing durable operation journal and keeps the provider-
neutral `o3k_kernel::Operation` separate from `o3k_store::OperationRecord`.
Store state conversion is explicit and provider execution metadata remains
internal to the store/reconciler layers.

Idempotency reservations are durable and unique on `(owner_scope, action,
idempotency_key)`. For a newly accepted canonical request, the legacy operation
row, canonical operation metadata, and reservation commit in one transaction.
Identity validation binds their operation IDs, action, owner scope, resource
identity, and service/resource/action namespaces before insertion. Equivalent
replay is resolved before inspecting a new proposal's target and validates the
complete canonical operation plus its durable resource owner; conflicting reuse
creates no rows. The bounded request constructor fingerprints the action,
resource type, target, and recursively key-ordered semantic JSON with SHA-256.

Generation compare-and-set continues to use the existing atomic store update
primitives; `sqlite_generation_cas_allows_only_one_concurrent_writer` proves
one winner, one stale writer, and one generation advance.
`revive_resource_and_operation_persists_atomically_and_fences_generation`
proves that a stale durable mutation claim leaves no competing operation.
Provider dispatch remains downstream of this durable claim; there is no
provider callback inside the store CAS primitive. Native mutation HTTP wiring
and `If-Match` route integration remain deferred to #731. Recovery and
`UnknownOutcome` behavior remain owned by the existing reconciler journal and
are not duplicated here; reconciler test
`unknown_outcome_is_observed_without_duplicate_create` is the executable
observe-before-retry evidence.

`GET /o3k/v1/operations/{id}` is an ownership-safe native read boundary. It
returns the canonical Kernel operation and maps foreign and missing operations
to the same non-disclosure response. Test
`operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields`
creates the complete canonical triplet, closes and reopens SQLite, then proves
owner access, foreign/missing non-disclosure, and provider/agent/internal-field
redaction through the real store adapter and native router.

SQLite evidence includes the concurrent equivalent, conflict, and scope/action
races in `sqlite_idempotency_concurrent_*`; those tests exercise the complete
three-table primitive, and losing proposals and metadata are absent after
commit. `sqlite_canonical_idempotency_rolls_back_every_insert_on_failure`
injects failures at the canonical-metadata and reservation inserts and proves
all three tables roll back. The file-backed
`sqlite_canonical_idempotency_reopens_and_replays_complete_operation` proves
reopen and replay reconstruction. PostgreSQL uses the mandatory CI-provisioned
PostgreSQL 16.4 service and focused P12.4 integration target for the equivalent
runtime evidence. Test
`postgres_p12_4_atomic_triplet_concurrency_recovery_and_cas` covers canonical
conversion after reconnect, equivalent/conflicting/cross-scope races, losing
row absence, injected rollback, and concurrent generation CAS. Absence of
`O3K_DATABASE_URL` is a failure in that gate.

Historical rows without `canonical_operation_metadata` remain usable by legacy
OperationJournal paths but cannot be reconstructed as a public Kernel
Operation; conversion requires complete typed action, scope, resource, actor,
and timestamp metadata. Test
`legacy_operation_without_canonical_metadata_remains_internal` proves both
halves of that compatibility boundary. Provider-only fields are never
serialized by the native operation response.
