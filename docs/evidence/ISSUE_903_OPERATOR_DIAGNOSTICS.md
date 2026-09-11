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
- **Status:** draft; CI not yet run to completion on this branch (`[pending]`). An adversarial review converged on the corrected SPEC-0045 wording (fail-closed bounds, service staleness gate, saturating capacity) and added the contract/store tests below; review convergence is still in progress (`[pending]`).

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
- Service staleness gate: **75 000 ms** (`SERVICE_OBSERVATION_THRESHOLD_MS`, five 15 s probe intervals). An external controller with a `session` whose observation is older than this is `stale` / `observation_stale` even if its last state was `Ready`; in-process services (no `session`) are configuration-authoritative and never go stale from age.
- Service `observed_at_unix_ms`: probe/stored observation or process start.
- Restart: durable placement state alone never reports healthy; `unknown`/`stale` until fresh re-observation.
- Capacity status reflects durable placement state + agent liveness (a durably draining/deleted provider is never healthy here): `Unknown` when no providers; `Stale` when durable last-known without fresh observation; `Degraded` on partial provider failure; `Degraded` when a dimension is over-allocated; else `Healthy`.

## Capacity dimensions

Supported (placement-authoritative, unit in parentheses):

- `VCPU` (`count`)
- `MEMORY_MB` (`mib`)
- `DISK_GB` (`gib`)

`available = allocatable − reserved − allocated` with saturating arithmetic
(never negative, never wraps; negative remainder clamps to zero). An
over-allocated dimension (`allocated > allocatable − reserved`, i.e. a
drifted/corrupt durable invariant) makes the whole capacity status `degraded`
while `available` stays saturating.

Explicitly **unsupported** (never exposed, not claimed):

- Storage (non-placement) — not a placement-authoritative dimension
- Network — not a placement-authoritative dimension
- Ceph — not a placement-authoritative dimension
- Quota — not a placement-authoritative dimension

Capacity class bound: 64 (fail closed). Page bound: `limit` default 50, max 200
(`limit=0` treated as the default 50; above 200 is `BadRequest`). Service
collections fail closed at `MAX_SERVICES = 256`; provider aggregates fail closed
at `MAX_PROVIDERS = 65 536`.

## Security boundary

- Single canonical action `operator:ReadDiagnostics`, System scope + durable `operator` role via `StaticAuthorizer`.
- Tenant/project-scoped callers denied regardless of role name / route / IdP claim.
- Never exposes: agent `node_id` field, agent epoch, session ids, digests, service principal, `health.detail`, connection strings, credentials, host paths. The providers endpoint exposes only the durable placement `provider_id` (which by placement design equals the agent/node id); no additional node identity is exposed.
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
  - `summary_validates_against_diagnostics_contract_schema`
  - `public_dtos_never_carry_credentials_or_node_identity`
  - `service_validates_against_diagnostics_contract_schema`
  - `provider_validates_against_diagnostics_contract_schema`
  - `capacity_validates_against_diagnostics_contract_schema`
  - `page_validates_against_diagnostics_contract_schema`
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
  - `provider_lifecycle_never_observed_stale_recovery_never_fabricates`
  - `service_lifecycle_tracks_controller_health_transitions`
  - `external_controller_with_stale_observation_is_not_healthy`
  - `in_process_service_readiness_is_configuration_not_observation`
  - `summary_and_capacity_reflect_durable_draining`
  - `capacity_over_allocated_is_degraded_not_healthy`
- `crates/o3k-store` (bounded placement reads exercised by the adapter tests against the real SQLite adapter): `list_providers_bounded`, `capacity_summary` fail-closed at 64 classes, and the direct store test `sqlite_list_provider_states_pagination_and_narrow_read` for `list_provider_states`.
- `bins/o3kd/tests/native_diagnostics_process.rs` (real-adapter process tests): cover the adapter leak boundary that the DTO structural check cannot.

## CI / evidence not yet run

- `[pending]` `cargo fmt --all -- --check`
- `[pending]` `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `[pending]` `cargo test --workspace --all-features`
- `[pending]` confirm head SHA and record per-file coverage
- `[pending]` full-profile/process-level verification (protected workflow) — not part of the portable gate
