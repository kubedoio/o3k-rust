# P15.0 — Post-Araf current-state audit

Repository-backed statement of what exists at the P15 re-baseline. This report
is the ground truth input for
[ADR-0184](../adr/ADR-0184-p15-scale-and-composition-foundation.md) and
[SPEC-0047](../specs/SPEC-0047-p15-scale-and-composition-foundation.md); it
makes no claim that is not backed by a cited in-tree artifact.

| Item | Value |
| --- | --- |
| Baseline | `21fe687c387a04f107b6e87fac04060b1c28e449` (protected `origin/main`, merge of PR #928, Araf P2 northbound convergence for #907) |
| Verified | 2026-09-12; no newer protected commits exist |
| Branch | `p15-0-scale-composition-rebaseline` |
| Issue | #930 (P15.0), under umbrella #929 (P15) |
| Method | Prior full audit plus re-verification of a sample of cited paths, line numbers, and route registrations against the baseline SHA |

## What #928 proves

PR #928 is the Araf P2 northbound convergence gate (#907). Its in-tree gate is
`bins/o3kd/tests/araf_p2_convergence.rs` (test
`araf_p2_northbound_convergence`, `#[ignore]`), orchestrated by
`tests/araf-p2-convergence.sh` (disposable PostgreSQL 16.4 plus pinned Keycloak
25.0.6, a SQLite parity pass, and one deliberate mid-journey SIGTERM +
relaunch), and wired as CI job `araf-p2-convergence`
(`.github/workflows/ci.yml:298`). The evidence ledger is
`docs/evidence/ISSUE_907_ARAF_P2_NORTHBOUND_CONVERGENCE.md` (16-item matrix,
all PASS; still DRAFT with `[pending]` placeholders until the converged head is
frozen). Concretely, #928 proves:

- Real process boundary: the `o3kd` binary runs as a subprocess behind the
  production composition router; no in-process handler construction and no
  injected `AuthContext`.
- Real OIDC: pinned Keycloak 25.0.6 issues RS256 tokens; the production
  `OidcValidator` and `POST /o3k/v1/identity/tokens` federated exchange derive
  every `AuthContext`; unbound subjects and foreign-project scopes are denied.
- Durable state on both supported stores: disposable PostgreSQL 16.4 for the
  primary pass, SQLite for the parity pass, with restart/recovery across the
  SIGTERM + relaunch on the same durable store.
- Cross-tenant concealment and a secret-leak scan over the exercised paths.
- Northbound convergence across native discovery, operations, quota,
  governance, audit, diagnostics, metering, relationships-read, and the
  OpenStack compatibility surface.
- Thirteen real product defects found and fixed by the gate work, including the
  missing audit show route (fix `c70494b1`).

## What #928 does not prove

- No real execution: the gate runs `O3K_PROVIDER=fake` (env var parsed at
  `crates/o3k-config/src/lib.rs:300`; fake provider struct at
  `crates/o3k-provider/src/lib.rs:406`). No real hypervisor, network, or
  storage is exercised.
- The scheduler and agent inventory publisher are not wired behind the
  agent-control mTLS gate (`bins/o3kd/src/composition/mod.rs:481-485`), so the
  gate does not prove agent-backed scheduling.
- Diagnostics honestly pin `unknown`/`never_observed` capacity; the gate does
  not prove observed capacity reporting.
- The relationship write side remains the external-controller composition
  boundary (mTLS + delegation keys), not a native write path.
- No controlled degradation is exercised.
- The gate must not be cited as progress on E2D-02, E2D-08, E2D-09, E2D-10, or
  E2D-17 (see `docs/architecture/p15-e2d-gap-register.md`).

## Cloud Kernel capability inventory

Status values: **Implemented + evidenced** means code, contract, tests, and a
traceability record all exist at the baseline. PR numbers are the delivery
merges; issue numbers follow the issue-to-delivery mapping recorded in the
P15.0 audit.

### #887 — Native location discovery (PR #889)

Status: Implemented + evidenced.

- ADR-0181 / SPEC-0038; contract
  `contracts/native-location-discovery-v1.schema.json`.
- `LocationRegistry`, `RegionDeclaration`, `AvailabilityDomain` in
  `crates/o3k-kernel/src/location.rs`; seeded from `O3K_LOCATIONS`
  (`bins/o3kd/src/composition/mod.rs:77-97,610-620`); manifests reference
  canonical IDs and fail closed.
- `GET /o3k/v1/regions` (`crates/o3k-api/src/lib.rs:522`).

Evidence: unit tests `crates/o3k-kernel/src/location.rs:297-657`;
`crates/o3k-api/tests/native_location_routes.rs`; composition tests
`bins/o3kd/src/composition/mod.rs:1280-1319`.

Honest limitations: an availability domain is only an identifier — no hierarchy
below AZ and no links to hosts, providers, fabrics, or storage. `seed_core`
still publishes `regions: vec![]`
(`crates/o3k-kernel/src/manifest.rs:1612`). The Keystone catalog hard-codes
`RegionOne` from the static registry (projection call at
`crates/o3k-identity/src/lib.rs:1538-1553`; `RegionOne` literals at
`crates/o3k-identity/src/lib.rs:715` and
`crates/o3k-kernel/src/registry.rs:288-293`); there is no `OS-EXT-AZ`
projection in `crates/o3k-api`. Tracked as E2D-01 (PARTIAL).

### #888 — Native resource and action schemas (PR #890)

Status: Implemented + evidenced.

- SPEC-0040; contracts `native-create-request-v1`, `native-action-input-v1`,
  `native-resource-envelope-v1`, `native-resource-list-response-v1`,
  `native-mutation-result-v1` under `contracts/`.
- `crates/o3k-native-api/src/resource_contract.rs`.
- Routes `/o3k/v1/services`, `/o3k/v1/resource-types`,
  `/o3k/v1/resource-schemas/{ns}/{c}/{version}`
  (`crates/o3k-api/src/lib.rs:513-521`).

Evidence: `discovery_advertises_only_reachable_lifecycle_operations`
(`crates/o3k-native-api/src/lib.rs`) plus native discovery route tests.

Honest limitations: the schema catalog covers the registered native resource
types and their advertised lifecycle operations; it is not a general-purpose
schema facility.

### #906 — A0 bounded query foundation (PR #920)

Status: Implemented + evidenced.

- `crates/o3k-native-api/src/pagination.rs`: HMAC-signed opaque cursor, keyset
  `LIMIT N+1`, `id.asc` ordering only, `MAX_PAGE_SIZE` 200, over-limit requests
  rejected with 400.
- Repository bounds in `crates/o3k-store/src/port/durable.rs` with
  sqlite/postgres/unified parity; filters are deliberately unsupported in v1.

Evidence: verifier `scripts/verify-a0-query-foundation.sh`; store conformance
suite `crates/o3k-store/src/conformance.rs`.

Honest limitations: single sort key (`id.asc`), 200-row page ceiling, and no
server-side filtering by contract.

### #898 + #899 — A1 operations read and relationships read (PR #921)

Status: Implemented + evidenced.

- #898, SPEC-0008: `crates/o3k-native-api/src/operation.rs`,
  `bins/o3kd/src/native_adapters/operation.rs`,
  `crates/o3k-kernel/src/operation.rs`; `GET /o3k/v1/operations[/{id}]`
  (`crates/o3k-api/src/lib.rs:565-571`); bounded, cursor-paginated,
  owner-scoped; filterless by contract.
- #899: `GET /o3k/v1/{ns}/{c}/{id}/relationships`
  (`crates/o3k-api/src/lib.rs:667-670`); `crates/o3k-native-api/src/resource.rs`;
  stores `crates/o3k-store/src/{sqlite,postgres,unified}/relationship.rs`.

Evidence: process test `bins/o3kd/tests/p12_6_process.rs`; #907/#928 gate items
cover both routes.

Honest limitations: relationships are a read projection. Write authority is the
external-controller composition path (`CompositionResourceHandler`, mTLS plus
delegation keys); the honest limitation is documented in
`docs/evidence/ISSUE_907_ARAF_P2_NORTHBOUND_CONVERGENCE.md:67-87`.

### #897 + #905 — A2 native actions and PUT update (PR #925)

Status: Implemented + evidenced.

- #897: `POST /o3k/v1/{ns}/{c}/{id}/actions/{action_name}`
  (`crates/o3k-api/src/lib.rs:671-674`); action implementations in
  `crates/o3k-compute/src/actions.rs`.
- #905: `PUT /o3k/v1/{ns}/{c}/{id}` and `PUT /compute/servers/{id}` with
  `If-Match` generation optimistic concurrency; `compute:UpdateServer`
  registered in `contracts/cloud-kernel-actions.yaml`.

Evidence: the pre-gate implementation was broken three ways and fixed (gate
defects 3/6/11); regressions in
`crates/o3k-api/tests/native_compute_update_route.rs` and
`bins/o3kd/src/native_adapters/tests.rs`; covered by the #928 gate.

Honest limitations: update applies only to resources exposing a native update
action; `compute:UpdateServer` is the registered instance at the baseline.

### #902 — Durable audit (PR #926)

Status: Implemented + evidenced.

- SPEC-0042; `crates/o3k-kernel/src/audit.rs` and `durable_audit.rs`;
  `crates/o3k-native-api/src/audit.rs`; store implementations
  `crates/o3k-store/src/sqlite/audit_store.rs`,
  `crates/o3k-store/src/postgres/audit_store.rs`, and
  `crates/o3k-store/src/unified/audit.rs`, with migrations `0041`/`0042`
  (SQLite) and `0024`/`0025` (PostgreSQL).
- Routes `GET /o3k/v1/audit` and `GET /o3k/v1/audit/{id}`
  (`crates/o3k-api/src/lib.rs:572-573`); the show route was added by gate
  defect fix `c70494b1`.
- Audit publication is production-mandatory via `DurableAuditSink`
  (`bins/o3kd/src/composition/mod.rs:309-310`).

Evidence: regression `production_router_exposes_audit_show_route`; #928 gate
audit items.

Honest limitations: the route surface is read-only; audit writes are
service-internal through the required audit publisher.

### #900 — Native quota (PR #927)

Status: Implemented + evidenced.

- SPEC-0043; contract `contracts/native-quota-v1.schema.json`; modules
  `crates/o3k-native-api/src/quota.rs`, `crates/o3k-kernel/src/quota.rs`,
  `crates/o3k-store` quota repositories (sqlite/postgres/unified), and the
  `bins/o3kd` adapter.
- Routes `/o3k/v1/quota[/{ns}/{dim}]` and `/o3k/v1/operator/quotas`
  (`crates/o3k-api/src/lib.rs:574-587`); CAS generations in migrations `0043`
  (SQLite) and `0026` (PostgreSQL).

Evidence: `crates/o3k-store/src/postgres/quota.rs`,
`bins/o3kd/tests/p12_7_convergence.rs`, and the regression
`native_compute_quota_exceeded_is_forbidden_without_provider_side_effect`.

Honest limitations: the enforced guarantee is rejection before provider
side-effect; quota dimensions are those registered by the in-tree services.

### #901 — Native IAM governance (PR #912)

Status: Implemented + evidenced.

- SPEC-0044; contract `contracts/native-governance-v1.schema.json`;
  `crates/o3k-native-api/src/governance.rs`.
- Routes under `/o3k/v1/operator/governance/` for `projects`, `principals`,
  `roles`, `capabilities`, `assignments`, `operator-assignments`
  (`crates/o3k-api/src/lib.rs:588-630`).

Evidence: #928 gate governance items; native governance route tests.

Honest limitations: an operator-scoped surface; it does not replace the
OpenStack-compatible policy projection for compatibility clients.

### #903 — Operator diagnostics (PR #914)

Status: Implemented + evidenced.

- SPEC-0045; contract `contracts/native-diagnostics-v1.schema.json`; adapter
  `bins/o3kd/src/native_adapters/diagnostics.rs`.
- Routes `/o3k/v1/operator/diagnostics{,/services,/providers,/capacity}`
  (`crates/o3k-api/src/lib.rs:631-647`).
- Honest status vocabulary: `unknown`/`never_observed` never collapse to
  healthy; agent state projection rules in
  `bins/o3kd/src/native_adapters/diagnostics.rs:82-121`.

Evidence: #928 gate diagnostics items; ledger
`docs/evidence/ISSUE_903_OPERATOR_DIAGNOSTICS.md` (still DRAFT).

Honest limitations: capacity that has never been observed is reported as
unknown, not healthy; diagnostics is a status projection, not a metrics or
telemetry system (see E2D-11).

### #904 — Native metering (PR #915)

Status: Implemented + evidenced.

- ADR-0183 / SPEC-0046; contract `contracts/native-metering-v1.schema.json`;
  `crates/o3k-kernel/src/metering.rs` (catalog, intervals, additive hourly
  buckets, CAS close, checked arithmetic) and the `bins/o3kd` adapter.
- Routes `/o3k/v1/metering/definitions` and `/o3k/v1/metering/usage`
  (`crates/o3k-api/src/lib.rs:648-655`).
- Meters: `compute:instance_seconds` and `volume:allocated_byte_seconds` (the
  latter only when a native storage provider exists).

Evidence: `bins/o3kd/tests/native_metering_lifecycle_process.rs` (1193 lines),
`crates/o3k-api/tests/native_metering_routes.rs`,
`crates/o3k-store/src/{sqlite,postgres,unified}/metering.rs`,
`crates/o3k-store/tests/metering_repository.rs`, and postgres metering tests;
ledger `docs/evidence/ISSUE_904_METERING.md` (still DRAFT).

Honest limitations: two meters at the baseline; volume metering is conditional
on native storage being present.

### #905 fixes + #907 — Araf P2 convergence gate (PR #928)

Status: Implemented + evidenced (as a gate).

- In-tree gate `bins/o3kd/tests/araf_p2_convergence.rs`
  (`araf_p2_northbound_convergence`, `#[ignore]`); orchestrator
  `tests/araf-p2-convergence.sh`; CI job at `.github/workflows/ci.yml:298`.
- Ledger `docs/evidence/ISSUE_907_ARAF_P2_NORTHBOUND_CONVERGENCE.md`: 16-item
  matrix, all PASS, DRAFT with `[pending]` placeholders.
- Found and fixed 13 real product defects, including the audit show-route gap.

Evidence: see "What #928 proves" above; the gate is the current top of the
evidence ladder.

Honest limitations: see "What #928 does not prove" above. Execution is fake,
the scheduler/agent-registry wiring sits behind the agent-control mTLS gate,
and the ledger is not yet frozen.

## Execution provider and storage boundary today

- `O3K_PROVIDER` selects the compute execution boundary: `fake`
  (`FakeComputeProvider`, also runs the provider conformance suite), `cellhv`
  (`CellHvProvider`, gRPC + mTLS), `agent` (the production libvirt profile:
  `AgentComputeProvider` plus scheduler and agent registry behind agent-control
  mTLS), and `libvirt` (direct in-daemon libvirt is rejected; the host agent is
  the only libvirt path).
- Storage: optional native LVM host-local volumes (`O3K_LVM_*`) and the
  external Cinder connector (`O3K_CINDER_ENDPOINT`, SPEC-0023).
- Verified execution evidence: LVM host locality and serial Ceph RBD
  cross-host attachment (`tests/lvm-provider-workflow-guards.sh`,
  `tests/ceph-rbd-workflow-guards.sh`); mTLS enrollment and agent epochs
  (`tests/real-compute-agent-mtls.sh`,
  `tests/real-compute-agent-process-mtls.sh`).

## Native API surface today

Per SPEC-0030, the native surface under `/o3k/v1` comprises: discovery
(`/services`, `/resource-types`, `/resource-schemas/...`, `/regions`),
identity (`/identity/tokens`, `/identity/scopes`, `/identity/me`,
`/operator/profile`), compute `/compute/servers`, volume `/volume/volumes`,
network `/network/address-realms`, generic `{ns}/{c}[/{id}]` CRUD plus PUT
update, `/relationships`, `/actions`, `/operations`, `/audit`, `/quota` and
`/operator/quotas`, `/operator/governance/*`, `/operator/diagnostics/*`, and
`/metering/*`. Query behavior: `MAX_PAGE_SIZE` 200, HMAC-signed opaque cursors,
no microversions.

## Araf integration today

Araf is an external repository (default `ARAF_ROOT=/root/araf`) providing
Tenant and Operator BFFs; O3K contains no Araf code. The boundary is: browser
to Araf BFF to OIDC auth-code + PKCE to the O3K federated exchange
(`/identity/scopes`, `/identity/tokens`) to a native scoped token to
`/identity/me` to the native API. Process evidence:
`tests/p12-iam-8-real-araf-process.sh`; convergence evidence: the #928 gate.
After #928, Araf P2.2–P2.8 have no known missing O3K contract.

## Persistence and deployment today

- Stores: SQLite is the supported minimal/TestLab default; a real
  `PostgresStore` adapter exists (`crates/o3k-store/src/postgres/*`,
  migrations under `migrations_postgres/`) with a unified conformance suite
  (`crates/o3k-store/src/conformance.rs`) and `postgres_*` parity tests.
- Multi-controller correctness: durable work leases and controller fencing are
  proven (`crates/o3k-compute/tests/multi_controller_acceptance.rs`; evidence in
  `docs/reports/P7_MULTI_CONTROLLER_ACCEPTANCE_EVIDENCE.md`; controller
  sessions and heartbeat at `bins/o3kd/src/composition/mod.rs:255-302`).
- Kubernetes: `deployments/helm/o3k` chart v0.1.0; PostgreSQL is strictly
  required (SQLite rejected); `replicaCount > 1` requires
  `multiController.enabled=true`; privileged hypervisor execution stays outside
  Kubernetes. No HA claim is made beyond the ADR-0167 preconditions.
- Installer: one-line TestLab installer `packaging/get-o3k.sh` (pinned
  `v0.2.0-alpha.2`, documented in `docs/INSTALLER.md`), bounded to the libvirt
  TestLab. Per `README.md:237-238` it must not be presented as the production
  flow; `bins/o3k` has no `init`/`join` commands (`bins/o3k/src/main.rs:17-70`).

## Explicitly not claimed

The following are not claimed at this baseline, regardless of any adjacent
working machinery:

- datacenter scale, or thousands of hosts;
- multi-region deployments;
- production HA (Kubernetes or otherwise, beyond ADR-0167 preconditions);
- production live migration (`README.md:217-218`);
- production workload evacuation or storage migration;
- zero-downtime block maintenance;
- arbitrary OpenStack compatibility or all OpenStack services (Octavia,
  Designate, Barbican are external-hosted entries only, `README.md:373-375`);
- production bootstrap in seconds (the TestLab installer is not that flow);
- cells, sharding, or hierarchical scheduling (`o3k-cellhv` is a gRPC
  hypervisor provider client, not a scheduler cell).

Maximum real multi-host evidence remains the P11 profile: three nested-KVM
hosts plus 15 simulated scale hosts (`README.md:262`). Existing measurements
are workflow-latency measurements, not scale evidence.
