# SPEC-0042 — Durable Cloud Kernel Audit v1

Status: accepted implementation contract for the B0 native northbound closure.

This specification defines the O3K-owned Audit authority. Audit is durable
security and control-plane evidence; it is not tracing, metrics, an Operation
store, or an application log. The canonical event is created by the Cloud
Kernel after authorization and carries the request and (when applicable)
Operation correlation established by the authenticated `AuthContext`.

## Authority and scope

The effective owner scope is derived from `AuthContext` by the application
boundary. A request, query parameter, event field, relationship, or provider
identity cannot broaden it. Tenant reads are limited to the caller's effective
project when tenant Audit visibility is enabled. Cross-project and system-wide
reads require an explicitly authorized system/operator action. Existence of an
event never grants access to its resource or to another event.

The durable repository is the canonical Audit history. Operation state remains
authoritative for work state; Audit records the evidence and correlation, and
must not become a second mutable Operation state machine.

## Mandatory durability classes

The following events are mandatory Audit evidence:

* authentication and security decisions exposed by the Cloud Kernel, including
  authorization denials;
* accepted resource mutations and domain actions, including successful,
  deterministic-failure, and `unknown_outcome` results;
* IAM governance mutations;
* quota administration;
* privileged operator/system mutations and diagnostics actions that change
  control-plane state.

Ordinary successful reads, health polling, debug output, and provider telemetry
are not mandatory Audit unless a later version explicitly classifies them.
They must not be silently represented as mandatory control-plane evidence.

## Event representation and redaction

An event contains only typed, bounded canonical fields: event ID, timestamp,
request/audit correlation IDs, principal identity/kind, effective scope,
service, canonical ActionId, resource type and public resource ID, Operation
ID when applicable, outcome, and a bounded canonical reason/category.

The public and durable representations structurally have no fields for
passwords, bearer or refresh tokens, private keys, CHAP or provider
credentials, user-data, arbitrary request/response bodies, raw environment,
connection strings, host/device paths, or unrestricted backend exceptions.
Provider failures are mapped to a bounded reason category and safe detail, if
any. Raw provider payloads are never an Audit serialization fallback.

## Mutation and Audit ordering

For a mandatory event, an O3K mutation may not acknowledge authoritative
success merely because an event was placed in an in-memory queue. The required
Audit durability boundary must return success before the control-plane success
is acknowledged.

Where resource/Operation state and Audit share the same database transaction,
the event and state transition are committed atomically. Where an external
provider is involved, O3K does not claim distributed atomicity: intent and
Operation state are persisted before the provider side effect, and provider
uncertainty is recorded as `unknown_outcome` when the durable boundary is
available. A database or Audit failure before the required boundary completes
fails the operation according to its canonical error contract and cannot be
reported as successful.

An acknowledged mandatory event is never dependent on a later best-effort log
write. A crash before commit may lose an unacknowledged attempt, but must not
lose a committed event. Recovery/reconciliation may append a distinct truthful
recovery event; it must not manufacture a duplicate successful event.

## Failure, backpressure, and readiness

The repository is the durability boundary. A bounded in-memory queue may be
used only as an optimization after durable admission is guaranteed, never as
the acknowledgement boundary for mandatory events.

If the database is unavailable, a commit fails, the writer terminates, or the
bounded pending capacity is exhausted, mandatory publication fails closed with
a typed unavailable/degraded result. No event is silently dropped and no
unbounded retry or queue is permitted. Retry attempts are finite and
observable; shutdown drains only work already durably admitted and does not
acknowledge new mandatory work after the boundary is unavailable. Readiness
must expose Audit degradation so operators can distinguish unavailable Audit
from healthy service operation.

Duplicate event IDs are idempotent only when the canonical event payload is
equivalent. Reuse with different payload or scope is an explicit conflict.

## Query and page contract

Audit collection uses the A0 `ResourceQuery`/`ResourcePage` bounded contract.
The repository receives validated effective scope, supported filters, stable
ordering, and a decoded continuation key; it never receives or creates a
public opaque cursor. The query uses deterministic keyset ordering and a
bounded `limit + 1` lookahead. `items.len() <= limit`; `has_more` is true only
when a later item was observed; and `has_more == true` requires a continuation
cursor. Empty and final pages have `has_more == false` and no cursor.

The MVP filter vocabulary is bounded and indexed: time range, service,
ActionId, outcome, resource type, resource ID, Operation ID, principal,
request ID, and audit/correlation ID. Arbitrary predicates and unbounded time
ranges are unsupported. Cursors are integrity-protected and bind the format
version, effective scope/mode, filters, ordering, and continuation identity.
Oversized or malformed cursors fail before expensive decoding. A cursor from a
different tenant, mode, resource type, or filter set fails closed without
revealing event existence.

Pagination has documented weak-consistency semantics: inserts before an
already-issued anchor are not returned by that continuation, inserts after the
anchor may appear on a later page, deletion of an anchor does not invalidate
the keyset continuation, and updates to the sort key may move an event between
pages. Snapshot isolation is not claimed.

## Retention

Each deployment profile configures a minimum retention horizon. Pruning is a
privileged maintenance operation, bounded per invocation, deterministic by
timestamp and event ID, and observable. Pruning never rewrites or mutates
remaining event identity. Cursors that advance across pruned rows follow the
normal weak-consistency keyset rules and cannot cross scope. External archive
and compliance export are future adapters, not hidden durability assumptions.

## Store parity and recovery

SQLite and PostgreSQL implementations must provide equivalent insert,
idempotency/conflict, show, bounded page, index, prune, and restart semantics.
The query must execute with a database limit of at most `requested + 1`; the
service must not load all history into memory. Required indexes cover effective
scope, deterministic ordering, and each advertised filter combination as
appropriate to the supported profile.

Migrations are forward-only and restart-safe. A successful append remains
queryable after process restart. Transaction failures produce no false
successful event. Concurrency must not lose events or create event-ID
collisions.

## Operation correlation

For an asynchronous mutation or action, the event references the canonical
Operation ID returned by the native Operation API. Audit may record attempts
and recovery transitions only with distinct event identity and truthful
outcome. It does not duplicate or override Operation status.

## Versioning and evidence

The native Audit API and schema are versioned independently of storage
implementation. Discovery advertises Audit only when its bounded repository,
authorization, and live readiness are registered. The B0 acceptance verifier
must prove durability, failure semantics, isolation, redaction, bounded
queries, SQLite/PostgreSQL parity, retention, restart, and production
`o3kd` wiring. `NOT PROVEN` is not a production claim.
