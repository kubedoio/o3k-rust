# ADR-0123 — Keep the protobuf compatibility baseline off the checkout branch

Status: Accepted

## Context

The CI checkout is already on the pull-request or `main` commit. Fetching the
protobuf compatibility baseline into `refs/heads/main` therefore fails on
GitHub-hosted runners because that branch is checked out in the worktree. The
failure prevented the required Rust job from reaching the compatibility check.

## Decision

Fetch the baseline into the remote-tracking ref `refs/remotes/origin/main` and
point Buf at `origin/main`. This preserves the comparison against the current
default branch without mutating or colliding with the checked-out local branch.
A portable CI workflow contract test prevents regression to the conflicting
ref.

## Consequences

The protobuf compatibility check can run on both pull-request and `main` CI
checkouts. This fixes CI execution only; it does not claim any real-host or
release evidence.
