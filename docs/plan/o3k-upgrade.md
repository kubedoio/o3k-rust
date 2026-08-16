# O3K upgrade/rollback — design and acceptance plan (issue #626)

Status: plan (implementation follows this document).

## Required agent plan record

- **Issue**: #626 (o3k upgrade/rollback milestone; one issue, one coherent PR set).
- **Deployment/evidence profile**: TestLab install profile — Ubuntu 24.04 x86_64 and
  Debian 12 x86_64, libvirt agent provider, SQLite. No production/HA/PostgreSQL/
  Kubernetes claims.
- **Canonical O3K service/domain**: O3K Cloud Kernel control plane (`o3kd`) owns
  all preserved state; the upgrade command is an operator-side orchestration of
  the kernel's installation, never a new authority. No OpenStack compatibility
  surface changes.
- **Authority mode**: `o3k-implemented` — the upgrade engine lives inside the
  existing `o3k` operator CLI (`bins/o3k`). No new daemon, no new crate, no new
  process.
- **Files expected to change**: `bins/o3k/src/main.rs`, `bins/o3k/src/lib.rs`
  (CLI dispatch) plus new modules `bins/o3k/src/upgrade/` (engine, state,
  backup, release, fence, failure policy) and `bins/o3k/src/checks/` (three
  new doctor checks);
  `packaging/get-o3k.sh` (version fence + delegation notice),
  `packaging/make-release.sh` (manifest `schema_version` +
  `upgrade_from.min_version`),
  `docs/plan/o3k-upgrade.md`, `docs/INSTALLER.md`/`docs/RELEASE.md` updates;
  tests: `bins/o3k/tests/`, new `tests/upgrade-*.sh` process tests,
  `tests/ci-workflow.sh` governance assertion.
- **Contracts/specs affected**: none normative; additive doctor checks extend
  `contracts/o3k-doctor-output.schema.json` documentation only. Release
  manifest gains an optional field (backward-compatible for older readers).
- **Public reference inputs**: current published releases v0.2.0-alpha.2
  (tag `d3e9011`) and v0.3.0-alpha.1 (tag `256cb63`); GitHub Releases API for
  artifact URLs; sqlx migration directory as the single migration authority.
- **Operations/actions/resources**: `systemctl stop/start o3kd o3k-compute`,
  atomic rename of `/usr/local/bin/{o3kd,o3k,o3k-compute}`, read-only doctor,
  public OpenStack API smoke (token, server list/show, console, lifecycle).
- **Database/execution assumptions**: SQLite only; migrations are sqlx forward
  migrations in `crates/o3k-store/migrations/`; verified that **zero migrations
  were added between v0.2.0-alpha.2 and v0.3.0-alpha.1**, so the first real
  journey is a pure binary swap with a provably DB-compatible rollback.
- **Cross-service dependencies/compensation**: none beyond the o3kd↔o3k-compute
  pair; upgrade stops services in compute-then-control order and starts
  control-then-compute; the agent re-registers with a fresh epoch (existing,
  tested behavior). No libvirt daemon restart. Guests/domains are never
  recreated.
- **Required evidence tier**: domain/unit tests → portable simulated-release
  process tests → component real-host (Ubuntu first) → full dual-distro
  real-host upgrade/rollback/re-upgrade + interruption matrix → release gate.
- **Tests to add first**: version-fence unit tests; state-machine transition
  tests; backup integrity tests; migration-compatibility decision tests;
  failure-policy tests per phase; doctor check tests; installer fence tests.
- **Known uncertainties**: exact shape of `install.sh` convergence mode for the
  ownership-ledger update (resolve during implementation by reading
  `packaging/install.sh`); whether GitHub API pagination is needed for release
  asset discovery (single-page suffices for current releases).
- **Explicit non-goals**: PostgreSQL/Kubernetes/HA; zero-downtime/rolling
  upgrades; automatic curl|sh upgrades (installer prints `sudo o3k upgrade`
  instead); signed-artifact authenticity claims (checksums are integrity, not
  signature — unchanged from current releases); repair of broken installs
  (preflight refuses, never hides).

