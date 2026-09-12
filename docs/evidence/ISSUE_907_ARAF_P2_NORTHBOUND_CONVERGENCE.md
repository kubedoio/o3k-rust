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
| 2 | Discovery | `GET /o3k/v1/services`, `/resource-types`, `/resource-schemas/{ns}/{c}/{v}`, `/regions` | All served; compute/network/identity advertised; `compute:server` advertises create/show/list/update/delete; the start/stop/reboot domain actions are manifest-declared and proven executable by the journey (the discovery `actions` metadata projects canonical lifecycle operations per SPEC-0040); 1 region / 2 AZ from `O3K_LOCATIONS`. Advertised-implies-executable is enforced both ways: every advertised capability is exercised later in the journey, and the readiness/reachability discovery gates (defect fix 5 below) keep unreachable operations out of the advertisement | `ManifestRegistry::seed_core` (composition root) | ✓ | ✓ | ✓ | PASS |
| 3 | Generic lifecycle | `POST/GET/PUT/DELETE /o3k/v1/compute/servers[/{id}]`, `POST /o3k/v1/network/networks` | Create with idempotency key + exact replay (same key → same resource id, no duplicate); bounded list + cursor continuation; show with owner scope; PUT update with `If-Match: generation-N` (missing → 400, stale → 409, live → 200 — after waiting for action convergence so the generation is stable); delete → 204 | `resources` ledger, canonical operations + idempotency reservations | ✓ | ✓ | ✓ | PASS |
| 4 | Domain actions | `POST /o3k/v1/compute/servers/{id}/actions/{stop,start,reboot}` | Stop/start/reboot through the discovered action schema; canonical Operation IDs returned; replay-safe keys; states converge (ACTIVE→STOPPED→ACTIVE) | canonical operations + reconciler projection | ✓ | ✓ | ✓ | PASS |
| 5 | Operations | `GET /o3k/v1/operations[/{id}]` | Every mutation's operation shown (`id` agrees with collection); bounded `?limit=1` collection where a continuation cursor provably advances the page (page 2 non-empty and disjoint from page 1); every collected operation id is show-able again after restart | `operations` + `canonical_operation_metadata` | ✓ | ✓ | ✓ | PASS |
| 6 | Relationships | `GET /o3k/v1/{ns}/{c}/{id}/relationships` | One canonical relationship row is seeded through the documented environment-setup path (the same durable relationship authority the external-controller composition writer uses; see the note below), then the northbound projection is proven through the real production router: Tenant A receives 200 with exactly the seeded row — slot `network-primary`, canonical `network:network` child type, canonical public UUID child id (never provider-private), `bound`/`exclusive` state vocabulary, `has_more == false`; the bounded-page canonical-vocabulary assertions hold over the real row; Tenant B receives 404 with durable rows present (genuine concealment, not an empty page) | `resource_relationships` (seeded via `reserve_relationship`/`bind_relationship`; read projection from #899) | ✓ | ✓ | ✓ | PASS (see note below) |
| 7 | Quota | `PUT /o3k/v1/operator/quotas/{p}/{ns}/{dim}`, `GET /o3k/v1/quota/{ns}/{dim}`, `GET /o3k/v1/operator/quotas/{p}` | Finite limit (usage-relative for the shared PG store) → allocate to the limit → next create 403 with no reservation leak → delete frees the slot → re-allocation succeeds; stale-generation limit write 409; tenant self-administration 403; foreign quota read 403. The no-provider-side-effect pin lives in the defect-2 regression (`native_compute_quota_exceeded_is_forbidden_without_provider_side_effect`), which asserts the fake provider's instance count is unchanged by the 403 | `quota_limits` + committed usage, CAS generation | ✓ | ✓ | ✓ | PASS |
| 8 | Governance | `GET /o3k/v1/operator/governance/{projects,principals,roles,capabilities}`, `POST/DELETE .../assignments[...]` | Operator lists; grant of member role to Tenant B in project-a reflected in a refreshed exchanged token; revocation removes it again; tenant→operator routes 403; self-grant of operator 403 | durable assignments + role inventory | ✓ | ✓ | ✓ | PASS |
| 9 | Audit | `GET /o3k/v1/audit[/{id}]` | Creation/action/quota/governance mutations recorded with canonical actor/scope/target, proven via bounded per-`operation_id` filtered queries (order-independent on a shared store; the collection bound itself pinned by one malformed-cursor → 400 probe); quota/governance events proven via per-`service` filtered queries in the operator scope; per-event show through the production router; Tenant B's operation-filtered view contains none of Tenant A's event ids; after restart the exact pre-restart event ids survive | `audit_events` | ✓ | ✓ | ✓ | PASS |
| 10 | Diagnostics | `GET /o3k/v1/operator/diagnostics[/{summary,services,providers,capacity}]` | Operator reads summary (version v1, honest status vocabulary) and region/AZ topology from `O3K_LOCATIONS`; capacity honesty pinned as mutually exclusive shapes: the fake-provider profile wires neither the scheduler nor the agent inventory publisher (both gated on agent-control mTLS in the composition root), so the honest report is `unknown`/`never_observed` with empty dimensions — required exactly, never optional — while a profile WITH inventory must project all three canonical VCPU/MEMORY_MB/DISK_GB classes; tenant 403. Controlled degradation is not achievable with the fake provider, so the gate asserts the honest status vocabulary instead of fabricating a degradation, per the accepted diagnostics honesty contract | lifecycle registry + placement/location authorities | ✓ | ✓ | ✓ | PASS |
| 11 | Metering | `GET /o3k/v1/metering/definitions`, `GET /o3k/v1/metering/usage` | `compute:instance_seconds` advertised with `instance_second` unit; `volume:allocated_byte_seconds` **honestly not advertised** (no native storage provider in this profile — non-advertisement is the requirement, not a gap). Usage over an hour-aligned window: unit, meter key, non-null UTC `start`/`end`/`observed_through`/`authority_started_at`, and `observed_through ≤ end`; an unaligned start instant is rejected 400; run/stop/start/delete journey total frozen by the closes is **finite and > 0** (a never-accruing "0.000" would make the restart comparison vacuous) and byte-identical across restart with unit/meter key re-asserted; narrowed to the gate's own closed resource so unrelated open intervals cannot flake the comparison; cross-scope tenant read 403; operator `?scope=project-a` 200 | `metering_authority`, `metering_intervals`, `metering_aggregates` | ✓ | ✓ | ✓ | PASS |
| 12 | Cross-tenant isolation | all of the above + probes | Tenant B against the **live, ACTIVE** Tenant A server: show → 404 and action → 404 (genuine scoping denial — cannot be explained by deletion), with the server verified unaffected afterwards; collection-level concealment on list, including a **live-row** probe against the still-active Tenant A network (the compute list probe targets a since-deleted server and cannot alone prove live concealment); foreign network 404; foreign operation 404; foreign quota 403; audit isolation; metering foreign scope 403; governance 403; foreign-project token exchange denied 401; signed-cursor reuse by another tenant → 400 (no existence oracle); tampered cursor → 400 | AuthContext scoping on every route | ✓ | ✓ | ✓ | PASS |
| 13 | Secret leak scan | every retained response body + both o3kd log generations | Structural DTO assertions along the journey plus an explicit scan of all retained bodies for: bootstrap password, token signing key, native cursor signing key, OIDC/Keycloak access tokens, the PostgreSQL DSN password when present, DB URLs, `-----BEGIN` private-key markers, `agent_epoch` internals. `agent_epoch` is body-only: the body scan rejects any occurrence as an internals-leak marker, and the diagnostics structural test likewise excludes the epoch value from response DTOs; it is a legitimate tracing field / SQL column, so the process-log scan deliberately does not check it there. The o3kd process logs (append mode, both generations) are separately scanned for the secret markers above, the `postgres://` DSN form, and `-----BEGIN` private-key markers. Verified across the restart phase; no marker appears in any body or log | response DTOs and logs carry no credential material | ✓ | ✓ | ✓ | PASS |
| 14 | Restart/recovery | whole journey phase 2 | SIGTERM → clean exit → relaunch on same store/port family: resources preserved; **every** collected operation id (create/stop/start/reboot/delete for server A, create/delete for server B) returns 200 from `GET /operations/{id}`; quota limit AND freed usage (back to the pre-journey baseline) persist; governance revocation persists (re-exchange denied 401); the exact pre-restart audit event ids survive; metering totals unchanged and still positive; diagnostics return honest (non-fabricated) status. The restart phase runs unconditionally in both backend passes | all of the above durable authorities | ✓ | ✓ | ✓ | PASS |
| 15 | Bounded queries | collections exercised above | Operations collection (guaranteed non-empty in this journey, independent of tombstone rows): invalid cursor → 400, tampered cursor → 400, cross-tenant cursor reuse → 400; page-size bound per SPEC-0030 pagination: `limit=500` above MAX_PAGE_SIZE (200) → 400 on the servers collection (rejected, not clamped); audit collection: malformed cursor → 400; metering requires hour-aligned instants (unaligned start → 400) with hard meter/range/bucket bounds per SPEC-0046 | `CursorConfig` HMAC + repository bounds | ✓ | ✓ | ✓ | PASS |
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
5. **Discovery advertised operations the live runtime would reject.**
   `discover_resource_types` readiness-gated only `list`, so a not-ready
   resource (e.g. `volume:volume` without a native storage provider) still
   advertised show/create/delete that the runtime fails closed with 503, and
   `create` was advertised for resources with no reachable create contract
   (e.g. `network:subnet`, which fails closed at the mutation boundary).
   Advertising an operation the composition cannot execute violates the
   #907 item-2 advertised-implies-executable discovery contract. Fixed in
   `crates/o3k-native-api/src/lib.rs`: readiness gates ALL lifecycle
   operations, and `create` additionally requires a public create contract
   or external-controller ownership (mirroring the generic create handler's
   reachability rule). Regression:
   `discovery_advertises_only_reachable_lifecycle_operations`
   (`crates/o3k-native-api/src/lib.rs`).
6. **A stale `If-Match` on the generic update committed a phantom durable
   operation.** The update arm created the canonical `lifecycle:update`
   operation and consumed the idempotency reservation BEFORE checking the
   generation precondition, so a rejected 409 left a `Pending` operation the
   reconciler would retry forever and made a same-key retry replay as 202 for
   an operation that could never complete. Fixed in
   `bins/o3kd/src/native_adapters/resource.rs` with a new non-creating store
   lookup (`DurableStore::get_idempotency_reservation`, SQLite/PostgreSQL/
   unified parity): reservation-first replay detection returns the durable
   result of an already-accepted update regardless of the now-advanced
   generation (true idempotent replay) and rejects key reuse with different
   semantics via the stored fingerprint; only a never-accepted request
   reaches the generation precondition, which is validated before any
   durable write. Regressions:
   `native_update_stale_if_match_leaves_no_pending_operation`
   (`bins/o3kd/src/native_adapters/tests.rs`) — 409, no durable operation at
   the deterministic operation id, replay stays 409, live generation still
   updates and bumps the durable generation;
   `sqlite_idempotency_reservation_is_scoped_and_atomic` extended with the
   non-creating lookup (`crates/o3k-store/src/tests.rs`).
7. **A network create whose ledger insert failed orphaned canonical
   authority.** The canonical network (and its quota reservation) committed
   before the generic-ledger row; a ledger failure left a network visible to
   native show yet invisible to native list and undeletable through it.
   Fixed: both ledger-failure branches (generic store failure and an
   `AlreadyExists` row that is genuinely a different resource) compensate by
   deleting the canonical network before the error is returned. Regression:
   `native_network_create_compensates_canonical_on_ledger_conflict`.
8. **Native network create/show dropped the public spec name.** Returning
   the raw ledger projection emitted `spec: {}`; the pre-#928 shape carried
   `spec.name`. Fixed: `network_external_json` whitelist-projects the
   name-only public contract (raw desired_state still never crosses the
   boundary — imported rows can carry provider references). Regression:
   `native_network_create_replay_returns_same_resource` (create response,
   show, and collection all carry `spec.name`).
9. **Deleted native networks were still readable through show.** The ledger
   tombstone made show return 200 with `status.state: DELETED` while compute
   conceals deleted servers (404). Fixed: show is the live-resource view and
   conceals finalized rows; the collection keeps projecting tombstones with
   an explicit DELETED state (documented below). Regression:
   `native_network_show_conceals_deleted_resource`.
10. **Network create ignored `Idempotency-Key`.** The canonical id was a
    fresh UUIDv7 per call, so a same-key retry name-conflicted (409) or
    duplicated instead of converging, and the migration-envelope fields the
    arm still read were unreachable through the HTTP contract
    (`NetworkCreateSpec` is `deny_unknown_fields`). Fixed: the canonical id
    derives deterministically from the idempotency key with the resource
    type in the derivation material (`{scope}:network:network:{key}`; the
    volume and volume-attachment arms use the same type-prefixed form so one
    key can never collide across types), a same-key retry with identical
    semantics returns the original resource (canonical and ledger replay are
    both recognized), a same-key retry with a changed name is an
    IdempotencyConflict, a same-key retry after deletion fails closed
    (a DELETED ledger tombstone is never a replay target), the dead envelope
    reads are removed, and `update_for` rejects a mismatched `kind` exactly
    like create. Upgrade note: the type-prefixed derivation is an observable id
    change for a given key. These arms do not write generic idempotency
    reservations, so a same-key retry issued across the upgrade derives a
    different id, and the per-arm outcome differs: for `network` the canonical
    name-uniqueness check rejects the retry with 409 when the name matches,
    otherwise a second network is created; for `volume` there is no name
    uniqueness, so the retry would create a duplicate volume; for
    `volume_attachment` the only conflict source is volume-id uniqueness.
    Clients should retry outstanding creates with a fresh key after the
    upgrade. Regressions: `native_network_create_replay_returns_same_resource`,
    `native_network_replay_with_changed_name_conflicts`,
    `native_network_replay_after_delete_is_not_a_replay`,
    `production_router_update_rejects_mismatched_resource_kind`
    (`crates/o3k-api/tests/native_compute_update_route.rs`).

11. Generic update pre-check panicked on a corrupt non-object
    `desired_state`. The update path parsed the durable `desired_state` and
    then assigned through `serde_json`'s `IndexMut`, which panics on a
    valid-JSON non-object value (corruption or a bad migration), dropping the
    connection instead of failing closed. The parse now guards
    `as_object_mut()` and returns the same clean conflict the sibling code
    paths use. Regression:
    `native_update_with_non_object_desired_state_fails_closed_without_panic`
    (`bins/o3kd/src/native_adapters/tests.rs`).

12. Network-create quota denial surfaced as 409. The rewritten network arm
    treated every `create_network_for_project_with_id` error as a potential
    replay probe, so a durable `QuotaExceeded` became a 409 conflict —
    telling the tenant a non-retryable limit is retryable. The quota denial
    now maps to 403 before the probe, exactly as on the compute surface.
    Regression:
    `native_network_quota_denial_is_forbidden_not_a_replay_conflict`
    (`bins/o3kd/src/native_adapters/tests.rs`). Note: the volume-create arm
    enforces no quota dimension (it performs no `reserve_quota`), so no
    volume-quota mapping exists or is claimed.

13. Volume-create same-key replay ignored changed semantics. The arm writes
    no idempotency reservation, so the deterministic id is its only replay
    identity: reusing a key with a different spec derived the same id, hit
    `ResourceAlreadyExists`, and returned the original volume as if it were
    the new request's result. The replay arm now pins the durable volume's
    name, description, size, type, availability zone, and metadata to the
    request and returns `IdempotencyConflict` on mismatch, exactly as the
    generic idempotency contract treats changed bodies. Regression:
    `native_volume_create_replay_with_changed_semantics_conflicts`
    (`bins/o3kd/src/native_adapters/tests.rs`).

## Validation

Repository gates (each command exercised green on the current head while
converging; `[pending]` marks are placeholders filled only after the converged
head is frozen):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --all-targets --all-features` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo test --workspace --all-features` | PASS (113 suites, 0 failures) |
| `git diff --check` | PASS |
| `bash tests/maintainability-guards.sh` | PASS |
| `bash tests/adr-governance.sh` | PASS |
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
- The generic native resource collection projects the durable ledger
  **including finalized "DELETED" tombstone rows with explicit
  `status.state`**: deletion finalizes the row rather than erasing history,
  and clients filter by `status.state`. Cursor probes therefore target the
  operations collection (guaranteed live rows), not the resource ledger.
- The discovery `actions` metadata projects canonical lifecycle operations;
  domain actions (start/stop/reboot) are manifest-declared and proven
  executable by the journey rather than part of that metadata projection.
- The PostgreSQL pass intentionally shares one durable store with the
  P12-IAM.7 suite (a single IdP/store fixture via `O3K_P12_7_AFTER_HOOK`):
  quota limits are set relative to live usage, metering totals are narrowed
  to the gate's own closed resources, and the operator assignment
  provenance is asserted loudly at the start of the gate (`seed_store`) rather than assumed.
- The update arm's post-acceptance terminalization (fix 6) has no direct
  unit test: injecting a store failure between the resource write and the
  operation-state write requires a fault-injection seam the concrete
  `O3kStore` type does not expose. The branch is defense-in-depth over an
  already-durable precondition design; the replay/conflict/precondition
  paths around it are fully covered. Corollary: if the resource write fails
  AND the best-effort terminalization write also fails, the operation row
  stays `Pending` and a same-key replay returns `202 complete:false`
  indefinitely (no reconciler owns `lifecycle:update`). This requires two
  consecutive durable-store failures after an accepted precondition and is
  documented in-code as best-effort.
- Cross-surface network symmetry is directional: a natively created network
  is visible to the Neutron-compatible surface (shared canonical authority),
  but a Neutron-created network has no generic-ledger row by design, so the
  native generic collection and native delete route do not manage it; the
  canonical Neutron routes remain its authority.
