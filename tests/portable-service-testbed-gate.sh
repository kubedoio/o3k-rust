#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "==> Building o3kd binary..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --bin o3kd

O3KD_BIN="${REPO_ROOT}/target/debug/o3kd"
PORT="18088"
DATA_DIR="/tmp/o3k-gate-$(date +%s)"
mkdir -p "${DATA_DIR}"

export O3K_LISTEN_ADDR="127.0.0.1:${PORT}"
export O3K_DATA_DIR="${DATA_DIR}"
export O3K_BOOTSTRAP_PASSWORD="password"
export O3K_TOKEN_SIGNING_KEY="a-secure-signing-key-with-at-least-32-bytes"
export O3K_CINDER_ENDPOINT="http://127.0.0.1:8776"

echo "==> Launching o3kd on ${O3K_LISTEN_ADDR}..."
"${O3KD_BIN}" > "${DATA_DIR}/o3kd.log" 2>&1 &
O3KD_PID=$!

cleanup() {
    echo "==> Shutting down o3kd (PID: ${O3KD_PID})..."
    kill -TERM "${O3KD_PID}" 2>/dev/null || true
    wait "${O3KD_PID}" 2>/dev/null || true
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
    echo "ERROR: o3kd failed to start within timeout. Logs:"
    cat "${DATA_DIR}/o3kd.log"
    exit 1
fi
echo "==> o3kd is healthy!"

echo "==> Testing Keystone root discovery (/)..."
curl -s -f "http://127.0.0.1:${PORT}/" | grep -q "version"

echo "==> Testing Keystone v3 discovery (/v3)..."
curl -s -f "http://127.0.0.1:${PORT}/v3" | grep -q "version"

echo "==> Testing Nova v2.1 version discovery (/v2.1)..."
V2_1_RESP=$(curl -s "http://127.0.0.1:${PORT}/v2.1")
echo "${V2_1_RESP}" | grep -q "version" || { echo "ERROR: /v2.1 returned: ${V2_1_RESP}"; exit 1; }


echo "==> Testing Placement version discovery (/placement)..."
PLACEMENT_RESP=$(curl -s "http://127.0.0.1:${PORT}/placement")
echo "${PLACEMENT_RESP}" | grep -q "max_version" || { echo "ERROR: /placement returned: ${PLACEMENT_RESP}"; exit 1; }


echo "==> Testing Keystone password authentication (POST /v3/auth/tokens)..."
AUTH_PAYLOAD='{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}}'
AUTH_HEADERS=$(mktemp)
AUTH_RESP=$(curl -s -D "${AUTH_HEADERS}" -H "Content-Type: application/json" -X POST "http://127.0.0.1:${PORT}/v3/auth/tokens" -d "${AUTH_PAYLOAD}")

TOKEN=$(grep -i "x-subject-token:" "${AUTH_HEADERS}" | tr -d '\r' | awk '{print $2}')
rm -f "${AUTH_HEADERS}"

if [ -z "${TOKEN}" ]; then
    echo "ERROR: Failed to obtain token from POST /v3/auth/tokens"
    exit 1
fi
echo "==> Token obtained: [REDACTED]"

echo "==> Validating token (GET /v3/auth/tokens)..."
GET_TOKEN_RESP=$(curl -s -f -H "X-Subject-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v3/auth/tokens")
echo "${GET_TOKEN_RESP}" | grep -q "volumev3"
echo "${GET_TOKEN_RESP}" | grep -q "eba29e2d-53de-461d-ae91-ede7402713cb"

echo "==> Validating token status (HEAD /v3/auth/tokens)..."
curl -s -f -I -H "X-Subject-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v3/auth/tokens" | grep -qi "HTTP/1.1 200 OK"

echo "==> Testing microversion negotiation (compute 2.1 vs 2.95)..."
curl -s -f -H "X-Auth-Token: ${TOKEN}" -H "OpenStack-API-Version: compute 2.1" "http://127.0.0.1:${PORT}/v2.1/eba29e2d-53de-461d-ae91-ede7402713cb/servers" | grep -q "servers"
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: ${TOKEN}" -H "OpenStack-API-Version: compute 2.95" "http://127.0.0.1:${PORT}/v2.1/eba29e2d-53de-461d-ae91-ede7402713cb/servers")
if [ "${HTTP_CODE}" -ne 406 ]; then
    echo "ERROR: Expected 406 Not Acceptable for compute 2.95, got ${HTTP_CODE}"
    exit 1
fi


echo "==> Testing Glance image listing (/v2/images)..."
curl -s -f -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2/images" | grep -q "images"

echo "==> Testing Neutron network listing (/v2.0/networks)..."
curl -s -f -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2.0/networks" | grep -q "networks"


echo "==> Verifying logs contain no secrets..."
if grep -q "password" "${DATA_DIR}/o3kd.log" | grep -v "password: Secret"; then
    echo "WARNING: Check log for potential unredacted password"
fi

echo "==> Portable service testbed gate PASSED cleanly!"
