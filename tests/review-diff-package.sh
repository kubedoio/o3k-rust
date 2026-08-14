#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-review-diff.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

BASE="$(git -C "$ROOT_DIR" rev-list --max-parents=0 HEAD | tail -1)"
bash "$ROOT_DIR/packaging/generate-review-diff.sh" \
  --base "$BASE" --candidate HEAD --output "$WORK_DIR"
SUMMARY="$WORK_DIR/candidate-diff-summary.md"
[[ -s "$SUMMARY" ]]
grep -Fq -- "Base commit: \`$BASE\`" "$SUMMARY"
CANDIDATE="$(git -C "$ROOT_DIR" rev-parse HEAD)"
grep -Fq -- "Candidate commit: \`$CANDIDATE\`" "$SUMMARY"
EXPECTED="$(git -C "$ROOT_DIR" rev-list --count "$BASE..$CANDIDATE")"
grep -Fq -- "Commits in range: **$EXPECTED**" "$SUMMARY"

echo "review diff package tests passed"
