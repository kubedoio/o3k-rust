# ADR-0061 — Finalize measurement cleanup status on process exit

Status: Accepted for the measurement and release-gate boundary.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: cli, governance

## Context

The measurement harness produced a measured benchmark artifact while marking
cleanup as `not_measured`. The release gate correctly requires passed cleanup
for every supplied artifact, so a normal successful measurement could never be
accepted.

## Decision

The harness writes a temporary `cleanup: pending` value and finalizes it in the
exit trap after stopping the owned `o3kd` process and removing its temporary
data directory. Measured runs become `cleanup: passed`; skipped runs retain
their explicit skipped/not-run semantics.

## Consequences

Benchmark artifacts now carry evidence that cleanup completed before the gate
evaluates them. An interrupted or failed measurement still cannot be mistaken
for a clean measured run.
