#!/usr/bin/env bash
set -Eeuo pipefail

# Black-box P9 control-plane gate.  It drives the public o3kd API, while the
# real o3k-network binary owns the host-local TAP/bridge/DHCP mutation.  This
# is intentionally a portable process gate: real guest packet evidence stays
# in the privileged QEMU gate.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_PUBLIC_API_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_PUBLIC_API_INNER=1 "$0" "$@"
fi
[[ "${O3K_P9_DEBUG:-}" == 1 ]] && set -x

for tool in cargo curl ip nft openssl setsid unshare python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

cargo build -p o3kd -p o3k-network-bin >/dev/null
BIN="$ROOT_DIR/target/debug/o3kd"
NETWORK_BIN="$ROOT_DIR/target/debug/o3k-network-bin"
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-public-api.XXXXXX")"
O3KD_PID=""
NETWORK_PID=""
EXT_PID=""
PORT="$(python3 - <<'PY'
import socket
s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()
PY
)"
NETWORK_PORT="$(python3 - <<'PY'
import socket
s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()
PY
)"
PROJECT_ID="eba29e2d-53de-461d-ae91-ede7402713cb"
FOREIGN_LINK=o3kp9foreign0
UPLINK=p9apiuplink0
EXT_PEER=p9apipeer0
EXTERNAL_REALM=00000000-0000-0000-0000-000000000004

ip link set lo up
ip link add "$FOREIGN_LINK" type dummy
setsid unshare -n sleep 180 &
EXT_PID=$!
sleep 0.1
ip link add "$UPLINK" type veth peer name "$EXT_PEER"
ip link set "$EXT_PEER" netns "$EXT_PID"
ip link set "$UPLINK" up
ip addr add 198.51.100.1/24 dev "$UPLINK"
nsenter -t "$EXT_PID" -n -- ip link set lo up
nsenter -t "$EXT_PID" -n -- ip link set "$EXT_PEER" up
nsenter -t "$EXT_PID" -n -- ip addr add 198.51.100.2/24 dev "$EXT_PEER"
FOREIGN_SNAPSHOT="$(ip -o -4 addr show dev "$FOREIGN_LINK")"
chmod 0755 "$DNSMASQ"
mkdir -p "$WORK_DIR"/{network/{executor,ownership,dhcp},data}

cleanup() {
    set +e
    if [[ "${O3K_P9_KEEP_LOGS:-}" == 1 ]]; then
        echo "--- o3kd log ---" >&2
        tail -120 "$WORK_DIR/o3kd.log" >&2 2>/dev/null
        echo "--- network log ---" >&2
        tail -120 "$WORK_DIR/network.log" >&2 2>/dev/null
        echo "logs kept at $WORK_DIR" >&2
    fi
    [[ -n "$O3KD_PID" ]] && kill "$O3KD_PID" 2>/dev/null
    [[ -n "$NETWORK_PID" ]] && kill "$NETWORK_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && kill "$EXT_PID" 2>/dev/null
    [[ -n "$O3KD_PID" ]] && wait "$O3KD_PID" 2>/dev/null
    [[ -n "$NETWORK_PID" ]] && wait "$NETWORK_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && wait "$EXT_PID" 2>/dev/null
    ip link del o3kp9api0 2>/dev/null
    ip link del "$UPLINK" 2>/dev/null
    ip link del "$FOREIGN_LINK" 2>/dev/null
    [[ "${O3K_P9_KEEP_LOGS:-}" == 1 ]] || rm -rf "$WORK_DIR"
}
trap cleanup EXIT

start_network_agent() {
    O3K_NETWORK_AGENT_ID=agent-api \
    O3K_NETWORK_AGENT_EPOCH=epoch-1 \
    O3K_NETWORK_CONTROLLER_ID=controller-api \
    O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
    O3K_NETWORK_FENCING_TOKEN=7 \
    O3K_NETWORK_ROOT="$WORK_DIR/network/executor" \
    O3K_NETWORK_BRIDGE=o3kp9api0 \
    O3K_NETWORK_OWNERSHIP_ROOT="$WORK_DIR/network/ownership" \
    O3K_NETWORK_DHCP_ROOT="$WORK_DIR/network/dhcp" \
    O3K_NETWORK_POLICY_ROOT="$WORK_DIR/network/policy" \
    O3K_NETWORK_EXTERNAL_REALM_ID="$EXTERNAL_REALM" \
    O3K_NETWORK_UPLINK="$UPLINK" \
    O3K_NETWORK_ROUTED_ROOT="$WORK_DIR/network/routed" \
    O3K_NETWORK_PUBLIC_ROOT="$WORK_DIR/network/public" \
    O3K_NETWORK_DNSMASQ="$DNSMASQ" \
    O3K_NETWORK_LISTEN="127.0.0.1:$NETWORK_PORT" \
    O3K_NETWORK_TLS_CERT="$FIXTURES/server-chain.pem" \
    O3K_NETWORK_TLS_KEY="$FIXTURES/server-key.pem" \
    O3K_NETWORK_TLS_CLIENT_CA="$FIXTURES/ca.pem" \
    "$NETWORK_BIN" >>"$WORK_DIR/network.log" 2>&1 &
    NETWORK_PID=$!
}
start_network_agent

