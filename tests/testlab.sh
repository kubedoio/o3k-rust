#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${O3K_TESTLAB_PROFILE:-fake}"
ARTIFACT_DIR="${O3K_TESTLAB_ARTIFACT_DIR:-${ROOT_DIR}/target/testlab-artifacts}"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-testlab.XXXXXX")"
PORT="${O3K_TESTLAB_PORT:-18080}"
BASE_URL="http://127.0.0.1:${PORT}"
LOG_FILE="${ARTIFACT_DIR}/o3kd.log"
TOKEN=""
SERVER_ID=""
IMAGE_ID=""
NETWORK_ID=""
SUBNET_ID=""
PORT_ID=""
STARTED_AT="$(date -u +%s)"
mkdir -p "${ARTIFACT_DIR}"

cleanup() {
    set +e
    if [[ -n "${O3KD_PID:-}" ]]; then kill -TERM "${O3KD_PID}" 2>/dev/null || true; wait "${O3KD_PID}" 2>/dev/null || true; fi
    rm -rf "${DATA_DIR}"
}
trap cleanup EXIT

write_result() {
    local status="$1"
    python3 - "${ARTIFACT_DIR}/result.json" "${status}" "${PROFILE}" "${STARTED_AT}" <<'PY'
import json, sys, time
path, status, profile, started = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"status": status, "profile": profile, "started_at": int(started), "finished_at": int(time.time()), "compatibility": "testlab-alpha"}, output, indent=2)
    output.write("\n")
PY
}
trap 'write_result failed' ERR

if [[ "${PROFILE}" == "cellhv" ]]; then
    if [[ -z "${CELLHV_ENDPOINT:-}" ]]; then
        echo "O3K_TESTLAB_PROFILE=cellhv requires CELLHV_ENDPOINT; no endpoint was supplied" >&2
        exit 2
    fi
    echo "CellHV profile is environment-gated; configure its external harness before running" >&2
    exit 2
fi
if [[ "${PROFILE}" != "fake" ]]; then echo "unsupported TestLab profile" >&2; exit 2; fi

cargo build --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" --bin o3kd
O3K_BOOTSTRAP_PASSWORD="${O3K_BOOTSTRAP_PASSWORD:-password}" O3K_TOKEN_SIGNING_KEY="${O3K_TOKEN_SIGNING_KEY:-testlab-signing-key-with-at-least-32-bytes}" "${ROOT_DIR}/target/debug/o3kd" --listen-addr "127.0.0.1:${PORT}" --data-dir "${DATA_DIR}" --log-filter warn >"${LOG_FILE}" 2>&1 &
O3KD_PID=$!

for _ in $(seq 1 100); do
    if curl -fsS "${BASE_URL}/readyz" >/dev/null 2>&1; then break; fi
    sleep 0.1
done
curl -fsS "${BASE_URL}/readyz" >/dev/null

AUTH_PASSWORD="${O3K_BOOTSTRAP_PASSWORD:-password}"
AUTH_BODY="{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"${AUTH_PASSWORD}\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
TOKEN_HEADERS="$(curl -fsSi -X POST "${BASE_URL}/v3/auth/tokens" -H 'content-type: application/json' --data "${AUTH_BODY}")"
TOKEN="$(python3 -c 'import sys; print(next((line.split(":",1)[1].strip() for line in sys.stdin.read().splitlines() if line.lower().startswith("x-subject-token:")), ""))' <<<"${TOKEN_HEADERS}")"
[[ -n "${TOKEN}" ]]

mkdir -p "${ARTIFACT_DIR}/compatibility"
O3K_COMPATIBILITY_PASSWORD="${AUTH_PASSWORD}" OS_AUTH_TOKEN="${TOKEN}" \
    python3 "${ROOT_DIR}/tests/compatibility-harness.py" \
    --target rust \
    --base-url "${BASE_URL}" \
    --project-id bootstrap-project \
    --source-commit "$(git -C "${ROOT_DIR}" rev-parse HEAD)" \
    --json-out "${ARTIFACT_DIR}/compatibility/rust.json" \
    --junit-out "${ARTIFACT_DIR}/compatibility/rust.xml"
