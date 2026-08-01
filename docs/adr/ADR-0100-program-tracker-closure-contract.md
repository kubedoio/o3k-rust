# ADR-0100 — Keep the program tracker fail closed

## Status

Accepted for the repository-side preparation of issue #94. This ADR does not
close the program or issue #93.

## Context

The release tracker recorded useful prose, but it had no machine-checked owner
for the closure decision and no guard against accidentally changing pending
program state into a release or closure claim. The release gate and release
publication belong to issue #93 and must remain separate from tracker
bookkeeping.

## Decision

Add a small, source-controlled tracker contract with explicit ownership
(`owner_issue: 94`), release-gate ownership (`release_issue: 93`), and
fail-closed `blocked`/`pending` state. Validate the contract in CI, require
an explicit row for every issue in #94's closure chain, require each such row
to carry exactly `closure evidence: pending`, and require the decision log and
evidence-closure sections to remain present. Every closure row also rejects
positive `release-ready`, `published`, `signed`, or program-closure claims.
PR #85 is a
pre-existing prerequisite in #94, not a closure issue, so it is intentionally
not part of that row set. The validator checks documentation shape and
non-claiming state only; it does not inspect host artifacts, verify a human
reviewer, verify signatures, or publish a release.

## Alternatives rejected

- Extending `packaging/release-gate.sh` would mix #94 bookkeeping with #93
  release acceptance and could change the release contract.
- Treating a merged PR or a green CI run as program closure would contradict
  the tracker’s real-host and human-review requirements.
- Relying on prose review alone would allow accidental positive status claims
  without a failing check.

## Verification

`tests/program-tracker.sh` validates the current tracker, rejects a `ready`
program state or an explicit closure claim, rejects positive claims in any
closure-chain row, rejects removal of a required closure-chain row, and rejects
a non-pending closure-evidence marker on any required row. The check is
documentation validation only and intentionally leaves host, human, signing,
and publication evidence pending.
