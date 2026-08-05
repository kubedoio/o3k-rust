#!/usr/bin/env bash
set -euo pipefail

# Component-level mock Cinder test.
#
# This is NOT a real Cinder deployment. It starts a stateful Python HTTP mock
# of the frozen Cinder v3 subset and verifies that a running o3kd process:
#   - issues and validates tokens through its Keystone-compatible API;
#   - authenticates the Cinder service user;
#   - advertises the durable volumev3 catalog endpoint pointing at the mock;
#   - lets the mock validate O3K-issued tokens through the public Identity API;
#   - completes the volume + attachment lifecycle against the mock.
#
# Real-service evidence must come from the protected real Cinder profile
# (scripts/real-cinder-testbed-runner.sh), not from this component test.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "==> Building o3kd for the mock Cinder component test..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
PORT="18089"
CINDER_PORT="18776"

DATA_DIR="/tmp/o3k-cinder-mock-$(date +%s)"
mkdir -p "${DATA_DIR}"

export O3K_LISTEN_ADDR="127.0.0.1:${PORT}"
export O3K_DATA_DIR="${DATA_DIR}"
export O3K_BOOTSTRAP_PASSWORD="password"
export O3K_TOKEN_SIGNING_KEY="a-secure-signing-key-with-at-least-32-bytes"
export O3K_CINDER_PASSWORD="cinder-password"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"

echo "==> Starting stateful mock Cinder on port ${CINDER_PORT}..."
python3 - "${PORT}" "${CINDER_PORT}" <<'PY' &
import http.server
import socketserver
import urllib.request
import sys
import json

PORT, CINDER_PORT = int(sys.argv[1]), int(sys.argv[2])

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
        f"http://127.0.0.1:{PORT}/v3/auth/tokens",
        headers={"X-Subject-Token": token, "X-Auth-Token": token},
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status == 200
    except Exception:
        return False


def next_id(prefix):
    COUNTER[0] += 1
    return f"{prefix}-{COUNTER[0]:08}"


def volume_json(vid):
    return {"id": vid, "status": "available", "size": 1, "name": "mock-volume"}


