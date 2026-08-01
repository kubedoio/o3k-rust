#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOTSTRAP="$ROOT_DIR/scripts/bootstrap-disposable-testlab.sh"
CLEANUP="$ROOT_DIR/scripts/cleanup-disposable-testlab.sh"

python3 - "$BOOTSTRAP" "$ROOT_DIR/.github/workflows/real-host-validation.yml" <<'PY'
from pathlib import Path
import sys

bootstrap = Path(sys.argv[1]).read_text(encoding="utf-8")
workflow = Path(sys.argv[2]).read_text(encoding="utf-8")
assert 'PASSWORD="$(openssl rand -hex 32)"' in bootstrap
assert 'echo "::add-mask::${PASSWORD}"' in bootstrap
assert bootstrap.index('echo "::add-mask::${PASSWORD}"') < bootstrap.index('O3K_BOOTSTRAP_PASSWORD=')
assert 'OS_PASSWORD=%s\\n' in bootstrap
assert 'OS_PROJECT_NAME=admin' in bootstrap
assert 'OS_PASSWORD:' not in workflow
assert workflow.count('scripts/bootstrap-disposable-testlab.sh') == 1
assert workflow.count('scripts/cleanup-disposable-testlab.sh') == 1
PY

tmp_root="$(mktemp -d /tmp/o3k-disposable-bootstrap-test.XXXXXX)"
trap 'rm -rf -- "$tmp_root"' EXIT
chmod 0755 "$tmp_root"
run_id=local-991
state="$tmp_root/o3k-testlab/$run_id"
pid_root="$tmp_root/o3k-testlab-pids/$run_id"
venv="$tmp_root/o3k-openstack-venv.test"
image="$tmp_root/cirros-0.6.3-x86_64-disk.img.test"
mkdir -p "$state" "$pid_root" "$venv"
mkdir -p "$state/bin"
cp /bin/sleep "$state/bin/o3kd"
printf 'o3k-disposable-testlab-v1\ncommit=test\nrun=%s\n' "$run_id" >"$state/.o3k-run-owned"
"$state/bin/o3kd" 60 & service_pid=$!
printf '%s\n' "$service_pid" >"$pid_root/o3kd.pid"
touch "$image"

RUNNER_TEMP="$tmp_root" GITHUB_RUN_ID="$run_id" \
  O3K_TESTLAB_STATE_ROOT="$state" O3K_TESTLAB_PID_ROOT="$pid_root" \
  O3K_OPENSTACK_VENV="$venv" O3K_TESTLAB_IMAGE_PATH="$image" \
  bash "$CLEANUP"

[[ ! -e "$state" && ! -e "$pid_root" && ! -e "$venv" && ! -e "$image" ]]
! kill -0 "$service_pid" 2>/dev/null

mkdir -p "$state"
printf 'foreign-state\n' >"$state/.o3k-run-owned"
if RUNNER_TEMP="$tmp_root" GITHUB_RUN_ID="$run_id" O3K_TESTLAB_STATE_ROOT="$state" \
  O3K_TESTLAB_PID_ROOT="$pid_root" bash "$CLEANUP" 2>/dev/null; then
  echo "cleanup accepted a foreign state marker" >&2
  exit 1
fi
[[ -e "$state/.o3k-run-owned" ]]

echo "disposable TestLab bootstrap contract tests passed"
