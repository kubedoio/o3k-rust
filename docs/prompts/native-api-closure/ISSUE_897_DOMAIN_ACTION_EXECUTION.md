# Implementation Prompt — Issue #897

## Mission

Implement **#897 — Execute declared resource domain actions through canonical Operations** on this branch. This is production infrastructure. Do not stop at planning, a mock route, or a frontend-compatible shortcut.

## Authority

Before editing code:

1. fetch/rebase this branch onto the current authoritative `o3kio/o3k` `main`;
2. read issue #897 in full;
3. inspect current ADR/SPEC/contract authority for native resources, ServiceManifest, ActionId/authorization, canonical mutations, Operations, controllers and selected OpenStack compatibility;
4. inspect issue/implementation state of #888 because #888 owns action schema discovery, while this issue owns execution;
5. search the repository for every existing start/stop/reboot path and prove which application/domain authority is canonical.

The issue requirements are mandatory, but suggested endpoint/wire shapes are **not pre-decided**. Choose the smallest generic contract that is consistent with existing architecture and document the decision.

## Required implementation

Deliver a versioned native action-execution contract that:

- resolves only authoritatively declared ActionIds;
- validates action input against the authoritative schema when one exists;
- authorizes against canonical AuthContext + durable target ownership;
- cannot use caller JSON to select owner/system scope;
- has bounded request/body handling;
- supports idempotency/replay conflict detection;
- establishes a canonical durable Operation before asynchronous provider side effects;
- returns truthful accepted/succeeded/failed/unknown semantics;
- conceals foreign resources according to the accepted native policy;
- works generically for future service actions without arbitrary executable action strings or provider RPC passthrough.

Prove the mechanism using real existing Compute `StartServer`, `StopServer` and `RebootServer` authority. Do **not** duplicate Compute state-machine/provider logic in the API crate.

## Architecture and contracts

Add or update ADR/SPEC/public JSON Schema/OpenAPI artifacts when required. Keep public DTOs provider-neutral and secret-safe. Add drift/contract tests so declared action metadata, schema and runtime dispatch cannot silently diverge.

If an accepted existing contract conflicts with the issue, do not silently override it: reconcile the conflict explicitly in the PR with the smallest architecture change and evidence.

## Production evidence

At minimum test:

- tenant positive start/stop/reboot real process path;
- Project A -> Project B denial/concealment;
- missing permission denial;
- undeclared action rejection;
- wrong resource/action confusion rejection;
- malformed/oversized body rejection;
- equivalent retry returns canonical Operation/result;
- conflicting idempotency reuse fails;
- restart/recovery preserves accepted action/Operation truth;
- provider timeout/unknown outcome remains truthful;
- OpenStack/native overlap converges on the same Compute authority;
- no provider credentials/private identifiers in public payloads/errors.

Use SQLite and PostgreSQL where durable semantics are involved. Add race/concurrency tests, not only happy-path unit tests.

## Validation gate

Run the repository's full required validation, including at minimum formatting, workspace Clippy with warnings denied, workspace check, workspace tests, relevant ignored PostgreSQL/process tests, contract/schema checks, and targeted real-process native/compatibility convergence tests. Run any stronger project-defined gates discovered on current `main`.

Do not mark the PR ready until CI and local evidence cover the production `o3kd` composition.

## Stop conditions

Do not stop because implementation is large or because another crate must change. Make the required cross-crate changes in this PR when they belong to #897.

Stop and report `BLOCKED` only for a genuinely external dependency that cannot be implemented safely in this repository, with exact reproducible evidence. Do not invent production truth, bypass authorization, use fixtures, or fall back to OpenStack routes to avoid the native contract.

## Completion report

Update the PR body with:

- final contract/route shape;
- exact canonical application methods used;
- Operation/idempotency semantics;
- security negatives;
- SQLite/PostgreSQL/process evidence;
- compatibility convergence evidence;
- CI status;
- remaining bounded deviations.

Finish only with `#897 COMPLETE` when every issue exit criterion is actually proven.