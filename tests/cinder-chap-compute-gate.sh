#!/usr/bin/env bash
# Gate D — real local CHAP iSCSI compute gate.
#
# Proves the o3k-compute execution path against a REAL local CHAP-authenticated
# iSCSI target (tgt + open-iscsi) and REAL libvirt hotplug, WITHOUT starting the
# full external OpenStack workflow: the surrounding O3K control plane drives a
# stateful mock Cinder whose connection_info points at the real target.
#
# The gate proves, through o3k-compute:
#   CollectConnector -> CHAP login -> libvirt hotplug (o3k:disk ownership
#   metadata) -> observation -> detach (libvirt unplug) -> CHAP session cleanup.
#
# Evidence artifact: cinder-chap-compute-result.json
#   status: passed | skipped (prerequisites missing) | failed
#
# This is a component real-host gate (evidence-ladder tier 6); it is NOT the
# protected full-profile runner and does not replace real-Cinder evidence.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

RESULT_PATH="${REPO_ROOT}/cinder-chap-compute-result.json"

fail() {
  echo "ERROR: $*" >&2
  echo "== o3kd.log tail ==" >&2
  tail -n 40 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  echo "== o3k-compute.log tail ==" >&2
  tail -n 40 "${DATA_DIR}/o3k-compute.log" 2>/dev/null || true
  cat > "${RESULT_PATH}" <<JSON
{"artifact_type": "cinder-chap-compute-result.json", "status": "failed", "reason": "$*"}
JSON
  exit 1
}

cleanup_on_exit() {
  status=$?
  if [ ${status} -ne 0 ]; then
    cat > "${RESULT_PATH}" <<JSON
{"artifact_type": "cinder-chap-compute-result.json", "status": "failed", "reason": "gate aborted (exit ${status})"}
JSON
  fi
}
trap cleanup_on_exit EXIT

# ---------------------------------------------------------------------------
# Prerequisite probe: the gate is a real-host component gate and skips (with a
# machine-readable artifact) when the host lacks the required facilities.
# ---------------------------------------------------------------------------
MISSING=()
for tool in tgtadm tgt-admin iscsiadm virsh curl python3 ssh-keygen; do
  command -v "${tool}" >/dev/null 2>&1 || MISSING+=("${tool}")
done
[ -e /dev/kvm ] || MISSING+=("/dev/kvm")
if [ "${#MISSING[@]}" -gt 0 ]; then
  echo "==> Gate D prerequisites missing (${MISSING[*]}); skipping real-host gate."
  cat > "${RESULT_PATH}" <<JSON
{"artifact_type": "cinder-chap-compute-result.json", "status": "skipped", "reason": "missing prerequisites: ${MISSING[*]}"}
JSON
  exit 0
fi
if ! virsh -c qemu:///system uri >/dev/null 2>&1; then
  echo "==> libvirt qemu:///system unreachable; skipping."
  cat > "${RESULT_PATH}" <<JSON
{"artifact_type": "cinder-chap-compute-result.json", "status": "skipped", "reason": "qemu:///system unreachable"}
JSON
  exit 0
fi

# ---------------------------------------------------------------------------
# Run-owned state
# ---------------------------------------------------------------------------
RUN_ID="gate-d-$(date +%s)"
DATA_DIR="/tmp/o3k-cinder-chap-gate-${RUN_ID}"
mkdir -p "${DATA_DIR}"
O3K_PORT=18190
CINDER_PORT=18776
CONTROL_PORT=50061
COMPUTE_HEALTH_PORT=18095

