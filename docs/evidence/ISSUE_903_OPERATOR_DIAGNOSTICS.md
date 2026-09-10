# Issue #903 — Operator diagnostics evidence (DRAFT)

> Status: **draft** — placeholders to be filled after final CI.
> Every value in brackets (`[...]`) is an exact placeholder. Nothing below
> that is not already measured should be read as a passed claim.

## Scope

Issue #903 implements the native operator diagnostics/capacity projection
(SPEC-0045, contract `contracts/native-diagnostics-v1.schema.json`) for the
`native-rust-testlab` profile. This document records the evidence for that
implementation at the current working branch.

- **Base SHA (authoritative `main`):** `1bdecfcb8517133383902b5bb9062019ed945567`
- **Working branch:** `planning/operator-diagnostics-capacity`
- **Candidate head SHA:** `[fill: git rev-parse HEAD after CI]`
- **Status:** draft; CI not yet run to completion on this branch (`[pending]`).

## Authority map

Each projection source class and its canonical authority:

| Projection | Authority |
|---|---|
| Services / controllers | Shared in-process `ManifestRegistry` (configuration readiness); composition probe re-observes |
| External controllers | Periodic probe, 5 s timeout → synthesized unhealthy |
| Providers | `AgentNodeRegistry` availability + `observed_at_unix_ms` + durable placement state |
| Capacity | Bounded placement aggregate (`capacity_summary`, fail-closed at 64 classes) + agent observation clock |
| Control-plane liveness | Durable `controller_sessions` active leases |
| Locations | #887 `LocationRegistry` topology only (no fabricated liveness) |

## Status semantics

- Vocabulary: `healthy`, `degraded`, `unavailable`, `stale`, `unknown` — distinct by design; `unknown` (never observed) and `stale` (observation older than freshness) never collapse into `healthy`.
- Provider precedence: never-observed → `unknown` even if durable `Enabled` (restart safety); `Disabled`/`Deleted` → `unavailable`; `Draining` → `degraded`; agent `Unavailable` → `stale`/`unavailable` (`heartbeat_lost`); heartbeat older than lease → `stale`; else `healthy`.
- Service mapping: `Ready`→healthy, `NotReady`→unavailable/`readiness_failed`, `Incompatible`→degraded/`protocol_incompatible`, `Disabled`→unavailable/`administratively_disabled`, `Declared`→unknown/`never_observed`.
- Aggregate summary: worst of services and providers aggregates; any unavailable/stale/degraded degrades the platform.

## Freshness thresholds

- Agent lease: **15 000 ms** (`AGENT_LEASE_MS`, mirrors compute-agent `DEFAULT_LEASE` = 15 s).
- Controller probe interval: 15 s; external-controller probe timeout 5 s.
- Service `observed_at_unix_ms`: probe/stored observation or process start.
- Restart: durable placement state alone never reports healthy; `unknown`/`stale` until fresh re-observation.
- Capacity status: `Unknown` when no providers; `Stale` when durable last-known without fresh observation; `Degraded` on partial provider failure; else `Healthy`.

## Capacity dimensions

Supported (placement-authoritative, unit in parentheses):

- `VCPU` (`count`)
- `MEMORY_MB` (`mib`)
- `DISK_GB` (`gib`)

`available = allocatable − reserved − allocated` with saturating arithmetic
(never negative, never wraps; negative remainder clamps to zero).

Explicitly **unsupported** (never exposed, not claimed):

- Storage (non-placement) — not a placement-authoritative dimension
- Network — not a placement-authoritative dimension
- Ceph — not a placement-authoritative dimension
- Quota — not a placement-authoritative dimension

Capacity class bound: 64 (fail closed). Page bound: `limit` default 50, max 200.

## Security boundary

- Single canonical action `operator:ReadDiagnostics`, System scope + durable `operator` role via `StaticAuthorizer`.
- Tenant/project-scoped callers denied regardless of role name / route / IdP claim.
- Never exposes: node id, agent epoch, session ids, digests, service principal, `health.detail`, connection strings, credentials, host paths.
- Provider unhealthy is data (HTTP 200), not HTTP 500; `NotAvailable` when authority unconfigured; `Corrupt` → HTTP 500.

## Validation checklist

The following tests were added by the implementation and must pass:

- `crates/o3k-native-api/src/diagnostics.rs`
  - `status_vocabulary_is_stable_and_distinct`
  - `aggregate_never_collapses_unknown_into_healthy`
  - `aggregate_all_healthy_is_healthy`
  - `aggregate_partial_failure_is_degraded_not_healthy`
  - `aggregate_all_unavailable_is_unavailable`
  - `aggregate_stale_is_degraded_not_healthy`
  - `capacity_available_is_saturating`
  - `cursor_round_trips_and_rejects_invalid`
  - `page_size_is_bounded`
- `bins/o3kd/src/native_adapters/diagnostics.rs`
  - `provider_never_observed_is_unknown_even_when_durable_state_enabled`
  - `provider_healthy_when_agent_observed_fresh`
  - `provider_stale_when_heartbeat_older_than_lease`
  - `provider_draining_is_degraded`
  - `provider_disabled_is_unavailable`
  - `capacity_unknown_when_no_providers`
  - `capacity_stale_when_providers_exist_but_no_fresh_observation`
  - `capacity_arithmetic_is_saturating`
  - `service_declared_is_unknown_ready_is_healthy`
  - `provider_status_unknown_when_no_snapshot_despite_enabled_durable_state`
  - `parse_timestamp_accepts_rfc3339_and_sqlite_datetime`
- `crates/o3k-store` (bounded placement reads exercised by the adapter tests against the real SQLite adapter): `list_providers_bounded`, `capacity_summary` fail-closed at 64 classes.

## CI / evidence not yet run

- `[pending]` `cargo fmt --all -- --check`
- `[pending]` `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `[pending]` `cargo test --workspace --all-features`
- `[pending]` confirm head SHA and record per-file coverage
- `[pending]` full-profile/process-level verification (protected workflow) — not part of the portable gate