## 1. Product surface

The `o3k` CLI gains three subcommands next to `doctor`:

```
o3k version                     # binary version + installed release version
sudo o3k upgrade [--to vX.Y.Z] [--check] [--yes] [--json]
sudo o3k rollback [--yes] [--json]
```

- `upgrade --check` is strictly read-only (no download, no mutation): runs the
  preflight and prints a verdict.
- `rollback` is never a hidden installer flag.
- Exit codes: 0 success, 1 upgrade/rollback failed or blocked (including
  preflight), 2 usage. JSON output records `source_version`, `target_version`,
  `phase`, `backup_id`, `status`, `rollback_performed`, `doctor_status` — no
  secrets.
- `o3k doctor` remains read-only and is the post-upgrade validation gate.

## 2. Upgrade state machine

Durable state file `/var/lib/o3k/.o3k-upgrade-state.json` (owner `o3k`, mode
0600), written with fsync + atomic rename. It is host-local operational state,
not control-plane resource metadata (same class as the installer ownership
manifest).

Phases:

```
DISCOVERED -> RELEASE_DOWNLOADED -> RELEASE_VERIFIED -> PREFLIGHT_PASSED
-> BACKUP_CREATED -> SERVICES_STOPPED -> BINARIES_INSTALLED
-> MIGRATIONS_APPLIED -> SERVICES_STARTED -> HEALTH_VERIFIED
-> DOCTOR_PASSED -> COMMITTED
```

- A concurrent-invocation lock (`flock` on `/run/lock/o3k-upgrade.lock`) plus
  the persisted state make two upgrades impossible to interleave.
- Every phase persists its state BEFORE its side effects; interruption +
  re-run resumes from the recorded phase or rolls back (per the failure policy
  below). Timeout during service stop/start is treated as unknown outcome and
  re-observed, never guessed.
- `COMMITTED` writes the backup record into the rollback chain
  (`/var/lib/o3k/backups/backup-chain.json`, same permission class) and clears
  the in-progress state.

## 3. Release verification

The upgrade consumes only official GitHub Release assets for the requested
target:

1. Resolve the target release: explicit `--to vX.Y.Z` or the newest published
   release in the same channel family as the installed version (alpha), via
   the GitHub Releases API (repository `kubedoio/o3k-rust`).
2. Download `o3k-<version>-linux-x86_64.tar.gz` + `.sha256` + `install.sh`
   into a private temporary directory (`/var/lib/o3k/upgrade-download`, mode
   0700).
3. Verify: tarball sha256 vs the published `.sha256`; the bundle's own
   `SHA256SUMS` covers exactly the extracted regular files; `manifest.json`
   declares the requested version and an
   `upgrade_from` fence (see below); the bundle `install.sh` byte-matches the
   published `install.sh` asset (installer_sha256 from the manifest).
4. Refuse if the fence fails, the artifact is partial, or verification is
   impossible. Never execute any downloaded script; never fall back to `main`.

## 4. Upgrade-path fence

- Version comparison is semver-with-prerelease aware, implemented in Rust with
  no new dependency (a ~80-line parser/comparator with unit tests).
- Target must be strictly newer than installed, same profile (libvirt), same
  channel family (alpha). The new release's `manifest.json` gains
  `"upgrade_from": {"min_version": "v0.2.0-alpha.2"}` written by
  `make-release.sh` (default: the previous published release). Upgrading from
  anything older than `min_version` fails closed with "unsupported upgrade
  path; reinstall required".
- Downgrade via `--to` (target < installed) is refused explicitly, never
  silent.
- The installer (`get-o3k.sh`) gains the matching fence: installed version
  newer than target → refuse; installed version older → print
  `Run: sudo o3k upgrade` and exit 0 (no automatic mutation through curl|sh);
  same version → existing idempotent re-run behavior.

## 5. Preflight (read-only, before any mutation)

