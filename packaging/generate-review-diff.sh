#!/usr/bin/env bash
set -Eeuo pipefail

BASE=
CANDIDATE=HEAD
OUTPUT=
while (($#)); do
  case "$1" in
    --base) BASE="${2:?missing base commit}"; shift 2;;
    --candidate) CANDIDATE="${2:?missing candidate commit}"; shift 2;;
    --output) OUTPUT="${2:?missing output directory}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
if [[ -z "$BASE" || -z "$OUTPUT" ]]; then
  echo "usage: $0 --base COMMIT [--candidate COMMIT] --output DIRECTORY" >&2
  exit 2
fi
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
git -C "$ROOT_DIR" rev-parse --verify "${BASE}^{commit}" >/dev/null
git -C "$ROOT_DIR" rev-parse --verify "${CANDIDATE}^{commit}" >/dev/null
BASE_SHA="$(git -C "$ROOT_DIR" rev-parse "${BASE}^{commit}")"
CANDIDATE_SHA="$(git -C "$ROOT_DIR" rev-parse "${CANDIDATE}^{commit}")"
mkdir -p -- "$OUTPUT"
COUNT="$(git -C "$ROOT_DIR" rev-list --count "${BASE_SHA}..${CANDIDATE_SHA}")"
FIRST_PARENT_COUNT="$(git -C "$ROOT_DIR" rev-list --first-parent --count "${BASE_SHA}..${CANDIDATE_SHA}")"
STAT="$(git -C "$ROOT_DIR" diff --stat --summary "${BASE_SHA}..${CANDIDATE_SHA}")"
{
  printf '# Candidate diff summary\n\n'
  printf -- '- Base commit: `%s`\n' "$BASE_SHA"
  printf -- '- Candidate commit: `%s`\n' "$CANDIDATE_SHA"
  printf -- '- Commits in range: **%s** (first-parent: **%s**)\n\n' "$COUNT" "$FIRST_PARENT_COUNT"
  printf 'The counts and statistics below were generated from Git; do not edit them by hand.\n\n'
  printf '## Diff statistics\n\n```text\n%s\n```\n' "$STAT"
} >"$OUTPUT/candidate-diff-summary.md"
echo "generated $OUTPUT/candidate-diff-summary.md for $CANDIDATE_SHA"
