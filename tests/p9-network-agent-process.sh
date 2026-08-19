#!/usr/bin/env bash
set -Eeuo pipefail

# Black-box process gate for the real node-local network executor. This is
# intentionally narrower than the protected P9 profile: it proves the actual
# o3k-network binary, mTLS protocol client, durable replay, and owned cleanup
# in an isolated network namespace, without claiming real guest traffic.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_NETWORK_AGENT_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_NETWORK_AGENT_INNER=1 "$0"
fi

for tool in cargo ip unshare; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "missing required tool: $tool" >&2
        exit 2
    }
done

cargo build -p o3k-network-bin >/dev/null
cargo build -p o3k-network-protocol --example network-agent-client >/dev/null

BIN="$ROOT_DIR/target/debug/o3k-network-bin"
CLIENT="$ROOT_DIR/target/debug/examples/network-agent-client"
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-agent-process.XXXXXX")"
AGENT_PID=""

ip link set lo up

cleanup() {
    set +e
    [[ -n "$AGENT_PID" ]] && kill "$AGENT_PID" 2>/dev/null
    [[ -n "$AGENT_PID" ]] && wait "$AGENT_PID" 2>/dev/null
    ip link del o3kp9br0 2>/dev/null
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

chmod 0755 "$DNSMASQ"
mkdir -p "$WORK_DIR"/{executor,ownership,dhcp}
cat >"$WORK_DIR/plan.json" <<'JSON'
{
  "schema_version": 1,
  "plan_id": "00000000-0000-0000-0000-000000000001",
  "node_id": "agent-process",
  "operation_id": "00000000-0000-0000-0000-000000000002",
  "deadline_unix_ms": 4102444800000,
  "resource_generations": {
    "00000000-0000-0000-0000-000000000003": 1
  },
  "intents": [
    {
      "AddressRealm": {
        "realm_id": "00000000-0000-0000-0000-000000000004",
        "prefix": {"network": "10.0.0.0", "prefix_len": 24},
        "gateway": "10.0.0.1"
      }
    },
    {
      "EndpointAttachment": {
        "endpoint_id": "00000000-0000-0000-0000-000000000003",
        "mac": "02:00:00:00:00:03",
        "fixed_ip": "10.0.0.3",
        "generation": 1
      }
    },
    {
      "AddressAssignment": {
        "endpoint_id": "00000000-0000-0000-0000-000000000003",
        "address": "10.0.0.3",
        "generation": 1
      }
    }
  ],
  "fingerprint_sha256": "0000000000000000000000000000000000000000000000000000000000000000"
}
JSON
sed 's/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000005/g' \
    "$WORK_DIR/plan.json" >"$WORK_DIR/plan-remove.json"

start_agent() {
    O3K_NETWORK_AGENT_ID=agent-process \
    O3K_NETWORK_AGENT_EPOCH=epoch-1 \
    O3K_NETWORK_CONTROLLER_ID=controller-process \
    O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
    O3K_NETWORK_FENCING_TOKEN=7 \
    O3K_NETWORK_ROOT="$WORK_DIR/executor" \
    O3K_NETWORK_BRIDGE=o3kp9br0 \
    O3K_NETWORK_OWNERSHIP_ROOT="$WORK_DIR/ownership" \
    O3K_NETWORK_DHCP_ROOT="$WORK_DIR/dhcp" \
    O3K_NETWORK_DNSMASQ="$DNSMASQ" \
    O3K_NETWORK_LISTEN=127.0.0.1:19181 \
    O3K_NETWORK_TLS_CERT="$FIXTURES/server-chain.pem" \
    O3K_NETWORK_TLS_KEY="$FIXTURES/server-key.pem" \
    O3K_NETWORK_TLS_CLIENT_CA="$FIXTURES/ca.pem" \
    "$BIN" >>"$WORK_DIR/agent.log" 2>&1 &
    AGENT_PID=$!
}

start_agent

client() {
    local command_id="$1" operation_id="$2" key="$3" mode="${4:-}"
    local plan_path="${5:-$WORK_DIR/plan.json}"
    timeout 5 "$CLIENT" \
        https://127.0.0.1:19181 o3k-control-plane \
        "$FIXTURES/ca.pem" "$FIXTURES/agent-chain.pem" \
        "$FIXTURES/agent-key-pkcs8.pem" \
        agent-process epoch-1 controller-process epoch-1 7 \
        "$command_id" "$operation_id" "$key" 4102444800000 \
        "$plan_path" "$mode"
}

deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
    if result="$(client 00000000-0000-0000-0000-000000000010 \
        00000000-0000-0000-0000-000000000002 process-apply 2>/dev/null)"; then
        [[ "$result" == "succeeded false" ]] || {
            echo "unexpected initial result: $result" >&2
            sed -n '1,160p' "$WORK_DIR/agent.log" >&2
            exit 1
        }
        break
    fi
    sleep 0.2
done
[[ -n "${result:-}" ]] || {
    echo "network agent did not accept the initial command" >&2
    sed -n '1,160p' "$WORK_DIR/agent.log" >&2
    exit 1
}

if ! second_result="$(client 00000000-0000-0000-0000-000000000010 \
    00000000-0000-0000-0000-000000000002 process-apply)"; then
    echo "replay client failed" >&2
    cat "$WORK_DIR/executor/accepted-network-plans.json" >&2
    exit 1
fi
[[ "$second_result" == "replayed true" ]] || {
    echo "unexpected replay result: $second_result" >&2
    cat "$WORK_DIR/executor/accepted-network-plans.json" >&2
    exit 1
}
[[ -e "$WORK_DIR/executor/accepted-network-plans.json" ]]
ip link show o3kp9br0 >/dev/null

# Restart the real agent while its owned bridge and dnsmasq process remain.
# Startup must adopt the durable process identity, and replay must not mutate
# or duplicate the already-realized plan.
kill "$AGENT_PID"
wait "$AGENT_PID" 2>/dev/null || true
AGENT_PID=""
start_agent
deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
    if restart_result="$(client 00000000-0000-0000-0000-000000000010 \
        00000000-0000-0000-0000-000000000002 process-apply 2>/dev/null)"; then
        [[ "$restart_result" == "replayed true" ]] || {
            echo "unexpected restart replay result: $restart_result" >&2
            sed -n '1,200p' "$WORK_DIR/agent.log" >&2
            exit 1
        }
        break
    fi
    sleep 0.2
done
[[ "${restart_result:-}" == "replayed true" ]] || {
    echo "network agent did not recover after restart" >&2
    sed -n '1,200p' "$WORK_DIR/agent.log" >&2
    exit 1
}
ip link show o3kp9br0 >/dev/null

[[ "$(client 00000000-0000-0000-0000-000000000011 \
    00000000-0000-0000-0000-000000000005 process-remove remove \
    "$WORK_DIR/plan-remove.json")" == "succeeded false" ]]
deadline=$((SECONDS + 10))
while ip link show o3kp9br0 >/dev/null 2>&1 && ((SECONDS < deadline)); do
    sleep 0.1
done
if ip link show o3kp9br0 >/dev/null 2>&1; then
    echo "owned process-test bridge remained after removal" >&2
    exit 1
fi

echo "P9 network-agent process gate passed (mTLS, real binary, replay, cleanup)"
