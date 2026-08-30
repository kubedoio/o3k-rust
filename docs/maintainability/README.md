# Maintainability Program

This directory contains the Issue #758 maintainability evidence and the
permanent architecture policy established after the structural refactoring.
The immutable P13.4 baseline remains historical evidence for dependency-cycle
regression; it is not an ongoing permission to place SQL or host execution in
arbitrary modules.

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

## Permanent architecture policy

The executable policy is `scripts/check-maintainability-guards.py`, and its
regression matrix is `tests/maintainability-guards.sh`. A newly introduced
path is not approved merely because it appeared in the P13.4 baseline.

### SQL ownership

Production SQL is allowed only in explicit persistence implementations and
named database diagnostic/upgrade tooling. In the store, this means the
`sqlite/`, `postgres/`, and currently accepted specialized persistence
modules. `domain/`, `port/`, `unified/`, `o3kd/src/composition/`, and service
application code remain SQL-free. A new SQL-owning adapter requires
architecture review and a deliberate exact path or boundary addition.

The current exact-file allowance for `crates/o3k-image/src/lib.rs` is a
residual exception for the sandboxed `run_qemu_img` qemu-img invocation. It is
kept exact because that execution still lives in the existing mixed module;
the desired follow-up is to extract it into an explicit Image execution
adapter. This exception must not be broadened.

### Host-execution ownership

Host subprocess execution is allowed only in explicit execution-adapter files
or adapter crates. `o3kd` composition, Compute `main.rs` and `runtime.rs`,
and canonical Network modules are host-command-free; Linux Network realization
stays under `o3k-network/src/linux_fabric/`. Shell wrapping is not an approved
execution mechanism. A new host tool requires architecture review answering:
“Is this path itself an explicit persistence or execution boundary?” If not,
move the implementation to the proper boundary rather than extending an
allowlist.

The focused R2b, Network, and Compute boundary scripts provide local evidence;
the permanent Python guard is the authoritative repository-wide policy and is
run by the protected Rust CI check.

## R0 historical rules

1. **No runtime behavior changes.** This phase is measurement and classification only.
2. **No code moves.** Production code stays in its current crate/file.
3. **No interface redesigns.**
4. **No SQL modification.**
5. **No API modification.**
6. **No provider behavior modification.**

All structural refactoring begins in R1.
