# ADR-0122 — Require a clean source tree before release packaging

## Status

Accepted for the repository-side preparation of issue #93. This does not claim
that release evidence, human approval, or publication exists.

## Context

`packaging/make-release.sh` recorded `HEAD` as the bundle source commit but
could package modified or untracked source files. The resulting manifest could
therefore identify a commit that did not describe the shipped contents.

## Decision

Fail before building whenever the repository has staged, unstaged, or
untracked changes. Release packaging is permitted only from a clean checkout,
so the manifest source commit is provenance for the exact source used to build.

## Consequences

Operators must commit or remove local changes before packaging. This is a
provenance guard only; signed tags, reproducible builds, publication, and
independent human approval remain separate release requirements.
