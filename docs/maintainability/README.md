# Maintainability Program

This directory contains the results of the Issue #758 repository maintainability
program (R0 baseline). The goal is to establish a reproducible, machine-generated
architecture and maintainability baseline **before** any structural refactoring
begins.

## Contents

| File | Description |
|------|-------------|
| `README.md` | This file |
| `architecture-baseline.md` | Snapshot of key architecture metrics |
| `architecture-roles.md` | Architectural role classification for every crate |
| `refactor-contract.md` | Checklist for verifying refactoring PRs preserve behavior |

## Generated Outputs

The inventory script in `scripts/maintainability-inventory.py` produces
deterministic machine-readable output under:

```
target/generated-maintainability/
    baseline.json
    dependencies.json
    sql-inventory.json
    host-command-inventory.json
    safety-inventory.json
    architecture-roles.json
    hotspots.json
    summary.md
```

## Regenerating

```bash
python3 scripts/maintainability-inventory.py
```

The script is idempotent and deterministic for the same git SHA.

## Baseline

- **SHA**: `edb464ec43b1d12207faae903ea14b7824123f39`
- **Branch**: `r0-maintainability-baseline`
- **Date**: 2026-08-28

## R0 Rules

1. **No runtime behavior changes.** This phase is measurement and classification only.
2. **No code moves.** Production code stays in its current crate/file.
3. **No interface redesigns.**
4. **No SQL modification.**
5. **No API modification.**
6. **No provider behavior modification.**

All structural refactoring begins in R1.
