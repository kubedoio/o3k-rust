# ADR-0102 — Fence protected runner capability artifacts to one workflow attempt

## Status

Accepted

## Context

Issue #76's capability probe ran on a persistent self-hosted workspace. The
pre-run guard previously accepted any readable redacted artifact whose status
was `passed`. A failed or interrupted probe could therefore leave, or reuse, a
passing artifact from an earlier workflow attempt. Direct JSON replacement
also allowed an interrupted write to leave a partial artifact.

## Decision

The protected workflow passes its run ID, run attempt, and source commit to the
capability probe and pre-run guard. The probe records those values in the
redacted artifact and publishes it through a flushed temporary file and atomic
rename. It removes the previous output path before probing so a probe startup
failure cannot preserve an old result. The guard requires the exact artifact
type, schema version, redacted marker, integer completion timestamp, and any
workflow identity values supplied by the protected workflow. A missing,
malformed, stale, or mismatched artifact remains unavailable and blocks the
host-mutating lifecycle step.

Portable tests may omit workflow identity values because they do not execute
inside GitHub Actions; the protected workflow always supplies them.

## Consequences

Persistent runner workspaces cannot reuse capability evidence across workflow
attempts or source revisions. Atomic publication prevents partial JSON from
being mistaken for evidence. The identity fields are provenance metadata, not
a cryptographic attestation; host acceptance still requires the real protected
workflow run and human inspection.

## Alternatives rejected

- Relying on `actions/checkout` cleanup was rejected because generated files
  under `target/` can survive on self-hosted runners.
- Checking only `finished_at` was rejected because clocks and timestamp
  resolution do not identify a workflow attempt.
- Signing the artifact was rejected as out of scope for this repository-side
  runner fence; it would not replace host isolation or the protected workflow.

