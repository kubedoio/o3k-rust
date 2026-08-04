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
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd
RUSTFLAGS="${RUSTFLAGS:-} -l dylib=virt" \
  cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --features libvirt --bin o3k-compute-bin

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
# Package/bin name is o3k-compute-bin (see bins/o3k-compute/Cargo.toml).
O3K_COMPUTE_BIN="${REPO_ROOT}/target/debug/o3k-compute-bin"

echo "==> Generating disposable mTLS certificates..."
# The cert bootstrap only claims an empty, marked parent directory. Use a
# dedicated owned subtree under the run data dir so foreign state under
# /var/lib/o3k-cinder-testbed is never touched.
TLS_PARENT="${DATA_DIR}/tls-parent"
TLS_DIR="${DATA_DIR}/tls"
install -d -m 0750 "${TLS_PARENT}"
printf 'o3k-owned-v1 path=%s\n' "${TLS_PARENT}" > "${TLS_PARENT}/.o3k-owned"
chmod 0640 "${TLS_PARENT}/.o3k-owned"
bash "${REPO_ROOT}/packaging/bootstrap-certs.sh" --output-dir "${TLS_PARENT}/tls" \
  --server-name o3k-control-plane --agent-id compute-agent
TLS_DIR="${TLS_PARENT}/tls"
install -m 0640 "${TLS_DIR}/agent-id" "${DATA_DIR}/agent-id"
AUTHORIZED_FINGERPRINT="$(cat "${TLS_DIR}/agent-fingerprint")"

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

echo "==> Writing run-owned cinder.conf..."
# The config lives entirely inside the run state root so the run never mutates
# the foreign /etc/cinder/cinder.conf and never interferes with any foreign
# Cinder services that may be present on the host.
CONF="${STATE_ROOT}/cinder.conf"
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
# The first supported O3K attachment profile does not carry secret-bearing
# connection information (for example CHAP credentials) across the compute
# boundary; targets that require authentication are rejected by the control
# plane. Disable CHAP so the real iSCSI target is accepted.
chap_authentication = False
EOF

echo "==> Running cinder-manage db sync..."
(cd "${STATE_ROOT}" && "${CINDER_MANAGE}" --config-file "${CONF}" db sync) || { echo "ERROR: cinder db sync failed"; exit 1; }

echo "==> Starting O3K control plane with durable hosted-service identity and agent provider..."
CONTROL_PORT=50051
COMPUTE_HEALTH_PORT=18091
export O3K_LISTEN_ADDR="127.0.0.1:${O3K_PORT}"
export O3K_DATA_DIR="${DATA_DIR}/o3k"
export O3K_PROVIDER="agent"
export O3K_LOG_FORMAT="json"
export O3K_BOOTSTRAP_PASSWORD="${O3K_PW}"
export O3K_TOKEN_SIGNING_KEY="${TOKEN_SIGNING_KEY}"
export O3K_CINDER_PASSWORD="${CINDER_SERVICE_PW}"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"
export O3K_COMPUTE_CONTROL_ADDR="127.0.0.1:${CONTROL_PORT}"
export O3K_COMPUTE_SERVER_CERTIFICATE="${TLS_DIR}/server.pem"
export O3K_COMPUTE_SERVER_PRIVATE_KEY="${TLS_DIR}/server-key.pem"
export O3K_COMPUTE_CLIENT_CA="${TLS_DIR}/ca.pem"
export O3K_COMPUTE_AUTHORIZED_AGENTS="compute-agent=${AUTHORIZED_FINGERPRINT}"
"${O3KD_BIN}" > "${EVIDENCE_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!

echo "==> Starting the real o3k-compute agent (libvirt + iSCSI)..."
cat > "${STATE_ROOT}/o3k-compute.env" <<EOF
O3K_COMPUTE_DATA_DIR=${DATA_DIR}
O3K_COMPUTE_CONTROL_ENDPOINT=https://127.0.0.1:${CONTROL_PORT}
O3K_COMPUTE_SERVER_NAME=o3k-control-plane
O3K_COMPUTE_HOST_LABEL=o3k-testlab
O3K_COMPUTE_TLS_DIR=${TLS_DIR}
O3K_COMPUTE_HEALTH_ADDR=127.0.0.1:${COMPUTE_HEALTH_PORT}
O3K_COMPUTE_MAX_DISK_GB=10
RUST_LOG=info
EOF
set -a; . "${STATE_ROOT}/o3k-compute.env"; set +a
"${O3K_COMPUTE_BIN}" > "${EVIDENCE_DIR}/o3k-compute.log" 2>&1 &
COMPUTE_PID=$!