test -s "${ARTIFACT_DIR}/compatibility/rust.json"
test -s "${ARTIFACT_DIR}/compatibility/rust.xml"

json() { curl -fsS "$@" -H "x-auth-token: ${TOKEN}"; }
field() { python3 -c 'import json,sys; value=json.load(sys.stdin)
for part in sys.argv[1].split("."): value=value[part] if not isinstance(value,list) else value[int(part)]
print(value)' "$1"; }

IMAGE_ID="$(json -X POST "${BASE_URL}/v2/images" -H 'content-type: application/json' --data '{"name":"testlab-image","container_format":"bare","disk_format":"raw"}' | field id)"
curl -fsS -X PUT "${BASE_URL}/v2/images/${IMAGE_ID}/file" -H "x-auth-token: ${TOKEN}" -H 'content-type: application/octet-stream' --data-binary 'testlab-image-bytes' >/dev/null
NETWORK_ID="$(json -X POST "${BASE_URL}/v2.0/networks" -H 'content-type: application/json' --data '{"network":{"name":"testlab-network"}}' | field network.id)"
SUBNET_ID="$(json -X POST "${BASE_URL}/v2.0/subnets" -H 'content-type: application/json' --data "{\"subnet\":{\"name\":\"testlab-subnet\",\"network_id\":\"${NETWORK_ID}\",\"cidr\":\"192.0.2.0/29\"}}" | field subnet.id)"
PORT_ID="$(json -X POST "${BASE_URL}/v2.0/ports" -H 'content-type: application/json' --data "{\"port\":{\"name\":\"testlab-port\",\"network_id\":\"${NETWORK_ID}\"}}" | field port.id)"
FLAVOR_ID="$(json "${BASE_URL}/v2.1/bootstrap-project/flavors" | python3 -c 'import json,sys; print(json.load(sys.stdin)["flavors"][0]["id"])')"
SERVER_ID="$(json -X POST "${BASE_URL}/v2.1/bootstrap-project/servers" -H 'content-type: application/json' -H 'x-openstack-request-id: testlab-server-create' --data "{\"server\":{\"name\":\"testlab-server\",\"image\":{\"id\":\"${IMAGE_ID}\"},\"flavor\":{\"id\":\"${FLAVOR_ID}\"},\"networks\":[{\"uuid\":\"${PORT_ID}\"}]}}" | field server.id)"

kill -TERM "${O3KD_PID}"; wait "${O3KD_PID}"; unset O3KD_PID
O3K_BOOTSTRAP_PASSWORD="${O3K_BOOTSTRAP_PASSWORD:-password}" O3K_TOKEN_SIGNING_KEY="${O3K_TOKEN_SIGNING_KEY:-testlab-signing-key-with-at-least-32-bytes}" "${ROOT_DIR}/target/debug/o3kd" --listen-addr "127.0.0.1:${PORT}" --data-dir "${DATA_DIR}" --log-filter warn >>"${LOG_FILE}" 2>&1 &
O3KD_PID=$!
for _ in $(seq 1 100); do curl -fsS "${BASE_URL}/readyz" >/dev/null 2>&1 && break; sleep 0.1; done
json "${BASE_URL}/v2.1/bootstrap-project/servers/${SERVER_ID}" >/dev/null
json "${BASE_URL}/v2.1/bootstrap-project/servers" >/dev/null

json -X DELETE "${BASE_URL}/v2.1/bootstrap-project/servers/${SERVER_ID}" >/dev/null
json -X DELETE "${BASE_URL}/v2.0/ports/${PORT_ID}" >/dev/null
json -X DELETE "${BASE_URL}/v2.0/subnets/${SUBNET_ID}" >/dev/null
json -X DELETE "${BASE_URL}/v2.0/networks/${NETWORK_ID}" >/dev/null
json -X DELETE "${BASE_URL}/v2/images/${IMAGE_ID}" >/dev/null
write_result passed
echo "TestLab workflow passed; artifacts: ${ARTIFACT_DIR}"