- supported distro (ubuntu 24.04 / debian 12, x86_64) and root/sudo identity;
- enough free disk for tarball + extraction + backup (2.5x current DB size +
  200 MB floor, measured, not guessed);
- installed ownership manifest (`.o3k-installed`) parses and all entries exist;
- installed binaries match the installed `SHA256SUMS` where it exists (mismatch
  → FAIL, never silently upgraded);
- SQLite integrity: `quick_check` clean and WAL journal mode; database schema
  version readable from the `_sqlx_migrations` table;
- no `.o3k-upgrade-state.json` in a non-terminal phase, and no live upgrade
  lock;
- `o3k doctor` (read-only) does not report a release-blocking FAIL in the
  release/critical subset.

A broken current installation is never upgraded to hide the problem.

## 6. Backup

Root `/var/lib/o3k/backups/<backup-id>/` (created 0700, files 0600), where
`backup-id = o3k-upgrade-<from-version>-<to-version>-<epoch>`. Contents:

- `backup.json` — source/target version, source commit identity (from the
  installed release manifest), timestamp, binary sha256 list, DB schema
  version, backup id, and a `rollback_eligible` record describing the
  migration-compatibility decision;
- `o3k.sqlite.backup` — crash-consistent snapshot via the existing
  `SqliteStore::backup_to_file` (VACUUM INTO) with a WAL checkpoint first;
  never a raw `cp` of a live WAL database;
- `config/` — copy of `/etc/o3k` configuration files (env files, openrc,
  clouds.yaml, tls material) with original modes; credentials/TLS are copied
  verbatim, never regenerated, never printed;
- `release/` — installed release-manifest.json + SHA256SUMS + `.o3k-installed`.

Backup verification (size non-zero, JSON parses, DB snapshot opens
read-only and quick_checks clean) happens before any mutation. Private
permissions are asserted after creation.

## 7. Migration compatibility policy (load-bearing)

- Schema version = the maximum applied migration version from the
  `_sqlx_migrations` table, read with a plain sqlx connection (the doctor
  already uses raw sqlx; the architecture boundary forbids application crates
  from naming the concrete `SqliteStore`, so the upgrade engine never links
  it).
- The target's expected schema version is declared by the release
  itself: `make-release.sh` writes `"schema_version": <max-migration-number>`
  into `manifest.json` (computed from `crates/o3k-store/migrations/` at build
  time). No migration SQL is embedded in the `o3k` binary.
- Forward: `o3kd` applies migrations at startup through its embedded
  migrator — the single, already-tested migration applier. The upgrade's
  MIGRATIONS_APPLIED phase records the pre-start schema version, starts o3kd,
  then verifies the post-start version reached the manifest's
  `schema_version`; anything else fails closed and rolls back per §9.
- Reversibility: **no migration is reversible** (no `.down.sql` exists).
  Rollback after a schema change therefore follows rule B from the goal:
  restore the pre-upgrade `o3k.sqlite.backup` together with the previous
  binary set. Rule A (prove the old binary supports the new schema) applies
  only when the schema version did NOT change, in which case the backup is
  kept as a safety net but the live DB is left untouched.
- The decision is recorded in `backup.json` at backup time
  (`db_restore_required_on_rollback: true|false`). Fail closed whenever the
  schema version cannot be determined on either side.
- Verified fact for the first real journey: v0.2.0-alpha.2 → v0.3.0-alpha.1
  added **zero** migrations (both at 0017), so the real acceptance is a pure
  binary swap with DB-compatible rollback. Later journeys with schema changes
  exercise the restore path via the interruption matrix.

## 8. Binary/package switch and service ordering

Derived from the current architecture (compute is a consumer of the control
plane; the control plane fences stale agent epochs):

