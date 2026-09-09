# SPEC-0041 — Native metering definitions and snapshot usage v1

Status: implemented for the supported native profile

This specification defines the bounded native metering capability currently
advertised by O3K. It is intentionally narrower than billing or historical
time-series usage.

## Authority and scope

The canonical `resources` repository is the authority for the meters in this
version. A meter is advertised only when its value can be computed from that
repository without provider telemetry or client-side counting. The current
profile exposes project-scoped resource-count gauges for the resource kinds
registered by the native metering adapter.

O3K does not advertise network traffic, CPU utilization, storage I/O,
pricing, cost, invoices, or historical duration meters in v1.

## Definition contract

`GET /o3k/v1/metering/definitions` returns objects conforming to
`contracts/native-meter-definition-v1.schema.json`. Each definition has a
stable namespaced ID, owning service, unit, aggregation kind, applicability,
supported granularity, visibility and readiness status. The only supported
granularity is `instant`, and the only aggregation is a `gauge`.

## Snapshot contract

`GET /o3k/v1/metering` returns the current project-scoped snapshot. Each value
includes an RFC3339 `as_of` timestamp and the fixed authority
`o3k-resource-lifecycle`. `complete=true` means the bounded aggregate query
completed against the canonical repository; it does not imply historical
coverage. There is no pricing multiplication and no client-defined scope.

The query is authorized from `AuthContext` and the canonical metering action.
System scope and foreign project scope are denied by the native contract.

## Boundedness and failure

The adapter uses repository `COUNT` aggregation per declared resource kind;
it does not enumerate resources into application memory. A repository failure
returns an unavailable/internal response and never produces a fabricated zero.
Only a fixed, code-declared meter set is evaluated, so the request has a
bounded number of aggregate queries.

The bounded historical aggregate contract is exposed at
`GET /o3k/v1/metering/usage?meter_id=...&effective_from=...&effective_to=...`.
It reads only append-only durable metering events, is bounded by the optional
`limit` (maximum 10,000), and returns `complete=false` when more events exist
than the requested bound. It does not expose raw event payloads. The aggregate
is not billing, pricing, utilization, or a duration estimate: only lifecycle
events explicitly recorded by O3K may contribute. Event replay is idempotent
by event ID and conflicting reuse is rejected by both supported repositories.
Snapshot gauge IDs (for example `compute:server_count`) are not historical
event streams and therefore are rejected by this endpoint; an empty event
aggregate must never be presented as an authoritative historical zero.
Historical aggregation does not imply that every lifecycle path emits events;
meters without authoritative events remain unavailable rather than fabricated.

## Security

Definitions and snapshots contain no provider IDs, credentials, telemetry
payloads or pricing data. Tenant authorization is performed before the reader
is called, and the reader receives only the authenticated effective project
ID.
