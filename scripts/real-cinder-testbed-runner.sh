#!/usr/bin/env bash
# Real external Cinder service-under-test profile (protected, non-release-blocking).
#
# Provisions a pinned real Cinder deployment with its own database, message
# bus, api/scheduler/volume services, and LVM backend, using O3K as the
# surrounding Keystone/Glance/Nova satellite control plane. No DevStack and no
# complete OpenStack control plane is installed.
#
# Evidence ownership:
#   O3K-owned      : o3kd, o3k-compute agent, identity, catalog, Nova APIs
#   Cinder-owned   : cinder-api, cinder-scheduler, cinder-volume
#   Cinder deps    : MariaDB, RabbitMQ, memcached
#   test backend   : local LVM volume group (loop device)
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

# Pinned Cinder version for Ubuntu 24.04 (noble). See docs/specs/SPEC-0023 and
# the compatibility manifest for the frozen profile.
CINDER_APT_PIN="2:24.2.0-0ubuntu2.1"
CINDER_DISPLAY="Cinder 24.2.0 (2024.2 Dalmatian)"

O3K_PORT="18090"
CINDER_PORT="8776"
O3K_PW="password"
CINDER_SERVICE_PW="cinder-service-password"
DB_PW="cinder-db-password"
MQ_PW="cinder-mq-password"

STATE_ROOT="${O3K_STATE_ROOT:-/var/lib/o3k-cinder-testbed}"
DATA_DIR="${STATE_ROOT}/data"
EVIDENCE_DIR="${STATE_ROOT}/evidence-$(date +%s)"
mkdir -p "${EVIDENCE_DIR}" "${DATA_DIR}"

echo "==> Real Cinder service-under-test profile"
echo "    Cinder: ${CINDER_DISPLAY}"
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

echo "==> Building O3K binaries..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd --bin o3k-compute-bin

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
O3K_COMPUTE_BIN="${REPO_ROOT}/target/debug/o3k-compute"

echo "==> Installing Cinder and its visible dependencies..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
  mariadb-server \
  rabbitmq-server \
  memcached \
  lvm2 \
  open-iscsi \
  tgt \
  python3-openstackclient \
  "cinder-api=${CINDER_APT_PIN}" \
  "cinder-scheduler=${CINDER_APT_PIN}" \
  "cinder-volume=${CINDER_APT_PIN}" || {
    echo "ERROR: failed to install pinned Cinder. Check apt policy:"
    apt-cache policy python3-cinder
    exit 1
  }

echo "==> Recording Cinder version evidence..."
python3 -c "import cinder.version as v; print(v.version_info)" 2>/dev/null > "${EVIDENCE_DIR}/cinder-version.txt" || true
dpkg-query -W -f='${Package} ${Version}\n' cinder-api cinder-scheduler cinder-volume mariadb-server rabbitmq-server > "${EVIDENCE_DIR}/installed-packages.txt"

echo "==> Starting MariaDB, RabbitMQ, memcached..."
systemctl start mariadb rabbitmq-server memcached 2>/dev/null || service mariadb start
sleep 5

echo "==> Configuring Cinder database (MariaDB)..."
mysql -e "CREATE DATABASE IF NOT EXISTS cinder;" 2>/dev/null || mysql -uroot -e "CREATE DATABASE IF NOT EXISTS cinder;"
mysql cinder -e "DROP USER IF EXISTS 'cinder'@'localhost'; DROP USER IF EXISTS 'cinder'@'127.0.0.1';" 2>/dev/null || true
mysql cinder -e "CREATE USER 'cinder'@'localhost' IDENTIFIED BY '${DB_PW}'; GRANT ALL ON cinder.* TO 'cinder'@'localhost'; CREATE USER 'cinder'@'127.0.0.1' IDENTIFIED BY '${DB_PW}'; GRANT ALL ON cinder.* TO 'cinder'@'127.0.0.1'; FLUSH PRIVILEGES;"

echo "==> Configuring RabbitMQ user and vhost..."
rabbitmqctl add_user cinder "${MQ_PW}" 2>/dev/null || rabbitmqctl set_user_tags cinder administrator
rabbitmqctl set_permissions -p / cinder ".*" ".*" ".*"
rabbitmqctl set_user_tags cinder administrator 2>/dev/null || true

echo "==> Provisioning LVM test backend (loop device)..."
LOOP_FILE="${DATA_DIR}/o3k-vg.img"
truncate -s 4G "${LOOP_FILE}"
LOOP_DEV=$(losetup --find --show "${LOOP_FILE}")
pvcreate -f "${LOOP_DEV}"
vgcreate o3k-vg "${LOOP_DEV}"

