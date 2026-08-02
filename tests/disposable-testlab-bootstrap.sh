#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOTSTRAP="$ROOT_DIR/scripts/bootstrap-disposable-testlab.sh"
CLEANUP="$ROOT_DIR/scripts/cleanup-disposable-testlab.sh"

python3 - "$BOOTSTRAP" "$CLEANUP" "$ROOT_DIR/.github/workflows/real-host-validation.yml" <<'PY'
from pathlib import Path
import sys

bootstrap = Path(sys.argv[1]).read_text(encoding="utf-8")
cleanup = Path(sys.argv[2]).read_text(encoding="utf-8")
workflow = Path(sys.argv[3]).read_text(encoding="utf-8")
assert 'scripts/generate-passwords.sh' in bootstrap
assert 'echo "::add-mask::${PASSWORD}"' in bootstrap
assert bootstrap.rindex('scripts/generate-passwords.sh') < bootstrap.rindex('echo "::add-mask::${PASSWORD}"')
assert 'OS_PASSWORD=%s\\n' in bootstrap
assert 'OS_PROJECT_NAME=admin' in bootstrap
assert '--no-create-home' in bootstrap
assert 'o3k-disposable-account-v1' in bootstrap
assert 'protobuf-compiler' in bootstrap
assert 'SERVICE_STATE_BASE=/var/lib/o3k-testlab' in bootstrap
assert 'SupplementaryGroups' not in bootstrap
assert '.o3k-supplementary-groups-added' in bootstrap
assert 'usermod --append --groups' in bootstrap
assert 'gpasswd --delete o3k' in bootstrap
assert '/readyz' in bootstrap
assert 'GITHUB_PATH' in bootstrap
assert 'O3K_REAL_HOST_PROTECTED_PATHS=%s\\nO3K_REAL_HOST_INVENTORY_ROOT=%s' in bootstrap
assert 'userdel o3k' in cleanup
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
printf 'o3k-disposable-venv-v1\n' >"$venv/.o3k-venv-owned"
mkdir -p "$state/bin"
cp /bin/sleep "$state/bin/o3kd"
printf 'o3k-disposable-testlab-v1\ncommit=test\nrun=%s\n' "$run_id" >"$state/.o3k-run-owned"
printf 'o3k-owned-v1 path=%s\n' "$state" >"$state/.o3k-owned"
sudo chown -R o3k:o3k "$state"
sudo chmod 0700 "$state"
sudo -n -u o3k -- "$state/bin/o3kd" 60 & supervisor_pid=$!
sleep 0.2
service_pid="$(sudo -n pgrep -P "$supervisor_pid" -u o3k)"
start_ticks="$(sudo -n awk '{print $22}' "/proc/$service_pid/stat")"
printf '%s|%s|o3k|o3kd\n' "$service_pid" "$start_ticks" >"$pid_root/o3kd.pid"
touch "$image"
printf 'o3k-disposable-image-v1\n' >"$image.o3k-owned"

RUNNER_TEMP="$tmp_root" GITHUB_RUN_ID="$run_id" \
  O3K_TESTLAB_STATE_ROOT="$state" O3K_TESTLAB_PID_ROOT="$pid_root" \
  O3K_OPENSTACK_VENV="$venv" O3K_TESTLAB_IMAGE_PATH="$image" \
  bash "$CLEANUP"

[[ ! -e "$state" && ! -e "$pid_root" && ! -e "$venv" && ! -e "$image" && ! -e "$image.o3k-owned" ]]
! sudo -n kill -0 "$service_pid" 2>/dev/null

mkdir -p "$state"
printf 'foreign-state\n' >"$state/.o3k-run-owned"
if RUNNER_TEMP="$tmp_root" GITHUB_RUN_ID="$run_id" O3K_TESTLAB_STATE_ROOT="$state" \
  O3K_TESTLAB_PID_ROOT="$pid_root" bash "$CLEANUP" 2>/dev/null; then
  echo "cleanup accepted a foreign state marker" >&2
  exit 1
fi
[[ -e "$state/.o3k-run-owned" ]]

echo "disposable TestLab bootstrap contract tests passed"
