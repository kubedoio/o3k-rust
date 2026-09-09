# SPEC-0042 — Native audit query v1

The native audit API exposes durable Cloud Kernel audit events at
`GET /o3k/v1/audit/events` and `GET /o3k/v1/audit/events/{id}`. The effective
scope is taken only from the authenticated O3K `AuthContext`; query parameters
cannot broaden it.

Collection reads are repository-bounded and use an opaque cursor bound to the
effective scope and every filter. Supported filters are event ID, timestamp
range, principal, service, action, outcome, resource type/ID, operation ID,
request ID, and audit ID. Values are bounded and control characters are
rejected. Timestamp comparisons require canonical, lexically ordered O3K
timestamps. The service rejects an inverted range.

The SQLite and PostgreSQL repositories apply scope, filters, continuation, and
limit in SQL. No native handler materializes an unbounded audit history.

Tenant/project readers receive only events for their effective project. System
and operator-wide queries are a distinct authorization path: they require the
canonical `operator:ReadAudit` action and an operator role. A tenant caller
cannot select a target scope through query parameters; an operator may use the
bounded optional `scope` filter according to the system API contract.

Audit events remain distinct from Operations; `operation_id`, when present, is
only a correlation field. Public views contain no raw request/response bodies,
credentials, tokens, provider payloads, or arbitrary backend errors.

## Durability and retention

The audit repository is the durable authority. Production composition uses
`DurableAuditSink`, which applies bounded synchronous admission: `record`
returns only after the event has been durably inserted; a full queue or
acknowledgement timeout fails closed, and a database or
serialization failure moves the sink to a sticky failed state. The sink stops
consuming later events after a write failure so it cannot silently create a
hole in the audit stream. Reopening the same repository after process restart
preserves already acknowledged events.

Retention is an explicit operator-maintenance policy, not a request-path
operation. `prune_audit_events_before` deletes at most 1,000 rows per call,
selected by canonical timestamp and deterministic event-id order. A scheduler
repeats bounded batches and records its policy/evidence separately. Events at
or after the cutoff are never deleted, and tenant API queries cannot invoke
retention. SQLite and PostgreSQL implement the same bounded semantics.

The legacy synchronous `AuditSink::record` method is retained for compatibility,
but the kernel also exposes `record_checked`, which returns the stable
`AuditSinkError::Unavailable` identity. Production mutation paths that require
fail-closed audit semantics must call `record_checked` before an external side
effect and abort on failure. `DurableAuditSink` returns that error after a
sticky persistence failure and readiness remains failed. Transactional
mutation-plus-audit atomicity is not claimed by this v1 contract for mutation
paths that have not adopted an outbox-owned transaction; those paths must not
advertise atomic audit semantics.

## Mutation-path coverage

Authenticated native Compute server create, delete, and lifecycle actions use
the canonical durable operation journal. Compatibility entrypoints for those
same server mutations delegate to that journal as well; synchronous
compatibility callers map a non-terminal-success operation to the historical
conflict response, while the native API exposes the canonical operation state.
Network compatibility port deletion, network/subnet updates, port rename, and
security-group/security-group-rule update/delete paths record checked audit
admission before changing canonical authority. These paths fail closed on
audit outage and preserve retry identity where an operation key exists.

This does not promote every legacy mutation to transactional audit semantics.
Compute attachment/keypair/flavor helpers and remaining Network compatibility
mutations (including security-group binding replacement and other composite
policy workflows) still require their own canonical operation/outbox migration
before they can claim mutation-plus-audit atomicity. They remain explicitly
outside the production atomic-audit claim until that migration is complete.
