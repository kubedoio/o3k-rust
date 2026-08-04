# ADR-0041 — Emit bounded console observations from compute agents

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, identity, governance

## Context

The compute-agent protocol already defines bounded console-log commands and
observation fields, but the stream discarded console data because executors
could only return lifecycle results. The fake executor consequently rejected
console reads, leaving the protocol path untestable.

## Decision

Extend the executor result with an optional bounded console payload. When
present, the authenticated agent stream emits the existing `Observation`
message with offset, completion, and truncation metadata. The fake executor
stores a bounded boot-log fixture and honors offset/max-bytes requests. No
protobuf change is needed.

## Consequences

Agent-side framing, bounds, and observation emission are now testable. The
control-plane API still needs routing and observation persistence, and the
production libvirt executor still needs a real serial-console source; those
remain explicit follow-up work.
