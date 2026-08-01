#!/usr/bin/env bash
set -Eeuo pipefail

RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
RUN_ID="${GITHUB_RUN_ID:-}"
[[ "$RUN_ID" =~ ^[0-9]+$|^local-[0-9]+$ ]] || { echo "cleanup: invalid workflow run id" >&2; exit 1; }
EXPECTED_ROOT="${RUNNER_TEMP%/}/o3k-testlab/${RUN_ID}"
STATE_ROOT="${O3K_TESTLAB_STATE_ROOT:-$EXPECTED_ROOT}"
PID_ROOT="${O3K_TESTLAB_PID_ROOT:-${RUNNER_TEMP%/}/o3k-testlab-pids/${RUN_ID}}"
OPENSTACK_VENV="${O3K_OPENSTACK_VENV:-}"
IMAGE_PATH="${O3K_TESTLAB_IMAGE_PATH:-}"
[[ "$STATE_ROOT" == "$EXPECTED_ROOT" ]] || { echo "cleanup: state root is not run-owned" >&2; exit 1; }
[[ "$PID_ROOT" == "${RUNNER_TEMP%/}/o3k-testlab-pids/${RUN_ID}" ]] \
  || { echo "cleanup: pid root is not run-owned" >&2; exit 1; }
if [[ -n "$OPENSTACK_VENV" ]]; then
  [[ "$(dirname -- "$OPENSTACK_VENV")" == "${RUNNER_TEMP%/}" \
    && "$(basename -- "$OPENSTACK_VENV")" == o3k-openstack-venv.* \
    && "$OPENSTACK_VENV" != *..* && ! -L "$OPENSTACK_VENV" ]] \
    || { echo "cleanup: OpenStack virtualenv is not run-owned" >&2; exit 1; }
fi
if [[ -n "$IMAGE_PATH" ]]; then
  [[ "$(dirname -- "$IMAGE_PATH")" == "${RUNNER_TEMP%/}" \
    && "$(basename -- "$IMAGE_PATH")" == cirros-0.6.3-x86_64-disk.img.* \
    && "$IMAGE_PATH" != *..* && ! -L "$IMAGE_PATH" ]] \
    || { echo "cleanup: image path is not run-owned" >&2; exit 1; }
fi
if [[ ! -e "$STATE_ROOT" ]]; then
  [[ -z "$OPENSTACK_VENV" || ! -e "$OPENSTACK_VENV" ]] || rm -rf -- "$OPENSTACK_VENV"
  [[ -z "$IMAGE_PATH" || ! -e "$IMAGE_PATH" ]] || rm -f -- "$IMAGE_PATH"
  echo "disposable TestLab cleanup: no state to remove"
  exit 0
fi
[[ -d "$STATE_ROOT" && ! -L "$STATE_ROOT" ]] \
  || { echo "cleanup: state root is not a real directory" >&2; exit 1; }
sudo -n test -f "$STATE_ROOT/.o3k-run-owned" \
  && ! sudo -n test -L "$STATE_ROOT/.o3k-run-owned" \
  || { echo "cleanup: ownership marker is missing" >&2; exit 1; }
sudo -n grep -Fqx 'o3k-disposable-testlab-v1' "$STATE_ROOT/.o3k-run-owned" \
  || { echo "cleanup: ownership marker is invalid" >&2; exit 1; }
for pid_file in "$PID_ROOT/o3kd.pid" "$PID_ROOT/o3k-compute.pid"; do
  if [[ -f "$pid_file" ]]; then
    pid="$(<"$pid_file")"
    [[ "$pid" =~ ^[0-9]+$ ]] || { echo "cleanup: invalid pid" >&2; exit 1; }
    binary="${pid_file##*/}"
    binary="${binary%.pid}"
    if sudo -n kill -0 "$pid" 2>/dev/null; then
      command_line="$(sudo -n sh -c 'tr "\\0" " " < "/proc/$1/cmdline"' _ "$pid" 2>/dev/null)" \
        || { echo "cleanup: cannot inspect process identity" >&2; exit 1; }
      [[ "$command_line" == *"$STATE_ROOT/bin/$binary"* ]] \
        || { echo "cleanup: refusing to signal a foreign process" >&2; exit 1; }
      sudo -n kill "$pid" 2>/dev/null || true
    fi
  fi
done
for _ in $(seq 1 20); do
  alive=false
  for pid_file in "$PID_ROOT/o3kd.pid" "$PID_ROOT/o3k-compute.pid"; do
    [[ -f "$pid_file" ]] || continue
    pid="$(<"$pid_file")"
    if sudo -n kill -0 "$pid" 2>/dev/null; then
      binary="${pid_file##*/}"
      binary="${binary%.pid}"
      command_line="$(sudo -n sh -c 'tr "\\0" " " < "/proc/$1/cmdline"' _ "$pid" 2>/dev/null)" \
        || { echo "cleanup: cannot inspect process identity" >&2; exit 1; }
      [[ "$command_line" == *"$STATE_ROOT/bin/$binary"* ]] \
        || { echo "cleanup: refusing to wait on a foreign process" >&2; exit 1; }
      alive=true
    fi
  done
  [[ "$alive" == false ]] && break
  sleep 1
done
[[ "$alive" == false ]] || { echo "cleanup: service did not stop" >&2; exit 1; }
sudo -n rm -rf -- "$STATE_ROOT"
rm -rf -- "$PID_ROOT"
if [[ -n "$OPENSTACK_VENV" && -e "$OPENSTACK_VENV" ]]; then
  rm -rf -- "$OPENSTACK_VENV"
fi
if [[ -n "$IMAGE_PATH" && -e "$IMAGE_PATH" ]]; then
  rm -f -- "$IMAGE_PATH"
fi
echo "disposable TestLab cleanup completed"
