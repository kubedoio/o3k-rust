# ADR-0130 — Fence release version input before packaging cleanup

## Status

Accepted for the repository-side implementation of issue #93. This does not
claim that a release is ready or that any artifact has been published.

## Decision

`packaging/make-release.sh` accepts only numeric release versions with one or
two dots and an optional dot-separated alphanumeric prerelease suffix, such as
`0.2.0-alpha.1` and the repository's `0.0-dirty-test` test value. Invalid
values are rejected before Cargo is invoked or the output directory is
constructed and recursively removed.

## Rationale

The version is part of the output path and manifest JSON. Validating it before
any build or cleanup prevents path traversal, shell/JSON control characters,
and partial packaging from unsafe input. The output remains under `dist/` for
all accepted values.

## Verification

`tests/packaging-bundle.sh` verifies that traversal, slash, whitespace, and
quote-containing values are rejected before the fake Cargo command runs and
that an external sentinel is unchanged. Existing dirty-tree and bundle
installer checks remain covered.
