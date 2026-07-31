# ADR-0101 — Bind release readiness to approved human review

## Status

Accepted for the repository-side implementation of issue #93. This ADR does
not assert that the alpha release is ready or that a human review exists.

## Context

Issue #92 defines a fail-closed `human-review.json` schema and validator, but
the release gate accepted its other evidence inputs without consuming that
artifact. A candidate could therefore produce a machine-readable `ready`
report while omitting the required independent architecture/security decision.
The review also has to apply to the exact source commit being released.

## Decision

`packaging/release-gate.sh` now requires `--human-review` and
`--source-commit`. It invokes the bundled
`validate-human-review.sh --require-approved` validator and rejects a review
whose `reviewed_commit` differs from the supplied source commit. The resulting
report records only the review artifact path, status, and reviewed commit; it
does not synthesize approval or verify a person's identity or judgment.

The release bundle includes the validator so the same fail-closed check is
available when the gate is run from packaged files. The source commit remains
an explicit operator input rather than an implicit working-tree lookup, which
keeps detached bundles and reproducible release procedures unambiguous.

## Alternatives rejected

- Treating the presence of a review file as approval would bypass the existing
  schema validator and allow pending or rejected decisions.
- Deriving the commit from a local Git checkout would fail for detached release
  bundles and could silently validate the wrong source.
- Generating a review artifact in the gate would fabricate human evidence.

## Verification

`tests/release-gate.sh` covers a ready report with an approved review, missing
review evidence, and a review bound to a different commit. The packaging
bundle test verifies that the validator is shipped with the release gate.

## Non-goals

This change does not provide real-host evidence, authenticate a reviewer,
create signed tags, publish artifacts, or change the compatibility matrix.