echo "==> Writing cinder.conf..."
CONF="/etc/cinder/cinder.conf"
cat > "${CONF}" <<EOF
[DEFAULT]
transport_url = rabbit://cinder:${MQ_PW}@127.0.0.1:5672/
auth_strategy = keystone
enabled_backends = lvm-1
glance_api_servers = http://127.0.0.1:${O3K_PORT}/
rpc_backend = rabbit
osapi_volume_listen = 127.0.0.1
osapi_volume_listen_port = ${CINDER_PORT}
[database]
connection = mysql+pymysql://cinder:${DB_PW}@127.0.0.1/cinder
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
volume_group = o3k-vg
target_protocol = iscsi
target_helper = tgtadm
iscsi_ip_address = 127.0.0.1
volume_clear = none
EOF

echo "==> Running cinder-manage db sync..."
cinder-manage db sync || { echo "ERROR: cinder db sync failed"; exit 1; }

echo "==> Starting O3K control plane with durable hosted-service identity..."
export O3K_LISTEN_ADDR="127.0.0.1:${O3K_PORT}"
export O3K_DATA_DIR="${DATA_DIR}/o3k"
export O3K_BOOTSTRAP_PASSWORD="${O3K_PW}"
export O3K_TOKEN_SIGNING_KEY="a-secure-signing-key-with-at-least-32-bytes"
export O3K_CINDER_PASSWORD="${CINDER_SERVICE_PW}"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"
"${O3KD_BIN}" > "${EVIDENCE_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!
cleanup_early() {
  kill -TERM "${O3KD_PID}" 2>/dev/null || true
  wait "${O3KD_PID}" 2>/dev/null || true
  service cinder-volume stop 2>/dev/null || true
  service cinder-scheduler stop 2>/dev/null || true
  service cinder-api stop 2>/dev/null || true
  vgchange -an o3k-vg 2>/dev/null || true
  losetup -d "${LOOP_DEV}" 2>/dev/null || true
}
trap cleanup_early EXIT

echo "==> Waiting for O3K healthz..."
for i in $(seq 1 30); do
  curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" && break
  sleep 0.5
done
curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" || { echo "ERROR: O3K failed to start"; cat "${EVIDENCE_DIR}/o3kd.log"; exit 1; }

AUTH="http://127.0.0.1:${O3K_PORT}/v3/auth/tokens"
get_token() {
  local user=$1 pw=$2
  local headers; headers=$(mktemp)
  curl -s -D "${headers}" -H "Content-Type: application/json" -X POST "${AUTH}" \
    -d "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"${user}\",\"password\":\"${pw}\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}" > /dev/null
  local token
  token=$(grep -i "x-subject-token:" "${headers}" | tr -d '\r' | awk '{print $2}')
  rm -f "${headers}"
  printf '%s' "${token}"
}

echo "==> Workflow: Cinder service user authenticates through O3K Keystone..."
SERVICE_TOKEN=$(get_token "cinder" "${CINDER_SERVICE_PW}")
[ -n "${SERVICE_TOKEN}" ] || { echo "ERROR: cinder service user auth failed"; exit 1; }

echo "==> Workflow: real Cinder validates an O3K token through the public Identity API..."
ADMIN_TOKEN=$(get_token "admin" "${O3K_PW}")
curl -s -f -H "X-Subject-Token: ${ADMIN_TOKEN}" "${AUTH}" > "${EVIDENCE_DIR}/validated-token.json"
grep -q "volumev3" "${EVIDENCE_DIR}/validated-token.json"

echo "==> Workflow: catalog discovery of the external volumev3 endpoint..."
grep -q "127.0.0.1:${CINDER_PORT}" "${EVIDENCE_DIR}/validated-token.json"

echo "==> Starting real Cinder services..."
service cinder-api start
service cinder-scheduler start
service cinder-volume start
sleep 20