```
verify + backup
-> systemctl stop o3k-compute   (bounded wait, unknown = timeout re-observe)
-> systemctl stop o3kd          (bounded wait)
-> replace /usr/local/bin/{o3kd,o3k-compute,o3k} by atomic rename from the
   verified extraction (same release, never mixed)
-> update /usr/local/share/o3k/release-manifest.json + SHA256SUMS +
   .o3k-installed via the new release's install.sh convergence path
   (ownership-ledger rules preserved; config digest ledger prevents
   regeneration of operator-edited files)
-> systemctl start o3kd; wait /healthz + /readyz
   (o3kd applies any pending embedded migrations at startup — the single
   migration applier; the upgrade then verifies the schema version matches
   the target manifest's `schema_version`)
-> systemctl start o3k-compute; wait agent registration (fresh epoch)
-> doctor full run must be healthy (or warning-only)
-> public API smoke: token, server list/show (same UUIDs), console,
   lifecycle action on the existing test-vm, Placement consistency
-> COMMITTED
```

`systemctl stop o3k-compute` before `o3kd` prevents the agent from observing a
half-upgraded control plane; starting `o3kd` first restores the registration
listener before the agent reconnects. The libvirt daemon is never restarted and
existing domains are never undefined or recreated. Mixed-version installs fail
`release.binary_set_consistent` (new doctor check).

## 9. Failure policy per phase

- download/verification failure → no mutation, state cleared, exit 1;
- preflight failure → no mutation, exit 1;
- backup failure → no mutation, exit 1;
- binary replacement failure (partial) → restore the saved old binary set
  (kept in the backup dir), re-run doctor, report;
- migration failure → restore pre-upgrade DB backup + old binary set, re-run
  doctor, report;
- service startup failure → attempt rollback to the backup; if the DB schema
  did not change, rollback is binaries-only;
- doctor failure after start → do NOT commit; attempt safe rollback; if
  rollback also fails, leave an explicit recoverable `FAILED_UPGRADE` state
  with the recorded backup id and a `sudo o3k rollback` instruction;
- no endless upgrade↔rollback loops: one automatic rollback attempt per
  invocation, then terminal state with instructions.

## 10. Rollback

`sudo o3k rollback` selects the immediately previous successful upgrade
snapshot from `backup-chain.json`:

- validate the backup record (O3K-created, hashes intact, JSON parses, DB
  snapshot opens clean) — only O3K-created verified records are eligible,
  never arbitrary user-selected directories;
- stop services (compute then o3kd);
- restore the previous binary set + release manifest + SHA256SUMS;
- restore DB only when `db_restore_required_on_rollback` is true (per §7);
- restore config files only when a recorded migration changed them (never in
  the first real journey); credentials/TLS are preserved by construction;
- start o3kd then o3k-compute; wait readiness + agent registration;
- doctor must be healthy; public API smoke (same server UUID/IP/console/
  lifecycle, Placement consistent);
- record the rollback in the chain; second rollback is a no-op with a notice
  (no backup exists for it).

## 11. Doctor integration (read-only)

Three additive checks:

- `release.binary_set_consistent` — the three installed binaries hash against
  the installed SHA256SUMS and share the same manifest version (mixed-version
  = FAIL);
- `release.backup_available` — PASS when a valid O3K-created backup exists in
  the chain, WARN when absent on an otherwise healthy install;
- `release.upgrade_state` — terminal in-progress state → WARN
  `release.upgrade_incomplete` with the exact safe next command
  (`sudo o3k upgrade` or `sudo o3k rollback`); committed → PASS.

Doctor never repairs.

## 12. Installer integration

`get-o3k.sh` gains a version fence (§4). Fresh host → install (unchanged);
same version → idempotent convergence (unchanged); older installed version →
download + verify the new release bundle into the private upgrade-download
directory (no installation mutation) and print the exact next command
(`sudo /var/lib/o3k/upgrade-download/o3k-<ver>/bin/o3k upgrade`) — the
upgrade machinery runs only from verified artifacts and only after an
explicit operator action, so noninteractive curl|sh never mutates an existing
install (safety over convenience); newer installed version → refuse implicit
downgrade with exit 1. After the first upgrade the installed `o3k` binary
itself provides `sudo o3k upgrade` for all future journeys.

