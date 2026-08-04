#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "==> Building o3kd for external Cinder service-under-test profile..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
PORT="18089"
CINDER_PORT="18776"

DATA_DIR="/tmp/o3k-cinder-testbed-$(date +%s)"
mkdir -p "${DATA_DIR}"

export O3K_LISTEN_ADDR="127.0.0.1:${PORT}"
export O3K_DATA_DIR="${DATA_DIR}"
export O3K_BOOTSTRAP_PASSWORD="password"
export O3K_TOKEN_SIGNING_KEY="a-secure-signing-key-with-at-least-32-bytes"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:${CINDER_PORT}"

echo "==> Starting mock external Cinder service on port ${CINDER_PORT}..."
python3 - <<PY &
import http.server
import socketserver
import urllib.request
import sys
import json

class CinderMockHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        token = self.headers.get("X-Auth-Token") or self.headers.get("X-Subject-Token")
        if not token:
            self.send_response(401)
            self.end_headers()
            return
        
        # Verify token with O3K Keystone
        req = urllib.request.Request(
            f"http://127.0.0.1:${PORT}/v3/auth/tokens",
            headers={"X-Subject-Token": token, "X-Auth-Token": token}
        )
        try:
            with urllib.request.urlopen(req) as resp:
                if resp.status == 200:
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(json.dumps({"volumes": []}).encode())
                    return
        except Exception as e:
            pass

        self.send_response(401)
        self.end_headers()

socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", ${CINDER_PORT}), CinderMockHandler) as httpd:
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

echo "==> Authenticating against Keystone..."
AUTH_PAYLOAD='{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}}'
AUTH_HEADERS=$(mktemp)
curl -s -D "${AUTH_HEADERS}" -H "Content-Type: application/json" -X POST "http://127.0.0.1:${PORT}/v3/auth/tokens" -d "${AUTH_PAYLOAD}" > /dev/null

TOKEN=$(grep -i "x-subject-token:" "${AUTH_HEADERS}" | tr -d '\r' | awk '{print $2}')
rm -f "${AUTH_HEADERS}"

if [ -z "${TOKEN}" ]; then
    echo "ERROR: Failed to authenticate with Keystone"
    exit 1
fi
echo "==> Token obtained!"

echo "==> Verifying external Cinder validates Keystone token..."
CINDER_CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${CINDER_PORT}/v3/bootstrap-project/volumes")
if [ "${CINDER_CODE}" -ne 200 ]; then
    echo "ERROR: External Cinder token validation failed with HTTP ${CINDER_CODE}"
    exit 1
fi
echo "==> External Cinder successfully validated O3K Keystone token!"

echo "==> External Cinder service-under-test runner test PASSED cleanly!"