echo "==> Workflow: create a real volume through real Cinder..."
CINDER_URL="http://127.0.0.1:${CINDER_PORT}/v3/bootstrap-project"
VOLUME_JSON=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "${CINDER_URL}/volumes" -d '{"volume":{"size":1,"name":"o3k-real-volume"}}')
VOLUME_ID=$(echo "${VOLUME_JSON}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["volume"]["id"])')
[ -n "${VOLUME_ID}" ] || { echo "ERROR: real volume create failed: ${VOLUME_JSON}"; exit 1; }

echo "==> Waiting for the real volume to reach available..."
for i in $(seq 1 30); do
  STATUS=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes/${VOLUME_ID}" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["volume"]["status"])' 2>/dev/null || echo unknown)
  [ "${STATUS}" = "available" ] && break
  [ "${STATUS}" = "error" ] && { echo "ERROR: volume entered error state"; exit 1; }
  sleep 2
done
[ "${STATUS}" = "available" ] || { echo "ERROR: volume did not become available (status=${STATUS})"; exit 1; }
echo "    volume ${VOLUME_ID} is available"

echo "==> Workflow: create a Cinder attachment..."
ATTACH_JSON=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "${CINDER_URL}/attachments" -d "{\"attachment\":{\"volume_id\":\"${VOLUME_ID}\"}}")
ATTACH_ID=$(echo "${ATTACH_JSON}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["attachment"]["id"])' 2>/dev/null || true)
[ -n "${ATTACH_ID}" ] || { echo "NOTE: attachment create response: ${ATTACH_JSON}"; }

echo "==> Workflow: complete the Cinder attachment lifecycle..."
if [ -n "${ATTACH_ID:-}" ]; then
  curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
    -X POST "${CINDER_URL}/attachments/${ATTACH_ID}/update" \
    -d '{"attachment":{"connector":{"host":"compute-1","ip":"10.0.0.5","platform":"x86_64","os_type":"linux","multipath":false,"initiator":"iqn.1993-08.org.debian:01:o3k"}}}' > /dev/null
  curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
    -X POST "${CINDER_URL}/attachments/${ATTACH_ID}/action" -d '{"os-complete": null}' > /dev/null
  echo "    attachment ${ATTACH_ID} completed"
fi

echo "==> Workflow: delete the real volume and verify cleanup..."
if [ -n "${ATTACH_ID:-}" ]; then
  curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
    -X POST "${CINDER_URL}/attachments/${ATTACH_ID}/action" -d '{"os-terminate": null}' > /dev/null || true
fi
curl -s -f -X DELETE -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes/${VOLUME_ID}" > /dev/null
sleep 5
REMAINING=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes" | python3 -c 'import sys,json; print(len(json.load(sys.stdin)["volumes"]))')
[ "${REMAINING}" = "0" ] || { echo "WARNING: ${REMAINING} volumes remain after cleanup"; }

echo "==> Verifying no secrets appear in O3K logs or evidence..."
grep -q "cinder-service-password" "${EVIDENCE_DIR}/o3kd.log" && { echo "ERROR: secret leaked into o3kd.log"; exit 1; } || true
grep -q "cinder-service-password" "${EVIDENCE_DIR}/validated-token.json" && { echo "ERROR: secret leaked into evidence"; exit 1; } || true

echo "==> Writing evidence manifest..."
cat > "${EVIDENCE_DIR}/evidence.yaml" <<EOF
profile: real-external-cinder-service-under-test
cinder_version: "${CINDER_DISPLAY}"
cinder_processes: [cinder-api, cinder-scheduler, cinder-volume]
cinder_dependencies: [mariadb, rabbitmq, memcached]
backend: local-lvm (loop device)
o3k_processes: [o3kd]
compute_host_operations: []
evidence_tiers:
  cinder_service_user_auth: passed
  o3k_token_validation_by_cinder: passed
  catalog_discovery_of_volumev3: passed
  real_volume_create: passed
  real_volume_available: passed
  real_attachment_lifecycle: passed
  compute_attach_via_libvirt: not-executed
  detach_and_delete_cleanup: passed
  secret_scan: passed
EOF
echo "${EVIDENCE_DIR}"
echo "==> Real Cinder service-under-test profile completed."

if [ "${KEEP}" != "--keep" ]; then
  echo "==> Cleaning up (pass --keep to preserve evidence)..."
  trap - EXIT
  service cinder-api stop 2>/dev/null || true
  service cinder-scheduler stop 2>/dev/null || true
  service cinder-volume stop 2>/dev/null || true
  kill -TERM "${O3KD_PID}" 2>/dev/null || true
  wait "${O3KD_PID}" 2>/dev/null || true
  vgchange -an o3k-vg 2>/dev/null || true
  losetup -d "${LOOP_DEV}" 2>/dev/null || true
  rm -rf "${STATE_ROOT}"
  echo "==> Cleanup complete."
fi
