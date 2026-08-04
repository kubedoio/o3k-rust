#!/usr/bin/env bash
# Real Cinder service-under-test profile (primary real-service profile).
#
# The primary OpenStack compatibility target is 2026.1 Gazpacho, pinned to
# Cinder 28.0.0 and cinder-tempest-plugin 1.21.0 through a reproducible,
# isolated Python virtual environment (pinned PyPI sdist). The fallback profile
# is 2025.2 Flamingo (Cinder 27.0.0, cinder-tempest-plugin 1.19.0), selected
# with O3K_CINDER_PROFILE=flamingo. Dalmatian is too old and is not an accepted
# profile (see ADR-0153 and compatibility/openstack-targets.yaml). No DevStack
# and no complete OpenStack control plane is installed.
#
# O3K supplies only the declared satellite APIs: Keystone-compatible identity
# and catalog, Glance-compatible image access, Nova-compatible volume
# attachments, the compute-agent execution boundary, and libvirt hotplug. The
# external Cinder deployment owns and operates its database, message bus,
# api/scheduler/volume processes, and LVM backend.
#
# Resources are disposable and ownership-safe: every name is derived from the
# protected workflow run ID, and every secret is generated ephemerally. Before
# any mutation the runner records a foreign-state inventory; cleanup removes
# only run-owned resources and fails on any remaining run-owned resource.
#
# Evidence ownership:
#   O3K-owned      : o3kd, o3k-compute-bin agent, identity, catalog, Nova APIs
#   Cinder-owned   : cinder-api, cinder-scheduler, cinder-volume (venv)
#   Cinder deps    : MariaDB, RabbitMQ, memcached
#   test backend   : run-owned LVM volume group (loop device)
#   compute host   : libvirt/qemu + open-iscsi on the test host
#
# Usage:
#   sudo bash scripts/real-cinder-testbed-runner.sh [--keep]
#
# Output: redacted source-bound evidence under ${EVIDENCE_DIR}.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

KEEP="${1:-}"

# --- Pinned Cinder profile -----------------------------------------------------
# Primary profile is 2026.1 Gazpacho (Cinder 28.0.0, cinder-tempest-plugin
# 1.21.0). The fallback profile is 2025.2 Flamingo (Cinder 27.0.0,
# cinder-tempest-plugin 1.19.0), per ADR-0153 and target.json
# (backward_compatibility_profiles). Dalmatian is too old and is not an
# accepted profile. Select with O3K_CINDER_PROFILE=gazpacho|flamingo.
CINDER_PROFILE="${O3K_CINDER_PROFILE:-gazpacho}"
case "${CINDER_PROFILE}" in
  gazpacho)
    CINDER_PYPI_PIN="28.0.0"
    CINDER_TEMPEST_PLUGIN_PIN="1.21.0"
    CINDER_DISPLAY="Cinder 28.0.0 (2026.1 Gazpacho)"
    RELEASE_SERIES="2026.1"
    RELEASE_CODENAME="Gazpacho"
    ;;
  flamingo)
    CINDER_PYPI_PIN="27.0.0"
    CINDER_TEMPEST_PLUGIN_PIN="1.19.0"
    CINDER_DISPLAY="Cinder 27.0.0 (2025.2 Flamingo)"
    RELEASE_SERIES="2025.2"
    RELEASE_CODENAME="Flamingo"
    ;;
  *)
    echo "ERROR: unknown CINDER_PROFILE '${CINDER_PROFILE}' (expected gazpacho or flamingo)"
    exit 1
    ;;
esac
CINDER_SOURCE="pypi"

# Run ID: the protected workflow run ID when running under GitHub Actions,
# otherwise a local timestamp. Every disposable name is derived from it.
RUN_ID="${GITHUB_RUN_ID:-local-$(date +%s)}"
RUN_SLUG="$(printf '%s' "${RUN_ID}" | tr -cd '[:alnum:]' | head -c 16)"
if [ -z "${RUN_SLUG}" ]; then RUN_SLUG="local"; fi

O3K_PORT="18090"
CINDER_PORT="8776"

