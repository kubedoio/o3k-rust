# ADR-0103 — Retain protected real-host evidence for a bounded period

Status: Accepted for the repository-side implementation of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

#77. This ADR does
not claim that a protected workflow has run successfully.

## Context

The protected real-host workflow always uploads its redacted evidence,
including failure artifacts, but previously relied on GitHub's unspecified
default artifact retention. That made the audit window implicit and could
cause evidence to disappear before release review.

## Decision

Set `actions/upload-artifact` retention explicitly to 14 days for the complete
`real-host-workflow-artifacts` bundle. The workflow remains manually
dispatchable, protected, serialized, and fail-closed; retention does not turn
a skipped or failed artifact into acceptance evidence.

## Verification

`tests/real-host-workflow-guards.sh` requires the explicit retention setting,
and the workflow still uploads artifacts under `if: always()`.

## Non-goals

This does not provide a runner, execute privileged workflows, validate host
capabilities, or close issue #77.
