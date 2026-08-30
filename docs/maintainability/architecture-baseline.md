# Current Architecture Snapshot

Generated from workspace inventory at `343452bb68fea5b8a9e4126ac9888af2f426522c`.

This is a current-tree monitoring snapshot, not the immutable P13.4 baseline.
That historical evidence remains under
`docs/maintainability/baselines/p13-4/` and is never regenerated here.

## Snapshot

- 278 Rust source files across 31 workspace crates
- ~105,442 production LOC, ~60,314 test LOC
- 100 candidate hotspot files identified

## Crate Roles

See `architecture-roles.md` for the full classification.

## Key Numbers

- SQL usage sites: 928 (0 unexplained)
- Host command execution sites: 417 production
- Dependency cycles: 0
- Production safety occurrences: 182

## Production LOC by Responsibility

| Responsibility | Files | Production LOC (approx) |
|---|---:|---:|
| Compute application | 13 | 4,803 |
| Compute host/runtime composition | 8 | 2,619 |
| Image application | 1 | 1,371 |
| Image execution | 1 | 223 |
| Network Linux execution | 18 | 6,765 |
| Network application | 27 | 9,272 |
| Network execution/runtime composition | 2 | 742 |
| PostgreSQL persistence | 11 | 6,761 |
| SQLite persistence | 9 | 7,322 |
| o3kd control-plane composition | 6 | 2,199 |
| o3kd native adapters | 10 | 2,038 |
| o3kd process entry | 5 | 34 |
| other workspace responsibility | 131 | 48,421 |
| store domain | 4 | 1,067 |
| store ports | 3 | 856 |
| store specialized persistence | 29 | 10,949 |


## Monitoring interpretation

The inventory reports hotspots, application roots, composition roots, SQL and
host-execution ownership, and dependency cycles for review. It does not enforce
arbitrary file-size or LOC thresholds.

## Integrity

Run `scripts/maintainability-inventory.py` to refresh this current snapshot.
