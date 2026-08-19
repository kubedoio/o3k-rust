#!/usr/bin/env bash
set -Eeuo pipefail

# Black-box provider gate for the real o3k-network process. This proves that
# routed, public-address, and policy realization are reached through the mTLS
# agent boundary and that the routed uplink is not folded into the tenant
# bridge. Packet traffic is covered by the real-QEMU gate.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_PROVIDER_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_PROVIDER_INNER=1 "$0"
fi
for tool in cargo ip nft nsenter unshare setsid; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done
cargo build -p o3k-network-bin >/dev/null
cargo build -p o3k-network-protocol --example network-agent-client >/dev/null
cargo build -p o3k-network --example network-plan-fingerprint >/dev/null
BIN="$ROOT_DIR/target/debug/o3k-network-bin"
CLIENT="$ROOT_DIR/target/debug/examples/network-agent-client"
FINGERPRINT="$ROOT_DIR/target/debug/examples/network-plan-fingerprint"
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-provider.XXXXXX")"
EXT_PID=""
AGENT_PID=""
UPLINK=p9qext0
EXT_PEER=p9qextpeer0
BRIDGE=o3kp9br0
EXTERNAL_REALM=00000000-0000-0000-0000-000000000004
cleanup() {
    set +e
    [[ -n "$AGENT_PID" ]] && kill "$AGENT_PID" 2>/dev/null
    [[ -n "$AGENT_PID" ]] && wait "$AGENT_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && kill "$EXT_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && wait "$EXT_PID" 2>/dev/null
    nft delete table ip o3k_p9 2>/dev/null
    nft delete table ip o3k_public 2>/dev/null
    nft delete table ip o3k_policy 2>/dev/null
    ip link del "$UPLINK" 2>/dev/null
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT
chmod 0755 "$DNSMASQ"
mkdir -p "$WORK_DIR"/{executor,ownership,dhcp,routed,policy,public}
setsid unshare -n sleep 120 &
EXT_PID=$!
sleep 0.1
ip link add "$UPLINK" type veth peer name "$EXT_PEER"
ip link set "$EXT_PEER" netns "$EXT_PID"
ip link set lo up
ip link set "$UPLINK" up
ip addr add 198.51.100.1/24 dev "$UPLINK"
ns() { nsenter -t "$EXT_PID" -n -- "$@"; }
ns ip link set lo up
ns ip link set "$EXT_PEER" up
ns ip addr add 198.51.100.2/24 dev "$EXT_PEER"
sysctl -q -w net.ipv4.ip_forward=1 >/dev/null
O3K_NETWORK_AGENT_ID=agent-provider \
O3K_NETWORK_AGENT_EPOCH=epoch-1 \
O3K_NETWORK_CONTROLLER_ID=controller-provider \
O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
O3K_NETWORK_FENCING_TOKEN=7 \
O3K_NETWORK_ROOT="$WORK_DIR/executor" \
O3K_NETWORK_BRIDGE="$BRIDGE" \
O3K_NETWORK_OWNERSHIP_ROOT="$WORK_DIR/ownership" \
O3K_NETWORK_DHCP_ROOT="$WORK_DIR/dhcp" \
O3K_NETWORK_DNSMASQ="$DNSMASQ" \
O3K_NETWORK_EXTERNAL_REALM_ID="$EXTERNAL_REALM" \
O3K_NETWORK_UPLINK="$UPLINK" \
O3K_NETWORK_ROUTED_ROOT="$WORK_DIR/routed" \
O3K_NETWORK_POLICY_ROOT="$WORK_DIR/policy" \
O3K_NETWORK_PUBLIC_ROOT="$WORK_DIR/public" \
O3K_NETWORK_LISTEN=127.0.0.1:19182 \
O3K_NETWORK_TLS_CERT="$FIXTURES/server-chain.pem" \
O3K_NETWORK_TLS_KEY="$FIXTURES/server-key.pem" \
O3K_NETWORK_TLS_CLIENT_CA="$FIXTURES/ca.pem" \
"$BIN" >"$WORK_DIR/agent.log" 2>&1 &
AGENT_PID=$!
cat >"$WORK_DIR/plan.json" <<JSON
{"schema_version":1,"plan_id":"00000000-0000-0000-0000-000000000001","node_id":"agent-provider","operation_id":"00000000-0000-0000-0000-000000000002","deadline_unix_ms":4102444800000,"resource_generations":{"00000000-0000-0000-0000-000000000003":1},"intents":[{"AddressRealm":{"realm_id":"00000000-0000-0000-0000-000000000004","prefix":{"network":"10.0.0.0","prefix_len":24},"gateway":"10.0.0.1"}},{"EndpointAttachment":{"endpoint_id":"00000000-0000-0000-0000-000000000003","mac":"02:00:00:00:00:03","fixed_ip":"10.0.0.3","generation":1}},{"AddressAssignment":{"endpoint_id":"00000000-0000-0000-0000-000000000003","address":"10.0.0.3","generation":1}},{"Egress":{"external_realm_id":"$EXTERNAL_REALM","enabled":true,"nat":true}},{"PublicAddressBinding":{"id":"00000000-0000-0000-0000-000000000005","project_id":"project-a","public_address":"198.51.100.10","endpoint_id":"00000000-0000-0000-0000-000000000003","generation":1}},{"Policy":{"id":"00000000-0000-0000-0000-000000000007","endpoint_id":"00000000-0000-0000-0000-000000000003","direction":"Egress","protocol":"Tcp","ports":{"start":8082,"end":8082},"source":null,"destination":{"network":"198.51.100.0","prefix_len":24},"action":"Deny"}}],"fingerprint_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}
JSON
"$FINGERPRINT" "$WORK_DIR/plan.json" >"$WORK_DIR/plan.signed.json"
mv "$WORK_DIR/plan.signed.json" "$WORK_DIR/plan.json"
sed 's/198.51.100.10/198.51.100.11/' "$WORK_DIR/plan.json" >"$WORK_DIR/conflict.json"
"$FINGERPRINT" "$WORK_DIR/conflict.json" >"$WORK_DIR/conflict.signed.json"
mv "$WORK_DIR/conflict.signed.json" "$WORK_DIR/conflict.json"
sed 's/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000005/g' "$WORK_DIR/plan.json" >"$WORK_DIR/remove.json"
"$FINGERPRINT" "$WORK_DIR/remove.json" >"$WORK_DIR/remove.signed.json"
mv "$WORK_DIR/remove.signed.json" "$WORK_DIR/remove.json"
client_identity() {
    local agent_epoch="$1"
    local controller_epoch="$2"
    local fencing_token="$3"
    shift 3
    timeout 8 "$CLIENT" https://127.0.0.1:19182 o3k-control-plane \
        "$FIXTURES/ca.pem" "$FIXTURES/agent-chain.pem" "$FIXTURES/agent-key-pkcs8.pem" \
        agent-provider "$agent_epoch" controller-provider "$controller_epoch" "$fencing_token" "$1" "$2" "$3" \
        4102444800000 "$4" "${5:-}"
}
client_epoch() { client_identity "$1" epoch-1 7 "${@:2}"; }
client() { client_identity epoch-1 epoch-1 7 "$@"; }
deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
    if result="$(client 00000000-0000-0000-0000-000000000010 00000000-0000-0000-0000-000000000002 provider-apply "$WORK_DIR/plan.json" 2>/dev/null)"; then
        [[ "$result" == "succeeded false" ]] || { echo "unexpected result: $result" >&2; cat "$WORK_DIR/agent.log" >&2; exit 1; }
        break
    fi
    sleep 0.2
done
[[ "${result:-}" == "succeeded false" ]] || { cat "$WORK_DIR/agent.log" >&2; exit 1; }
if client 00000000-0000-0000-0000-000000000010 00000000-0000-0000-0000-000000000002 provider-conflict "$WORK_DIR/conflict.json" >/dev/null 2>&1; then
    echo "conflicting replay was accepted" >&2
    exit 1
fi
if client_epoch epoch-0 00000000-0000-0000-0000-000000000012 00000000-0000-0000-0000-000000000002 stale-epoch "$WORK_DIR/plan.json" >/dev/null 2>&1; then
    echo "stale agent epoch was accepted" >&2
    exit 1
fi
if client_identity epoch-1 epoch-0 6 00000000-0000-0000-0000-000000000013 00000000-0000-0000-0000-000000000002 stale-controller "$WORK_DIR/plan.json" >/dev/null 2>&1; then
    echo "stale controller lease was accepted" >&2
    exit 1
fi
ip link show "$BRIDGE" >/dev/null
if ip -o link show "$UPLINK" | grep -q "master $BRIDGE"; then
    echo "routed external uplink was enslaved into tenant bridge" >&2
    exit 1
fi
ip -4 addr show dev "$UPLINK" | grep -q '198.51.100.10/32'
nft list table ip o3k_p9 >/dev/null
nft list table ip o3k_public >/dev/null
nft list table ip o3k_policy >/dev/null
[[ "$(client 00000000-0000-0000-0000-000000000010 00000000-0000-0000-0000-000000000002 provider-apply "$WORK_DIR/plan.json")" == "replayed true" ]]
[[ "$(client 00000000-0000-0000-0000-000000000011 00000000-0000-0000-0000-000000000005 provider-remove "$WORK_DIR/remove.json" remove)" == "succeeded false" ]]
deadline=$((SECONDS + 10))
while { ip link show "$BRIDGE" >/dev/null 2>&1 || nft list table ip o3k_p9 >/dev/null 2>&1 || nft list table ip o3k_public >/dev/null 2>&1 || nft list table ip o3k_policy >/dev/null 2>&1; } && ((SECONDS < deadline)); do
    sleep 0.1
done
if ip link show "$BRIDGE" >/dev/null 2>&1 || ip -4 addr show dev "$UPLINK" | grep -q '198.51.100.10/32'; then
    echo "owned routed/public resources remained after removal" >&2
    exit 1
fi
echo "P9 network-agent provider gate passed (routed/public/policy realization, replay, cleanup)"