def attachment_json(aid):
    return {
        "id": aid,
        "status": ATTACHMENTS[aid]["status"],
        "volume_id": ATTACHMENTS[aid]["volume_id"],
        "connection_info": ATTACHMENTS[aid].get("connection_info"),
    }


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def _send(self, code, body):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())

    def do_GET(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        if self.path.endswith("/volumes"):
            return self._send(200, {"volumes": [volume_json(v) for v in VOLUMES]})
        if "/attachments" in self.path and self.path.endswith("/attachments"):
            return self._send(200, {"attachments": [attachment_json(a) for a in ATTACHMENTS]})
        parts = self.path.split("/")
        if "volumes" in parts and parts[-1] in VOLUMES:
            return self._send(200, {"volume": volume_json(parts[-1])})
        if "attachments" in parts and parts[-1] in ATTACHMENTS:
            return self._send(200, {"attachment": attachment_json(parts[-1])})
        return self._send(404, {"error": {"message": "not found"}})

    def do_POST(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path.endswith("/volumes"):
            vid = next_id("volume")
            VOLUMES[vid] = {"id": vid}
            return self._send(202, {"volume": volume_json(vid)})
        if self.path.endswith("/attachments"):
            volume_id = body.get("attachment", {}).get("volume_id")
            if volume_id not in VOLUMES:
                return self._send(400, {"error": {"message": "volume not found"}})
            aid = next_id("attachment")
            ATTACHMENTS[aid] = {"status": "creating", "volume_id": volume_id, "connection_info": None}
            return self._send(202, {"attachment": attachment_json(aid)})
        if "/update" in self.path:
            aid = self.path.split("/")[-2]
            if aid not in ATTACHMENTS:
                return self._send(404, {"error": {"message": "attachment not found"}})
            ATTACHMENTS[aid]["status"] = "reserved"
            ATTACHMENTS[aid]["connection_info"] = {
                "driver_volume_type": "iscsi",
                "data": {
                    "target_portal": "10.0.0.10:3260",
                    "target_iqn": "iqn.2026-01.example.com:volume",
                    "target_lun": 1,
                },
            }
            return self._send(200, {"attachment": attachment_json(aid)})
        if "/action" in self.path:
            aid = self.path.split("/")[-2]
            if aid not in ATTACHMENTS:
                return self._send(404, {"error": {"message": "attachment not found"}})
            if "os-complete" in body:
                ATTACHMENTS[aid]["status"] = "attached"
            elif "os-terminate" in body:
                del ATTACHMENTS[aid]
            return self._send(200, {"attachment": attachment_json(aid) if aid in ATTACHMENTS else {"id": aid, "status": "deleted"}})
        return self._send(400, {"error": {"message": "unsupported operation"}})

    def do_DELETE(self):
        if not validate(self.headers):
            return self._send(401, {"error": "unauthorized"})
        parts = self.path.split("/")
        if "volumes" in parts and parts[-1] in VOLUMES:
            del VOLUMES[parts[-1]]
            return self._send(202, {})
        return self._send(404, {"error": {"message": "not found"}})


socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", CINDER_PORT), Handler) as httpd:
    httpd.serve_forever()
PY
CINDER_PID=$!

echo "==> Launching o3kd on ${O3K_LISTEN_ADDR}..."
"${O3KD_BIN}" > "${DATA_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!

cleanup() {
    echo "==> Cleaning up o3kd and mock Cinder processes..."
    kill -TERM "${O3KD_PID}" 2>/dev/null || true
    kill -TERM "${CINDER_PID}" 2>/dev/null || true
    wait "${O3KD_PID}" 2>/dev/null || true
    wait "${CINDER_PID}" 2>/dev/null || true
    rm -rf "${DATA_DIR}"
}
trap cleanup EXIT

echo "==> Waiting for o3kd healthz..."
READY=0
for i in $(seq 1 30); do
    if curl -s "http://127.0.0.1:${PORT}/healthz" | grep -q "ok"; then
        READY=1
        break
    fi
    sleep 0.2
done

if [ "${READY}" -ne 1 ]; then
    echo "ERROR: o3kd failed to start. Logs:"
    cat "${DATA_DIR}/o3kd.log"
    exit 1
fi
echo "==> o3kd is healthy!"

AUTH="http://127.0.0.1:${PORT}/v3/auth/tokens"

echo "==> Authenticating bootstrap admin..."
ADMIN_HEADERS=$(mktemp)
curl -s -D "${ADMIN_HEADERS}" -H "Content-Type: application/json" -X POST "${AUTH}" \
  -d '{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}}' > /dev/null
ADMIN_TOKEN=$(grep -i "x-subject-token:" "${ADMIN_HEADERS}" | tr -d '\r' | awk '{print $2}')
rm -f "${ADMIN_HEADERS}"
[ -n "${ADMIN_TOKEN}" ] || { echo "ERROR: admin token missing"; exit 1; }

echo "==> Authenticating Cinder service user..."
CINDER_HEADERS=$(mktemp)
curl -s -D "${CINDER_HEADERS}" -H "Content-Type: application/json" -X POST "${AUTH}" \
  -d '{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"cinder","password":"cinder-password"}}},"scope":{"project":{"name":"admin"}}}}' > /dev/null
CINDER_TOKEN=$(grep -i "x-subject-token:" "${CINDER_HEADERS}" | tr -d '\r' | awk '{print $2}')
rm -f "${CINDER_HEADERS}"
[ -n "${CINDER_TOKEN}" ] || { echo "ERROR: cinder service token missing"; exit 1; }

echo "==> Validating token through the public Identity API..."
curl -s -f -H "X-Subject-Token: ${ADMIN_TOKEN}" "${AUTH}" > /dev/null

echo "==> Confirming catalog advertises volumev3 at the mock endpoint..."
CATALOG=$(curl -s -H "X-Subject-Token: ${ADMIN_TOKEN}" "${AUTH}")
echo "${CATALOG}" | grep -q "volumev3"
echo "${CATALOG}" | grep -q "127.0.0.1:${CINDER_PORT}"

echo "==> Creating a volume on the mock (token validated by the mock through O3K)..."
VOLUME_JSON=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/volumes" \
  -d '{"volume":{"size":1,"name":"mock-vol"}}')
VOLUME_ID=$(echo "${VOLUME_JSON}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["volume"]["id"])')
[ -n "${VOLUME_ID}" ] || { echo "ERROR: volume id missing"; exit 1; }

echo "==> Creating, updating, completing, and terminating an attachment..."
ATTACH_JSON=$(curl -s -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/attachments" \
  -d "{\"attachment\":{\"volume_id\":\"${VOLUME_ID}\"}}")
ATTACH_ID=$(echo "${ATTACH_JSON}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["attachment"]["id"])')
[ -n "${ATTACH_ID}" ] || { echo "ERROR: attachment id missing"; exit 1; }

curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/attachments/${ATTACH_ID}/update" \
  -d '{"attachment":{"connector":{"host":"compute-1","ip":"10.0.0.5","platform":"x86_64","os_type":"linux","multipath":false}}}' > /dev/null

curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/attachments/${ATTACH_ID}/action" \
  -d '{"os-complete": null}' > /dev/null

curl -s -f -H "X-Auth-Token: ${ADMIN_TOKEN}" -H "Content-Type: application/json" \
  -X POST "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/attachments/${ATTACH_ID}/action" \
  -d '{"os-terminate": null}' > /dev/null

echo "==> Deleting the volume and verifying cleanup..."
curl -s -f -X DELETE -H "X-Auth-Token: ${ADMIN_TOKEN}" \
  "http://127.0.0.1:${CINDER_PORT}/v3/eba29e2d-53de-461d-ae91-ede7402713cb/volumes/${VOLUME_ID}" > /dev/null

echo "==> Mock Cinder component test PASSED cleanly."
