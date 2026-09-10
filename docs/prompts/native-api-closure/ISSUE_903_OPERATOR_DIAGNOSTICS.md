# Implementation Prompt — Issue #903

## Mission

Implement **#903 — Expose system-scoped service, provider health and capacity
diagnostics** as a production-safe, read-only Operator API, not a shell/proxy
surface.

## Baseline

This branch is refreshed directly onto current authoritative `main` and carries
only this planning prompt. Do not inherit planning state — including the old
`diagnostics`/`capacity` prototype, or the audit/governance/quota/metering
bundles — from any earlier draft branch. The prior
`planning/operator-diagnostics-capacity` draft and the closed #913 draft both
carried prototypes written against pre-#887/#898/#900/#901 architecture
(`lifecycle_registry`/`capacity_reader` state on `NativeApiState`, and
`ManifestRegistry::all_bounded`, `LocationRegistry::regions_bounded`,
`PlacementLedger::capacity_summary`). None of those shapes exist on current
`main`. Start from current `main` and the issue text.

## Merged dependencies (authoritative — build on these, do not recreate them)

Treat the following as **MERGED and authoritative** on `main`. They are the
canonical contracts #903 must consume:

- **#887 canonical regions / availability domains** — location identity and
  discovery are already canonical:
  `docs/specs/SPEC-0038-canonical-location-discovery-v1.md`,
  `contracts/native-location-discovery-v1.schema.json`, `GET /regions`,
  `crates/o3k-kernel/src/location.rs` (`LocationRegistry`). Reuse the canonical
  region/AZ IDs; do not introduce a second region model.
- **#898 canonical bounded Operations collection** — `GET /operations`,
  `GET /operations/{id}` under `operation:ReadOperation` with bounded,
  query-bound cursor pagination (`crates/o3k-native-api/src/operation.rs`).
- **#902 durable Audit** — `docs/specs/SPEC-0042-durable-audit-v1.md`,
  `crates/o3k-kernel/src/durable_audit.rs`, `GET /audit`, `GET /audit/{id}`.
  Any diagnostics read or action that emits control-plane evidence must use this
  durable path; do not add a second audit sink.
- **#900 operator authorization and native quota conventions** —
  `docs/specs/SPEC-0043-native-quota-v1.md`,
  `bins/o3kd/src/native_adapters/quota.rs`. System/operator routes live under an
  explicit `/operator/...` path; every operator action maps to a named canonical
  action; effective tenant scope is always taken from `AuthContext`.
- **#901 canonical IAM governance and operator authority** —
  `docs/specs/SPEC-0044-native-iam-governance-v1.md`,
  `GET /operator/governance/...`. Durable operator/system authority is the
  authority for "who may read diagnostics"; do not invent a parallel operator
  model or infer operator status from role names.

Build on these contracts. Do not recreate, fork, or shadow them.

## Authority audit first

Refresh from current authoritative `main`, read #903, and inspect the canonical
authorities the diagnostics view must project:

- service lifecycle/health and controller registration in
  `crates/o3k-kernel/src/manifest.rs` (`ManifestRegistry`, `ServiceManifest`,
  `ServiceHealth`, `ControllerRegistration`);
- canonical placement/capacity inventory in `crates/o3k-placement/src/lib.rs`
  (`PlacementLedger`, `ResourceProvider`, `ProviderState`, `Inventory`) and the
  SQLite/PostgreSQL placement stores;
- canonical locations from #887 (`crates/o3k-kernel/src/location.rs`);
- the merged operator authorization pattern (#900 quota, #901 governance);
- merged Operations (#898) and durable Audit (#902) for correlation;
- existing `/readyz` / doctor checks and every field that carries credentials,
  tokens, private backend topology, provider identity, or node identity.

## Required implementation

Define a versioned, system-authorized, **read-only** diagnostic model that
truthfully exposes only authoritative, currently available data for:

- platform/location status;
- region/AZ health identity (reusing merged #887 IDs);
- installed service lifecycle/readiness/health;
- controller/provider health, capabilities, and stale/last-observed state;
- bounded capacity dimensions/units and saturation where current O3K has
  canonical placement authority;
- bounded reason/status categories and timestamps;
- Operation/Audit correlation through the merged #898/#902 contracts where
  meaningful.

Do not fabricate capacity or health. If an authority cannot report a dimension,
expose explicit unavailable/unknown semantics rather than a default or a zero.
Diagnostics must never turn a provider-private error string, backend address, or
node/provider identity into a public field.

## Operator authorization pattern (follow merged #900/#901)

Follow the merged native quota and governance surfaces. System-only operator
routes live under an explicit `/operator/...` path; every action maps to a named
canonical action (for example a single `operator:ReadDiagnostics`-style action);
effective tenant scope is always taken from `AuthContext`. Prove ordinary
tenant and project-admin denial and explicit durable system/operator
authorization in the same environment. Authorization must resolve from durable
operator/system authority — a display role name must not be sufficient.

## Security boundary

Never expose SSH/kubectl/direct SQL/libvirt/QMP/Ceph shell, arbitrary RPC
passthrough, provider credentials/tokens, secret config, raw environment,
connection strings, provider IDs, node IDs, or secret-bearing logs.

If remediation actions are needed, they belong to separately declared
ActionIds/schemas/idempotent Operations with durable audit — not to this
read-only diagnostics contract.

## Freshness / scale

Define stale thresholds and unknown semantics so a dead provider cannot remain
healthy forever. Collection and capacity aggregation must be bounded; do not
enumerate all tenant resources or materialize all provider inventories on every
dashboard poll. Add strict server-side limits and indexed or cached canonical
aggregates with documented freshness. The response must remain bounded when many
providers, services, or availability domains exist.

## Required evidence

Prove real process cases:

1. healthy service/provider;
2. stale/dead provider becomes degraded/unavailable;
3. service readiness failure is visible;
4. merged #887 location mapping is preserved;
5. authoritative capacity changes after allocation/release where supported;
6. restart/recovery does not fabricate health;
7. tenant -> operator negative in the same environment (tenant and project-admin
   denied; durable system/operator allowed);
8. Operation/Audit correlation works through the merged #898/#902 contracts;
9. no secret/private-config/connection-string/provider-or-node-identity
   disclosure;
10. bounded behavior with many providers/services/AZs.

## Contracts / validation

Add/adjust ADR/SPEC/public schemas and canonical action permissions with least
privilege. Run full Rust gates, provider/controller/placement tests, real process
failure injection, system/tenant security negatives, PostgreSQL tests where
durable diagnostic state or aggregates are involved, and production `o3kd`
evidence.

## Forbidden shortcuts

Do not recreate region, Operation, Audit, quota, or IAM contracts. Do not add a
second health/placement authority. Do not proxy a provider or controller error
channel to the public boundary. Do not infer operator authority from role names.
Do not inherit or transplant the stale `all_bounded` / `regions_bounded` /
`capacity_summary` prototype shapes.

## Stop condition

Only report `BLOCKED` for a genuine external dependency outside `o3kio/o3k`.
Missing internal projections, ports, or aggregates needed for #903 are part of
this work.

Finish only with `#903 COMPLETE` when all issue exit criteria and production
process evidence are proven.