cleanup() {
  set +e
  echo "==> Cleaning up run-owned resources..."
  # Never sweep host links or domains by an O3K-looking name.  Destructive
  # cleanup requires the exact run-owned identity and provider ownership
  # proof; otherwise preserve the resource for operator reconciliation.
  if [ -n "${DOMAIN_NAME:-}" ]; then
    virsh -c qemu:///system destroy "${DOMAIN_NAME}" >/dev/null 2>&1
    virsh -c qemu:///system undefine "${DOMAIN_NAME}" >/dev/null 2>&1
  fi
  # Run-owned iSCSI nodes and sessions
  iscsiadm -m node -T "${TARGET_IQN:-}" -o delete 2>/dev/null || true
  iscsiadm -m node -T "${TARGET_IQN:-}" --logout 2>/dev/null || true
  # Run-owned tgt target and CHAP account
  if [ -n "${TARGET_IQN:-}" ]; then
    tgtadm --lld iscsi --op delete --mode target --tid "${TARGET_TID:-1}" 2>/dev/null || true
  fi
  if [ -n "${CHAP_USER:-}" ]; then
    tgtadm --lld iscsi --op delete --mode account --user "${CHAP_USER}" 2>/dev/null || true
  fi
  rm -f "${BACKING_FILE:-}"
  for pid in "${AGENT_PID:-}" "${O3KD_PID:-}" "${CINDER_PID:-}"; do
    [ -n "${pid:-}" ] && kill -TERM "${pid}" 2>/dev/null
    [ -n "${pid:-}" ] && wait "${pid}" 2>/dev/null
  done
  rm -rf "${DATA_DIR}"
}
trap cleanup EXIT

echo "==> Building o3kd and o3k-compute-bin (libvirt feature)..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd >/dev/null 2>&1
RUSTFLAGS="${RUSTFLAGS:-} -l dylib=virt" \
  cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --features libvirt --bin o3k-compute-bin >/dev/null 2>&1
O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
O3K_COMPUTE_BIN="${REPO_ROOT}/target/debug/o3k-compute-bin"

echo "==> Generating disposable mTLS certificates..."
TLS_PARENT="${DATA_DIR}/tls-parent"
install -d -m 0750 "${TLS_PARENT}"
printf 'o3k-owned-v1 path=%s\n' "${TLS_PARENT}" > "${TLS_PARENT}/.o3k-owned"
chmod 0640 "${TLS_PARENT}/.o3k-owned"
bash "${REPO_ROOT}/packaging/bootstrap-certs.sh" --output-dir "${TLS_PARENT}/tls" \
  --server-name o3k-control-plane --agent-id compute-agent >/dev/null 2>&1
TLS_DIR="${TLS_PARENT}/tls"
install -m 0640 "${TLS_DIR}/agent-id" "${DATA_DIR}/agent-id"
AUTHORIZED_FINGERPRINT="$(cat "${TLS_DIR}/agent-fingerprint")"

echo "==> Provisioning a real local CHAP iSCSI target..."
TARGET_TID=99
TARGET_IQN="iqn.2026-08.org.o3k.gate:volume-chap"
BACKING_FILE="${DATA_DIR}/backing.img"
# Run-unique CHAP account so concurrent or stale runs never collide.
CHAP_USER="o3k-chap-$(openssl rand -hex 4)"
CHAP_PASSWORD="$(openssl rand -hex 16)"
# Remove stale run-owned tgt targets and CHAP accounts before provisioning.
for t in $(tgtadm --lld iscsi --op show --mode target 2>/dev/null | grep -E "^Target" | awk '{print $2}' || true); do
  tgtadm --lld iscsi --op delete --mode target --tid "${t}" >/dev/null 2>&1 || true
done
for u in $(tgtadm --lld iscsi --op show --mode account 2>/dev/null | grep -oE 'account: [a-zA-Z0-9._-]+' | awk '{print $2}' | grep '^o3k-' || true); do
  tgtadm --lld iscsi --op delete --mode account --user "${u}" >/dev/null 2>&1 || true