Implementation note (resolved during implementation): the delegation
directory `/var/lib/o3k/upgrade-download/` (mode 0700) holds exactly the
verified tarball, its published `.sha256`, and a fresh copy of the published
`install.sh` asset — the installer never extracts or executes anything there
(extraction is the upgrade engine's job). A previous interrupted delegation
is handled by re-verifying the existing tarball against the published
`.sha256` and reusing it; a failed re-verification fails closed. The version
comparison is a small embedded python3 helper (deterministic, unit-tested by
`tests/installer-negative.sh`) because semver-with-prerelease comparison in
POSIX shell is error-prone.

## 13. Tests (in order)

1. Domain/unit: version parse/compare/fence; state machine transitions +
   persistence; backup manifest; failure-policy decisions; doctor check
   logic; JSON output shape. Add these FIRST (fail-before/fix-after).
2. Portable process tests (`tests/upgrade-process.sh`): a local fake release
   endpoint serving signed-shape tarballs; real `o3k upgrade` binary against a
   disposable state root with sandbox overrides (same pattern as
   `tests/doctor-process.sh`); every failure-matrix case (§14) driven through
   the real binary with tampered fixtures.
3. Component real-host (Ubuntu first): install v0.3.0-alpha.1 from the public
   installer, build a TestLab, upgrade with the new bundle via the state
   machine, verify identity preservation + doctor + reboot recovery.
4. Full dual-distro real release acceptance (§15) — the hard gate.

## 14. Failure matrix (portable tests; same list re-driven on real hosts where
        practical)

unsupported upgrade path · requested downgrade · bad version · missing release
asset · checksum failure · corrupt archive · insufficient backup disk space ·
corrupt current SQLite · corrupt backup · interrupted backup · interrupted
binary switch · migration failure · service startup failure · doctor failure ·
invalid ownership manifest · missing previous release artifact · concurrent
upgrade invocation · upgrade while previous failed transaction exists ·
rollback with no valid backup · rollback with tampered backup · second
rollback · repeated upgrade to same version.

## 15. Real release acceptance (Definition of Done)

Both Ubuntu 24.04 and Debian 12, using REAL published artifacts:

previous released version installed via its public installer
(actual latest at implementation time; the upgrade command ships in the next
release, so the real journey is v0.3.0-alpha.1 → v0.4.0-alpha.1)
→ working TestLab (image/network/port/flavor/keypair/test-vm ACTIVE,
console marker, recorded server UUID, libvirt domain UUID, fixed IP,
Placement allocation, credential/TLS fingerprints without secrets)
→ `sudo o3k upgrade` (via the verified new release bundle)
→ same server UUID/IP/domain/provider identity, credentials work, TLS
identity preserved, doctor healthy, lifecycle works, Placement consistent
→ host reboot, verify recovery
→ `sudo o3k rollback` → previous version restored, DB valid, resources
reconciled, doctor healthy, public API works, no duplicate domain/resource,
foreign state unchanged
→ upgrade again (prove upgrade-after-rollback)
→ cleanup: zero O3K-owned residue, foreign canaries/links/dnsmasq unchanged.

The foreign-state/leak verifier runs after every upgrade and rollback.

## 16. Security invariants

foreign libvirt canary unchanged · foreign links unchanged · foreign dnsmasq
unchanged · no privilege broadening (o3kd still cannot manage libvirt) ·
service identities unchanged · TLS keys never printed · credentials never
printed · backups private (0700/0600) · no secrets in evidence/logs.

## 17. Release/publish

The upgrade ships in the next release (v0.4.0-alpha.1): workspace bump,
channels pin, release doc, full release gate (e2e + clean installs +
upgrade/rollback artifacts + benchmark + human review) at the final SHA, then
the signed tag, GitHub release assets, get.o3k.io redirect approval, and the
final report on #626 with the exact verdict.

## Final verdict format

`DONE — O3K upgrade/rollback proven on supported installations`
or `NOT DONE — <precise remaining blocker>`.
