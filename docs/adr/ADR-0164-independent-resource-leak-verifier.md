# ADR-0164 — Independent resource-leak and foreign-state verifier

Status: Accepted
Date: 2026-08-09
Supersedes: ADR-0095
Superseded-by: none
Affected-services: compute, network, placement, governance

## Context

ADR-0095 established the stable, redacted owned-resource inventory and the
protected `resource-leak-result` artifact, but its inventory intentionally
stopped at domains, provider identities, `o3k-*` links, and foreign-state
digests. Issue #88's remaining acceptance requires an independent verifier
around both the normal real CirrOS E2E and the failure-injection matrix that
also covers TAPs, DHCP lease and binding state, managed filesystem state
(config-drive, image cache, artifact transfers, journals, console logs),
ports, Placement allocations, operations, daemon processes, and durable
ledger rows — and that fails when deliberately injected stale O3K artifacts
or foreign-state mutations are present.

## Decision

The inventory is extended to schema_version 3. All schema_version 2 fields
and behavior are preserved byte-identically; the new sections are
environment-gated (`O3K_REAL_HOST_STATE_ROOT`, `O3K_REAL_HOST_PID_ROOT`,
`O3K_REAL_HOST_CANARIES`) so a configuration that sets none of them still
produces an unchanged v2 artifact, and the pre/post-run guards accept both
schema versions.

- `managed_state` inventories the state root's `data/` subtree (config-drive,
  dhcp, image-cache, images content, agent artifact store, network ownership,
  placement journals, console logs) with relative paths, kinds, sizes, and
  SHA-256 content hashes bounded by entry and byte caps. Parsed O3K-owned
  ownership manifests (`.o3k-iso-ownership.json`, `network/ownership.json`,
  `image-cache/ownership/*`, artifact `.manifest` files) are recorded
  redacted. Incomplete-transfer temporaries are recognized by the exact
  names the runtime uses (`.part`, `.manifest.tmp`, config-drive
  `-tmp-`/`-old-`/`.iso-*` temporaries, `base-<sha>.tmp-*`, overlay
  temporaries, `ownership/<id>.json.tmp-*`, `<uuid>.upload-<uuid>`,
  `<identity>.commands.tmp`/`.state.tmp`). A configured root that does not
  exist records `status: "absent"` as valid clean state; the durable and dhcp
  sources still fail closed because they are configured and uninspectable.
- `dhcp` classifies dnsmasq processes by scanning `/proc/*/cmdline` (owned
  when the cmdline references the state root's dhcp directory; foreign
  processes recorded only as count plus a redacted args digest), parses the
  lease file and `state.json` bindings, and records pidfile presence.
- `processes` verifies `<pidroot>/<daemon>.pid` files
  (`pid|start_ticks|uid|binary`) against `/proc` (exe suffix, uid, start
  ticks) and records O3K daemons running under the state root's `bin/`
  without a matching pidfile as unmanaged potential leaks.
- `durable` opens the ledger read-only with `sqlite3` and records counts and
  identities for non-terminal operations, non-terminal agent commands,
  non-terminal artifact transfers, non-deleted resources, non-deleted ports,
  all Placement allocations, image-overlay ownership rows, and counts for
  provider references and operation-retry state. The non-terminal predicates
  are derived from the code: operations/commands are terminal in
  `succeeded`/`failed` (reconciler), transfers are terminal in
  `committed`/`rejected`/`expired` (`ArtifactTransferState::is_terminal`),
  resources are deleted only when `observed_state` decodes to
  `ServerState::Deleted`, and ports are hard-deleted rows.
- `canaries` records identity-level state for operator-configured foreign
  markers: libvirt domain presence plus UUID and raw `dumpxml` digest
  (deterministic for a defined domain), network-link presence plus kind and
  an address digest, and file presence plus content digest. A missing canary
  is `present: false`; the verifier treats its disappearance as a foreign
  change. An unreadable canary config fails closed.

