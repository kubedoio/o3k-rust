# ADR-0128 — Validate scripts used by protected workflows in normal CI

Status: Accepted

## Context

The normal Rust CI job checked shell syntax only for packaging and test
directories. The protected real-host workflow also executes shell and Python
helpers under `scripts/`, so syntax errors there could reach the host-gated
workflow without a portable CI failure.

## Decision

Normal CI runs `bash -n` over `scripts/*.sh` in addition to packaging and test
helpers, and runs `python3 -m compileall -q scripts` for the Python helpers.
The CI workflow contract test keeps these checks present.

## Consequences

Portable CI catches basic script breakage before a protected host is consumed.
This validates syntax and bytecode compilation only; it does not replace the
real-host workflow or claim host acceptance.
