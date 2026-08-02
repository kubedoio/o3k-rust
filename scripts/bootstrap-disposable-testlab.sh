#!/usr/bin/env bash
set -Eeuo pipefail

# Build and run an isolated O3K profile for one protected workflow run.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
[[ "$RUNNER_TEMP" == /* && "$RUNNER_TEMP" != *..* && -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] \
  || { echo "disposable TestLab bootstrap failed: runner temp path is unsafe" >&2; exit 1; }
command -v realpath >/dev/null 2>&1 \
  || { echo "disposable TestLab bootstrap failed: realpath is unavailable" >&2; exit 1; }
RUNNER_TEMP="$(realpath -e -- "$RUNNER_TEMP")"
RUN_ID="${GITHUB_RUN_ID:-local-$$}"
SOURCE_COMMIT="${GITHUB_SHA:-$(git -C "$ROOT_DIR" rev-parse HEAD)}"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-host-workflow-artifacts}"
STATE_ROOT="${RUNNER_TEMP%/}/o3k-testlab/${RUN_ID}"
PID_ROOT="${RUNNER_TEMP%/}/o3k-testlab-pids/${RUN_ID}"
INVENTORY_ROOT="${RUNNER_TEMP%/}/o3k-testlab-inventory/${RUN_ID}"
SERVICE_STATE_BASE=/var/lib/o3k-testlab
if [[ "$RUN_ID" =~ ^local- ]]; then
  STATE_ROOT="${RUNNER_TEMP%/}/o3k-testlab/${RUN_ID}"
else
  STATE_ROOT="${SERVICE_STATE_BASE}/${RUN_ID}"
fi
SERVICE_ACCOUNT=o3k
ACCOUNT_LOCK=/run/lock/o3k-testlab-account.lock
APT_LOCK=/run/lock/o3k-testlab-apt.lock
AUTH_PORT="${O3K_TESTLAB_PORT:-18080}"
CONTROL_PORT="${O3K_TESTLAB_CONTROL_PORT:-18551}"
COMPUTE_HEALTH_PORT="${O3K_TESTLAB_COMPUTE_HEALTH_PORT:-19100}"
O3K_PROVIDER="${O3K_PROVIDER:-fake}"
case "${O3K_PROVIDER}" in
  fake|agent) ;;
  *) echo "disposable TestLab bootstrap failed: unsupported provider ${O3K_PROVIDER}" >&2; exit 1 ;;
esac
O3KD_PID=
COMPUTE_PID=
OPENSTACK_VENV="${O3K_OPENSTACK_VENV:-}"
O3KD_READY=false
COMPUTE_READY=false
ACCOUNT_CREATED=false
GROUP_CREATED=false
SUPPLEMENTARY_GROUPS_ADDED=false
FAIL_REASON=bootstrap_failed

fail() { FAIL_REASON="$1"; echo "disposable TestLab bootstrap failed: $1" >&2; exit 1; }

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
  [[ "$uid" == "$SERVICE_ACCOUNT" ]] \
    && [[ "$(process_uid "$pid")" == "$SERVICE_ACCOUNT" ]] \
    && [[ "$(process_start_ticks "$pid")" == "$start_ticks" ]] \
    && process_matches "$pid" "$binary"
}

stop_owned_process() {
  local pid="$1" binary="$2"
  sudo -n kill -0 "$pid" 2>/dev/null || return 0
  process_matches "$pid" "$binary" || {
    echo "disposable TestLab cleanup refused foreign process ${pid}" >&2
    return 1
  }
  sudo -n kill "$pid" || return 1
  for _ in $(seq 1 20); do
    sudo -n kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  echo "disposable TestLab cleanup timed out stopping ${binary}" >&2
  return 1
}

remove_created_identity() {
  [[ "$ACCOUNT_CREATED" == true || "$GROUP_CREATED" == true ]] || return 0
  sudo -n flock -x "$ACCOUNT_LOCK" bash -c '
    set -euo pipefail
    pgrep -u o3k >/dev/null 2>&1 && exit 42 || true
    if [[ "$1" == true ]] && id o3k >/dev/null 2>&1; then userdel o3k; fi
    if [[ "$2" == true ]] && getent group o3k >/dev/null 2>&1; then groupdel o3k; fi
  ' _ "$ACCOUNT_CREATED" "$GROUP_CREATED"
}

remove_added_supplementary_groups() {
  [[ "$SUPPLEMENTARY_GROUPS_ADDED" == true ]] || return 0
  sudo -n test -f "$STATE_ROOT/.o3k-supplementary-groups-added" || return 0
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
  ' _ "$STATE_ROOT/.o3k-supplementary-groups-added"
  SUPPLEMENTARY_GROUPS_ADDED=false
}

write_result() {
  local status="$1" reason="$2"
  mkdir -p "$ARTIFACT_DIR"
  python3 - "$ARTIFACT_DIR/disposable-testlab-bootstrap.json" "$status" "$reason" \
    "$SOURCE_COMMIT" "$STATE_ROOT" "$SERVICE_ACCOUNT" "$AUTH_PORT" \
    "${O3KD_PID:-}" "${COMPUTE_PID:-}" "$O3KD_READY" "$COMPUTE_READY" "$O3K_PROVIDER" <<'PY'
import json, sys, time
path, status, reason, commit, state, account, port, o3kd_pid, compute_pid, o3kd_ready, compute_ready, provider = sys.argv[1:]
with open(path, "w", encoding="utf-8") as stream:
    json.dump({"artifact_type": "disposable-testlab-bootstrap", "status": status,
               "reason": reason, "redacted": True, "source_commit": commit,
               "state_directory": state, "service_account": account,
               "auth_url": f"http://127.0.0.1:{port}/v3", "username": "admin",
               "project": "admin", "project_id": "bootstrap-project",
               "region": "RegionOne", "o3kd_pid": o3kd_pid or None,
               "compute_pid": compute_pid or None,
               "provider": provider,
               "readiness": {"o3kd": o3kd_ready == "true", "compute": compute_ready == "true"},
               "finished_at": int(time.time())}, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY
}

failure_cleanup() {
  local status="$?"
  if ((status != 0)); then
    write_result failed "$FAIL_REASON" 2>/dev/null || true
    cleanup_failed=false
    [[ -z "$COMPUTE_PID" ]] || stop_owned_process "$COMPUTE_PID" o3k-compute || cleanup_failed=true
    [[ -z "$O3KD_PID" ]] || stop_owned_process "$O3KD_PID" o3kd || cleanup_failed=true
    if [[ "$cleanup_failed" == false ]]; then
      remove_added_supplementary_groups 2>/dev/null || true
      if [[ -d "$STATE_ROOT" && ! -L "$STATE_ROOT" \
        && -f "$STATE_ROOT/.o3k-run-owned" && ! -L "$STATE_ROOT/.o3k-run-owned" ]] \
        && sudo -n grep -Fqx 'o3k-disposable-testlab-v1' "$STATE_ROOT/.o3k-run-owned" 2>/dev/null; then
        sudo -n rm -rf -- "$STATE_ROOT" 2>/dev/null || true
      fi
      rm -rf -- "$PID_ROOT" 2>/dev/null || true
      if [[ -d "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] \
        && grep -Fqx 'o3k-disposable-inventory-v1' "$INVENTORY_ROOT/.o3k-inventory-owned" 2>/dev/null; then
        rm -rf -- "$INVENTORY_ROOT" 2>/dev/null || true
      fi
      remove_created_identity 2>/dev/null || true
    else
      echo "disposable TestLab cleanup incomplete; preserving owned state for retry" >&2
    fi
  fi
  exit "$status"
}
trap failure_cleanup EXIT

[[ "$RUN_ID" =~ ^[0-9]+$|^local-[0-9]+$ ]] || fail "invalid workflow run id"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-fA-F]{40}$ ]] || fail "invalid source commit"
[[ "$AUTH_PORT" =~ ^[0-9]+$ && "$CONTROL_PORT" =~ ^[0-9]+$ && "$COMPUTE_HEALTH_PORT" =~ ^[0-9]+$ ]] || fail "invalid service port"
for command in cargo openssl python3 curl sudo getent id pgrep ss flock stat readlink realpath timeout ps usermod gpasswd; do
  command -v "$command" >/dev/null 2>&1 || fail "$command is unavailable"
done
sudo -n true 2>/dev/null || fail "passwordless sudo is required"
sudo -n test -d "$(dirname "$ACCOUNT_LOCK")" || fail "account lock directory is unavailable"
[[ "$(git -C "$ROOT_DIR" rev-parse HEAD)" == "$SOURCE_COMMIT" ]] || fail "checkout is not immutable"

for port in "$AUTH_PORT" "$CONTROL_PORT" "$COMPUTE_HEALTH_PORT"; do
  ((port >= 1 && port <= 65535)) || fail "invalid service port ${port}"
done
for parent in "${RUNNER_TEMP}/o3k-testlab-pids" \
  "${RUNNER_TEMP}/o3k-testlab-inventory"; do
  if [[ -e "$parent" ]] && [[ ! -d "$parent" || -L "$parent" ]]; then
    fail "run state parent is not an owned directory: ${parent}"
  fi
done
if [[ ! -e "$STATE_ROOT" ]]; then
  [[ ! -e "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] \
    || fail "run inventory state already exists without matching service state"
  for port in "$AUTH_PORT" "$CONTROL_PORT" "$COMPUTE_HEALTH_PORT"; do
    if ss -H -ltn 2>/dev/null | awk -v suffix=":${port}" \
      'length($4) >= length(suffix) && substr($4, length($4)-length(suffix)+1) == suffix {found=1} END {exit !found}'; then
      fail "run port ${port} is already occupied by an existing service"
    fi
  done
fi

REUSE=false
PASSWORD=
if [[ -e "$STATE_ROOT" ]]; then
  [[ -d "$STATE_ROOT" && ! -L "$STATE_ROOT" ]] || fail "run state is not a directory"
  [[ -f "$STATE_ROOT/.o3k-run-owned" && ! -L "$STATE_ROOT/.o3k-run-owned" ]] \
    || fail "existing run state is not owned by this bootstrap"
  marker="$(sudo -n cat "$STATE_ROOT/.o3k-run-owned" 2>/dev/null)" \
    || fail "cannot inspect existing run ownership marker"
  grep -Fqx 'o3k-disposable-testlab-v1' <<<"$marker" \
    || fail "existing run state has a foreign ownership marker"
  grep -Fqx "commit=$SOURCE_COMMIT" <<<"$marker" \
    || fail "existing run state belongs to a different source commit"
  [[ -d "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] \
    || fail "existing run state has no inventory state"
  grep -Fqx 'o3k-disposable-inventory-v1' "$INVENTORY_ROOT/.o3k-inventory-owned" \
    || fail "existing inventory state is not owned by this bootstrap"
  PASSWORD="$(sudo -n cat "$STATE_ROOT/.password" 2>/dev/null)" \
    || fail "existing run state has no readable protected password"
  [[ "$PASSWORD" =~ ^[0-9a-f]{64}$ ]] \
    || fail "existing run state contains an invalid protected password"
  echo "::add-mask::${PASSWORD}"
  [[ -f "$PID_ROOT/o3kd.pid" && -f "$PID_ROOT/o3k-compute.pid" ]] \
    || fail "existing run state has no complete process identity"
  IFS='|' read -r O3KD_PID O3KD_START O3KD_UID O3KD_BINARY <"$PID_ROOT/o3kd.pid"
  IFS='|' read -r COMPUTE_PID COMPUTE_START COMPUTE_UID COMPUTE_BINARY <"$PID_ROOT/o3k-compute.pid"
  [[ "$O3KD_PID" =~ ^[0-9]+$ && "$COMPUTE_PID" =~ ^[0-9]+$ \
    && "$O3KD_START" =~ ^[0-9]+$ && "$COMPUTE_START" =~ ^[0-9]+$ \
    && "$O3KD_BINARY" == o3kd && "$COMPUTE_BINARY" == o3k-compute ]] \
    || fail "existing run state has an invalid process identity"
  process_record_matches "$O3KD_PID" "$O3KD_START" "$O3KD_UID" o3kd \
    && process_record_matches "$COMPUTE_PID" "$COMPUTE_START" "$COMPUTE_UID" o3k-compute \
    && sudo -n kill -0 "$O3KD_PID" 2>/dev/null && sudo -n kill -0 "$COMPUTE_PID" 2>/dev/null \
    || fail "existing run state services are not running"
  REUSE=true
fi

if [[ "$REUSE" == false ]]; then
umask 077
need_packages=false
command -v genisoimage >/dev/null 2>&1 || need_packages=true
command -v ssh-keygen >/dev/null 2>&1 || need_packages=true
python3 -m venv --help >/dev/null 2>&1 || need_packages=true
command -v pkg-config >/dev/null 2>&1 || need_packages=true
command -v protoc >/dev/null 2>&1 || need_packages=true
if [[ "$need_packages" == true ]] || ! pkg-config --exists libvirt 2>/dev/null; then
  command -v apt-get >/dev/null 2>&1 || fail "required host packages are missing and apt-get is unavailable"
  sudo -n test -d "$(dirname "$APT_LOCK")" || fail "package-manager lock directory is unavailable"
  sudo -n flock -x "$APT_LOCK" bash -c '
    set -euo pipefail
    timeout --signal=TERM --kill-after=30s 300s env DEBIAN_FRONTEND=noninteractive \
      apt-get -o DPkg::Lock::Timeout=300 update -qq
    timeout --signal=TERM --kill-after=30s 300s env DEBIAN_FRONTEND=noninteractive \
      apt-get -o DPkg::Lock::Timeout=300 install -y --no-install-recommends \
        genisoimage openssh-client python3-venv pkg-config libvirt-dev protobuf-compiler
  ' || fail "cannot install required host packages within the bounded package-manager timeout"
fi
sudo -n install -d -o root -g root -m 0755 "$SERVICE_STATE_BASE"
install -d -m 0755 "${PID_ROOT%/*}" "${INVENTORY_ROOT%/*}"
[[ ! -e "$INVENTORY_ROOT" && ! -L "$INVENTORY_ROOT" ]] || fail "run inventory state already exists"
install -d -m 0755 "$INVENTORY_ROOT"
printf 'o3k-disposable-inventory-v1\ncommit=%s\nrun=%s\n' "$SOURCE_COMMIT" "$RUN_ID" \
  >"$INVENTORY_ROOT/.o3k-inventory-owned"
chmod 0644 "$INVENTORY_ROOT/.o3k-inventory-owned"
account_state="$(sudo -n flock -x "$ACCOUNT_LOCK" bash -c '
  set -euo pipefail
  group_created=false
  account_created=false
  if ! getent group o3k >/dev/null 2>&1; then
    groupadd --system o3k
    group_created=true
  fi
  if ! id o3k >/dev/null 2>&1; then
    useradd --system --no-create-home --gid o3k --home-dir "$1" \
      --shell /usr/sbin/nologin o3k
    account_created=true
  fi
  printf "%s %s\\n" "$account_created" "$group_created"
' _ "$STATE_ROOT/home")" || fail "cannot provision packaged o3k service account"
read -r ACCOUNT_CREATED GROUP_CREATED <<<"$account_state"
[[ "$(id -u "$SERVICE_ACCOUNT")" != 0 ]] || fail "o3k service account is root"

[[ ! -e "$PID_ROOT" && ! -L "$PID_ROOT" ]] || fail "run pid state already exists"
install -d -m 0700 "$PID_ROOT"
[[ ! -e "$STATE_ROOT" && ! -L "$STATE_ROOT" ]] || fail "run state already exists"
sudo -n install -d -o "$(id -u)" -g "$(id -g)" -m 0700 \
  "$STATE_ROOT" "$STATE_ROOT/bin" "$STATE_ROOT/data" "$STATE_ROOT/log" "$STATE_ROOT/tls"
printf 'o3k-disposable-testlab-v1\ncommit=%s\nrun=%s\n' "$SOURCE_COMMIT" "$RUN_ID" >"$STATE_ROOT/.o3k-run-owned"
chmod 0600 "$STATE_ROOT/.o3k-run-owned"
printf 'o3k-owned-v1 path=%s\n' "$STATE_ROOT" >"$STATE_ROOT/.o3k-owned"
chmod 0640 "$STATE_ROOT/.o3k-owned"
if [[ "$ACCOUNT_CREATED" == true ]]; then
  printf 'o3k-disposable-account-v1\n' >"$STATE_ROOT/.o3k-account-created"
  chmod 0600 "$STATE_ROOT/.o3k-account-created"
fi
if [[ "$GROUP_CREATED" == true ]]; then
  printf 'o3k-disposable-group-v1\n' >"$STATE_ROOT/.o3k-group-created"
  chmod 0600 "$STATE_ROOT/.o3k-group-created"
fi
SUPPLEMENTARY_GROUPS_ADDED=true
sudo -n flock -x "$ACCOUNT_LOCK" bash -c '
  set -euo pipefail
  for group in libvirt kvm; do
    getent group "$group" >/dev/null 2>&1 \
      || { echo "required libvirt access group is unavailable: $group" >&2; exit 1; }
  done
  : >"$1"
  chmod 0600 "$1"
  for group in libvirt kvm; do
    if ! id -nG o3k | tr " " "\n" | grep -Fqx "$group"; then
      usermod --append --groups "$group" o3k
      printf "%s\n" "$group" >>"$1"
    fi
  done
' _ "$STATE_ROOT/.o3k-supplementary-groups-added" \
  || fail "cannot grant o3k access to libvirt and KVM"
if sudo -n test -s "$STATE_ROOT/.o3k-supplementary-groups-added"; then
  SUPPLEMENTARY_GROUPS_ADDED=true
else
  sudo -n rm -f -- "$STATE_ROOT/.o3k-supplementary-groups-added"
fi
command -v genisoimage >/dev/null 2>&1 || fail "genisoimage is unavailable after dependency setup"
command -v ssh-keygen >/dev/null 2>&1 || fail "ssh-keygen is unavailable after dependency setup"
python3 -m venv --help >/dev/null 2>&1 || fail "python3-venv is unavailable after dependency setup"
command -v pkg-config >/dev/null 2>&1 || fail "pkg-config is unavailable after dependency setup"
pkg-config --exists libvirt 2>/dev/null || fail "libvirt development files are unavailable after dependency setup"
command -v protoc >/dev/null 2>&1 || fail "protobuf compiler is unavailable after dependency setup"
cargo build --locked --release --bin o3kd
# virt-sys deliberately tolerates a missing pkg-config probe for docs builds;
# make the runtime link explicit after the host preflight proves libvirt exists.
RUSTFLAGS="${RUSTFLAGS:-} -l dylib=virt" \
  cargo build --locked --release --features libvirt --bin o3k-compute-bin
install -m 0755 "$ROOT_DIR/target/release/o3kd" "$STATE_ROOT/bin/o3kd"
install -m 0755 "$ROOT_DIR/target/release/o3k-compute-bin" "$STATE_ROOT/bin/o3k-compute"
sudo -n bash "$ROOT_DIR/packaging/bootstrap-certs.sh" --output-dir "$STATE_ROOT/tls" \
  --server-name o3k-control-plane --agent-id compute-agent
sudo -n install -m 0640 "$STATE_ROOT/tls/agent-id" "$STATE_ROOT/data/agent-id"
AUTHORIZED_FINGERPRINT="$(sudo -n cat "$STATE_ROOT/tls/agent-fingerprint")" \
  || fail "cannot read generated agent fingerprint"

PASSWORD="$(openssl rand -hex 32)"
[[ "$PASSWORD" =~ ^[0-9a-f]{64}$ ]] || fail "generated password format is unsafe"
echo "::add-mask::${PASSWORD}"
SIGNING_KEY="$(openssl rand -hex 48)"
password_tmp="$STATE_ROOT/.password.tmp.$$"
printf '%s\n' "$PASSWORD" >"$password_tmp"
chmod 0600 "$password_tmp"
mv -f -- "$password_tmp" "$STATE_ROOT/.password"
o3kd_env_tmp="$STATE_ROOT/o3kd.env.tmp.$$"
compute_env_tmp="$STATE_ROOT/o3k-compute.env.tmp.$$"
cat >"$o3kd_env_tmp" <<EOF
O3K_DATA_DIR=$(printf '%q' "$STATE_ROOT/data")
O3K_LISTEN_ADDR=$(printf '%q' "127.0.0.1:${AUTH_PORT}")
O3K_PROVIDER=$(printf '%q' "$O3K_PROVIDER")
O3K_LOG_FORMAT=json
O3K_LOG_FILTER=warn
O3K_BOOTSTRAP_PASSWORD=$(printf '%q' "$PASSWORD")
O3K_TOKEN_SIGNING_KEY=$(printf '%q' "$SIGNING_KEY")
O3K_COMPUTE_CONTROL_ADDR=$(printf '%q' "127.0.0.1:${CONTROL_PORT}")
O3K_COMPUTE_SERVER_CERTIFICATE=$(printf '%q' "$STATE_ROOT/tls/server.pem")
O3K_COMPUTE_SERVER_PRIVATE_KEY=$(printf '%q' "$STATE_ROOT/tls/server-key.pem")
O3K_COMPUTE_CLIENT_CA=$(printf '%q' "$STATE_ROOT/tls/ca.pem")
O3K_COMPUTE_AUTHORIZED_AGENTS=compute-agent=$(printf '%q' "$AUTHORIZED_FINGERPRINT")
EOF
cat >"$compute_env_tmp" <<EOF
O3K_COMPUTE_DATA_DIR=$(printf '%q' "$STATE_ROOT/data")
O3K_COMPUTE_CONTROL_ENDPOINT=$(printf '%q' "https://127.0.0.1:${CONTROL_PORT}")
O3K_COMPUTE_SERVER_NAME=o3k-control-plane
O3K_COMPUTE_HOST_LABEL=o3k-testlab
O3K_COMPUTE_TLS_DIR=$(printf '%q' "$STATE_ROOT/tls")
O3K_COMPUTE_HEALTH_ADDR=$(printf '%q' "127.0.0.1:${COMPUTE_HEALTH_PORT}")
O3K_COMPUTE_MAX_DISK_GB=10
EOF
if [[ "${O3K_AGENT_INSPECT_PROBE_ENABLED:-false}" == true ]]; then
  if [[ -n "${O3K_AGENT_INSPECT_PROBE_RESOURCE_FILE:-}" ]]; then
    probe_resource_file="${O3K_AGENT_INSPECT_PROBE_RESOURCE_FILE}"
    [[ "$probe_resource_file" == /* && "$probe_resource_file" != *..* ]] \
      || fail "agent inspect probe resource file is invalid"
    mkdir -p "$(dirname "$probe_resource_file")"
    touch "$probe_resource_file"
    chmod 0644 "$probe_resource_file"
    printf 'O3K_AGENT_INSPECT_PROBE_RESOURCE_FILE=%s\n' \
      "$(printf '%q' "$probe_resource_file")" >>"$o3kd_env_tmp"
  else
    [[ "${O3K_AGENT_INSPECT_PROBE_RESOURCE_ID:-}" =~ ^[0-9a-fA-F-]{36}$ ]] \
      || fail "agent inspect probe resource id is invalid"
    printf 'O3K_AGENT_INSPECT_PROBE_RESOURCE_ID=%s\n' \
      "$(printf '%q' "$O3K_AGENT_INSPECT_PROBE_RESOURCE_ID")" >>"$o3kd_env_tmp"
  fi
  printf 'O3K_AGENT_INSPECT_PROBE_OUTPUT=%s/agent-inspect-probe.json\n' "$STATE_ROOT" >>"$o3kd_env_tmp"
fi
chmod 0600 "$o3kd_env_tmp" "$compute_env_tmp"
mv -f -- "$o3kd_env_tmp" "$STATE_ROOT/o3kd.env"
mv -f -- "$compute_env_tmp" "$STATE_ROOT/o3k-compute.env"
sudo -n chown -R "$SERVICE_ACCOUNT:$SERVICE_ACCOUNT" "$STATE_ROOT"
sudo -n -u "$SERVICE_ACCOUNT" -- test -r "$STATE_ROOT/o3kd.env" \
  || fail "o3k service account cannot traverse the run state root"

start_service() {
  local name="$1" env_file="$2" binary="$3" log_file="$4" pid_file="$5"
  local supervisor child candidate
  sudo -n -u "$SERVICE_ACCOUNT" -- bash -c 'set -a; . "$1"; set +a; exec "$2" >>"$3" 2>&1' _ \
    "$env_file" "$binary" "$log_file" &
  supervisor=$!
  child=
  for _ in $(seq 1 50); do
    while read -r candidate; do
      if process_matches "$candidate" "$(basename "$binary")"; then
        child="$candidate"
        break
      fi
    done < <(sudo -n pgrep -u "$SERVICE_ACCOUNT" -x "$(basename "$binary")" 2>/dev/null || true)
    [[ -n "$child" ]] && break
    sudo -n kill -0 "$supervisor" 2>/dev/null || break
    sleep 0.1
  done
  if [[ -z "$child" ]]; then
    sudo -n kill "$supervisor" 2>/dev/null || true
    fail "$name did not start as the packaged service account"
  fi
  start_ticks="$(process_start_ticks "$child")"
  uid="$(process_uid "$child")"
  [[ "$start_ticks" =~ ^[0-9]+$ && "$uid" == "$SERVICE_ACCOUNT" ]] \
    || fail "$name did not expose a valid service identity"
  printf '%s|%s|%s|%s\n' "$child" "$start_ticks" "$uid" "$(basename "$binary")" >"$pid_file"
  if [[ "$name" == o3kd ]]; then O3KD_PID="$child"; else COMPUTE_PID="$child"; fi
}

wait_for_o3kd_health() {
  for _ in $(seq 1 60); do
    curl --fail --silent --max-time 2 "http://127.0.0.1:${AUTH_PORT}/healthz" >/dev/null 2>&1 && break
    sleep 1
  done
  curl --fail --silent --max-time 2 "http://127.0.0.1:${AUTH_PORT}/healthz" >/dev/null 2>&1 \
    || fail "o3kd health endpoint did not become ready"
}

wait_for_o3kd_ready() {
  for _ in $(seq 1 60); do
    curl --fail --silent --max-time 2 "http://127.0.0.1:${AUTH_PORT}/readyz" >/dev/null 2>&1 && break
    sleep 1
  done
  curl --fail --silent --max-time 2 "http://127.0.0.1:${AUTH_PORT}/readyz" >/dev/null 2>&1 \
    || fail "o3kd did not become ready"
  O3KD_READY=true
}

start_compute() {
  start_service o3k-compute "$STATE_ROOT/o3k-compute.env" "$STATE_ROOT/bin/o3k-compute" \
    "$STATE_ROOT/log/o3k-compute.log" "$PID_ROOT/o3k-compute.pid"
}

wait_for_compute_ready() {
  for _ in $(seq 1 30); do
    curl --fail --silent --max-time 2 "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/healthz" >/dev/null 2>&1 && break
    sleep 1
  done
  curl --fail --silent --max-time 2 "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/healthz" >/dev/null 2>&1 \
    || fail "o3k-compute health endpoint did not become ready"
  curl --fail --silent --max-time 2 "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/readyz" >/dev/null 2>&1 \
    || fail "o3k-compute libvirt/control-plane readiness did not become ready"
  COMPUTE_READY=true
}

start_service o3kd "$STATE_ROOT/o3kd.env" "$STATE_ROOT/bin/o3kd" "$STATE_ROOT/log/o3kd.log" "$PID_ROOT/o3kd.pid"
wait_for_o3kd_health
if [[ "$O3K_PROVIDER" == agent ]]; then
  start_compute
  wait_for_o3kd_ready
  wait_for_compute_ready
else
  wait_for_o3kd_ready
  start_compute
  wait_for_compute_ready
fi
else
  echo "reusing authenticated disposable TestLab for run ${RUN_ID}"
  curl --fail --silent --max-time 2 "http://127.0.0.1:${AUTH_PORT}/readyz" >/dev/null 2>&1 \
    || fail "reused o3kd is not ready"
  curl --fail --silent --max-time 2 "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/healthz" >/dev/null 2>&1 \
    || fail "reused o3k-compute is not healthy"
  O3KD_READY=true
  COMPUTE_READY=true
fi

if [[ -z "$OPENSTACK_VENV" ]]; then
  OPENSTACK_VENV="$(mktemp -d "${RUNNER_TEMP%/}/o3k-openstack-venv.XXXXXX")"
else
  [[ "$OPENSTACK_VENV" == "${RUNNER_TEMP%/}/o3k-openstack-venv."* ]] \
    || fail "OpenStack virtualenv is not run-owned"
fi
if [[ ! -x "$OPENSTACK_VENV/bin/openstack" ]]; then
  python3 -m venv "$OPENSTACK_VENV"
  PIP_CONFIG_FILE=/dev/null PIP_NO_CACHE_DIR=1 "$OPENSTACK_VENV/bin/python" -m pip install \
    --isolated --disable-pip-version-check --no-input "python-openstackclient==10.2.1"
fi
if [[ ! -f "$OPENSTACK_VENV/.o3k-venv-owned" ]]; then
  printf 'o3k-disposable-venv-v1\ncommit=%s\nrun=%s\n' "$SOURCE_COMMIT" "$RUN_ID" \
    >"$OPENSTACK_VENV/.o3k-venv-owned"
  chmod 0600 "$OPENSTACK_VENV/.o3k-venv-owned"
fi
grep -Fqx 'o3k-disposable-venv-v1' "$OPENSTACK_VENV/.o3k-venv-owned" \
  || fail "OpenStack virtualenv is not owned by this bootstrap"
export PATH="$OPENSTACK_VENV/bin:$PATH"
printf '%s\n' "$OPENSTACK_VENV/bin" >>"${GITHUB_PATH:-/dev/null}"
export OS_AUTH_URL="http://127.0.0.1:${AUTH_PORT}/v3" OS_USERNAME=admin OS_PASSWORD="$PASSWORD" \
  OS_PROJECT_NAME=admin OS_REGION_NAME=RegionOne OS_USER_DOMAIN_NAME=Default \
  OS_PROJECT_DOMAIN_NAME=Default OS_INTERFACE=public OS_IDENTITY_API_VERSION=3
openstack token issue >/dev/null 2>&1 || fail "generated password failed OpenStack authentication"

printf 'O3K_TESTLAB_STATE_ROOT=%s\nO3K_REAL_HOST_SERVICE_ACCOUNT=%s\n' "$STATE_ROOT" "$(id -un)" >>"${GITHUB_ENV:-/dev/null}"
printf 'O3K_REAL_HOST_DAEMON_ACCOUNT=%s\n' "$SERVICE_ACCOUNT" >>"${GITHUB_ENV:-/dev/null}"
printf 'O3K_TESTLAB_PID_ROOT=%s\n' "$PID_ROOT" >>"${GITHUB_ENV:-/dev/null}"
printf 'O3K_REAL_HOST_PROTECTED_PATHS=%s\nO3K_REAL_HOST_INVENTORY_ROOT=%s\nO3K_OPENSTACK_VENV=%s\n' \
  "$INVENTORY_ROOT" "$INVENTORY_ROOT" "$OPENSTACK_VENV" >>"${GITHUB_ENV:-/dev/null}"
printf 'OS_AUTH_URL=%s\nOS_USERNAME=admin\nOS_PROJECT_NAME=admin\nOS_REGION_NAME=RegionOne\nOS_PASSWORD=%s\n' \
  "$OS_AUTH_URL" "$PASSWORD" >>"${GITHUB_ENV:-/dev/null}"
write_result passed authenticated
trap - EXIT
echo "disposable TestLab bootstrap completed"
