# SPEC-0045 — Native Operator diagnostics v1
Status: Accepted
Related issue: [#903](https://github.com/o3kio/o3k/issues/903)
Related decision: [ADR-0181](../adr/ADR-0181-canonical-location-identity.md) (canonical location identity; #887) and the #887 location registry
Related contract: [native operator diagnostics v1](../../contracts/native-diagnostics-v1.schema.json)
Related normative sources:
- [SPEC-0038](SPEC-0038-canonical-location-discovery-v1.md) (canonical location topology)
- [SPEC-0042](SPEC-0042-durable-audit-v1.md) (durable audit — the evidence channel for correlation)
- [SPEC-0043](SPEC-0043-native-quota-v1.md) (native projection conventions)
- [SPEC-0044](SPEC-0044-native-iam-governance-v1.md) (system/operator authority)

This specification defines the v1 native operator diagnostics projection served by `o3kd` and the evidence required before it may be advertised.

## 1. Purpose

O3K needs a bounded, read-only, system/operator-authorized projection of canonical service, provider, location and placement-capacity authority so an operator can see platform health, provider state and fleet capacity without a shell gateway, provider-administration surface, or arbitrary RPC/log passthrough. Health and capacity truth originate from O3K authority, never from the projection inventing it; stale or never-observed components are always distinct from healthy.

## 2. Status and reason vocabulary

Every projected component carries a small, stable status vocabulary, distinct by design: `healthy`, `degraded`, `unavailable`, `stale`, and `unknown`. `unknown` (never observed in this process) and `stale` (last observation older than its source freshness threshold) must never collapse into `healthy`. Wire values are snake_case and stable.

Each status may carry one bounded, secret-safe reason from a fixed category set: `not_configured` (the authority is not configured, so no observation is possible), `never_observed`, `observation_stale`, `heartbeat_lost` (an external component stopped heartbeating), `administratively_disabled`, `draining`, `readiness_failed`, `protocol_incompatible`, `dependency_unavailable`, `reported_unhealthy`, `capacity_source_unavailable` (the capacity authority could not be read), and `unsupported` (a dimension/class that is not authoritative and is not claimed). Provider exception text, connection strings, and private topology are never forwarded into these values.

## 3. Endpoints

Responses are versioned `v1` and mounted under the native prefix in production `o3k_api::router_with_state`, so actual `o3kd` exposes them:

- `GET /o3k/v1/operator/diagnostics` — the summary (aggregate status, component counts, control-plane liveness, location topology).
- `GET /o3k/v1/operator/diagnostics/services` — bounded page of services.
- `GET /o3k/v1/operator/diagnostics/providers` — bounded page of providers.
- `GET /o3k/v1/operator/diagnostics/capacity` — the fleet capacity aggregate.

Bounded pages accept `limit` (default 50; an explicit `limit=0` is treated as the default page size 50; a request above the maximum of 200 is a `BadRequest` client error) and an opaque continuation `cursor` (base64 URL-safe no-pad encoding of the after-id key; an invalid cursor is an `InvalidCursor` client error that affects only which page the caller sees, never an escalation). Pages are ordered by stable id, and `has_more`/`next_cursor` signal continuation. The contract never materializes an unbounded collection.

## 4. Authorization

All four endpoints require a single canonical action, `operator:ReadDiagnostics`, against the `operator/diagnostics` resource type with System scope. The Cloud Kernel `StaticAuthorizer` registers the policy with accepted principal `User`, `require_ownership` false, and required role `operator`, and the authorize gate additionally requires effective System scope; a tenant or project-scoped caller carrying an `operator` role name, route shape, or IdP claim never satisfies it (denied with `ScopeMismatch`). A request without the reader configured is `NotAvailable`; an unauthorized caller is `Forbidden`.

## 5. Source authority map

Each source class projects canonical authority only:

- **Services and controllers** come from the shared in-process lifecycle `ManifestRegistry` (configuration readiness). The composition controller probe re-observes every registered controller; in-process services re-observe configuration without I/O, while external controllers are probed with a 5-second timeout that synthesizes an unhealthy observation on timeout, so a dead external controller is never reported healthy indefinitely.
- **Providers** come from `AgentNodeRegistry` availability plus `AgentNodeRegistry::observed_at_unix_ms` plus durable placement state.
- **Capacity** comes from the bounded placement aggregate (`capacity_summary`, which fails closed at the class bound) plus the agent observation clock.
- **Control-plane liveness** comes from the durable coordination lease authority (`controller_sessions` active leases), the only durable, timestamped platform liveness signal.
- **Locations** come from the canonical #887 `LocationRegistry`, topology only.

### Provider status precedence

From most to least severe, and never reporting `healthy` from durable placement state alone:

1. no agent snapshot → `unknown` / `never_observed` (a restart before re-observation is never healthy from durable state);
2. agent administratively `Disabled` → `unavailable` / `administratively_disabled`;
3. durable state `Deleted` → `unavailable` / `administratively_disabled`;
4. durable `Draining` or agent `Draining` → `degraded` / `draining`;
5. agent `Unavailable` → `stale` / `heartbeat_lost` when the last heartbeat is older than the lease, else `unavailable` / `heartbeat_lost`;
6. last heartbeat older than the lease → `stale` / `observation_stale`;
7. durable state `Unavailable` (scheduler out-of-service) while the agent reports healthy → `unavailable` / `administratively_disabled` (the durable authority dominates a healthy-looking agent);
8. otherwise → `healthy`.

### Service status mapping

From the canonical controller lifecycle state: `Ready` → `healthy`; `NotReady` → `unavailable` / `readiness_failed`; `Incompatible` → `degraded` / `protocol_incompatible`; `Disabled` → `unavailable` / `administratively_disabled`; `Declared` → `unknown` / `never_observed`.

### Aggregate summary status

Component-class counts aggregate by documented rules: nothing observed → `unknown`; all healthy → `healthy`; all unavailable → `unavailable`; any unavailable, stale, or degraded → `degraded`; otherwise `unknown`. The summary status is the worst of the services and providers aggregates, so one unavailable or stale source degrades the platform without masking the others.

## 6. Freshness semantics

The agent lease is 15 seconds (`AGENT_LEASE_MS = 15_000` in the production adapter, mirroring the compute-agent `DEFAULT_LEASE` of 15 seconds). A service's `observed_at_unix_ms` is the probe/stored observation or the process start time; the composition probe re-records observations every 15 seconds. External controllers are subject to a service staleness gate: a controller with a transport `session` whose last confirmed observation is older than 75 seconds (`SERVICE_OBSERVATION_THRESHOLD_MS = 75_000`, five 15-second probe intervals) is projected `stale` / `observation_stale` even if its last lifecycle state was `Ready` — a dead external controller is never reported healthy indefinitely. In-process services (no `session`) are configuration-authoritative: their readiness is a fact of the running composition, not an observation, so they never go stale from age. Providers use the 15-second agent lease. After a restart, durable placement state alone never reports a provider or capacity source healthy — a component stays `unknown`/`stale` until a fresh re-observation. Capacity status always reflects durable placement state plus agent liveness, so a durably draining or deleted provider is never counted healthy here. Capacity status is `unknown` when no providers exist, `stale` when durable last-known values exist without any fresh agent observation (covers restart and a fleet that stopped reporting), `degraded` on partial provider failure even when one provider is fresh, and `degraded` when a dimension is over-allocated (a drifted/corrupt durable invariant); otherwise `healthy`.

## 7. Capacity

Capacity reports only placement-authoritative dimensions: `VCPU`, `MEMORY_MB`, and `DISK_GB`. Per provider, `total`, `reserved`, `allocated`, and `available` are exposed; in the aggregate, `allocatable`, `reserved`, `allocated`, and `available`. `available = allocatable − reserved − allocated` uses saturating arithmetic so it is never negative and never wraps; a drifted or corrupt remainder surfaces as zero. An over-allocated dimension (`allocated > allocatable − reserved`, i.e. a drifted/corrupt durable invariant) makes the whole capacity status `degraded` rather than `healthy`, detected both in the aggregate and per-provider (a per-provider over-allocation is reported even when masked by another provider's slack via `providers_over_allocated`), while `available` stays saturating (never negative). Unit labels are `count` (VCPU), `mib` (MEMORY_MB), and `gib` (DISK_GB). Storage, network, Ceph, and quota are deliberately not placement-authoritative and are not exposed; an unsupported dimension/class is not claimed and not present.

## 8. Location aggregation

Locations reuse the canonical #887 IDs from the single #887 `LocationRegistry`; no second location registry exists and no provider→availability-domain mapping exists or is fabricated. Locations carry identity and counts only (`configured`, `regions`, `availability_domains`); there is no region/AZ liveness authority, so no health status is fabricated for locations.

## 9. Correlation (non-goal of v1)

The v1 projection does not emit Operation or Audit IDs. Canonical Operation/Audit correlation is explicitly out of scope for this read-only projection; the durable audit path (SPEC-0042) remains the evidence channel for correlating individual mutations. This is a deliberate non-goal, not an omission.

## 10. Partial degradation

The projection is component-wise: one provider down yields a provider `unavailable` and a degraded platform while the remaining providers stay `healthy`, and one unknown or stale capacity source does not corrupt the status of another. Status is derived independently per provider, service, and capacity class.

## 11. Errors

Errors use ProblemDetails. An unhealthy provider or stale component is data (HTTP 200), never an HTTP 500. `NotAvailable` is returned when the diagnostic authority is unconfigured or currently unavailable; a corrupt durable diagnostic/capacity state is `InternalError` (HTTP 500). `BadRequest`/`InvalidCursor` cover bounded-page query errors and `Forbidden` covers authorization failure.

## 12. Secret safety

The projection never exposes the agent `node_id` field, agent epoch, session ids, manifest digests, service principal, `health.detail`, connection strings, credentials, or host paths. The providers endpoint exposes only the durable placement `provider_id`, which by placement design equals the agent/node id; no additional node identity is exposed. Only bounded canonical identity and counts cross the wire.

## 13. Boundedness

Service collections are registry-backed (process-internal) and bound fail-closed at `MAX_SERVICES = 256`: a registry exceeding the bound yields an error, never a silently truncated projection. Provider aggregates (summary and capacity) read the durable provider set through the bounded `list_provider_states` read bound fail-closed at `MAX_PROVIDERS = 65_536`; the providers page uses the bounded `list_providers_bounded` placement read. The capacity aggregate uses the bounded `capacity_summary`, which fails closed at 64 resource classes; the page bound is 200 and the capacity class bound is 64. There is no `all()` → `filter` → `truncate` on production collections and no per-dashboard full infrastructure scan.

## 14. Non-goals

Operation/Audit correlation in the projection, tenant-facing health, shell/credential/private-config surface, provider administration, per-dashboard full-infrastructure scanning, and fabricated region/AZ/provider-topology liveness are out of scope. Any future region/AZ/provider mapping must be derived explicitly from canonical O3K identity.
