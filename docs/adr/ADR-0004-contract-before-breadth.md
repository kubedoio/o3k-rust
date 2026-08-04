# ADR-0004 — Contract evidence before endpoint breadth

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, governance

## Decision

An API operation is considered supported only when its intended contract, compatibility source, executable tests, and known deviations are committed.

## Consequences

- route count is not a release metric;
- unsupported extensions are not advertised;
- compatibility matrix is generated from evidence, not implementation claims.