SIGNING_KEY="$(openssl rand -hex 48)"
O3K_PROVIDER=fake \
O3K_DATA_DIR="$WORK_DIR/data" \
O3K_LISTEN_ADDR="127.0.0.1:$PORT" \
O3K_CONTROLLER_ID=controller-api \
O3K_CONTROLLER_EPOCH=epoch-1 \
O3K_BOOTSTRAP_PASSWORD=password \
O3K_TOKEN_SIGNING_KEY="$SIGNING_KEY" \
O3K_NETWORK_AGENT_ENDPOINT="https://127.0.0.1:$NETWORK_PORT" \
O3K_NETWORK_AGENT_SERVER_NAME=o3k-control-plane \
O3K_NETWORK_AGENT_CA="$FIXTURES/ca.pem" \
O3K_NETWORK_AGENT_CLIENT_CERT="$FIXTURES/agent-chain.pem" \
O3K_NETWORK_AGENT_CLIENT_KEY="$FIXTURES/agent-key-pkcs8.pem" \
O3K_NETWORK_AGENT_ID=agent-api \
O3K_NETWORK_AGENT_EPOCH=epoch-1 \
O3K_NETWORK_CONTROLLER_ID=controller-api \
O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
O3K_NETWORK_FENCING_TOKEN=7 \
O3K_NETWORK_EXTERNAL_REALM_ID="$EXTERNAL_REALM" \
O3K_PUBLIC_POOL_CIDR=198.51.100.0/24 \
O3K_PUBLIC_POOL_FIRST=198.51.100.10 \
O3K_PUBLIC_POOL_LAST=198.51.100.20 \
"$BIN" --listen-addr "127.0.0.1:$PORT" --data-dir "$WORK_DIR/data" --log-filter info >"$WORK_DIR/o3kd.log" 2>&1 &
O3KD_PID=$!

BASE="http://127.0.0.1:$PORT"
for _ in $(seq 1 120); do
    curl -fsS "$BASE/readyz" >/dev/null 2>&1 && break
    sleep 0.1
done
curl -fsS "$BASE/readyz" >/dev/null

AUTH_HEADERS="$WORK_DIR/auth.headers"
curl -fsSi -X POST "$BASE/v3/auth/tokens" -H 'content-type: application/json' \
    --data '{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","password":"password"}}},"scope":{"project":{"name":"admin"}}}}' >"$AUTH_HEADERS"
TOKEN="$(python3 - "$AUTH_HEADERS" <<'PY'
import sys
for line in open(sys.argv[1]):
    if line.lower().startswith("x-subject-token:"):
        print(line.split(":", 1)[1].strip()); break
PY
)"
[[ -n "$TOKEN" ]]

