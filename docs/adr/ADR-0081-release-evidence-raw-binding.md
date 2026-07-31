# ADR-0081 — Bind release benchmark summaries to raw evidence

## Context

The release gate accepted a measured benchmark summary without requiring the
raw measurements or proving that the summary was derived from the submitted
raw document. A later edit could therefore change samples or target outcomes
without changing the summary's release status.

## Decision

`packaging/release-gate.sh` requires a `--benchmark-raw` JSON input alongside
`--benchmark`. The raw artifact must be a measured libvirt benchmark with
redacted, non-empty `environment.uname` and `environment.rustc` metadata,
positive samples, measured guest/libvirt coverage, and declared target
thresholds. The gate requires the summary's `artifact_type`, `status`,
`profile`, `samples`, `control_plane`, and `guest_and_libvirt` fields to match
the raw artifact.

The summary carries `raw_sha256`, the lowercase SHA-256 digest of the raw
artifact's canonical JSON (sorted keys, compact separators, UTF-8, escaped
non-ASCII characters). This is a content binding, not a path or filesystem
timestamp binding. Invalid, missing, or mismatched raw evidence blocks the
gate with an explicit error. Existing non-benchmark artifact checks and CLI
options remain unchanged.

This change is limited to release-gate validation and its evidence contract;
measurement harness production of the new binding is a separate follow-up.

## Consequences

Release readiness now requires reviewers to retain both the evaluated summary
and the exact raw benchmark document used to derive it. Raw benchmark files
from the current harness need the producer-side binding follow-up before they
can be used as release evidence. The gate remains portable because hashing is
implemented with Python's standard library and does not add a dependency.

## Non-goals

This decision does not sign artifacts, establish trusted time, validate host
identity, or change benchmark thresholds.