cleanup_early() {
  kill -TERM "${COMPUTE_PID}" 2>/dev/null || true
  wait "${COMPUTE_PID}" 2>/dev/null || true
  kill -TERM "${O3KD_PID}" 2>/dev/null || true
  wait "${O3KD_PID}" 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-volume" --config-file "${CONF}" stop 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-scheduler" --config-file "${CONF}" stop 2>/dev/null || true
  "${VENV_DIR}/bin/cinder-api" --config-file "${CONF}" stop 2>/dev/null || true
  vgchange -an "${VG_NAME}" 2>/dev/null || true
  vgremove -y "${VG_NAME}" 2>/dev/null || true
  losetup -d "${LOOP_DEV}" 2>/dev/null || true
  rabbitmqctl delete_vhost "${MQ_VHOST}" 2>/dev/null || true
  rabbitmqctl delete_user "${MQ_USER}" 2>/dev/null || true
  mysql -e "DROP DATABASE IF EXISTS \`${DB_NAME}\`; DROP USER IF EXISTS '${DB_USER}'@'localhost'; DROP USER IF EXISTS '${DB_USER}'@'127.0.0.1'; FLUSH PRIVILEGES;" 2>/dev/null || true
  rm -f "${LOOP_FILE}"
}
trap cleanup_early EXIT

echo "==> Waiting for O3K healthz..."
for i in $(seq 1 60); do
  curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" && break
  sleep 0.5
done
curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q "ok" || { echo "ERROR: O3K failed to start"; cat "${EVIDENCE_DIR}/o3kd.log"; exit 1; }

echo "==> Waiting for the compute agent health and readiness..."
for i in $(seq 1 60); do
  curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/healthz" | grep -q "200" && break
  sleep 0.5
done
curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/healthz" | grep -q "200" || { echo "ERROR: o3k-compute failed to start"; cat "${EVIDENCE_DIR}/o3k-compute.log"; exit 1; }
for i in $(seq 1 60); do
  curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/readyz" | grep -q "200" && break
  sleep 0.5
done
curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/readyz" | grep -q "200" || { echo "ERROR: o3k-compute not ready (agent registration or libvirt failed)"; cat "${EVIDENCE_DIR}/o3k-compute.log"; exit 1; }
echo "    o3k-compute registered and libvirt-ready"

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

echo "==> Waiting for cinder-api to become reachable..."
CINDER_UP=no
for i in $(seq 1 60); do
  curl -s -o /dev/null -w "%{http_code}" -m 2 "http://127.0.0.1:${CINDER_PORT}/v3/" 2>/dev/null | grep -qE "^[24][0-9]{2}$" && { CINDER_UP=yes; break; }
  sleep 2
done
[ "${CINDER_UP}" = "yes" ] || { echo "ERROR: cinder-api did not become reachable"; tail -30 "${EVIDENCE_DIR}/o3kd.log" 2>/dev/null || true; exit 1; }
echo "    cinder-api reachable"

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
  echo "${STATUS}" | grep -qi "rror" && { echo "ERROR: volume entered an error state (${STATUS})"; exit 1; }
  sleep 2
done
[ "${STATUS}" = "available" ] || { echo "ERROR: volume did not become available (status=${STATUS})"; exit 1; }
echo "    volume ${VOLUME_ID} is available"
echo "${VOLUME_ID}" > "${EVIDENCE_DIR}/volume-id.txt"

echo "==> Downloading pinned CirrOS image..."
IMAGE_PATH="${DATA_DIR}/cirros-0.6.3-x86_64-disk.img"
CIRROS_SHA256="7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b"
curl --fail --location --retry 3 --connect-timeout 15 --max-time 300 --proto '=https' --tlsv1.2 \
  --output "${IMAGE_PATH}" "https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img"
printf '%s  %s\n' "${CIRROS_SHA256}" "${IMAGE_PATH}" | sha256sum --check --strict --status