Every O3K-owned object carries exactly one classification in
{`active_owned`, `expected_retained`, `stale_owned`, `inconsistent`}, derived
from the runtime contracts as they exist today, with a `contract` field
naming the code path: live resources and their host objects are
`active_owned`; content-addressed image-cache bases, committed artifact
manifests and content, the command journal (including a non-terminal command
row whose owning operation reached a terminal state — operation state is
authoritative, and such a row is durable evidence of an UnknownOutcome
boundary, a documented follow-up), durable DHCP files, placement
journals, ownership manifests, reusable unbound ports, terminal overlay
rows, and in-flight transfer parts are `expected_retained`; per-instance
files of terminally-deleted instances that the delete paths reap (domains,
TAPs, overlays, ownership manifests, console logs, config-drive artifacts,
orphan temps) are `stale_owned`; durable rows pointing at provably absent
host objects (active allocations for deleted resources, non-terminal rows of
deleted resources, bound ports without an instance, stale DHCP bindings) are
`inconsistent`. The verifier's first real-host observation was a contract
gap — no delete path invoked the existing config-drive reapers, so leftover
control-plane config-drive directories/ISOs and agent ConfigDriveIso
manifests of deleted instances were classified `stale_owned`; that gap was
closed in the same campaign (delete-terminal reaping and definitive
pre-libvirt create-failure reaping), and the classifier keeps flagging any
remaining media as `stale_owned`.

ADR-0095's decisions remain: two-read stability with a canonical-compare
loop, fail-closed collection, redacted foreign-state digests, protected-path
allowlist hashing with caps, and atomic publication via same-directory
temporary files and `os.replace`.

A new `scripts/real-host-leak-verifier.py` compares a baseline and an after
inventory per scope and aggregates per-scope verdicts:

- `compare` emits `owned_leaks` (after-only O3K-owned identities with
  classification and contract), `inconsistencies` (durable-vs-host
  contradictions), `foreign_changes` (digest mismatches plus per-canary
  identity comparisons naming only canary kind/name and what changed),
  `expected_retained` summaries, and a `status` in {passed, failed,
  blocked}. `blocked` covers unreadable/malformed/unavailable snapshots,
  unsupported or mismatched schema versions, asymmetric or disappearing
  sections, `--expect-state-root` violations, and canary configuration
  appearing or disappearing between snapshots.
- `negative-stale` and `negative-foreign` verify that deliberately injected
  stale O3K artifacts and canary mutations respectively make the comparison
  fail and are named in the diagnostic.
- `aggregate` produces the protected `resource-leak-result` artifact
  (schema_version 2, `artifact_type: "resource-leak-result"`); `passed`
  only when every scope verdict passed, the negatives detected their
  injections, all counts are zero, and every verdict's source identity
  matches the supplied commit. A missing scope or negative result file makes
  the aggregate `blocked`.

The verifier never trusts O3K API output alone: it diffs independent host
inventories and cross-checks durable state against host objects.

## Consequences

- The protected `resource-leak-result` artifact (schema_version 2) is now
  produced by the aggregate command and covers the normal E2E, every
  failure-recovery scenario, and the negative tests.
- Portable tests exercise the collector and verifier against fake binaries
  and a real sqlite3 ledger built from the migration files, including
  determinism, owned/foreign classification, tombstone non-leaks, allocation
  inconsistencies, orphan domains/TAPs/temps, canary mutation, fail-closed
  sources, secret redaction, and aggregate blocking.
- Owned-link classification now recognizes the real TAP prefix `o3ktap-`
  (`HostNetworkManager::tap_name`); previously a leaked TAP was hashed as
  foreign state and could stay invisible to the owned-link delta.
- The first expected real-host finding is the un-wired config-drive cleanup;
  runtime fixes remain separate issues and are not part of this decision.
- No real-host run or leak-free E2E result is claimed by this repository
  change; the verifier itself is repository-side tooling.

## Provenance

This is an independently authored repository decision based on issue #88,
ADR-0095, the release-evidence schema, and the existing protected workflow
contract. No private source or implementation was used.