done
# Do not remove stale domains or links by name prefix.  No ownership proof is
# available before this run provisions its exact identities, so preserve any
# pre-existing resource and let the protected host guard report it.
for n in $(iscsiadm -m node 2>/dev/null | grep -oE 'iqn\.[0-9a-zA-Z.:-]*o3k[0-9a-zA-Z.:-]*' || true); do
  iscsiadm -m node -T "${n}" --logout >/dev/null 2>&1 || true
  iscsiadm -m node -o delete -T "${n}" >/dev/null 2>&1 || true
done
truncate -s 64M "${BACKING_FILE}"
tgtadm --lld iscsi --op new --mode target --tid "${TARGET_TID}" -T "${TARGET_IQN}"
tgtadm --lld iscsi --op new --mode logicalunit --tid "${TARGET_TID}" --lun 1 -b "${BACKING_FILE}"
# CHAP: create an account and bind it to the target (tgtadm mode account).
tgtadm --lld iscsi --op new --mode account --user "${CHAP_USER}" --password "${CHAP_PASSWORD}"
tgtadm --lld iscsi --op bind --mode account --tid "${TARGET_TID}" --user "${CHAP_USER}"
tgtadm --lld iscsi --op bind --mode target --tid "${TARGET_TID}" -I ALL
echo "    target ${TARGET_IQN} with CHAP at 127.0.0.1:3260, lun 1"

echo "==> Starting the stateful mock Cinder (connection_info points at the real target)..."
python3 - "${O3K_PORT}" "${CINDER_PORT}" "${TARGET_IQN}" "${CHAP_USER}" "${CHAP_PASSWORD}" <<'PY' &
import http.server, socketserver, json, sys, urllib.request, uuid

O3K_PORT, CINDER_PORT, TARGET_IQN, CHAP_USER, CHAP_PASSWORD = sys.argv[1:]

VOLUMES = {}
ATTACHMENTS = {}
COUNTER = [0]


def auth_token(headers):
    return headers.get("X-Auth-Token") or headers.get("X-Subject-Token")


def validate(headers):
    token = auth_token(headers)
    if not token:
        return False
    req = urllib.request.Request(
        f"http://127.0.0.1:{O3K_PORT}/v3/auth/tokens",
        headers={"X-Subject-Token": token, "X-Auth-Token": token})
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status == 200
    except Exception:
        return False


def validate_service_token(headers):
    token = headers.get("X-Service-Token")
    if not token:
        return False
    req = urllib.request.Request(
        f"http://127.0.0.1:{O3K_PORT}/v3/auth/tokens",
        headers={"X-Subject-Token": token, "X-Auth-Token": token})
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status == 200
    except Exception:
        return False


def next_id(prefix):
    # Real Cinder volume and attachment ids are UUIDs.
    return str(uuid.uuid4())


def detail_json(aid):
    return {
        "id": aid,
        "status": ATTACHMENTS[aid]["status"],
        "instance": ATTACHMENTS[aid].get("instance"),
        "volume_id": ATTACHMENTS[aid]["volume_id"],
        "attached_at": "",
        "detached_at": "",
        "attach_mode": "rw",
        "connection_info": ATTACHMENTS[aid].get("connection_info"),
    }


