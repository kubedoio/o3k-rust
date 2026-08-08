# Issue #94 — Split `o3k-api` internally by protocol/service concern

GitHub: #535 (SPEC-0025 step 7 tracking issue). Local record number 94 is the
internal docs/issues tracker id; the GitHub issue is the authoritative
tracking issue for this work.

## Scope

SPEC-0025 step 7: split `crates/o3k-api/src/lib.rs` (currently one ~3055-line
file) into clear internal protocol-adapter modules, per SPEC-0025 section 4
("Public API adapters remain adapters"). Product profile:
`native-rust-testlab`.

This is a structural refactor only. No public route, request, response,
status code, header, microversion, or error behavior changes. No endpoint
additions and no "cleanup" behavior changes.

## Target shape

Implemented as single-file modules in `crates/o3k-api/src/`:

- `lib.rs`: crate public surface (`AppState`, `router()`,
  `router_with_state()`, `CONSOLE_AGENT_DISPATCH_TIMEOUT`), router
  composition, module declarations, and the cross-cutting
  health/version/discovery handlers;
- `error.rs`: shared error-envelope helpers;
- `auth.rs`: shared token validation helper (`require_token`);
- `identity.rs`: Keystone-compatible token issue/validate/check handlers and
  error mapping;
- `image.rs`: Glance-compatible image handlers, wire models, error mapping;
- `network.rs`: Neutron-compatible network/subnet/port handlers, wire
  models, error mapping;
- `compute.rs`: Nova-compatible flavor/keypair/server/action handlers, wire
  models, microversion helpers (`requested_compute_289`), the
  compute-scoped project-token check (`project_token`), error mapping, and
  the relocated unit tests;
- `placement.rs`: Placement discovery handler and microversion parsing;
- `volume_attachment.rs`: volume attachment handlers and wire models,
  limited to already-declared behavior;
- `middleware.rs`: the router-wide Nova + Placement microversion negotiation
  middleware.

Do not create one workspace crate per OpenStack service. `o3k-api` remains a
single non-application (adapter) crate; `contracts/core-architecture-
boundaries.toml` classification is unchanged.

## Acceptance criteria

- Axum types and OpenStack JSON wire models stay inside `o3k-api`.
- Domain/application crates keep no dependency on `o3k-api`
  (architecture-boundary checker passes unchanged).
- Route registration in `router_with_state` is preserved exactly: same
  paths, same method-to-handler bindings, same layer order
  (`microversion_middleware`, `DefaultBodyLimit`).
- Operation-scoped microversion behavior is preserved exactly: Nova 2.1-only
  negotiation with the GET-only 2.89 profile on volume attachment routes,
  Placement 1.0–1.28 with `latest` negotiation, exact error bodies, and the
  `OpenStack-API-Version`, `X-OpenStack-Nova-API-Version`, and `Vary`
  response headers.
- Error envelopes and project-scope behavior unchanged; error bodies keep
  their exact codes/titles/messages and must not expose credentials or
  provider payloads.
- Existing `o3k-api` tests are unchanged or minimally relocated (test bodies
  and assertions identical; `super::` paths adjusted). The integration tests
  `crates/o3k-api/tests/health.rs` and
  `crates/o3k-api/tests/gate_c_nova_callback.rs` keep passing unchanged.
- Capability inventory generation unchanged; TestLab API baseline unchanged;
  cross-implementation contract fixtures unchanged; OpenStack CLI portable
  workflow unchanged.
- Diff is reviewable by service concern; no compatibility or product
  evidence is upgraded merely because files moved.

## Non-goals

- No new workspace crate, no new dependency, no endpoint or microversion
  expansion, no behavior "improvement", no evidence-tier changes, no changes
  outside `crates/o3k-api/` and this issue record.

## Required validation

- `python3 scripts/check-architecture-boundaries.py`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `tests/testlab-api-baseline.sh`
- `tests/capability-inventory.sh` with a clean tree for
  `docs/compatibility/capability-inventory.json`
