# ADR-0064 — Require benchmark evidence for release readiness

Status: Accepted

## Context

The release gate validated benchmark artifacts when supplied, but classified
the benchmark input as optional. That allowed a release report to become ready
without the measurement required by the v0.2.0-alpha.1 definition of done.

## Decision

Treat the benchmark artifact as a required release-gate input. Missing,
skipped, malformed, or failed benchmark evidence keeps the gate blocked.

## Consequences

The gate now matches the release tracker and release documentation. Existing
local measurement runs may still be useful evidence, but readiness requires a
measured benchmark artifact with all declared targets passing.