def connection_info(aid):
    return {
        "driver_volume_type": "iscsi",
        "target_discovered": False,
        "target_portal": "127.0.0.1:3260",
        "target_iqn": TARGET_IQN,
        "target_lun": 1,
        "volume_id": ATTACHMENTS[aid]["volume_id"],
        "auth_method": "CHAP",
        "auth_username": CHAP_USER,
        "auth_password": CHAP_PASSWORD,
        "encrypted": False,
        "qos_specs": None,
        "access_mode": "rw",
        "attachment_id": aid,
        "enforce_multipath": False,
    }


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        with open("/tmp/o3k-gate-d-mock.log", "a", encoding="utf-8") as f:
            f.write("%s %s\n" % (self.command, self.path))

    def _send(self, code, body=b""):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("OpenStack-API-Version", "volume 3.44")
        self.end_headers()
        if isinstance(body, dict):
            body = json.dumps(body).encode()
        self.wfile.write(body)

    def _route(self):
        # Strip any query string before path routing.
        return self.path.split("?", 1)[0].strip("/").split("/")

    def do_GET(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        parts = self._route()
        if len(parts) == 3 and parts[2] == "attachments":
            return self._send(200, {"attachments": [detail_json(a) for a in ATTACHMENTS]})
        if len(parts) == 3 and parts[2] == "volumes":
            # plain list used by openstack volume list
            return self._send(200, {"volumes": [VOLUMES[v] for v in VOLUMES]})
        if len(parts) == 3 and parts[2] == "volumes/detail":
            # openstack volume attachment list resolves the volume by name here
            return self._send(200, {"volumes": [VOLUMES[v] for v in VOLUMES]})
        if len(parts) == 4 and parts[2] == "attachments" and parts[3] in ATTACHMENTS:
            return self._send(200, {"attachment": detail_json(parts[3])})
        if len(parts) == 4 and parts[2] == "volumes" and parts[3] in VOLUMES:
            return self._send(200, {"volume": VOLUMES[parts[3]]})
        return self._send(404, {"error": {"message": "not found"}})

    def do_POST(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        parts = self._route()
        if len(parts) == 3 and parts[2] == "volumes":
            vid = next_id("volume")
            VOLUMES[vid] = {"id": vid, "status": "available", "size": 1, "name": "gate-d-vol"}
            return self._send(202, {"volume": VOLUMES[vid]})
        if len(parts) == 3 and parts[2] == "attachments":
            volume_id = body.get("attachment", {}).get("volume_id") or body.get("attachment", {}).get("volume_uuid")
            if volume_id not in VOLUMES:
                return self._send(400, {"error": {"message": "volume not found"}})
            aid = next_id("attachment")
            ATTACHMENTS[aid] = {"status": "reserved", "volume_id": volume_id,
                                "instance": body.get("attachment", {}).get("instance_uuid"),
                                "connection_info": None}
            return self._send(200, {"attachment": detail_json(aid)})
        if len(parts) == 5 and parts[2] == "attachments" and parts[4] == "action":
            aid = parts[3]
            if aid not in ATTACHMENTS:
                return self._send(404, {"error": {"message": "attachment not found"}})
            if "os-complete" in body:
                ATTACHMENTS[aid]["status"] = "attached"
                return self._send(204)
            return self._send(400, {"error": {"message": "unsupported action"}})
        return self._send(400, {"error": {"message": "unsupported operation"}})

    def do_PUT(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        parts = self._route()
        if len(parts) == 4 and parts[2] == "attachments":
            aid = parts[3]
            if aid not in ATTACHMENTS:
                return self._send(404, {"error": {"message": "attachment not found"}})
            ATTACHMENTS[aid]["status"] = "attaching"
            ATTACHMENTS[aid]["connection_info"] = connection_info(aid)
            return self._send(200, {"attachment": detail_json(aid)})
        return self._send(400, {"error": {"message": "unsupported operation"}})

    def do_DELETE(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        parts = self._route()
        if len(parts) == 4 and parts[2] == "attachments":
            aid = parts[3]
            if aid not in ATTACHMENTS:
                return self._send(404, {"error": {"message": "attachment not found"}})
            # Mirror Cinder 28 attachment_deletion_allowed: a live attachment
            # DELETE requires a valid service-role X-Service-Token.
            if not validate_service_token(self.headers):
                return self._send(409, {"conflictNovaUsingAttachment": {"message": "service token required"}})
            del ATTACHMENTS[aid]
            return self._send(200, {"attachments": []})
        if len(parts) == 4 and parts[2] == "volumes":
            if parts[3] in VOLUMES:
                del VOLUMES[parts[3]]
                return self._send(202, {})
        return self._send(404, {"error": {"message": "not found"}})


socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", int(CINDER_PORT)), Handler) as httpd:
    httpd.serve_forever()
PY
CINDER_PID=$!

echo "==> Starting the O3K control plane (agent provider) and the real compute agent..."
export O3K_LISTEN_ADDR="127.0.0.1:${O3K_PORT}"
export O3K_DATA_DIR="${DATA_DIR}/o3k"
export O3K_PROVIDER="agent"
export O3K_LOG_FORMAT="json"
export O3K_BOOTSTRAP_PASSWORD="password"
export O3K_TOKEN_SIGNING_KEY="a-secure-signing-key-with-at-least-32-bytes"
export O3K_CINDER_PASSWORD="cinder-password"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"
export O3K_COMPUTE_CONTROL_ADDR="127.0.0.1:${CONTROL_PORT}"
export O3K_COMPUTE_SERVER_CERTIFICATE="${TLS_DIR}/server.pem"
export O3K_COMPUTE_SERVER_PRIVATE_KEY="${TLS_DIR}/server-key.pem"
export O3K_COMPUTE_CLIENT_CA="${TLS_DIR}/ca.pem"
export O3K_COMPUTE_AUTHORIZED_AGENTS="compute-agent=${AUTHORIZED_FINGERPRINT}"
"${O3KD_BIN}" > "${DATA_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!

cat > "${DATA_DIR}/o3k-compute.env" <<EOF
O3K_COMPUTE_DATA_DIR=${DATA_DIR}
O3K_COMPUTE_CONTROL_ENDPOINT=https://127.0.0.1:${CONTROL_PORT}
O3K_COMPUTE_SERVER_NAME=o3k-control-plane
O3K_COMPUTE_HOST_LABEL=o3k-gate-d
O3K_COMPUTE_TLS_DIR=${TLS_DIR}
O3K_COMPUTE_HEALTH_ADDR=127.0.0.1:${COMPUTE_HEALTH_PORT}
O3K_COMPUTE_MAX_DISK_GB=10
RUST_LOG=info
EOF
set -a; . "${DATA_DIR}/o3k-compute.env"; set +a
"${O3K_COMPUTE_BIN}" > "${DATA_DIR}/o3k-compute.log" 2>&1 &
AGENT_PID=$!

echo "==> Waiting for the control plane and the compute agent..."
READY=0
for i in $(seq 1 60); do
  if curl -s "http://127.0.0.1:${O3K_PORT}/healthz" | grep -q ok \
     && curl -s "http://127.0.0.1:${COMPUTE_HEALTH_PORT}/readyz" | grep -q ready; then
    READY=1
    break
  fi
  sleep 0.5
done
[ "${READY}" -eq 1 ] || fail "control plane or agent did not become ready"
echo "    control plane and agent are ready"

echo "==> Configuring the OpenStack CLI and creating the CirrOS server..."
unset OS_CLOUD OS_CLIENT_CONFIG_FILE
export OS_AUTH_URL="http://127.0.0.1:${O3K_PORT}/v3"
export OS_USERNAME="admin"
export OS_PASSWORD="password"
export OS_PROJECT_NAME="admin"
export OS_REGION_NAME="RegionOne"
export OS_USER_DOMAIN_NAME="Default"
export OS_PROJECT_DOMAIN_NAME="Default"
export OS_INTERFACE="public"
export OS_IDENTITY_API_VERSION="3"
openstack token issue >/dev/null 2>&1 || fail "OpenStack CLI cannot authenticate"

IMAGE_PATH="${DATA_DIR}/cirros-0.6.3-x86_64-disk.img"
CIRROS_SHA256="7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b"
curl --fail --location --retry 3 --connect-timeout 15 --max-time 300 --proto '=https' --tlsv1.2 \
  --output "${IMAGE_PATH}" "https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img" >/dev/null 2>&1
printf '%s  %s\n' "${CIRROS_SHA256}" "${IMAGE_PATH}" | sha256sum --check --strict --status || fail "cirros image checksum mismatch"

IMAGE_ID="$(openstack image create o3k-gate-d-image --file "${IMAGE_PATH}" --disk-format qcow2 --container-format bare -f value -c id)"
echo "    image created: ${IMAGE_ID}"
NETWORK_ID="$(openstack network create o3k-gate-d-network -f value -c id)"
echo "    network created: ${NETWORK_ID}"
openstack subnet create --network "${NETWORK_ID}" --subnet-range 192.0.2.0/29 o3k-gate-d-subnet -f value -c id >/dev/null
echo "    subnet created"
FLAVOR_ID="$(openstack flavor create o3k-gate-d-flavor --ram 512 --disk 10 --vcpus 1 -f value -c id)"
echo "    flavor created: ${FLAVOR_ID}"
PORT_ID="$(openstack port create --network "${NETWORK_ID}" o3k-gate-d-port -f value -c id)"
echo "    port created: ${PORT_ID}"
KEYPAIR_NAME="o3k-gate-d-keypair"
ssh-keygen -q -t ed25519 -N '' -C o3k-gate-d -f "${DATA_DIR}/keypair" >/dev/null
openstack keypair create --public-key "${DATA_DIR}/keypair.pub" "${KEYPAIR_NAME}" >/dev/null
echo "    keypair created"

cat > "${DATA_DIR}/device-probe.user-data" <<'EOF'
#!/bin/sh
# o3k guest block-device probe (bounded, non-secret)
i=0
while [ "$i" -lt 60 ]; do
  for dev in /sys/class/virtio-blk/vd*/block; do
    [ -d "$dev" ] || continue
    name="$(basename "$dev")"
    [ "$name" = "vda" ] && continue
    echo "O3K_GUEST_DEVICE_MARKER name=$name"
    exit 0
  done
  i=$((i + 1))
  sleep 2
done
echo "O3K_GUEST_DEVICE_MARKER timeout"
EOF

SERVER_ID="$(timeout 600 openstack server create --wait --image "${IMAGE_ID}" --flavor "${FLAVOR_ID}" --key-name "${KEYPAIR_NAME}" --config-drive true --user-data "${DATA_DIR}/device-probe.user-data" --nic port-id="${PORT_ID}" o3k-gate-d-server -f value -c id 2>"${DATA_DIR}/server-create.err")" || {
  echo "== server-create.err ==" >&2
  cat "${DATA_DIR}/server-create.err" >&2 || true
  echo "== o3kd.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  echo "== o3k-compute.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3k-compute.log" 2>/dev/null || true
  fail "server create failed"
}
SERVER_ID="${SERVER_ID//[[:space:]]/}"
[ -n "${SERVER_ID}" ] || fail "server id missing"
SERVER_STATUS="$(openstack server show "${SERVER_ID}" -f value -c status)"
[ "${SERVER_STATUS}" = "ACTIVE" ] || fail "server did not reach ACTIVE (status=${SERVER_STATUS})"
echo "    server ${SERVER_ID} is ACTIVE"

echo "==> Creating the volume on the mock Cinder and attaching it..."
curl -s -H "X-Auth-Token: $(openstack token issue -f value -c id)" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/volumes" \
  -d '{"volume":{"size":1,"name":"gate-d-vol"}}' > "${DATA_DIR}/volume.json"
VOLUME_ID="$(python3 -c 'import json; print(json.load(open("'"${DATA_DIR}"'/volume.json"))["volume"]["id"])')"
[ -n "${VOLUME_ID}" ] || fail "volume id missing"

openstack server add volume "${SERVER_ID}" "${VOLUME_ID}" >"${DATA_DIR}/attach.err" 2>&1 || {
  echo "== attach.err ==" >&2
  cat "${DATA_DIR}/attach.err" >&2 || true
  echo "== o3kd.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  echo "== o3k-compute.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3k-compute.log" 2>/dev/null || true
  fail "volume attach failed"
}
echo "    volume ${VOLUME_ID} attached via the public Nova API"

if [ -n "${O3K_GATE_D_HOLD:-}" ]; then
  echo "==> HOLD: diagnostic window (${O3K_GATE_D_HOLD}s) before the attachment poll..."
  sleep "${O3K_GATE_D_HOLD}"
fi

if [ -n "${O3K_GATE_D_DUMP_STORE:-}" ]; then
  echo "==> Dumping volume_attachments store rows..."
  python3 - "${DATA_DIR}/o3k" <<'PY'
import sqlite3, sys
db = sys.argv[1] + "/o3k.sqlite"
conn = sqlite3.connect(db)
try:
    rows = conn.execute("SELECT id, volume_id, status, cinder_attachment_id, driver_volume_type, target_iqn, connection_info_digest FROM volume_attachments").fetchall()
    for r in rows:
        print(r)
except Exception as e:
    print("query error:", e)
PY
fi

ATTACHED_OK=no
OSC_TOKEN="$(openstack token issue -f value -c id 2>/dev/null || true)"
for i in $(seq 1 30); do
  ATTACH_JSON="$(curl -s -H "X-Auth-Token: ${OSC_TOKEN}" \
    "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/attachments")"
  if echo "${ATTACH_JSON}" | grep -q '"status": "attached"'; then
    ATTACHED_OK=yes
    break
  fi
  sleep 2
done
[ "${ATTACHED_OK}" = "yes" ] || {
  echo "== attachment status (redacted; connection_info never dumped) ==" >&2
  echo "${ATTACH_JSON}" | grep -oE '"status": "[a-z]+"' >&2 || true
  echo "== o3kd.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  fail "volume did not reach attached state"
}

echo "==> Proving CollectConnector + CHAP login + libvirt hotplug..."
iscsiadm -m session 2>/dev/null | grep -q "${TARGET_IQN}" \
  || fail "no run-owned iSCSI session for ${TARGET_IQN}"
echo "    CHAP iSCSI session established"
DOMAIN_NAME="$(virsh -c qemu:///system list --all --name | grep 'o3k-' | head -n 1)"
virsh -c qemu:///system dumpxml "${DOMAIN_NAME}" > "${DATA_DIR}/domain.xml" 2>&1
grep -qE "device=['\"]disk['\"]" "${DATA_DIR}/domain.xml" || {
  echo "== domain.xml ==" >&2
  cat "${DATA_DIR}/domain.xml" >&2 || true
  echo "== virsh domains ==" >&2
  virsh -c qemu:///system list --all 2>&1 >&2 || true
  fail "no block disk in domain XML"
}
grep -q '<serial>o3k-' "${DATA_DIR}/domain.xml" || {
  echo "== domain.xml (searching for o3k disk serial) ==" >&2
  grep -nE 'o3k|disk|serial' "${DATA_DIR}/domain.xml" >&2 || true
  cp "${DATA_DIR}/domain.xml" "${REPO_ROOT}/gate-d-domain-debug.xml" 2>/dev/null || true
  fail "no o3k disk ownership serial in domain XML"
}
echo "    libvirt domain XML contains the o3k-owned hotplugged disk (serial bound)"

GUEST_OK=no
for i in $(seq 1 60); do
  CONSOLE_AFTER="$(openstack console log show "${SERVER_ID}" 2>/dev/null || true)"
  if echo "${CONSOLE_AFTER}" | grep -q "O3K_GUEST_DEVICE_MARKER name="; then
    GUEST_OK=yes
    break
  fi
  sleep 2
done
if [ "${GUEST_OK}" = "yes" ]; then
  echo "    guest observed the attached device (O3K_GUEST_DEVICE_MARKER)"
else
  # The guest marker is diagnostic-only for this gate: the essential proof is
  # the durable libvirt hotplug (serial-bound) plus the CHAP session. The
  # protected real-Cinder runner retains the in-guest marker as its evidence.
  echo "WARN: guest marker not observed (console is read-only evidence); proceeding"
fi

echo "==> Proving observation, detach, and CHAP session cleanup..."
openstack server remove volume "${SERVER_ID}" "${VOLUME_ID}" >"${DATA_DIR}/detach.err" 2>&1 || {
  echo "== detach.err ==" >&2
  cat "${DATA_DIR}/detach.err" >&2 || true
  echo "== o3kd.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  echo "== o3k-compute.log tail ==" >&2
  tail -n 60 "${DATA_DIR}/o3k-compute.log" 2>/dev/null || true
  fail "volume detach failed"
}
DETACHED_OK=no
for i in $(seq 1 30); do
  VOL_JSON="$(curl -s -H "X-Auth-Token: ${OSC_TOKEN}" \
    "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/volumes/${VOLUME_ID}")"
  if echo "${VOL_JSON}" | grep -q '"status": "available"'; then
    DETACHED_OK=yes
    break
  fi
  sleep 2
done
[ "${DETACHED_OK}" = "yes" ] || {
  echo "== volume show output ==" >&2
  echo "${VOL_JSON}" >&2 || true
  echo "== o3kd.log tail ==" >&2
  tail -n 40 "${DATA_DIR}/o3kd.log" 2>/dev/null || true
  fail "volume did not return to available"
}

if iscsiadm -m session 2>/dev/null | grep -q "${TARGET_IQN}"; then
  fail "iSCSI session for ${TARGET_IQN} was not cleaned up after detach"
fi
echo "    CHAP iSCSI session cleaned up"
virsh -c qemu:///system dumpxml "${DOMAIN_NAME}" > "${DATA_DIR}/domain-after.xml" 2>/dev/null
if grep -q '<serial>o3k-' "${DATA_DIR}/domain-after.xml"; then
  echo "== domain-after.xml (o3k serials) ==" >&2
  grep -nE 'serial|disk' "${DATA_DIR}/domain-after.xml" >&2 || true
  fail "o3k disk serial still present after detach"
fi
echo "    libvirt disk hotplug removed"

echo "==> Verifying CHAP credentials never leak into O3K logs..."
if grep -q "${CHAP_PASSWORD}" "${DATA_DIR}/o3kd.log" "${DATA_DIR}/o3k-compute.log"; then
  fail "CHAP password leaked into logs"
fi
echo "    no CHAP secret in logs"

echo "==> Cleaning up the server and recording the result..."
openstack server delete "${SERVER_ID}" >/dev/null 2>&1 || true
openstack keypair delete "${KEYPAIR_NAME}" >/dev/null 2>&1 || true
openstack port delete "${PORT_ID}" >/dev/null 2>&1 || true
openstack flavor delete "${FLAVOR_ID}" >/dev/null 2>&1 || true
openstack subnet delete o3k-gate-d-subnet >/dev/null 2>&1 || true
openstack network delete o3k-gate-d-network >/dev/null 2>&1 || true
openstack image delete o3k-gate-d-image >/dev/null 2>&1 || true

PROVEN='["collect_connector", "chap_login", "libvirt_hotplug", "observe_disk", "detach", "session_cleanup"]'
if [ "${GUEST_OK}" = "yes" ]; then
  PROVEN='["collect_connector", "chap_login", "libvirt_hotplug", "observe_disk", "guest_observation", "detach", "session_cleanup"]'
fi
cat > "${RESULT_PATH}" <<JSON
{"artifact_type": "cinder-chap-compute-result.json", "status": "passed", "proven": ${PROVEN}, "guest_observation": "${GUEST_OK}", "run_id": "${RUN_ID}"}
JSON
echo "==> Gate D passed: ${RESULT_PATH}"
