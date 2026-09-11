# Issue #907 — Araf P2 northbound convergence evidence ledger (DRAFT)

Status: draft; placeholders marked `[pending]` are filled only after the
converged head is frozen, so that no post-approval commit invalidates the
exact-head CI and human-approval gate.

## Scope

| Item | Value |
| --- | --- |
| Issue | #907 — [Program] Production northbound contract closure for Araf P2 |
| Nature | Evidence/test infrastructure, not a feature workstream |
| Base main SHA | `ad6d4b096ae8b917e16e3a4fc98037f3b10385e9` (post-#904/#914/#915) |
| Branch | `feat/araf-p2-convergence-907` |
| In-tree gate | `bins/o3kd/tests/araf_p2_convergence.rs` (`araf_p2_northbound_convergence`, `#[ignore]`) |
| Orchestrator | `tests/araf-p2-convergence.sh` (disposable PostgreSQL 16.4 + pinned Keycloak 25.0.6, PostgreSQL and SQLite passes) |
| IdP fixture | reused from `tests/p12-iam-7-real-idp.sh` via `O3K_P12_7_AFTER_HOOK` (one identical pinned Keycloak realm/users/tokens) |
| CI wiring | `.github/workflows/ci.yml` job `araf-p2-convergence` |
| Candidate head SHA | `[pending]` |
| GitHub CI | `[pending]` |

## Gate architecture (what "real" means here)

- Real `o3kd` binary spawned as a subprocess via `env!("CARGO_BIN_EXE_o3kd")`
  on an ephemeral 127.0.0.1 port (same pattern as `bins/o3kd/tests/shutdown.rs`);
  the production composition router `o3k_api::router_with_state` serves all
  traffic. No in-process router construction, no handler calls, no injected
  AuthContext.
- Real RS256 OIDC: pinned Keycloak 25.0.6 issues tokens; the production
  `OidcValidator` and `POST /o3k/v1/identity/tokens` federated exchange derive
  every AuthContext. Unbound subjects and foreign-project scopes are denied.
- Real durable store per pass: disposable PostgreSQL 16.4 (`O3K_DATABASE_URL`)
  for the primary production profile; `<O3K_DATA_DIR>/o3k.sqlite` for the
  SQLite parity pass.
- Real execution boundary: `O3K_PROVIDER=fake` compute provider; the gate
  never bypasses adapter/router/auth layers.
- Deterministic convergence: all waits are bounded polls on observable state;
  there are no fixed long sleeps.
- One deliberate mid-journey SIGTERM + relaunch against the same durable store
  (items 9/11/14) with clean-exit assertion.

## 16-item matrix

Every row names the exact northbound endpoint(s), the gate test that proves
it, the durable authority behind it, and the profile coverage
(PG = PostgreSQL pass, SQ = SQLite pass, R = across the mid-journey restart).

| # | Item | Endpoint(s) | Gate coverage (in `araf_p2_northbound_convergence`) | Durable authority | PG | SQ | R | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Federated login | `POST /o3k/v1/identity/scopes`, `POST /o3k/v1/identity/tokens`, `GET /o3k/v1/identity/me` | Alice/Bob project-scoped exchange (201), operator system exchange via canonical `operator-assignments` path, unbound/foreign/system-for-tenant denials, `identity/me` effective scope | `federated_bindings`, `operator_assignments`, Keystone-compatible users/projects seeded by `seed_identity_defaults` | ✓ | ✓ | ✓ | PASS |
| 2 | Discovery | `GET /o3k/v1/services`, `/resource-types`, `/resource-schemas/{ns}/{c}/{v}`, `/regions` | All served; compute/network/identity advertised; `compute:server` advertises create/show/list/update/delete + start/stop/reboot; 1 region / 2 AZ from `O3K_LOCATIONS`; every advertised capability is executed later in the journey | `ManifestRegistry::seed_core` (composition root) | ✓ | ✓ | ✓ | PASS |
| 3 | Generic lifecycle | `POST/GET/PUT/DELETE /o3k/v1/compute/servers[/{id}]`, `POST /o3k/v1/network/networks` | Create with idempotency key + exact replay (same key → same resource id, no duplicate); bounded list + cursor continuation; show with owner scope; PUT update with `If-Match: generation-N` (missing → 400, stale → 409, live → 200 — after waiting for action convergence so the generation is stable); delete → 204 | `resources` ledger, canonical operations + idempotency reservations | ✓ | ✓ | ✓ | PASS |
| 4 | Domain actions | `POST /o3k/v1/compute/servers/{id}/actions/{stop,start,reboot}` | Stop/start/reboot through the discovered action schema; canonical Operation IDs returned; replay-safe keys; states converge (ACTIVE→STOPPED→ACTIVE) | canonical operations + reconciler projection | ✓ | ✓ | ✓ | PASS |
| 5 | Operations | `GET /o3k/v1/operations[/{id}]` | Every mutation's operation shown (`id` agrees with collection); bounded `?limit=1` collection + signed cursor continuation | `operations` + `canonical_operation_metadata` | ✓ | ✓ | ✓ | PASS |
| 6 | Relationships | `GET /o3k/v1/{ns}/{c}/{id}/relationships` | One canonical relationship row is seeded through the documented environment-setup path (the same durable relationship authority the external-controller composition writer uses; see the note below), then the northbound projection is proven through the real production router: Tenant A receives 200 with exactly the seeded row — slot `network-primary`, canonical `network:network` child type, canonical public UUID child id (never provider-private), `bound`/`exclusive` state vocabulary, `has_more == false`; the bounded-page canonical-vocabulary assertions hold over the real row; Tenant B receives 404 with durable rows present (genuine concealment, not an empty page) | `resource_relationships` (seeded via `reserve_relationship`/`bind_relationship`; read projection from #899) | ✓ | ✓ | ✓ | PASS (see note below) |
| 7 | Quota | `PUT /o3k/v1/operator/quotas/{p}/{ns}/{dim}`, `GET /o3k/v1/quota/{ns}/{dim}`, `GET /o3k/v1/operator/quotas/{p}` | Finite limit (usage-relative for the shared PG store) → allocate to the limit → next create 403 with no provider side effect and no reservation leak → delete frees the slot → re-allocation succeeds; stale-generation limit write 409; tenant self-administration 403; foreign quota read 403 | `quota_limits` + committed usage, CAS generation | ✓ | ✓ | ✓ | PASS |
| 8 | Governance | `GET /o3k/v1/operator/governance/{projects,principals,roles,capabilities}`, `POST/DELETE .../assignments[...]` | Operator lists; grant of member role to Tenant B in project-a reflected in a refreshed exchanged token; revocation removes it again; tenant→operator routes 403; self-grant of operator 403 | durable assignments + role inventory | ✓ | ✓ | ✓ | PASS |
| 9 | Audit | `GET /o3k/v1/audit[/{id}]` | Creation/action/quota/governance mutations recorded with canonical actor/scope/target; bounded page; per-event show through the production router; Tenant B sees none of Tenant A's server actions; records byte-preserved across restart (count monotonic, contents unchanged) | `audit_events` | ✓ | ✓ | ✓ | PASS |
| 10 | Diagnostics | `GET /o3k/v1/operator/diagnostics[/{summary,services,providers,capacity}]` | Operator reads summary (version v1, honest status vocabulary), region/AZ topology from `O3K_LOCATIONS`, capacity dimensions restricted to VCPU/MEMORY_MB/DISK_GB; tenant 403. Controlled degradation is not achievable with the fake provider, so the gate asserts the honest status vocabulary (healthy/degraded/unavailable/stale/unknown) instead of fabricating a degradation, per the accepted diagnostics honesty contract | lifecycle registry + placement/location authorities | ✓ | ✓ | ✓ | PASS |
| 11 | Metering | `GET /o3k/v1/metering/definitions`, `GET /o3k/v1/metering/usage` | `compute:instance_seconds` advertised with `instance_second` unit; `volume:allocated_byte_seconds` **honestly not advertised** (no native storage provider in this profile — non-advertisement is the requirement, not a gap). Usage over an hour-aligned window: unit, meter key, `start`/`end`/`observed_through`/`authority_started_at` UTC correctness; run/stop/start/delete journey total frozen by the closes and stable across restart (no duplicate folding, narrowed to the gate's own closed resource so unrelated open intervals cannot flake the comparison); cross-scope tenant read 403; operator `?scope=project-a` 200 | `metering_authority`, `metering_intervals`, `metering_aggregates` | ✓ | ✓ | ✓ | PASS |
| 12 | Cross-tenant isolation | all of the above + probes | Tenant B: show/list/action on Tenant A server → 404/concealed; foreign network 404; foreign operation 404; foreign quota 403; audit isolation; metering foreign scope 403; governance 403; foreign-project token exchange denied; signed-cursor reuse by another tenant → 400 (no existence oracle); tampered cursor → 400 | AuthContext scoping on every route | ✓ | ✓ | ✓ | PASS |
| 13 | Secret leak scan | every retained response body (2xx and 4xx) | Structural DTO assertions along the journey plus an explicit scan of all bodies for: bootstrap password, token signing key, OIDC/Keycloak access tokens, DB URLs, `-----BEGIN` private-key markers, `agent_epoch` internals. Also verified across the restart phase | response DTOs carry no credential fields | ✓ | ✓ | ✓ | PASS |
| 14 | Restart/recovery | whole journey phase 2 | SIGTERM → clean exit → relaunch on same store/port family: resources, operations, quota limit + freed usage, governance revocation persistence, audit preservation, metering totals (frozen) unchanged; diagnostics return honest (non-fabricated) status. The restart phase runs unconditionally in both backend passes (mid-journey terminate + relaunch inside the single gate test on each backend) | all of the above durable authorities | ✓ | ✓ | ✓ | PASS |
| 15 | Bounded queries | collections exercised above | Excessive/invalid handling on every exercised collection: `limit=1` paging + signed cursor continuation; invalid cursor → 400; tampered cursor → 400; cross-tenant cursor reuse → 400; metering requires hour-aligned range (misalignment → 400) with hard meter/range/bucket bounds per SPEC-0046 | `CursorConfig` HMAC + repository bounds | ✓ | ✓ | ✓ | PASS |
| 16 | Compatibility convergence | `POST /v3/auth/tokens`, `GET /v2.1/{project}/servers`, `POST/GET /v2.0/{subnets,networks,ports}` | Keystone password grant mints a project token; the subnet and port are created through the OpenStack-compatible Neutron surface on the canonical network authority while the network itself is created natively; Nova lists the natively-created server under the same canonical ID; Neutron shows the natively-created network — one canonical authority, two protocol surfaces | same `resources` authority behind both surfaces | ✓ | ✓ | ✓ | PASS |

## Relationship item note (seeded durable row; no northbound writer)

The #899 contract merged the bounded relationship READ projection. The durable
write authority is the controller composition path
(`CompositionResourceHandler`, mTLS + delegation keys, consumed by external
controllers; store-level evidence in `bins/o3kd/tests/p12_6_process.rs`). The
in-process TestLab profile has no northbound canonical API that creates
compute→network relationship rows, and the gate does not invent one.

Item 6 is nonetheless proven over real durable state, not an empty page: as
documented environment setup, the gate seeds ONE canonical relationship row
through the same durable relationship authority the composition writer uses
(`reserve_relationship` for parent = the Tenant A server, slot
`network-primary`, child = the Tenant A network, `exclusive` ownership, then
`bind_relationship` to `bound`), and then asserts the northbound projection
surfaces that exact row through the real production router — canonical child
type and public UUID, bounded page, and cross-tenant concealment with rows
present (Tenant B receives 404). What the gate proves is the READ projection
over the durable authority; the WRITE side remains the external-controller
composition boundary, which would be a new cross-service write contract
outside #907's evidence-only scope.

## Product defects found by the gate and fixed (with regression tests)

1. **Production router missing `GET /o3k/v1/audit/{id}`.** The native audit API
   implements per-event show (SPEC-0042 store parity lists `show`; the native
   API test router always registered it) but the production composition router
   mounted only the collection. Fixed in `crates/o3k-api/src/lib.rs`;
   regression: `production_router_exposes_audit_show_route`
   (`bins/o3kd/tests/araf_p2_convergence.rs`), which drives the real
   production router.
2. **Quota denial surfaced as 500 on the generic native surface.**
   `compute_error` in `bins/o3kd/src/native_adapters/resource.rs` fell through
   to `ResourceApplicationError::Internal` for `ComputeError::QuotaExceeded`,
   while the OpenStack-compatible surface maps the same error to 403. The
   gate's allocate-to-the-limit step received
   `500 INTERNAL_ERROR`. Fixed: `QuotaExceeded` maps to `Forbidden` (403).
   Regression: `native_compute_quota_exceeded_is_forbidden_without_provider_side_effect`
   (`bins/o3kd/src/native_adapters/tests.rs`), which also pins "no provider
   side effect" via the fake provider's instance count.
3. **#905 generic PUT update unreachable in production.** Three layers:
   (a) the `seed_core` compute manifest did not declare the `update` lifecycle
   operation, so the generic update handler failed closed with
   `UnsupportedOperation` for every resource; (b) the static
   `/o3k/v1/compute/servers/{id}` route carried no PUT binding (axum
   prioritizes the static route over the wildcard, so the wildcard's PUT was
   shadowed); (c) the generic update handler extracts
   `Path<(namespace, collection, id)>` and cannot run on the concrete
   one-parameter route at all. Fixed: manifest declares
   `update → compute:UpdateServer` (with `compute:UpdateServer` added to the
   accepted-action inventories `contracts/cloud-kernel-actions.yaml` and
   `contracts/cloud-kernel-services.yaml`); the production router binds
   `.put(resource::update_compute)`; `resource::update_compute`/`update_for`
   mirror the existing `create_compute`/`delete_fixed` concrete-route pattern.
   Regressions: `seed_core_compute_server_declares_update_lifecycle_operation`
   (`crates/o3k-kernel/src/manifest.rs`),
   `production_router_compute_update_reaches_generic_update_application` and
   `production_discovery_advertises_compute_update_lifecycle_operation`
   (`crates/o3k-api/tests/native_compute_update_route.rs`) — both drive the
   real production router with the real `seed_core` manifest.
4. **Natively created networks were invisible to the generic native
   collection and undeletable through it.** The native network create in
   `bins/o3kd/src/native_adapters/resource.rs` inserted the durable generic
   resource-ledger row only when the request carried migration metadata, yet
   the generic list/delete routes read exactly that ledger — so a network
   created through `POST /o3k/v1/network/networks` could not be listed through
   `GET /o3k/v1/network/networks` nor deleted through the generic route
   (404), while discovery advertises list/show/create/delete for
   `network:network`. The restart phase of the gate exposed it: after a clean
   SIGTERM/relaunch the native network collection was empty. Fixed: the
   ledger row is recorded for every native network create, with its observed
   state in the canonical network vocabulary (`active`) so composition
   consumers (which observe children through this projection and accept the
   canonical states as ready) see unchanged semantics; the first revision of
   the fix used the migration-envelope `READY` convention and was caught by
   `p12_6_process` (the accepted example external controller composes
   `network:network` children), proving that suite's value as a conformance
   fixture. Regression: `native_network_create_is_listed_and_deletable_through_generic_collection`
   (`bins/o3kd/src/native_adapters/tests.rs`).

## Validation

Repository gates (each command exercised green on the current head while
converging; `[pending]` marks are placeholders filled only after the converged
head is frozen):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy -p o3kd --all-targets --all-features -- -D warnings` | PASS |
| `cargo test -p o3kd --all-features` | PASS (all suites incl. the new regressions) |
| `cargo test -p o3k-kernel -p o3k-api -p o3k-native-api --all-features` | PASS |
| `bash tests/araf-p2-convergence.sh` (PostgreSQL pass, disposable PG 16.4 + pinned Keycloak 25.0.6, full P12-IAM.7 suite + gate on the shared store) | PASS (`PASS: Araf P2 convergence (PostgreSQL)`) |
| `bash tests/araf-p2-convergence.sh` (SQLite parity pass, same fixture) | PASS (`PASS: Araf P2 convergence (SQLite)`) |
| `bash tests/architecture-boundaries.sh` (kernel action inventory validator) | PASS |
| Candidate head CI link | `[pending]` |

## Honest non-claims

- The gate proves convergence of the merged #887–#906 contracts in the
  production-profile composition; it does not add product surface.
- Diagnostics controlled-degradation oscillation is not exercised: the fake
  provider cannot degrade, so only the honest status vocabulary is pinned.
- The relationship row the projection proves is seeded through the canonical
  durable authority as documented environment setup; the write-side authority
  boundary (external-controller composition) is unchanged and documented
  above.
- `volume:allocated_byte_seconds` non-advertisement is the required evidence
  for this profile, not a missing meter.
