# Maintainability Program

This directory contains the Issue #758 maintainability evidence and the
permanent architecture policy established after the structural refactoring.
The current-tree inventory is also the structural monitoring report for the
completed R7 program.
The immutable P13.4 baseline remains historical evidence for dependency-cycle
regression; it is not an ongoing permission to place SQL or host execution in
arbitrary modules.

## Contents

| File | Description |
|------|-------------|
| `README.md` | This file |
| `architecture-baseline.md` | Current snapshot of architecture metrics (not the immutable baseline) |
| `architecture-roles.md` | Current architectural role classification for every crate and binary |
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

The generated report includes production LOC by responsibility, application
and composition roots, hotspots, SQL ownership, host-execution ownership, and
dependency cycles. These are review signals, not arbitrary file-size or LOC
limits. The machine-readable responsibility report is written to
`target/generated-maintainability/responsibility-inventory.json`.

## Immutable historical baseline

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
architecture review and a deliberate exact path or boundary addition. First
identify the persistence, diagnostic, or upgrade responsibility and record its
ownership in the relevant issue or ADR. Then add the narrowest path-aware guard
entry as part of that reviewed change; do not append a path merely to make CI
green.

The exact-file allowance for `crates/o3k-image/src/execution.rs` owns the
sandboxed `run_qemu_img` qemu-img invocation and its directly related process
safety. `crates/o3k-image/src/lib.rs` is intentionally not approved for host
execution. A future Image execution change must remain in this explicit
adapter boundary rather than broadening the allowance.

### Host-execution ownership

Host subprocess execution is allowed only in explicit execution-adapter files
or adapter crates. `o3kd` composition, Compute `main.rs` and `runtime.rs`,
and canonical Network modules are host-command-free; Linux Network realization
stays under `o3k-network/src/linux_fabric/`. Shell wrapping is not an approved
execution mechanism. A new host tool requires architecture review answering:
“Is this path itself an explicit persistence or execution boundary?” If not,
move the implementation to the proper boundary rather than extending an
allowlist.

The same review is required for a new host-execution adapter: identify the
provider or execution responsibility, keep canonical/application code free of
host mutation, and approve only the exact adapter path or cohesive execution
directory.

## Final ownership map

- `o3kd`: control-plane composition root; `composition/` owns service wiring,
  storage/network/compute adapters, controller setup, and runtime helpers.
- `o3k-compute`: Compute application/service; `bins/o3k-compute` owns host
  runtime composition and lifecycle adapters.
- `o3k-network`: canonical Network application/service; `linux_fabric/` owns
  Linux realization and host mutation; `bins/o3k-network` owns process/runtime
  composition.
- `o3k-store`: domain vocabulary and repository ports plus explicit
  `sqlite/` and `postgres/` persistence implementations; `unified/` owns
  backend dispatch and remains SQL-free.
- `o3k-image`: Image application/cache service; `execution.rs` owns the
  bounded qemu-img execution adapter.

The generated inventory identifies larger cohesive modules for review. A large
module is not a structural violation by itself; a new split requires a real
ownership or change-isolation problem.

The focused R2b, Network, and Compute boundary scripts provide local evidence;
the permanent Python guard is the authoritative repository-wide policy and is
run by the protected Rust CI check.

## Historical baseline context

1. **No runtime behavior changes.** This phase is measurement and classification only.
2. **No code moves.** Production code stays in its current crate/file.
3. **No interface redesigns.**
4. **No SQL modification.**
5. **No API modification.**
6. **No provider behavior modification.**

The R0 baseline was measurement-only. The current architecture policy above is
the authoritative guidance for new work after the #758 convergence program.
