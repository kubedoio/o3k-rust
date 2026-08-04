# ADR-0079 — Enforce release-evidence freshness

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, governance

## Context

The release gate checked only that each artifact's `finished_at` field was an
integer. A negative timestamp or an arbitrarily old artifact could therefore
contribute to a `ready` report. A future-dated artifact could also pass, which
would make the evidence timeline untrustworthy.

## Decision

The gate captures one current Unix epoch timestamp per invocation and applies
these checks to every artifact:

- `finished_at` must be a positive integer;
- `finished_at` must not be later than the gate timestamp;
- `finished_at` must be no older than 604800 seconds (seven days) by default.

`O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS` may override the maximum age for a
controlled invocation, but only with a positive integer. The default remains
the release policy and release workflows should leave the override unset.
Using one gate timestamp avoids inconsistent results when files are checked
across a second boundary. The exact maximum-age boundary is accepted.

## Consequences

Stale, invalid, and future-dated artifacts now block release readiness instead
of being treated as valid evidence. Local and CI tests can use a short,
explicit maximum age without sleeping or depending on a fixed wall-clock date.
Operators remain responsible for not weakening the default policy merely to
make old evidence pass.

## Non-goals

This decision does not establish trusted time, replace artifact signing, or
prove that an artifact was produced by the claimed host. Those remain release
evidence and provenance requirements.