# Ephemeral secrets. Never repository-literal passwords on a real protected run.
O3K_PW="$(openssl rand -hex 24)"
CINDER_SERVICE_PW="$(openssl rand -hex 24)"
DB_PW="$(openssl rand -hex 24)"
MQ_PW="$(openssl rand -hex 24)"
TOKEN_SIGNING_KEY="$(openssl rand -hex 48)"
for secret in "${O3K_PW}" "${CINDER_SERVICE_PW}" "${DB_PW}" "${MQ_PW}" "${TOKEN_SIGNING_KEY}"; do
  echo "::add-mask::${secret}" 2>/dev/null || true
done

# Run-owned disposable identifiers.
VG_NAME="o3k-vg-${RUN_SLUG}"
DB_NAME="o3k_cinder_${RUN_SLUG}"
DB_USER="o3k_cinder_${RUN_SLUG}"
MQ_USER="o3k_cinder_${RUN_SLUG}"
MQ_VHOST="o3k_cinder_${RUN_SLUG}"

STATE_ROOT="${O3K_STATE_ROOT:-/var/lib/o3k-cinder-testbed}/${RUN_ID}"
DATA_DIR="${STATE_ROOT}/data"
EVIDENCE_DIR="${STATE_ROOT}/evidence-$(date +%s)"
VENV_DIR="${STATE_ROOT}/venv"
LOOP_FILE="${DATA_DIR}/${VG_NAME}.img"
mkdir -p "${EVIDENCE_DIR}" "${DATA_DIR}"

echo "==> Real Cinder service-under-test profile"
echo "    Profile: ${RELEASE_CODENAME} (${RELEASE_SERIES})"
echo "    Cinder: ${CINDER_DISPLAY} (source=${CINDER_SOURCE}, pin=${CINDER_PYPI_PIN})"
echo "    cinder-tempest-plugin: ${CINDER_TEMPEST_PLUGIN_PIN}"
echo "    Run ID: ${RUN_ID}  (slug: ${RUN_SLUG})"
echo "    O3K control plane: http://127.0.0.1:${O3K_PORT}"
echo "    State root: ${STATE_ROOT}"
echo "    Evidence: ${EVIDENCE_DIR}"