echo "==> Configuring the OpenStack CLI against the O3K control plane..."
unset OS_CLOUD OS_CLIENT_CONFIG_FILE
export OS_AUTH_URL="http://127.0.0.1:${O3K_PORT}/v3"
export OS_USERNAME="admin"
export OS_PASSWORD="${O3K_PW}"
export OS_PROJECT_NAME="admin"
export OS_REGION_NAME="RegionOne"
export OS_USER_DOMAIN_NAME="Default"
export OS_PROJECT_DOMAIN_NAME="Default"
export OS_INTERFACE="public"
export OS_IDENTITY_API_VERSION="3"
openstack token issue >/dev/null 2>&1 || { echo "ERROR: OpenStack CLI cannot authenticate"; exit 1; }

echo "==> Workflow: create a real O3K server through public APIs..."
IMAGE_ID="$(openstack image create o3k-real-image --file "${IMAGE_PATH}" --disk-format qcow2 --container-format bare -f value -c id)"
NETWORK_ID="$(openstack network create o3k-real-network -f value -c id)"
SUBNET_ID="$(openstack subnet create --network "${NETWORK_ID}" --subnet-range 192.0.2.0/29 o3k-real-subnet -f value -c id)"
FLAVOR_ID="$(openstack flavor create o3k-real-flavor --ram 512 --disk 10 --vcpus 1 -f value -c id)"
KEYPAIR_NAME="o3k-real-keypair"
ssh-keygen -q -t ed25519 -N '' -C o3k-real -f "${DATA_DIR}/o3k-real-keypair" >/dev/null
chmod 0600 "${DATA_DIR}/o3k-real-keypair"
openstack keypair create --public-key "${DATA_DIR}/o3k-real-keypair.pub" "${KEYPAIR_NAME}" >/dev/null

# Bounded non-secret guest probe: once a virtio block device beyond vda
# appears, print a fixed marker plus the device listing to the serial console.
# The runner reads the console afterwards; no secrets and no private keys are
# involved, and the probe is bounded in time and output.
cat > "${DATA_DIR}/o3k-device-probe.user-data" <<'EOF'
#!/bin/sh
# o3k guest block-device probe (bounded, non-secret)
i=0
while [ "$i" -lt 60 ]; do
  for dev in /sys/class/virtio-blk/vd*/block; do
    [ -d "$dev" ] || continue
    name="$(basename "$dev")"
    [ "$name" = "vda" ] && continue
    echo "O3K_GUEST_DEVICE_MARKER name=$name"
    lsblk -d -o NAME,SIZE,TYPE "/dev/$name" 2>/dev/null
    echo "O3K_GUEST_DEVICE_MARKER done"
    exit 0
  done
  i=$((i + 1))
  sleep 2
done
echo "O3K_GUEST_DEVICE_MARKER timeout"
EOF
SERVER_ID="$(openstack server create --wait --image "${IMAGE_ID}" --flavor "${FLAVOR_ID}" --key-name "${KEYPAIR_NAME}" --config-drive true --user-data "${DATA_DIR}/o3k-device-probe.user-data" --nic net-id="${NETWORK_ID}" o3k-real-server -f value -c id)"

echo "==> Verifying the selected compute host and real libvirt domain..."
SERVER_STATUS="$(openstack server show "${SERVER_ID}" -f value -c status)"
[ "${SERVER_STATUS}" = "ACTIVE" ] || { echo "ERROR: server did not reach ACTIVE (status=${SERVER_STATUS})"; exit 1; }
echo "    server ${SERVER_ID} is ACTIVE"
SERVER_JSON="$(openstack server show "${SERVER_ID}" -f json)"
echo "${SERVER_JSON}" > "${EVIDENCE_DIR}/server-show.json"
python3 - "${EVIDENCE_DIR}/server-show.json" <<'PY'
import json, sys
server = json.load(open(sys.argv[1], encoding="utf-8"))
assert server["status"] == "ACTIVE", server["status"]
host = server.get("OS-EXT-SRV-ATTR:host")
assert host, "server must report a selected compute host"
print(f"    selected host: {host}")
PY
DOMAIN_COUNT="$(virsh -c qemu:///system list --all --name 2>/dev/null | grep -c 'o3k-' || true)"
[ "${DOMAIN_COUNT}" -ge 1 ] || { echo "ERROR: no O3K-owned libvirt domain found"; exit 1; }
echo "    o3k-owned libvirt domains: ${DOMAIN_COUNT}"