json() { curl -fsS "$@" -H "x-auth-token: $TOKEN"; }
field() { python3 -c 'import json,sys
value=json.load(sys.stdin)
for part in sys.argv[1].split("."): value=value[part]
print(value)' "$1"; }

IMAGE_ID="$(json -X POST "$BASE/v2/images" -H 'content-type: application/json' \
    --data '{"name":"p9-api-image","container_format":"bare","disk_format":"raw"}' | field id)"
curl -fsS -X PUT "$BASE/v2/images/$IMAGE_ID/file" -H "x-auth-token: $TOKEN" \
    -H 'content-type: application/octet-stream' --data-binary 'p9-api-image-bytes' >/dev/null
NETWORK_ID="$(json -X POST "$BASE/v2.0/networks" -H 'content-type: application/json' \
    --data '{"network":{"name":"p9-api-network"}}' | field network.id)"
SUBNET_ID="$(json -X POST "$BASE/v2.0/subnets" -H 'content-type: application/json' \
    --data "{\"subnet\":{\"name\":\"p9-api-subnet\",\"network_id\":\"$NETWORK_ID\",\"cidr\":\"192.0.2.0/29\"}}" | field subnet.id)"
PORT_ID="$(json -X POST "$BASE/v2.0/ports" -H 'content-type: application/json' \
    --data "{\"port\":{\"name\":\"p9-api-port\",\"network_id\":\"$NETWORK_ID\"}}" | field port.id)"

# Policy is admitted before scheduling.  The public API must persist it as
# pending, not reject it merely because the endpoint has not been bound yet.
json -X POST "$BASE/v2.0/network-policies" -H 'content-type: application/json' \
    -H 'idempotency-key: p9-api-policy' \
    --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"ingress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8080,\"end\":8080},\"source\":\"198.51.100.0/24\",\"action\":\"deny\"}}" >/dev/null

FLAVOR_ID="$(json "$BASE/v2.1/$PROJECT_ID/flavors" | python3 -c 'import json,sys; print(json.load(sys.stdin)["flavors"][0]["id"])')"
SERVER_ID="$(json -X POST "$BASE/v2.1/$PROJECT_ID/servers" -H 'content-type: application/json' \
    -H 'x-openstack-request-id: p9-api-server-create' \
    --data "{\"server\":{\"name\":\"p9-api-server\",\"image\":{\"id\":\"$IMAGE_ID\"},\"flavor\":{\"id\":\"$FLAVOR_ID\"},\"networks\":[{\"uuid\":\"$PORT_ID\"}]}}" | field server.id)"

# Restart the real network executor after the binding plan is durable.  The
# next public operation must reconnect and replay/reconcile through the same
# fenced journal rather than depending on the original process instance.
kill "$NETWORK_PID"
wait "$NETWORK_PID" 2>/dev/null || true
NETWORK_PID=""
start_network_agent
sleep 0.5
kill -0 "$NETWORK_PID"

FLOATING_JSON="$(json -X POST "$BASE/v2.0/floatingips" -H 'content-type: application/json' \
    -H 'x-openstack-request-id: p9-api-floating-create' \
    --data "{\"floatingip\":{\"floating_network_id\":\"$EXTERNAL_REALM\",\"port_id\":\"$PORT_ID\"}}")"
FLOATING_ID="$(printf '%s' "$FLOATING_JSON" | field floatingip.id)"

for _ in $(seq 1 100); do
    python3 - "$WORK_DIR/network/executor/accepted-network-plans.json" <<'PY' && break
import json, sys
try:
    plans = json.load(open(sys.argv[1]))["plans"]
except (OSError, ValueError, KeyError):
    raise SystemExit(1)
raise SystemExit(0 if plans and all(p["status"] == "Succeeded" for p in plans) else 1)
PY
    sleep 0.1
done
python3 - "$WORK_DIR/network/executor/accepted-network-plans.json" <<'PY'
import json, sys
plans = json.load(open(sys.argv[1]))["plans"]
assert plans and all(item["status"] == "Succeeded" for item in plans)
assert any(any("Policy" in intent for intent in item["plan"]["intents"]) for item in plans)
assert any(any("PublicAddressBinding" in intent for intent in item["plan"]["intents"]) for item in plans)
PY

json -X PUT "$BASE/v2.0/floatingips/$FLOATING_ID" -H 'content-type: application/json' \
    --data '{"floatingip":{}}' >/dev/null
json -X DELETE "$BASE/v2.0/floatingips/$FLOATING_ID" >/dev/null
json -X DELETE "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID" >/dev/null
json -X DELETE "$BASE/v2.0/ports/$PORT_ID" >/dev/null
json -X DELETE "$BASE/v2.0/subnets/$SUBNET_ID" >/dev/null
json -X DELETE "$BASE/v2.0/networks/$NETWORK_ID" >/dev/null
json -X DELETE "$BASE/v2/images/$IMAGE_ID" >/dev/null

for _ in $(seq 1 150); do
    if ! ip -o link show | grep -q 'o3ktap-' \
        && ! nft list table ip o3k_p9 >/dev/null 2>&1 \
        && ! nft list table ip o3k_public >/dev/null 2>&1 \
        && ! nft list table ip o3k_policy >/dev/null 2>&1 \
        && ! ip -4 addr show dev "$UPLINK" | grep -q '198.51.100.10/32'; then
        break
    fi
    sleep 0.1
done
if ip -o link show | grep -q 'o3ktap-'; then
    echo "owned endpoint TAP leaked after public lifecycle cleanup" >&2
    exit 1
fi
if nft list table ip o3k_p9 >/dev/null 2>&1 \
    || nft list table ip o3k_public >/dev/null 2>&1 \
    || nft list table ip o3k_policy >/dev/null 2>&1 \
    || ip -4 addr show dev "$UPLINK" | grep -q '198.51.100.10/32'; then
    echo "owned routed/public/policy state leaked after public lifecycle cleanup" >&2
    exit 1
fi
ip link show "$FOREIGN_LINK" >/dev/null
FOREIGN_AFTER="$(ip -o -4 addr show dev "$FOREIGN_LINK")"
if [[ "$FOREIGN_AFTER" != "$FOREIGN_SNAPSHOT" ]]; then
    echo "foreign interface address state changed during public lifecycle" >&2
    printf 'before: %s\nafter: %s\n' "$FOREIGN_SNAPSHOT" "$FOREIGN_AFTER" >&2
    exit 1
fi

echo "P9 public API -> real network-agent process gate passed (policy pending, binding dispatch, cleanup)"