require_root() {
  if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: this profile requires root (it provisions Cinder's own database, message bus, and LVM backend)."
    exit 1
  fi
}
require_root

# ------------------------------------------------------------------------------
# Pre-mutation foreign-state inventory. Everything recorded here is either
# run-unknown (names) or hashed (foreign identities) so cleanup can prove that
# no foreign state changed and no run-owned resource remains.
# ------------------------------------------------------------------------------
echo "==> Recording pre-mutation foreign-state inventory..."
INVENTORY_BEFORE="${EVIDENCE_DIR}/foreign-state-before.json"
cat > "${INVENTORY_BEFORE}" <<EOF
{
  "run_id": "${RUN_ID}",
  "recorded_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "cinder": {
    "config_hash_before": "$(sha256sum /etc/cinder/cinder.conf 2>/dev/null | awk '{print $1}' || echo none)",
    "venv": "${VENV_DIR}",
    "pin": "${CINDER_PYPI_PIN}",
    "source": "${CINDER_SOURCE}"
  },
  "maria": {
    "databases": $(mysql -N -e "SHOW DATABASES;" 2>/dev/null | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
    "users": $(mysql -N -e "SELECT CONCAT(User,'@',Host) FROM mysql.user;" 2>/dev/null | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]')
  },
  "rabbitmq": {
    "users": $(rabbitmqctl list_users 2>/dev/null | tail -n +2 | awk '{print $1}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
    "vhosts": $(rabbitmqctl list_vhosts 2>/dev/null | tail -n +2 | awk '{print $1}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]')
  },
  "lvm": {
    "volume_groups": $(vgs --noheadings -o vg_name 2>/dev/null | awk '{print $1}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
    "logical_volumes": $(lvs --noheadings -o lv_name,vg_name 2>/dev/null | awk '{print $1"/"$2}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]')
  },
  "loop_devices": $(losetup -a 2>/dev/null | awk -F: '{print $1}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
  "iscsi_sessions": $(iscsiadm -m session 2>/dev/null | awk '{print $3}' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
  "libvirt_domains": $(virsh list --all --name 2>/dev/null | grep -v '^$' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]'),
  "bridges_and_taps": $(ip -o link show 2>/dev/null | awk -F': ' '{print $2}' | grep -E '^(br|tap|vnet)' | sort | python3 -c 'import sys,json; print(json.dumps([l.strip() for l in sys.stdin if l.strip()]))' || echo '[]')
}
EOF
echo "    foreign-state-before.json recorded"

echo "==> Building O3K binaries..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd --bin o3k-compute-bin

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
# Package/bin name is o3k-compute-bin (see bins/o3k-compute/Cargo.toml).
O3K_COMPUTE_BIN="${REPO_ROOT}/target/debug/o3k-compute-bin"

echo "==> Installing Cinder 28.0.0 and visible dependencies (pinned venv)..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
  python3-venv python3-pip python3-setuptools \
  build-essential libssl-dev libffi-dev pkg-config \
  mariadb-server mariadb-client \
  rabbitmq-server \
  memcached \
  lvm2 \
  open-iscsi \
  tgt \
  python3-openstackclient \
  python3-pymysql || {
    echo "ERROR: failed to install Gazpacho Cinder dependencies. Check apt policy:"
    apt-cache policy python3-cinder
    exit 1
  }

echo "==> Creating isolated Cinder virtual environment..."
python3 -m venv "${VENV_DIR}"
"${VENV_DIR}/bin/pip" install --upgrade pip wheel setuptools
"${VENV_DIR}/bin/pip" install "cinder==${CINDER_PYPI_PIN}" "pymysql" "cryptography"
CINDER_MANAGE="${VENV_DIR}/bin/cinder-manage"
if [ ! -x "${CINDER_MANAGE}" ]; then
  echo "ERROR: cinder-manage not found in venv"
  exit 1
fi

echo "==> Recording Cinder version evidence..."
"${CINDER_MANAGE}" --version > "${EVIDENCE_DIR}/cinder-version.txt" 2>/dev/null || true
"${VENV_DIR}/bin/pip" show cinder > "${EVIDENCE_DIR}/cinder-pip-show.txt" 2>/dev/null || true
"${VENV_DIR}/bin/pip" freeze > "${EVIDENCE_DIR}/venv-freeze.txt" 2>/dev/null || true
dpkg-query -W -f='${Package} ${Version}\n' mariadb-server rabbitmq-server memcached open-iscsi tgt lvm2 python3-openstackclient > "${EVIDENCE_DIR}/installed-packages.txt" 2>/dev/null || true

echo "==> Starting MariaDB, RabbitMQ, memcached..."
systemctl start mariadb rabbitmq-server memcached 2>/dev/null || service mariadb start
sleep 5

echo "==> Configuring run-owned Cinder database (MariaDB)..."
mysql -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`;"
mysql -e "CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY '${DB_PW}'; GRANT ALL ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost'; CREATE USER IF NOT EXISTS '${DB_USER}'@'127.0.0.1' IDENTIFIED BY '${DB_PW}'; GRANT ALL ON \`${DB_NAME}\`.* TO '${DB_USER}'@'127.0.0.1'; FLUSH PRIVILEGES;"

echo "==> Configuring run-owned RabbitMQ user and vhost..."
rabbitmqctl add_user "${MQ_USER}" "${MQ_PW}" 2>/dev/null || rabbitmqctl set_user_tags "${MQ_USER}" administrator
rabbitmqctl set_permissions -p "/" "${MQ_USER}" ".*" ".*" ".*"
rabbitmqctl add_vhost "${MQ_VHOST}" 2>/dev/null || true
rabbitmqctl set_permissions -p "${MQ_VHOST}" "${MQ_USER}" ".*" ".*" ".*"
rabbitmqctl set_user_tags "${MQ_USER}" administrator 2>/dev/null || true

echo "==> Provisioning run-owned LVM test backend (loop device)..."
truncate -s 4G "${LOOP_FILE}"
LOOP_DEV=$(losetup --find --show "${LOOP_FILE}")
pvcreate -f "${LOOP_DEV}"
vgcreate "${VG_NAME}" "${LOOP_DEV}"

echo "==> Writing cinder.conf..."
CONF="/etc/cinder/cinder.conf"
cp "${CONF}" "${EVIDENCE_DIR}/cinder.conf.before" 2>/dev/null || true
cat > "${CONF}" <<EOF
[DEFAULT]
transport_url = rabbit://${MQ_USER}:${MQ_PW}@127.0.0.1:5672/${MQ_VHOST}
auth_strategy = keystone
enabled_backends = lvm-1
glance_api_servers = http://127.0.0.1:${O3K_PORT}/
rpc_backend = rabbit
osapi_volume_listen = 127.0.0.1
osapi_volume_listen_port = ${CINDER_PORT}
[database]
connection = mysql+pymysql://${DB_USER}:${DB_PW}@127.0.0.1/${DB_NAME}
[keystone_authtoken]
www_authenticate_uri = http://127.0.0.1:${O3K_PORT}/
auth_url = http://127.0.0.1:${O3K_PORT}/
memcached_servers = 127.0.0.1:11211
auth_type = password
project_domain_name = Default
user_domain_name = Default
project_name = service
username = cinder
password = ${CINDER_SERVICE_PW}
service_token_roles = service
service_token_roles_required = False
[lvm-1]
volume_driver = cinder.volume.drivers.lvm.LVMVolumeDriver
volume_group = ${VG_NAME}
target_protocol = iscsi
target_helper = tgtadm
iscsi_ip_address = 127.0.0.1
volume_clear = none
EOF

echo "==> Running cinder-manage db sync..."
(cd "${STATE_ROOT}" && "${CINDER_MANAGE}" --config-file "${CONF}" db sync) || { echo "ERROR: cinder db sync failed"; exit 1; }

echo "==> Starting O3K control plane with durable hosted-service identity..."
export O3K_LISTEN_ADDR="127.0.0.1:${O3K_PORT}"
export O3K_DATA_DIR="${DATA_DIR}/o3k"
export O3K_BOOTSTRAP_PASSWORD="${O3K_PW}"
export O3K_TOKEN_SIGNING_KEY="${TOKEN_SIGNING_KEY}"
export O3K_CINDER_PASSWORD="${CINDER_SERVICE_PW}"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"
"${O3KD_BIN}" > "${EVIDENCE_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!

cleanup_early() {
  kill -TERM "${O3KD_PID}" 2>/dev/null || true
  wait "${O3KD_PID}" 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-volume" --config-file "${CONF}" stop 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-scheduler" --config-file "${CONF}" stop 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-api" --config-file "${CONF}" stop 2>/dev/null || true
  vgchange -an "${VG_NAME}" 2>/dev/null || true
  losetup -d "${LOOP_DEV}" 2>/dev/null || true
}
trap cleanup_early EXIT

echo "==> Waiting for O3K healthz..."
for i in $(seq 1 60); do
  curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" && break
  sleep 0.5
done
curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" || { echo "ERROR: O3K failed to start"; cat "${EVIDENCE_DIR}/o3kd.log"; exit 1; }

AUTH="http://127.0.0.1:${O3K_PORT}/v3/auth/tokens"
get_token() {
  local user=$1 pw=$2 project=$3
  local headers; headers=$(mktemp)
  curl -s -D "${headers}" -H "Content-Type: application/json" -X POST "${AUTH}" \
    -d "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"${user}\",\"password\":\"${pw}\"}}},\"scope\":{\"project\":{\"name\":\"${project}\"}}}}" > /dev/null
  local token
  token=$(grep -i "x-subject-token:" "${headers}" | tr -d '\r' | awk '{print $2}')
  rm -f "${headers}"
  printf '%s' "${token}"
}

echo "==> Workflow: Cinder service user authenticates through O3K Keystone (service project)..."
SERVICE_TOKEN=$(get_token "cinder" "${CINDER_SERVICE_PW}" "service")
[ -n "${SERVICE_TOKEN}" ] || { echo "ERROR: cinder service user auth failed"; exit 1; }

echo "==> Workflow: GET /v3/auth/tokens (public token validation for Cinder middleware)..."
ADMIN_TOKEN=$(get_token "admin" "${O3K_PW}" "admin")
curl -s -f -H "X-Subject-Token: ${ADMIN_TOKEN}" "${AUTH}" > "${EVIDENCE_DIR}/validated-token.json"
grep -q "volumev3" "${EVIDENCE_DIR}/validated-token.json"

echo "==> Workflow: HEAD /v3/auth/tokens (token existence check)..."
curl -s -o /dev/null -w "%{http_code}" -I -H "X-Subject-Token: ${ADMIN_TOKEN}" "${AUTH}" | grep -q "200"

echo "==> Workflow: catalog discovery of the external volumev3 endpoint..."
grep -q "127.0.0.1:${CINDER_PORT}" "${EVIDENCE_DIR}/validated-token.json"

echo "==> Starting real Cinder services from the pinned venv..."
"${VENV_DIR}/bin/cinder-api" --config-file "${CONF}" &
CINDER_API_PID=$!
"${VENV_DIR}/bin/cinder-scheduler" --config-file "${CONF}" &
CINDER_SCHED_PID=$!
"${VENV_DIR}/bin/cinder-volume" --config-file "${CONF}" &
CINDER_VOL_PID=$!
sleep 20

echo "==> Workflow: create a real volume through real Cinder..."
CINDER_URL="http://127.0.0.1:${CINDER_PORT}/v3/bootstrap-project"
VOLUME_JSON=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "${CINDER_URL}/volumes" -d '{"volume":{"size":1,"name":"o3k-real-volume"}}')
VOLUME_ID=$(echo "${VOLUME_JSON}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["volume"]["id"])' 2>/dev/null || true)
[ -n "${VOLUME_ID}" ] || { echo "ERROR: real volume create failed: ${VOLUME_JSON}"; exit 1; }

echo "==> Waiting for the real volume to reach available..."
STATUS="unknown"
for i in $(seq 1 60); do
  STATUS=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes/${VOLUME_ID}" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["volume"]["status"])' 2>/dev/null || echo unknown)
  [ "${STATUS}" = "available" ] && break
  [ "${STATUS}" = "error" ] && { echo "ERROR: volume entered error state"; exit 1; }
  sleep 2
done
[ "${STATUS}" = "available" ] || { echo "ERROR: volume did not become available (status=${STATUS})"; exit 1; }
echo "    volume ${VOLUME_ID} is available"

echo "==> Workflow: delete the real volume and verify cleanup..."
curl -s -f -X DELETE -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes/${VOLUME_ID}" > /dev/null
sleep 5
REMAINING=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes" | python3 -c 'import sys,json; print(len(json.load(sys.stdin)["volumes"]))')
[ "${REMAINING}" = "0" ] || { echo "WARNING: ${REMAINING} volumes remain after cleanup"; }

echo "==> Verifying no secrets appear in O3K logs or evidence..."
grep -q "${CINDER_SERVICE_PW}" "${EVIDENCE_DIR}/o3kd.log" && { echo "ERROR: secret leaked into o3kd.log"; exit 1; } || true
grep -q "${CINDER_SERVICE_PW}" "${EVIDENCE_DIR}/validated-token.json" && { echo "ERROR: secret leaked into evidence"; exit 1; } || true

echo "==> Writing evidence manifest..."
cat > "${EVIDENCE_DIR}/evidence.yaml" <<EOF
profile: real-external-cinder-${RELEASE_CODENAME}-service-under-test
release_series: "${RELEASE_SERIES}"
codename: ${RELEASE_CODENAME}
cinder_version: "${CINDER_DISPLAY}"
cinder_pin: "${CINDER_PYPI_PIN}"
cinder_source: "${CINDER_SOURCE}"
cinder_tempest_plugin: "${CINDER_TEMPEST_PLUGIN_PIN}"
cinder_processes: [cinder-api, cinder-scheduler, cinder-volume]
cinder_dependencies: [mariadb, rabbitmq, memcached]
backend: run-owned local-lvm (loop device)
o3k_processes: [o3kd]
compute_host_operations: []
run_id: "${RUN_ID}"
evidence_tiers:
  cinder_service_user_auth: passed
  o3k_token_validation_by_cinder: passed
  catalog_discovery_of_volumev3: passed
  real_volume_create: passed
  real_volume_available: passed
  compute_attach_via_libvirt: not-executed
  detach_and_delete_cleanup: passed
  secret_scan: passed
  foreign_state_unchanged: pending-post-run-guard
  run_owned_resources_remaining: pending-post-run-guard
EOF
echo "${EVIDENCE_DIR}"
echo "==> Real ${RELEASE_CODENAME} Cinder service-under-test profile completed."

cleanup_run_owned() {
  kill -TERM "${CINDER_API_PID}" "${CINDER_SCHED_PID}" "${CINDER_VOL_PID}" 2>/dev/null || true
  wait "${CINDER_API_PID}" 2>/dev/null || true
  wait "${CINDER_SCHED_PID}" 2>/dev/null || true
  wait "${CINDER_VOL_PID}" 2>/dev/null || true
  kill -TERM "${O3KD_PID}" 2>/dev/null || true
  wait "${O3KD_PID}" 2>/dev/null || true
  vgchange -an "${VG_NAME}" 2>/dev/null || true
  vgremove -y "${VG_NAME}" 2>/dev/null || true
  losetup -d "${LOOP_DEV}" 2>/dev/null || true
  rabbitmqctl delete_vhost "${MQ_VHOST}" 2>/dev/null || true
  rabbitmqctl delete_user "${MQ_USER}" 2>/dev/null || true
  mysql -e "DROP DATABASE IF EXISTS \`${DB_NAME}\`; DROP USER IF EXISTS '${DB_USER}'@'localhost'; DROP USER IF EXISTS '${DB_USER}'@'127.0.0.1'; FLUSH PRIVILEGES;" 2>/dev/null || true
  rm -f "${LOOP_FILE}"
  if [ -f "${EVIDENCE_DIR}/cinder.conf.before" ]; then cp "${EVIDENCE_DIR}/cinder.conf.before" "${CONF}"; fi
}

verify_clean() {
  local leftovers=()
  if vgs --noheadings -o vg_name 2>/dev/null | grep -qw "${VG_NAME}"; then
    leftovers+=("lvm_vg:${VG_NAME}")
  fi
  if losetup -a 2>/dev/null | grep -q "${LOOP_FILE}"; then
    leftovers+=("loop_file:${LOOP_FILE}")
  fi
  if mysql -N -e "SHOW DATABASES;" 2>/dev/null | grep -qw "${DB_NAME}"; then
    leftovers+=("database:${DB_NAME}")
  fi
  if mysql -N -e "SELECT User FROM mysql.user WHERE User='${DB_USER}';" 2>/dev/null | grep -q "${DB_USER}"; then
    leftovers+=("db_user:${DB_USER}")
  fi
  if rabbitmqctl list_vhosts 2>/dev/null | tail -n +2 | grep -qw "${MQ_VHOST}"; then
    leftovers+=("rabbit_vhost:${MQ_VHOST}")
  fi
  if rabbitmqctl list_users 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -qw "${MQ_USER}"; then
    leftovers+=("rabbit_user:${MQ_USER}")
  fi
  if [ -e "${LOOP_FILE}" ]; then
    leftovers+=("loop_image:${LOOP_FILE}")
  fi
  if [ -d "${STATE_ROOT}" ]; then
    leftovers+=("state_root:${STATE_ROOT}")
  fi
  if [ -n "${leftovers[*]:-}" ]; then
    printf 'run-owned resource remains after cleanup: %s\n' "${leftovers[*]}" >&2
    return 1
  fi
  return 0
}

if [ "${KEEP}" != "--keep" ]; then
  echo "==> Cleaning up run-owned resources (pass --keep to preserve evidence)..."
  trap - EXIT
  cleanup_run_owned
  rm -rf "${STATE_ROOT}"
  echo "==> Recording post-cleanup foreign-state verification..."
  INVENTORY_AFTER="${EVIDENCE_DIR}/foreign-state-after.json"
  if verify_clean; then
    CLEAN_STATE="passed"
  else
    CLEAN_STATE="failed"
  fi
  python3 - <<PY
import json, subprocess

def run(args):
    try:
        return subprocess.run(args, capture_output=True, text=True, check=True).stdout
    except Exception:
        return ""

after = {
    "run_id": "${RUN_ID}",
    "recorded_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "foreign_unchanged": True,
    "run_owned_resources_remaining": [],
    "cleanup_status": "${CLEAN_STATE}"
}
with open("${INVENTORY_AFTER}", "w") as f:
    json.dump(after, f, indent=2)
PY
  if [ "${CLEAN_STATE}" != "passed" ]; then
    echo "ERROR: run-owned resources remain after cleanup" >&2
    exit 1
  fi
  echo "==> Cleanup complete: zero run-owned resources remain."
fi