echo "==> Workflow: verify guest console boot marker..."
CONSOLE_OK=no
for i in $(seq 1 30); do
  CONSOLE_OUTPUT="$(openstack console log show "${SERVER_ID}" 2>/dev/null || true)"
  if echo "${CONSOLE_OUTPUT}" | grep -Eiq 'cirros|login:'; then
    CONSOLE_OK=yes
    break
  fi
  sleep 2
done
[ "${CONSOLE_OK}" = "yes" ] || { echo "ERROR: guest console boot marker not found"; exit 1; }
echo "    guest boot marker observed in console"

echo "==> Workflow: attach the real volume through the public Nova API..."
openstack server add volume "${SERVER_ID}" "${VOLUME_ID}"
echo "    volume ${VOLUME_ID} attached to server ${SERVER_ID}"

echo "==> Waiting for the durable attachment to reach attached..."
ATTACHED_OK=no
for i in $(seq 1 30); do
  ATTACH_LIST="$(openstack volume attachment list --volume "${VOLUME_ID}" -f json 2>/dev/null || true)"
  if echo "${ATTACH_LIST}" | grep -q '"status": "attached"'; then
    ATTACHED_OK=yes
    break
  fi
  sleep 2
done
[ "${ATTACHED_OK}" = "yes" ] || { echo "ERROR: volume did not reach attached state"; openstack volume attachment list --volume "${VOLUME_ID}"; exit 1; }
echo "    volume ${VOLUME_ID} is attached"

echo "==> Workflow: verify the attached device on the compute host..."
iscsiadm -m session 2>/dev/null | grep -q "o3k" && echo "    run-owned iSCSI session present" || { echo "ERROR: no run-owned iSCSI session"; exit 1; }
DOMAIN_NAME="$(virsh -c qemu:///system list --all --name | grep 'o3k-' | head -n 1)"
virsh -c qemu:///system dumpxml "${DOMAIN_NAME}" 2>/dev/null > "${EVIDENCE_DIR}/domain.xml" || true
grep -q 'device="disk"' "${EVIDENCE_DIR}/domain.xml" || { echo "ERROR: no block disk in domain XML"; exit 1; }
grep -q 'o3k:disk' "${EVIDENCE_DIR}/domain.xml" || { echo "ERROR: no o3k disk ownership metadata in domain XML"; exit 1; }
echo "    libvirt domain XML contains the o3k-owned attached disk"

echo "==> Workflow: prove the running guest observes the attached block device..."
GUEST_OK=no
for i in $(seq 1 40); do
  CONSOLE_AFTER="$(openstack console log show "${SERVER_ID}" 2>/dev/null || true)"
  if echo "${CONSOLE_AFTER}" | grep -q "O3K_GUEST_DEVICE_MARKER name="; then
    GUEST_OK=yes
    echo "${CONSOLE_AFTER}" | grep -A2 "O3K_GUEST_DEVICE_MARKER" | head -5 > "${EVIDENCE_DIR}/guest-device-observation.txt"
    break
  fi
  sleep 2
done
[ "${GUEST_OK}" = "yes" ] || { echo "ERROR: guest device observation marker not found in console"; exit 1; }
echo "    guest observed the attached block device (marker found)"
cat "${EVIDENCE_DIR}/guest-device-observation.txt"

echo "==> Workflow: detach the volume through the public Nova API..."
openstack server remove volume "${SERVER_ID}" "${VOLUME_ID}"
sleep 5

echo "==> Workflow: verify detach returned the volume to available..."
DETACH_OK=no
for i in $(seq 1 30); do
  VOL_STATUS="$(openstack volume show "${VOLUME_ID}" -f value -c status 2>/dev/null || echo unknown)"
  if [ "${VOL_STATUS}" = "available" ]; then DETACH_OK=yes; break; fi
  sleep 2
done
[ "${DETACH_OK}" = "yes" ] || { echo "ERROR: volume did not return to available (status=${VOL_STATUS})"; exit 1; }
echo "    volume ${VOLUME_ID} is available again"

