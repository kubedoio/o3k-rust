#!/usr/bin/env bash
set -Eeuo pipefail

RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
RUN_ID="${GITHUB_RUN_ID:-}"
ACCOUNT_LOCK=/run/lock/o3k-testlab-account.lock
[[ "$RUN_ID" =~ ^[0-9]+$|^local-[0-9]+$ ]] || { echo "cleanup: invalid workflow run id" >&2; exit 1; }
[[ "$RUNNER_TEMP" == /* && "$RUNNER_TEMP" != *..* && -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] \
  || { echo "cleanup: runner temp path is unsafe" >&2; exit 1; }
command -v realpath >/dev/null 2>&1 || { echo "cleanup: realpath is unavailable" >&2; exit 1; }
command -v gpasswd >/dev/null 2>&1 || { echo "cleanup: gpasswd is unavailable" >&2; exit 1; }
RUNNER_TEMP="$(realpath -e -- "$RUNNER_TEMP")"
sudo -n test -d "$(dirname "$ACCOUNT_LOCK")" \
  || { echo "cleanup: account lock directory is unavailable" >&2; exit 1; }
EXPECTED_ROOT="${RUNNER_TEMP%/}/o3k-testlab/${RUN_ID}"
EXPECTED_INVENTORY_ROOT="${RUNNER_TEMP%/}/o3k-testlab-inventory/${RUN_ID}"
EXPECTED_ROOT="/var/lib/o3k-testlab/${RUN_ID}"
if [[ "$RUN_ID" =~ ^local- ]]; then
  EXPECTED_ROOT="${RUNNER_TEMP%/}/o3k-testlab/${RUN_ID}"
fi
STATE_ROOT="${O3K_TESTLAB_STATE_ROOT:-$EXPECTED_ROOT}"
INVENTORY_ROOT="${O3K_REAL_HOST_INVENTORY_ROOT:-$EXPECTED_INVENTORY_ROOT}"
PID_ROOT="${O3K_TESTLAB_PID_ROOT:-${RUNNER_TEMP%/}/o3k-testlab-pids/${RUN_ID}}"
OPENSTACK_VENV="${O3K_OPENSTACK_VENV:-}"
IMAGE_PATH="${O3K_TESTLAB_IMAGE_PATH:-}"
[[ "$STATE_ROOT" == "$EXPECTED_ROOT" ]] || { echo "cleanup: state root is not run-owned" >&2; exit 1; }
[[ "$INVENTORY_ROOT" == "$EXPECTED_INVENTORY_ROOT" ]] \
  || { echo "cleanup: inventory root is not run-owned" >&2; exit 1; }
[[ "$PID_ROOT" == "${RUNNER_TEMP%/}/o3k-testlab-pids/${RUN_ID}" ]] \
  || { echo "cleanup: pid root is not run-owned" >&2; exit 1; }
for parent in "${RUNNER_TEMP}/o3k-testlab-pids" \
  "${RUNNER_TEMP}/o3k-testlab-inventory"; do
  if [[ -e "$parent" ]] && [[ ! -d "$parent" || -L "$parent" ]]; then
    echo "cleanup: run state parent is not an owned directory" >&2
    exit 1
  fi
done
if [[ -n "$OPENSTACK_VENV" ]]; then
  [[ "$(dirname -- "$OPENSTACK_VENV")" == "${RUNNER_TEMP%/}" \
    && "$(basename -- "$OPENSTACK_VENV")" == o3k-openstack-venv.* \
    && "$OPENSTACK_VENV" != *..* && ! -L "$OPENSTACK_VENV" ]] \
    || { echo "cleanup: OpenStack virtualenv is not run-owned" >&2; exit 1; }
  if [[ -e "$OPENSTACK_VENV" ]]; then
    [[ -d "$OPENSTACK_VENV" && ! -L "$OPENSTACK_VENV" ]] \
      || { echo "cleanup: OpenStack virtualenv is not a real directory" >&2; exit 1; }
    grep -Fqx 'o3k-disposable-venv-v1' "$OPENSTACK_VENV/.o3k-venv-owned" \
      || { echo "cleanup: virtualenv ownership marker is invalid" >&2; exit 1; }
  fi
fi
if [[ -n "$IMAGE_PATH" ]]; then
  [[ "$(dirname -- "$IMAGE_PATH")" == "${RUNNER_TEMP%/}" \
    && "$(basename -- "$IMAGE_PATH")" == cirros-0.6.3-x86_64-disk.img.* \
    && "$IMAGE_PATH" != *..* && ! -L "$IMAGE_PATH" ]] \
    || { echo "cleanup: image path is not run-owned" >&2; exit 1; }
  if [[ -e "$IMAGE_PATH" || -e "$IMAGE_PATH.o3k-owned" ]]; then
    grep -Fqx 'o3k-disposable-image-v1' "$IMAGE_PATH.o3k-owned" \
      || { echo "cleanup: image ownership marker is invalid" >&2; exit 1; }
  fi
fi
if [[ ! -e "$STATE_ROOT" ]]; then
  if [[ -e "$INVENTORY_ROOT" ]]; then
    [[ -d "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] \
      || { echo "cleanup: inventory root is not a real directory" >&2; exit 1; }
    grep -Fqx 'o3k-disposable-inventory-v1' "$INVENTORY_ROOT/.o3k-inventory-owned" \
      || { echo "cleanup: inventory ownership marker is invalid" >&2; exit 1; }
    rm -rf -- "$INVENTORY_ROOT"
  fi
  [[ -z "$OPENSTACK_VENV" || ! -e "$OPENSTACK_VENV" ]] || rm -rf -- "$OPENSTACK_VENV"
  if [[ -n "$IMAGE_PATH" && -e "$IMAGE_PATH" ]]; then
    grep -Fqx 'o3k-disposable-image-v1' "$IMAGE_PATH.o3k-owned" \
      || { echo "cleanup: image ownership marker is invalid" >&2; exit 1; }
    rm -f -- "$IMAGE_PATH" "$IMAGE_PATH.o3k-owned"
  elif [[ -n "$IMAGE_PATH" && -e "$IMAGE_PATH.o3k-owned" ]]; then
    grep -Fqx 'o3k-disposable-image-v1' "$IMAGE_PATH.o3k-owned" \
      || { echo "cleanup: image ownership marker is invalid" >&2; exit 1; }
    rm -f -- "$IMAGE_PATH.o3k-owned"
  fi
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
sudo -n grep -Fqx "run=$RUN_ID" "$STATE_ROOT/.o3k-run-owned" \
  || { echo "cleanup: ownership marker belongs to another run" >&2; exit 1; }
sudo -n grep -Fqx "o3k-owned-v1 path=$STATE_ROOT" "$STATE_ROOT/.o3k-owned" \
  || { echo "cleanup: O3K ownership marker is invalid" >&2; exit 1; }
state_metadata="$(sudo -n stat -c '%U:%G:%a' "$STATE_ROOT" 2>/dev/null)"
case "$state_metadata" in
  o3k:o3k:700|o3k:o3k:750|o3k:kvm:710) ;;
  *) echo "cleanup: state ownership or permissions are not O3K-owned" >&2; exit 1 ;;
esac
account_created=false
group_created=false
supplementary_groups_added=false
sudo -n grep -Fqx 'o3k-disposable-account-v1' "$STATE_ROOT/.o3k-account-created" 2>/dev/null \
  && account_created=true
sudo -n grep -Fqx 'o3k-disposable-group-v1' "$STATE_ROOT/.o3k-group-created" 2>/dev/null \
  && group_created=true
sudo -n test -s "$STATE_ROOT/.o3k-supplementary-groups-added" \
  && supplementary_groups_added=true

remove_added_supplementary_groups() {
  [[ "$supplementary_groups_added" == true ]] || return 0
  sudo -n flock -x "$ACCOUNT_LOCK" bash -c '
    set -euo pipefail
    while IFS= read -r group; do
      [[ -n "$group" ]] || continue
      getent group "$group" >/dev/null 2>&1 || continue
      id o3k >/dev/null 2>&1 || continue
      if id -nG o3k | tr " " "\n" | grep -Fqx "$group"; then
        gpasswd --delete o3k "$group" >/dev/null
      fi
    done <"$1"
  ' _ "$STATE_ROOT/.o3k-supplementary-groups-added" \
    || { echo "cleanup: cannot restore o3k supplementary groups" >&2; return 1; }
  supplementary_groups_added=false
}

process_matches() {
  local pid="$1" binary="$2" expected executable
  expected="$STATE_ROOT/bin/$binary"
  executable="$(sudo -n readlink "/proc/$pid/exe" 2>/dev/null)" || return 1
  [[ "$executable" == "$expected" ]]
}

process_start_ticks() {
  local pid="$1"
  sudo -n awk '{print $22}' "/proc/$pid/stat" 2>/dev/null
}

process_uid() {
  local pid="$1"
  sudo -n ps -o user= -p "$pid" 2>/dev/null | tr -d ' '
}

process_record_matches() {
  local pid="$1" start_ticks="$2" uid="$3" binary="$4"
  [[ "$uid" == o3k ]] \
    && [[ "$(process_uid "$pid")" == o3k ]] \
    && [[ "$(process_start_ticks "$pid")" == "$start_ticks" ]] \
    && process_matches "$pid" "$binary"
}

stop_owned_process() {
  local pid="$1" start_ticks="$2" uid="$3" binary="$4"
  sudo -n kill -0 "$pid" 2>/dev/null || return 0
  process_record_matches "$pid" "$start_ticks" "$uid" "$binary" \
    || { echo "cleanup: refusing to signal a foreign process" >&2; return 1; }
  sudo -n kill "$pid" || return 1
  for _ in $(seq 1 20); do
    sudo -n kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  echo "cleanup: service did not stop" >&2
  return 1
}
for pid_file in "$PID_ROOT/o3kd.pid" "$PID_ROOT/o3k-compute.pid"; do
  if [[ -f "$pid_file" ]]; then
    IFS='|' read -r pid start_ticks uid binary extra <"$pid_file"
    [[ -z "${extra:-}" && "$pid" =~ ^[0-9]+$ && "$start_ticks" =~ ^[0-9]+$ \
      && ( "$binary" == o3kd || "$binary" == o3k-compute ) ]] \
      || { echo "cleanup: invalid process identity" >&2; exit 1; }
    stop_owned_process "$pid" "$start_ticks" "$uid" "$binary" || exit 1
  fi
done
for _ in $(seq 1 20); do
  alive=false
  for pid_file in "$PID_ROOT/o3kd.pid" "$PID_ROOT/o3k-compute.pid"; do
    [[ -f "$pid_file" ]] || continue
    IFS='|' read -r pid start_ticks uid binary extra <"$pid_file"
    [[ -z "${extra:-}" && "$pid" =~ ^[0-9]+$ && "$start_ticks" =~ ^[0-9]+$ \
      && ( "$binary" == o3kd || "$binary" == o3k-compute ) ]] \
      || { echo "cleanup: invalid process identity" >&2; exit 1; }
    if sudo -n kill -0 "$pid" 2>/dev/null; then
      process_record_matches "$pid" "$start_ticks" "$uid" "$binary" \
        || { echo "cleanup: refusing to wait on a foreign process" >&2; exit 1; }
      alive=true
    fi
  done
  [[ "$alive" == false ]] && break
  sleep 1
done
[[ "$alive" == false ]] || { echo "cleanup: service did not stop" >&2; exit 1; }
remove_added_supplementary_groups || exit 1
if [[ "$account_created" == true || "$group_created" == true ]]; then
  sudo -n flock -x "$ACCOUNT_LOCK" bash -c '
    set -euo pipefail
    pgrep -u o3k >/dev/null 2>&1 && exit 0 || true
    if [[ "$1" == true ]] && id o3k >/dev/null 2>&1; then userdel o3k || true; fi
    if [[ "$2" == true ]] && getent group o3k >/dev/null 2>&1; then groupdel o3k || true; fi
  ' _ "$account_created" "$group_created" || true
fi
sudo -n rm -rf -- "$STATE_ROOT"
rm -rf -- "$PID_ROOT"
if [[ -e "$INVENTORY_ROOT" ]]; then
  [[ -d "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] \
    || { echo "cleanup: inventory root is not a real directory" >&2; exit 1; }
  grep -Fqx 'o3k-disposable-inventory-v1' "$INVENTORY_ROOT/.o3k-inventory-owned" \
    || { echo "cleanup: inventory ownership marker is invalid" >&2; exit 1; }
  rm -rf -- "$INVENTORY_ROOT"
fi
if [[ -n "$OPENSTACK_VENV" && -e "$OPENSTACK_VENV" ]]; then
  grep -Fqx 'o3k-disposable-venv-v1' "$OPENSTACK_VENV/.o3k-venv-owned" \
    || { echo "cleanup: refusing to remove an unowned virtualenv" >&2; exit 1; }
  rm -rf -- "$OPENSTACK_VENV"
fi
if [[ -n "$IMAGE_PATH" && -e "$IMAGE_PATH" ]]; then
  grep -Fqx 'o3k-disposable-image-v1' "$IMAGE_PATH.o3k-owned" \
    || { echo "cleanup: refusing to remove an unowned image" >&2; exit 1; }
  rm -f -- "$IMAGE_PATH"
  rm -f -- "$IMAGE_PATH.o3k-owned"
fi
echo "disposable TestLab cleanup completed"
