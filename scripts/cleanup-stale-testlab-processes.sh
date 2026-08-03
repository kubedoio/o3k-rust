#!/usr/bin/env bash
set -Eeuo pipefail

# Remove only prior disposable TestLab runs that carry the O3K ownership
# marker. The per-run cleanup script remains the authority for process
# identity, state ownership, and bounded shutdown.
RUN_ID="${GITHUB_RUN_ID:-}"
RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
STATE_BASE="${O3K_TESTLAB_STATE_BASE:-/var/lib/o3k-testlab}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLEANUP_SCRIPT="${ROOT_DIR}/scripts/cleanup-disposable-testlab.sh"

[[ "$RUN_ID" =~ ^[0-9]+$|^local-[0-9]+$ ]] || {
  echo "stale cleanup: invalid current run id" >&2
  exit 1
}
[[ "$RUNNER_TEMP" == /* && "$RUNNER_TEMP" != *..* && -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] || {
  echo "stale cleanup: runner temp path is unsafe" >&2
  exit 1
}
[[ "$STATE_BASE" == /* && "$STATE_BASE" != *..* && ! -L "$STATE_BASE" ]] || {
  echo "stale cleanup: state base path is unsafe" >&2
  exit 1
}
[[ -x "$CLEANUP_SCRIPT" ]] || {
  echo "stale cleanup: per-run cleanup script is unavailable" >&2
  exit 1
}

if [[ ! -d "$STATE_BASE" ]]; then
  echo "stale cleanup: no disposable state base exists"
  exit 0
fi

for state_root in "$STATE_BASE"/*; do
  [[ -d "$state_root" && ! -L "$state_root" ]] || continue
  stale_run_id="${state_root##*/}"
  [[ "$stale_run_id" =~ ^[0-9]+$|^local-[0-9]+$ ]] || continue
  [[ "$stale_run_id" != "$RUN_ID" ]] || continue
  marker="$state_root/.o3k-run-owned"
  [[ -f "$marker" && ! -L "$marker" ]] || continue
  sudo -n grep -Fqx 'o3k-disposable-testlab-v1' "$marker" || continue
  sudo -n grep -Fqx "run=$stale_run_id" "$marker" || {
    echo "stale cleanup: refusing ambiguous run marker ${stale_run_id}" >&2
    exit 1
  }

  echo "stale cleanup: removing prior owned run ${stale_run_id}"
  RUNNER_TEMP="$RUNNER_TEMP" GITHUB_RUN_ID="$stale_run_id" \
    O3K_TESTLAB_STATE_BASE="$STATE_BASE" \
    bash "$CLEANUP_SCRIPT"
done

echo "stale cleanup: complete"