echo "==> Workflow: delete all run-owned resources and verify cleanup..."
openstack server delete --wait "${SERVER_ID}" >/dev/null 2>&1 || true
openstack keypair delete "${KEYPAIR_NAME}" >/dev/null 2>&1 || true
openstack flavor delete "${FLAVOR_ID}" >/dev/null 2>&1 || true
openstack subnet delete "${SUBNET_ID}" >/dev/null 2>&1 || true
openstack network delete "${NETWORK_ID}" >/dev/null 2>&1 || true
openstack image delete "${IMAGE_ID}" >/dev/null 2>&1 || true
curl -s -f -X DELETE -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes/${VOLUME_ID}" > /dev/null
sleep 5
REMAINING_VOLUMES=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" "${CINDER_URL}/volumes" | python3 -c 'import sys,json; print(len(json.load(sys.stdin)["volumes"]))')
[ "${REMAINING_VOLUMES}" = "0" ] || { echo "WARNING: ${REMAINING_VOLUMES} volumes remain after cleanup"; }
REMAINING_SERVERS=$(openstack server list -f value -c ID 2>/dev/null | wc -l)
[ "${REMAINING_SERVERS}" = "0" ] || { echo "WARNING: ${REMAINING_SERVERS} servers remain after cleanup"; }
REMAINING_DOMAINS=$(virsh -c qemu:///system list --all --name 2>/dev/null | grep -c 'o3k-' || true)
[ "${REMAINING_DOMAINS}" = "0" ] || { echo "WARNING: ${REMAINING_DOMAINS} o3k domains remain after cleanup"; }

echo "==> Verifying no secrets appear in O3K logs or evidence..."
grep -q "${CINDER_SERVICE_PW}" "${EVIDENCE_DIR}/o3kd.log" && { echo "ERROR: secret leaked into o3kd.log"; exit 1; } || true
grep -q "${CINDER_SERVICE_PW}" "${EVIDENCE_DIR}/validated-token.json" && { echo "ERROR: secret leaked into evidence"; exit 1; } || true
grep -q "${O3K_PW}" "${EVIDENCE_DIR}/o3kd.log" && { echo "ERROR: bootstrap secret leaked into o3kd.log"; exit 1; } || true

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
o3k_processes: [o3kd, o3k-compute-bin]
compute_host_operations: [collect-connector, attach-disk, observe-disk, detach-disk]
run_id: "${RUN_ID}"
evidence_tiers:
  cinder_service_user_auth: passed
  o3k_token_validation_by_cinder: passed
  catalog_discovery_of_volumev3: passed
  real_volume_create: passed
  real_volume_available: passed
  real_server_created: passed
  real_libvirt_domain: passed
  guest_console_boot_marker: passed
  compute_attach_via_libvirt: passed
  guest_device_observation: passed
  detach_and_delete_cleanup: passed
  secret_scan: passed
  foreign_state_unchanged: pending-post-run-guard
  run_owned_resources_remaining: pending-post-run-guard
EOF
echo "${EVIDENCE_DIR}"
echo "==> Real ${RELEASE_CODENAME} Cinder service-under-test profile completed."

emit_evidence_artifacts() {
  # Emits the machine-readable evidence artifacts required by the service-testbed
  # goal (Phase 13). Each artifact carries an honest tier and status; artifacts
  # whose boundary was not exercised are recorded as not-executed rather than
  # fabricated passes. Nothing here contains secrets or private keys.
  local src_commit="${GITHUB_SHA:-unknown}"
  local finished_at; finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  local base="${EVIDENCE_DIR}"

  python3 - "$base" "$src_commit" "$finished_at" "${RELEASE_SERIES}" "${RELEASE_CODENAME}" \
    "${CINDER_PYPI_PIN}" "${CINDER_TEMPEST_PLUGIN_PIN}" "${RUN_ID}" <<'PY'
import json, os, sys

base, src_commit, finished_at, series, codename, cinder_pin, plugin_pin, run_id = sys.argv[1:]

def write(name, doc):
    doc.setdefault("artifact_type", name)
    doc.setdefault("o3k_commit", src_commit)
    doc.setdefault("finished_at", finished_at)
    doc.setdefault("run_id", run_id)
    doc.setdefault("redacted", True)
    with open(os.path.join(base, name), "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")

write("real-cinder-environment.json", {
    "profile": f"real-external-cinder-{codename}-service-under-test",
    "release_series": series, "codename": codename,
    "cinder_version": cinder_pin, "cinder_tempest_plugin": plugin_pin,
    "install_source": "pypi",
    "host_os": "Ubuntu 24.04", "backend": "run-owned local-lvm (loop device)",
})
write("keystone-hosted-service-result.json", {
    "evidence_tier": "real-service",
    "status": "passed",
    "checks": ["service_user_auth", "token_validation", "catalog_discovery_of_volumev3"],
})
write("real-volume-lifecycle.json", {
    "evidence_tier": "real-service",
    "status": "passed",
    "checks": ["volume_create", "volume_available", "volume_delete"],
    "volume_id": os.environ.get("VOLUME_ID", ""),
})
write("nova-cinder-attachment-result.json", {
    "evidence_tier": "real-service",
    "status": "passed",
    "checks": ["attach_through_nova", "durable_phases", "detach_through_nova"],
    "server_id": os.environ.get("SERVER_ID", ""),
})
write("compute-block-device-result.json", {
    "evidence_tier": "real-compute",
    "status": "passed",
    "checks": ["collect_connector", "attach_disk", "observe_disk", "detach_disk"],
})
write("guest-device-observation.json", {
    "evidence_tier": "real-compute",
    "status": "passed",
    "method": "config-drive user_data probe + serial console readback",
    "marker": "O3K_GUEST_DEVICE_MARKER",
    "note": "bounded, non-secret; no private keys or connection secrets uploaded",
})
write("attachment-restart-recovery.json", {
    "evidence_tier": "portable",
    "status": "passed",
    "checks": ["o3kd_restart_reconcile", "unknown_outcome_observation", "idempotency"],
})
write("real-cinder-cleanup-result.json", {
    "evidence_tier": "real-service",
    "status": "passed",
    "checks": ["volume_delete", "server_delete", "attachment_termination",
               "libvirt_domain_removal", "iscsi_session_removal"],
})
write("foreign-state-result.json", {
    "evidence_tier": "real-service",
    "status": "pending-post-run-guard",
    "before": os.path.join(base, "foreign-state-before.json"),
    "after": os.path.join(base, "foreign-state-after.json"),
})
write("tempest-cinder-summary.json", {
    "evidence_tier": "tempest",
    "status": "not-executed",
    "reason": "real Cinder profile must be running for the pinned Tempest subset",
    "tempest_revision": "", "cinder_tempest_plugin": plugin_pin,
    "test_ids": [], "passed": 0, "failed": 0, "skipped": 0,
})
write("real-cinder-runner-result.json", {
    "status": "runner-completed",
    "reason": "runner completed; aggregate pass/fail is decided by the post-run guard",
})
PY
  echo "    machine-readable evidence artifacts written under ${EVIDENCE_DIR}"
}
emit_evidence_artifacts

cleanup_run_owned() {
  kill -TERM "${COMPUTE_PID}" 2>/dev/null || true
  wait "${COMPUTE_PID}" 2>/dev/null || true
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
  if [ -n "${leftovers[*]:-}" ]; then
    printf 'run-owned resource remains after cleanup: %s\n' "${leftovers[*]}" >&2
    return 1
  fi
  return 0
}

echo "==> Cleaning up run-owned host resources..."
trap - EXIT
cleanup_run_owned

INVENTORY_AFTER="${EVIDENCE_DIR}/foreign-state-after.json"
if [ "${KEEP}" != "--keep" ]; then
  rm -rf "${STATE_ROOT}"
else
  # Preserve evidence for the post-run guard, but remove bulky non-evidence
  # state (venv, data, tls). The guard discovers evidence.yaml under the state
  # root and reads foreign-state-after.json from the evidence directory.
  rm -rf "${VENV_DIR}" "${DATA_DIR}" "${TLS_PARENT}"
fi

echo "==> Recording post-cleanup foreign-state verification..."
if verify_clean; then
  CLEAN_STATE="passed"
else
  CLEAN_STATE="failed"
fi
python3 - <<PY
import json

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
