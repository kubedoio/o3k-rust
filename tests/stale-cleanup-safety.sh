#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-stale-cleanup.XXXXXX")"
trap 'rm -rf -- "$WORK_DIR"' EXIT

mkdir -p "$WORK_DIR/bin" "$WORK_DIR/runner/o3k-testlab"
cat >"$WORK_DIR/bin/virsh" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
case "${1:-}" in
  -c)
    if [[ "${3:-}" == list ]]; then
      printf 'o3k-foreign-canary\n'
    else
      printf 'foreign domain was targeted\n' >&2
      exit 1
    fi
    ;;
  *) printf 'foreign domain was targeted\n' >&2; exit 1 ;;
esac
EOF
cat >"$WORK_DIR/bin/ip" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
if [[ "${1:-}" == -o && "${2:-}" == link && "${3:-}" == show ]]; then
  printf '9: o3k-foreign-canary: <BROADCAST>\n'
else
  printf 'foreign link was targeted\n' >&2
  exit 1
fi
EOF
chmod +x "$WORK_DIR/bin/virsh" "$WORK_DIR/bin/ip"

PATH="$WORK_DIR/bin:/usr/bin:/bin" RUNNER_TEMP="$WORK_DIR/runner" \
  GITHUB_RUN_ID=991991 O3K_TESTLAB_STATE_BASE="$WORK_DIR/runner/o3k-testlab" \
  bash "$ROOT_DIR/scripts/cleanup-stale-testlab-processes.sh"

echo "stale cleanup foreign-name safety test passed"
