#!/usr/bin/env bash
set -Eeuo pipefail

# Black-box P9 control-plane gate.  It drives the public o3kd API, while the
# real o3k-network binary owns the host-local TAP/bridge/DHCP mutation.  This
# is intentionally a portable process gate by default. With
# O3K_P9_PUBLIC_API_QEMU=1 it continues the same lifecycle into a disposable
# real QEMU guest through the packet-path fixture. With
# O3K_P9_PUBLIC_API_LIBVIRT=1 it uses the same public API lifecycle with the
# real agent provider and qemu:///system/libvirt.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_PUBLIC_API_INNER:-}" != 1 && "${O3K_P9_PUBLIC_API_LIBVIRT:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_PUBLIC_API_INNER=1 "$0" "$@"
fi
[[ "${O3K_P9_DEBUG:-}" == 1 ]] && set -x

TOOLS=(cargo curl ip nft openssl setsid unshare python3)
if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 ]]; then
    TOOLS+=(qemu-system-x86_64 cpio gzip)
fi
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    TOOLS+=(virsh sha256sum)
    [[ -e /dev/kvm ]] || { echo "missing required device: /dev/kvm" >&2; exit 2; }
    virsh -c qemu:///system uri >/dev/null 2>&1 || {
        echo "qemu:///system is unavailable" >&2
        exit 2
    }
    [[ -n "${O3K_P9_CIRROS_IMAGE:-}" && -r "${O3K_P9_CIRROS_IMAGE}" ]] || {
        echo "O3K_P9_CIRROS_IMAGE must name a readable CirrOS image" >&2
        exit 2
    }
fi
for tool in "${TOOLS[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

cargo build -p o3kd -p o3k-network-bin >/dev/null
BIN="$ROOT_DIR/target/debug/o3kd"
NETWORK_BIN="$ROOT_DIR/target/debug/o3k-network-bin"
COMPUTE_BIN=""
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    cargo build --features libvirt --bin o3k-compute-bin >/dev/null
    COMPUTE_BIN="$ROOT_DIR/target/debug/o3k-compute-bin"
fi
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    DNSMASQ="${O3K_P9_LIBVIRT_DNSMASQ:-/usr/sbin/dnsmasq}"
fi
KERNEL="${O3K_P9_QEMU_KERNEL:-/boot/vmlinuz}"
BUSYBOX="${O3K_P9_QEMU_BUSYBOX:-/usr/bin/busybox}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-public-api.XXXXXX")"
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    # qemu:///system runs as the libvirt qemu user. Grant the kvm group only
    # traversal through this disposable run directory; sensitive files retain
    # their own permissions and the cache below remains the only readable
    # artifact tree.
    chgrp kvm "$WORK_DIR"
    chmod 0710 "$WORK_DIR"
fi
O3KD_PID=""
NETWORK_PID=""
EXT_PID=""
QEMU_PID=""
EXTERNAL_SERVER_PID=""
COMPUTE_PID=""
DOMAIN_NAME=""
NETWORK_TAP_USER=""
NETWORK_TAP_GROUP=""
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    NETWORK_TAP_USER=libvirt-qemu
    NETWORK_TAP_GROUP=kvm
fi
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
COMPUTE_CONTROL_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
COMPUTE_HEALTH_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
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

remove_owned_network_tables() {
    local name marker table
    for name in o3k_policy o3k_public o3k_p9; do
        case "$name" in
            o3k_policy) marker='o3k-p9-policy:' ;;
            o3k_public) marker='o3k-p9-public' ;;
            o3k_p9) marker='o3k-p9-managed' ;;
        esac
        table="$(nft list table ip "$name" 2>/dev/null || true)"
        # Markers are the provider ownership proof. Never remove an unmarked
        # table: the gate must not mutate foreign firewall state.
        if grep -Fq "$marker" <<<"$table"; then
            nft delete table ip "$name" >/dev/null 2>&1 || true
        fi
    done
}

# A previous interrupted gate may have left the provider's explicitly marked
# table behind after its disposable state directory was removed.
remove_owned_network_tables
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    # qemu needs read/traverse access to this run-owned cache through kvm;
    # keep it group-scoped instead of weakening permissions globally.
    install -d -o root -g kvm -m 2770 "$WORK_DIR/compute"
fi

cleanup() {
    set +e
    if [[ "${O3K_P9_KEEP_LOGS:-}" == 1 ]]; then
        echo "--- o3kd log ---" >&2
        tail -120 "$WORK_DIR/o3kd.log" >&2 2>/dev/null
        echo "--- network log ---" >&2
        tail -120 "$WORK_DIR/network.log" >&2 2>/dev/null
        echo "--- compute log ---" >&2
        tail -120 "$WORK_DIR/compute.log" >&2 2>/dev/null
        echo "--- host network links ---" >&2
        ip -d link show >&2 2>/dev/null || true
        echo "logs kept at $WORK_DIR" >&2
    fi
    [[ -n "$QEMU_PID" ]] && kill "$QEMU_PID" 2>/dev/null
    [[ -n "$QEMU_PID" ]] && wait "$QEMU_PID" 2>/dev/null
    if [[ -z "$DOMAIN_NAME" && -n "${SERVER_ID:-}" && "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
        for candidate in $(virsh -c qemu:///system list --all --name 2>/dev/null); do
            xml="$(virsh -c qemu:///system dumpxml "$candidate" 2>/dev/null || true)"
            if grep -Fq "server_id=\"$SERVER_ID\"" <<<"$xml" \
                && grep -Fq 'managed_by="o3k-compute"' <<<"$xml"; then
                DOMAIN_NAME="$candidate"
                break
            fi
        done
    fi
    if [[ -n "$DOMAIN_NAME" ]]; then
        DOMAIN_XML="$(virsh -c qemu:///system dumpxml "$DOMAIN_NAME" 2>/dev/null || true)"
        if printf '%s\n' "$DOMAIN_XML" | grep -Fq 'managed_by="o3k-compute"' \
            && printf '%s\n' "$DOMAIN_XML" | grep -Fq "server_id=\"${SERVER_ID:-}\""; then
            virsh -c qemu:///system destroy "$DOMAIN_NAME" >/dev/null 2>&1 || true
            virsh -c qemu:///system undefine "$DOMAIN_NAME" >/dev/null 2>&1 || true
        fi
    fi
    [[ -n "$EXTERNAL_SERVER_PID" ]] && kill "$EXTERNAL_SERVER_PID" 2>/dev/null
    [[ -n "$EXTERNAL_SERVER_PID" ]] && wait "$EXTERNAL_SERVER_PID" 2>/dev/null
    # The DHCP supervisor may have re-parented its child before the agent is
    # stopped.  Reap only the pid recorded for this run, and verify its
    # command line still names this run's configuration before signalling it.
    for pid_file in "$WORK_DIR"/network/dhcp/*.pid; do
        [[ -r "$pid_file" ]] || continue
        dnsmasq_pid="$(cat "$pid_file" 2>/dev/null || true)"
        [[ "$dnsmasq_pid" =~ ^[0-9]+$ ]] || continue
        if [[ -r "/proc/$dnsmasq_pid/cmdline" ]] \
            && tr '\0' ' ' <"/proc/$dnsmasq_pid/cmdline" \
                | grep -Fq "$WORK_DIR/network/dhcp/dnsmasq.conf"; then
            kill "$dnsmasq_pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
                kill -0 "$dnsmasq_pid" 2>/dev/null || break
                sleep 0.05
            done
        fi
    done
    [[ -n "$O3KD_PID" ]] && kill "$O3KD_PID" 2>/dev/null
    [[ -n "$NETWORK_PID" ]] && kill "$NETWORK_PID" 2>/dev/null
    [[ -n "$COMPUTE_PID" ]] && kill "$COMPUTE_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && kill "$EXT_PID" 2>/dev/null
    [[ -n "$O3KD_PID" ]] && wait "$O3KD_PID" 2>/dev/null
    [[ -n "$NETWORK_PID" ]] && wait "$NETWORK_PID" 2>/dev/null
    [[ -n "$COMPUTE_PID" ]] && wait "$COMPUTE_PID" 2>/dev/null
    [[ -n "$EXT_PID" ]] && wait "$EXT_PID" 2>/dev/null
    ip link del o3kp9api0 2>/dev/null
    ip link del "$UPLINK" 2>/dev/null
    ip link del "$FOREIGN_LINK" 2>/dev/null
    remove_owned_network_tables
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
    O3K_NETWORK_TAP_USER="$NETWORK_TAP_USER" \
    O3K_NETWORK_TAP_GROUP="$NETWORK_TAP_GROUP" \
    O3K_NETWORK_LISTEN="127.0.0.1:$NETWORK_PORT" \
    O3K_NETWORK_TLS_CERT="$FIXTURES/server-chain.pem" \
    O3K_NETWORK_TLS_KEY="$FIXTURES/server-key.pem" \
    O3K_NETWORK_TLS_CLIENT_CA="$FIXTURES/ca.pem" \
    RUST_LOG="${O3K_P9_LOG_FILTER:-info}" "$NETWORK_BIN" >>"$WORK_DIR/network.log" 2>&1 &
    NETWORK_PID=$!
}
start_network_agent
for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$NETWORK_PORT") 2>/dev/null; then
        exec 3>&-
        exec 3<&-
        break
    fi
    kill -0 "$NETWORK_PID" 2>/dev/null || {
        echo "network agent exited before listening" >&2
        cat "$WORK_DIR/network.log" >&2 || true
        exit 1
    }
    sleep 0.1
done

SIGNING_KEY="$(openssl rand -hex 48)"
PROVIDER=fake
AUTHORIZED_FINGERPRINT=""
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    PROVIDER=agent
    AUTHORIZED_FINGERPRINT="$(openssl x509 -in "$FIXTURES/agent.pem" -outform DER | sha256sum | awk '{print $1}')"
    printf 'node-test\n' >"$WORK_DIR/compute/agent-id"
    chmod 0640 "$WORK_DIR/compute/agent-id"
    export O3K_COMPUTE_AUTHORIZED_AGENTS="node-test=$AUTHORIZED_FINGERPRINT"
else
    # No compute agent or compute-control TLS is started in the portable mode.
    unset O3K_COMPUTE_AUTHORIZED_AGENTS
fi
export O3K_PROVIDER="$PROVIDER" O3K_DATA_DIR="$WORK_DIR/data" \
    O3K_LISTEN_ADDR="127.0.0.1:$PORT" O3K_CONTROLLER_ID=controller-api \
    O3K_CONTROLLER_EPOCH=epoch-1 O3K_BOOTSTRAP_PASSWORD=password \
    O3K_TOKEN_SIGNING_KEY="$SIGNING_KEY" \
    O3K_NETWORK_AGENT_ENDPOINT="https://127.0.0.1:$NETWORK_PORT" \
    O3K_NETWORK_AGENT_SERVER_NAME=o3k-control-plane \
    O3K_NETWORK_AGENT_CA="$FIXTURES/ca.pem" \
    O3K_NETWORK_AGENT_CLIENT_CERT="$FIXTURES/agent-chain.pem" \
    O3K_NETWORK_AGENT_CLIENT_KEY="$FIXTURES/agent-key-pkcs8.pem" \
    O3K_NETWORK_AGENT_ID=agent-api O3K_NETWORK_AGENT_EPOCH=epoch-1 \
    O3K_NETWORK_CONTROLLER_ID=controller-api O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
    O3K_NETWORK_FENCING_TOKEN=7 O3K_NETWORK_EXTERNAL_REALM_ID="$EXTERNAL_REALM" \
    O3K_PUBLIC_POOL_CIDR=198.51.100.0/24 O3K_PUBLIC_POOL_FIRST=198.51.100.10 \
    O3K_PUBLIC_POOL_LAST=198.51.100.20
if [[ "$PROVIDER" == agent ]]; then
    export O3K_COMPUTE_CONTROL_ADDR="127.0.0.1:$COMPUTE_CONTROL_PORT" \
        O3K_COMPUTE_SERVER_CERTIFICATE="$FIXTURES/server.pem" \
        O3K_COMPUTE_SERVER_PRIVATE_KEY="$FIXTURES/server-key.pem" \
        O3K_COMPUTE_CLIENT_CA="$FIXTURES/ca.pem"
else
    unset O3K_COMPUTE_SERVER_CERTIFICATE O3K_COMPUTE_SERVER_PRIVATE_KEY O3K_COMPUTE_CLIENT_CA
fi
BASE="http://127.0.0.1:$PORT"
start_controller() {
    "$BIN" --listen-addr "127.0.0.1:$PORT" --data-dir "$WORK_DIR/data" --log-filter debug \
        >"$WORK_DIR/o3kd.log" 2>&1 &
    O3KD_PID=$!
    for _ in $(seq 1 120); do
        if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
            curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break
        else
            curl -fsS "$BASE/readyz" >/dev/null 2>&1 && break
        fi
        sleep 0.1
    done
    if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
        curl -fsS "$BASE/healthz" >/dev/null
    else
        curl -fsS "$BASE/readyz" >/dev/null
    fi
}

restart_controller() {
    kill "$O3KD_PID" 2>/dev/null || true
    wait "$O3KD_PID" 2>/dev/null || true
    O3KD_PID=""
    start_controller
}

start_controller

if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    O3K_COMPUTE_DATA_DIR="$WORK_DIR/compute" \
    O3K_COMPUTE_CONTROL_ENDPOINT="https://127.0.0.1:$COMPUTE_CONTROL_PORT" \
    O3K_COMPUTE_SERVER_NAME=o3k-control-plane \
    O3K_COMPUTE_HOST_LABEL=compute-agent \
    O3K_COMPUTE_TLS_DIR="$FIXTURES" \
    O3K_COMPUTE_HEALTH_ADDR="127.0.0.1:$COMPUTE_HEALTH_PORT" \
    O3K_COMPUTE_MAX_DISK_GB=10 \
    O3K_COMPUTE_NETWORK_EXTERNAL=1 \
    O3K_COMPUTE_NETWORK_ROOT="$WORK_DIR/network/ownership" \
    O3K_COMPUTE_BRIDGE_NAME=o3kp9api0 \
    O3K_COMPUTE_DHCP_BINARY="$DNSMASQ" \
    RUST_LOG=info \
    "$COMPUTE_BIN" >"$WORK_DIR/compute.log" 2>&1 &
    COMPUTE_PID=$!
    for _ in $(seq 1 120); do
        curl -fsS "http://127.0.0.1:$COMPUTE_HEALTH_PORT/readyz" >/dev/null 2>&1 && break
        sleep 0.1
    done
    curl -fsS "http://127.0.0.1:$COMPUTE_HEALTH_PORT/readyz" >/dev/null
    for _ in $(seq 1 120); do
        curl -fsS "$BASE/readyz" >/dev/null 2>&1 && break
        sleep 0.1
    done
    curl -fsS "$BASE/readyz" >/dev/null
fi

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

if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    IMAGE_ID="$(json -X POST "$BASE/v2/images" -H 'content-type: application/json' \
        --data '{"name":"p9-api-cirros","container_format":"bare","disk_format":"qcow2"}' | field id)"
    curl -fsS -X PUT "$BASE/v2/images/$IMAGE_ID/file" -H "x-auth-token: $TOKEN" \
        -H 'content-type: application/octet-stream' --data-binary "@$O3K_P9_CIRROS_IMAGE" >/dev/null
else
    IMAGE_ID="$(json -X POST "$BASE/v2/images" -H 'content-type: application/json' \
        --data '{"name":"p9-api-image","container_format":"bare","disk_format":"raw"}' | field id)"
    curl -fsS -X PUT "$BASE/v2/images/$IMAGE_ID/file" -H "x-auth-token: $TOKEN" \
        -H 'content-type: application/octet-stream' --data-binary 'p9-api-image-bytes' >/dev/null
fi
NETWORK_ID="$(json -X POST "$BASE/v2.0/networks" -H 'content-type: application/json' \
    --data '{"network":{"name":"p9-api-network"}}' | field network.id)"
SUBNET_JSON="$(json -X POST "$BASE/v2.0/subnets" -H 'content-type: application/json' \
    --data "{\"subnet\":{\"name\":\"p9-api-subnet\",\"network_id\":\"$NETWORK_ID\",\"cidr\":\"10.0.0.0/24\"}}")"
SUBNET_ID="$(printf '%s' "$SUBNET_JSON" | field subnet.id)"
GATEWAY_IP="$(printf '%s' "$SUBNET_JSON" | field subnet.gateway_ip)"
PORT_JSON="$(json -X POST "$BASE/v2.0/ports" -H 'content-type: application/json' \
    --data "{\"port\":{\"name\":\"p9-api-port\",\"network_id\":\"$NETWORK_ID\"}}")"
PORT_ID="$(printf '%s' "$PORT_JSON" | field port.id)"
FIXED_IP="$(printf '%s' "$PORT_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["port"]["fixed_ips"][0]["ip_address"])')"
PORT_MAC="$(printf '%s' "$PORT_JSON" | field port.mac_address)"

# The bounded Neutron projection is exercised before scheduling.  Rules remain
# project-owned canonical policy and are expanded into the attachment plan;
# the API must persist them even while the endpoint is unbound.
SECURITY_GROUP_ID="$(json -X POST "$BASE/v2.0/security-groups" -H 'content-type: application/json' \
    --data '{"security_group":{"name":"p9-api-web","description":"bounded IPv4 policy"}}' \
    | field security_group.id)"
json "$BASE/v2.0/security-groups" | grep -Fq "$SECURITY_GROUP_ID"
json "$BASE/v2.0/security-groups/$SECURITY_GROUP_ID" | grep -Fq "$SECURITY_GROUP_ID"
json -X PUT "$BASE/v2.0/security-groups/$SECURITY_GROUP_ID" -H 'content-type: application/json' \
    --data '{"security_group":{"name":"p9-api-web","description":"bounded IPv4 policy updated"}}' \
    | grep -Fq 'bounded IPv4 policy updated'
INGRESS_RULE_JSON="$(json -X POST "$BASE/v2.0/security-group-rules" -H 'content-type: application/json' \
    --data "{\"security_group_rule\":{\"security_group_id\":\"$SECURITY_GROUP_ID\",\"direction\":\"ingress\",\"protocol\":\"tcp\",\"port_range_min\":8080,\"port_range_max\":8080,\"remote_ip_prefix\":\"0.0.0.0/0\",\"ethertype\":\"IPv4\"}}"
)"
INGRESS_RULE_ID="$(printf '%s' "$INGRESS_RULE_JSON" | field security_group_rule.id)"
EGRESS_RULE_JSON="$(json -X POST "$BASE/v2.0/security-group-rules" -H 'content-type: application/json' \
    --data "{\"security_group_rule\":{\"security_group_id\":\"$SECURITY_GROUP_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"port_range_min\":8081,\"port_range_max\":8081,\"remote_ip_prefix\":\"198.51.100.0/24\",\"ethertype\":\"IPv4\"}}"
)"
EGRESS_RULE_ID="$(printf '%s' "$EGRESS_RULE_JSON" | field security_group_rule.id)"
json "$BASE/v2.0/security-group-rules?security_group_id=$SECURITY_GROUP_ID" \
    | grep -Fq "$INGRESS_RULE_ID"
json "$BASE/v2.0/security-group-rules/$EGRESS_RULE_ID" \
    | grep -Fq "$EGRESS_RULE_ID"
json -X PUT "$BASE/v2.0/ports/$PORT_ID" -H 'content-type: application/json' \
    --data "{\"port\":{\"security_groups\":[\"$SECURITY_GROUP_ID\"]}}" \
    | grep -Fq "$SECURITY_GROUP_ID"
# Native O3K deny remains a separate canonical policy action; this proves the
# compatibility allow projection does not silently change fail-closed deny
# semantics.
POLICY_JSON="$(json -X POST "$BASE/v2.0/network-policies" -H 'content-type: application/json' \
    -H 'idempotency-key: p9-api-policy' \
    --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"deny\"}}")"
POLICY_ID="$(printf '%s' "$POLICY_JSON" | field policy.id)"

FLAVOR_ID="$(json "$BASE/v2.1/$PROJECT_ID/flavors" | python3 -c 'import json,sys; print(json.load(sys.stdin)["flavors"][0]["id"])')"
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    nsenter -t "$EXT_PID" -n -- python3 -m http.server 8081 --bind 198.51.100.2 \
        >/dev/null 2>&1 &
    EXTERNAL_SERVER_PID=$!
    nsenter -t "$EXT_PID" -n -- python3 -m http.server 8082 --bind 198.51.100.2 \
        >/dev/null 2>&1 &
    USER_DATA_FILE="$WORK_DIR/p9-user-data"
    cat >"$USER_DATA_FILE" <<'GUEST'
#!/bin/sh
exec >/dev/console 2>&1
mkdir -p /tmp/www
printf 'o3k-public-api-libvirt-guest\n' >/tmp/www/index.html
busybox httpd -f -p 8080 -h /tmp/www &
echo O3K_PUBLIC_API_LIBVIRT_GUEST_READY
busybox wget -q -T 5 -O /dev/null http://198.51.100.2:8081/
echo O3K_PUBLIC_API_LIBVIRT_EGRESS_ALLOWED
if busybox wget -q -T 3 -O /dev/null http://198.51.100.2:8082/; then
    echo O3K_PUBLIC_API_LIBVIRT_EGRESS_DENIED_FAILED
else
    echo O3K_PUBLIC_API_LIBVIRT_EGRESS_DENIED
fi
while ! busybox wget -q -T 2 -O /dev/null http://198.51.100.2:8082/; do sleep 1; done
echo O3K_PUBLIC_API_POLICY_UPDATE_ALLOWED
while busybox wget -q -T 2 -O /dev/null http://198.51.100.2:8082/; do sleep 1; done
echo O3K_PUBLIC_API_POLICY_UPDATE_DENIED
GUEST
    SERVER_REQUEST="$WORK_DIR/server-request.json"
    python3 - "$SERVER_REQUEST" "$IMAGE_ID" "$FLAVOR_ID" "$PORT_ID" "$USER_DATA_FILE" <<'PY'
import json
import pathlib
import sys

out, image, flavor, port, user_data = sys.argv[1:]
payload = {
    "server": {
        "name": "p9-api-libvirt-server",
        "image": {"id": image},
        "flavor": {"id": flavor},
        "networks": [{"uuid": port}],
        "config_drive": True,
        "ssh_public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA p9-gate",
        "user_data": pathlib.Path(user_data).read_text(),
    }
}
pathlib.Path(out).write_text(json.dumps(payload))
PY
    SERVER_RESPONSE="$(curl -sS -X POST "$BASE/v2.1/$PROJECT_ID/servers" -H 'content-type: application/json' \
        -H 'x-openstack-request-id: p9-api-server-create' \
        --data-binary "@$SERVER_REQUEST" -H "x-auth-token: $TOKEN")"
else
    SERVER_RESPONSE="$(curl -sS -X POST "$BASE/v2.1/$PROJECT_ID/servers" -H 'content-type: application/json' \
        -H 'x-openstack-request-id: p9-api-server-create' \
        --data "{\"server\":{\"name\":\"p9-api-server\",\"image\":{\"id\":\"$IMAGE_ID\"},\"flavor\":{\"id\":\"$FLAVOR_ID\"},\"networks\":[{\"uuid\":\"$PORT_ID\"}]}}" -H "x-auth-token: $TOKEN")"
fi
SERVER_ID="$(printf '%s' "$SERVER_RESPONSE" | field server.id 2>/dev/null || true)"
[[ -n "$SERVER_ID" ]] || {
    echo "server create failed: $SERVER_RESPONSE" >&2
    exit 1
}
if [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    for _ in $(seq 1 180); do
        SERVER_STATUS="$(json "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID" | field server.status)"
        [[ "$SERVER_STATUS" == ACTIVE ]] && break
        [[ "$SERVER_STATUS" == ERROR ]] && { echo "real public API server entered ERROR" >&2; exit 1; }
        sleep 0.5
    done
    [[ "$SERVER_STATUS" == ACTIVE ]] || { echo "real public API server did not become ACTIVE: $SERVER_STATUS" >&2; exit 1; }
fi

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
    --data "{\"floatingip\":{\"floating_network_id\":\"$EXTERNAL_REALM\",\"port_id\":\"$PORT_ID\"}}" 2>/dev/null || true)"
if [[ -z "$FLOATING_JSON" ]]; then
    for _ in $(seq 1 80); do
        FLOATING_JSON="$(json -X POST "$BASE/v2.0/floatingips" \
            -H 'content-type: application/json' \
            -H 'x-openstack-request-id: p9-api-floating-create-retry' \
            --data "{\"floatingip\":{\"floating_network_id\":\"$EXTERNAL_REALM\",\"port_id\":\"$PORT_ID\"}}" \
            2>/dev/null || true)"
        [[ -n "$FLOATING_JSON" ]] && break
        sleep 0.1
    done
fi
[[ -n "$FLOATING_JSON" ]] || { echo "floating-IP creation did not converge after network-agent restart" >&2; exit 1; }
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

if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 ]]; then
    [[ -r "$KERNEL" && -x "$BUSYBOX" ]] || {
        echo "QEMU kernel or busybox unavailable" >&2
        exit 2
    }
    nsenter -t "$EXT_PID" -n -- python3 -m http.server 8081 --bind 198.51.100.2 \
        >/dev/null 2>&1 &
    EXTERNAL_SERVER_PID=$!
    nsenter -t "$EXT_PID" -n -- python3 -m http.server 8082 --bind 198.51.100.2 \
        >/dev/null 2>&1 &

    ROOTFS="$WORK_DIR/rootfs"
    INITRD="$WORK_DIR/initrd.cpio.gz"
    SERIAL="$WORK_DIR/serial.log"
    mkdir -p "$ROOTFS"/{bin,dev,etc,proc,sys,tmp,www}
    cp "$BUSYBOX" "$ROOTFS/bin/busybox"
    ln -s busybox "$ROOTFS/init"
    for applet in cat grep httpd ip kill mount poweroff sh sleep wget; do
        ln -s busybox "$ROOTFS/bin/$applet"
    done
    cat >"$ROOTFS/etc/guest-init" <<GUEST
#!/bin/sh
set -eu
exec >/dev/console 2>&1
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
ip link set lo up
ip link set eth0 up
ip addr add $FIXED_IP/24 dev eth0
ip route add default via $GATEWAY_IP
mkdir -p /tmp/www
printf 'o3k-public-api-real-guest\\n' >/tmp/www/index.html
httpd -f -p 8080 -h /tmp/www &
echo O3K_PUBLIC_API_GUEST_READY
wget -q -T 4 -O /dev/null http://198.51.100.2:8081/
echo O3K_PUBLIC_API_EGRESS_ALLOWED
wget -q -T 2 -O /dev/null http://198.51.100.2:8082/ &
probe_pid=\$!
sleep 4
kill "\$probe_pid" 2>/dev/null || true
wait "\$probe_pid" 2>/dev/null || true
echo O3K_PUBLIC_API_EGRESS_DENIED
while ! wget -q -T 2 -O /dev/null http://198.51.100.2:8082/; do sleep 1; done
echo O3K_PUBLIC_API_POLICY_UPDATE_ALLOWED
while wget -q -T 2 -O /dev/null http://198.51.100.2:8082/; do sleep 1; done
echo O3K_PUBLIC_API_POLICY_UPDATE_DENIED
sleep 30
GUEST
    chmod 0755 "$ROOTFS/etc/guest-init"
    ln -sf /etc/guest-init "$ROOTFS/init"
    (cd "$ROOTFS" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9 >"$INITRD")
    TAP="o3ktap-$(printf '%s' "$PORT_ID" | sha256sum | cut -c1-8)"
    qemu-system-x86_64 -enable-kvm -cpu host -m 128M -smp 1 -kernel "$KERNEL" \
        -initrd "$INITRD" \
        -append 'console=ttyS0,115200 init=/init net.ifnames=0 biosdevname=0' \
        -nographic -nodefaults -no-reboot -serial "file:$SERIAL" \
        -device "virtio-net-pci,netdev=net0,mac=$PORT_MAC" \
        -netdev "tap,id=net0,ifname=$TAP,script=no,downscript=no" \
        >/dev/null 2>&1 &
    QEMU_PID=$!
    wait_marker() {
        local marker="$1" deadline=$((SECONDS + 40))
        while ((SECONDS < deadline)); do
            grep -q "$marker" "$SERIAL" 2>/dev/null && return 0
            kill -0 "$QEMU_PID" 2>/dev/null || return 1
            sleep 0.1
        done
        return 1
    }
    wait_marker O3K_PUBLIC_API_GUEST_READY
    FLOATING_ADDRESS="$(printf '%s' "$FLOATING_JSON" | field floatingip.floating_ip_address)"
    nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 4 \
        "http://$FLOATING_ADDRESS:8080/" | grep -q o3k-public-api-real-guest
    ip link set "$UPLINK" down
    if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
        "http://$FLOATING_ADDRESS:8080/" >/dev/null 2>&1; then
        echo "external uplink outage did not interrupt public traffic" >&2
        exit 1
    fi
    ip link set "$UPLINK" up
    recovered=0
    for _ in $(seq 1 80); do
        if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
            "http://$FLOATING_ADDRESS:8080/" 2>/dev/null \
            | grep -q o3k-public-api-real-guest; then
            recovered=1
            break
        fi
        sleep 0.1
    done
    [[ "$recovered" == 1 ]] || { echo "public traffic did not recover after external uplink restoration" >&2; exit 1; }
    wait_marker O3K_PUBLIC_API_EGRESS_ALLOWED
    wait_marker O3K_PUBLIC_API_EGRESS_DENIED
    json -X PUT "$BASE/v2.0/network-policies/$POLICY_ID" -H 'content-type: application/json' \
        --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"allow\"}}" >/dev/null
    wait_marker O3K_PUBLIC_API_POLICY_UPDATE_ALLOWED
    json -X PUT "$BASE/v2.0/network-policies/$POLICY_ID" -H 'content-type: application/json' \
        --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"deny\"}}" >/dev/null
    wait_marker O3K_PUBLIC_API_POLICY_UPDATE_DENIED
    nft list chain ip o3k_policy forward 2>/dev/null \
        | grep -Eq 'counter packets [1-9][0-9]*.* drop'

    # Replay the durable binding while the real guest remains attached.  The
    # public API operation forces a fresh control-plane connection and must
    # preserve the existing TAP/fixed identity without duplicate mutation.
    kill "$NETWORK_PID"
    wait "$NETWORK_PID" 2>/dev/null || true
    NETWORK_PID=""
    start_network_agent
    replayed=""
    replay_error=""
    replay_ok=0
    for _ in $(seq 1 80); do
        replayed="$(curl -sS -X PUT "$BASE/v2.0/floatingips/$FLOATING_ID" \
            -H 'content-type: application/json' \
            -H 'x-openstack-request-id: p9-api-floating-replay' \
            -H "x-auth-token: $TOKEN" \
            --data "{\"floatingip\":{\"port_id\":\"$PORT_ID\"}}" \
            -w $'\n%{http_code}' 2>/dev/null || true)"
        replay_status="${replayed##*$'\n'}"
        replay_body="${replayed%$'\n'*}"
        if [[ "$replay_status" == 2* ]]; then
            replay_ok=1
            break
        fi
        replay_error="$replay_body (HTTP $replay_status)"
        sleep 0.1
    done
    [[ "$replay_ok" == 1 ]] || {
        echo "floating-IP replay did not converge after network-agent restart: $replay_error" >&2
        exit 1
    }
    nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 4 \
        "http://$FLOATING_ADDRESS:8080/" | grep -q o3k-public-api-real-guest
    restart_controller
    json "$BASE/v2.0/floatingips/$FLOATING_ID" | grep -Fq "$PORT_ID"
    nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 4 \
        "http://$FLOATING_ADDRESS:8080/" | grep -q o3k-public-api-real-guest
    kill "$QEMU_PID"
    wait "$QEMU_PID" 2>/dev/null || true
    QEMU_PID=""
elif [[ "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    FLOATING_ADDRESS="$(printf '%s' "$FLOATING_JSON" | field floatingip.floating_ip_address)"
    CONSOLE_OUTPUT=""
    for _ in $(seq 1 180); do
        CONSOLE_OUTPUT="$(json -X POST "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID/action" \
            -H 'content-type: application/json' \
            --data '{"os-getConsoleOutput":{"length":65536}}' 2>/dev/null \
            | field output || true)"
        if grep -q O3K_PUBLIC_API_LIBVIRT_GUEST_READY <<<"$CONSOLE_OUTPUT" \
            && grep -q O3K_PUBLIC_API_LIBVIRT_EGRESS_ALLOWED <<<"$CONSOLE_OUTPUT" \
            && grep -q O3K_PUBLIC_API_LIBVIRT_EGRESS_DENIED <<<"$CONSOLE_OUTPUT"; then
            break
        fi
        sleep 0.5
    done
    grep -q O3K_PUBLIC_API_LIBVIRT_GUEST_READY <<<"$CONSOLE_OUTPUT"
    grep -q O3K_PUBLIC_API_LIBVIRT_EGRESS_ALLOWED <<<"$CONSOLE_OUTPUT"
    grep -q O3K_PUBLIC_API_LIBVIRT_EGRESS_DENIED <<<"$CONSOLE_OUTPUT"
    json -X PUT "$BASE/v2.0/network-policies/$POLICY_ID" -H 'content-type: application/json' \
        --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"allow\"}}" >/dev/null
    for _ in $(seq 1 120); do
        CONSOLE_OUTPUT="$(json -X POST "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID/action" -H 'content-type: application/json' --data '{"os-getConsoleOutput":{"length":65536}}' 2>/dev/null | field output || true)"
        grep -q O3K_PUBLIC_API_POLICY_UPDATE_ALLOWED <<<"$CONSOLE_OUTPUT" && break
        sleep 0.5
    done
    grep -q O3K_PUBLIC_API_POLICY_UPDATE_ALLOWED <<<"$CONSOLE_OUTPUT"
    json -X PUT "$BASE/v2.0/network-policies/$POLICY_ID" -H 'content-type: application/json' \
        --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"deny\"}}" >/dev/null
    for _ in $(seq 1 120); do
        CONSOLE_OUTPUT="$(json -X POST "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID/action" -H 'content-type: application/json' --data '{"os-getConsoleOutput":{"length":65536}}' 2>/dev/null | field output || true)"
        grep -q O3K_PUBLIC_API_POLICY_UPDATE_DENIED <<<"$CONSOLE_OUTPUT" && break
        sleep 0.5
    done
    grep -q O3K_PUBLIC_API_POLICY_UPDATE_DENIED <<<"$CONSOLE_OUTPUT"
    if grep -q O3K_PUBLIC_API_LIBVIRT_EGRESS_DENIED_FAILED <<<"$CONSOLE_OUTPUT"; then
        echo "real guest reached the denied external service" >&2
        exit 1
    fi
    ingress_ok=0
    for _ in $(seq 1 80); do
        if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
            "http://$FLOATING_ADDRESS:8080/" 2>/dev/null \
            | grep -q o3k-public-api-libvirt-guest; then
            ingress_ok=1
            break
        fi
        sleep 0.1
    done
    [[ "$ingress_ok" == 1 ]] || {
        echo "libvirt floating-IP ingress did not converge after network-agent restart" >&2
        exit 1
    }
    ip link set "$UPLINK" down
    if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
        "http://$FLOATING_ADDRESS:8080/" >/dev/null 2>&1; then
        echo "libvirt external uplink outage did not interrupt public traffic" >&2
        exit 1
    fi
    ip link set "$UPLINK" up
    recovered=0
    for _ in $(seq 1 80); do
        if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
            "http://$FLOATING_ADDRESS:8080/" 2>/dev/null \
            | grep -q o3k-public-api-libvirt-guest; then
            recovered=1
            break
        fi
        sleep 0.1
    done
    [[ "$recovered" == 1 ]] || { echo "libvirt public traffic did not recover after uplink restoration" >&2; exit 1; }
    nft list chain ip o3k_policy forward 2>/dev/null \
        | grep -Eq 'counter packets [1-9][0-9]*.* drop'

    # Exercise the same public floating-IP replay after executor restart while
    # the libvirt domain and its TAP remain alive.
    kill "$NETWORK_PID"
    wait "$NETWORK_PID" 2>/dev/null || true
    NETWORK_PID=""
    start_network_agent
    replay_ok=0
    replay_error=""
    for _ in $(seq 1 80); do
        replayed="$(curl -sS -X PUT "$BASE/v2.0/floatingips/$FLOATING_ID" \
            -H 'content-type: application/json' \
            -H 'x-openstack-request-id: p9-api-libvirt-floating-replay' \
            -H "x-auth-token: $TOKEN" \
            --data "{\"floatingip\":{\"port_id\":\"$PORT_ID\"}}" \
            -w $'\n%{http_code}' 2>/dev/null || true)"
        replay_status="${replayed##*$'\n'}"
        replay_error="${replayed%$'\n'*} (HTTP $replay_status)"
        if [[ "$replay_status" == 2* ]]; then
            replay_ok=1
            break
        fi
        sleep 0.1
    done
    [[ "$replay_ok" == 1 ]] || { echo "libvirt floating-IP replay failed: $replay_error" >&2; exit 1; }
    ingress_ok=0
    for _ in $(seq 1 80); do
        if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
            "http://$FLOATING_ADDRESS:8080/" 2>/dev/null \
            | grep -q o3k-public-api-libvirt-guest; then
            ingress_ok=1
            break
        fi
        sleep 0.1
    done
    [[ "$ingress_ok" == 1 ]] || {
        echo "libvirt floating-IP ingress did not converge after network-agent restart" >&2
        exit 1
    }
    restart_controller
    json "$BASE/v2.0/floatingips/$FLOATING_ID" | grep -Fq "$PORT_ID"
    ingress_ok=0
    for _ in $(seq 1 80); do
        if nsenter -t "$EXT_PID" -n -- curl --fail --silent --max-time 2 \
            "http://$FLOATING_ADDRESS:8080/" 2>/dev/null \
            | grep -q o3k-public-api-libvirt-guest; then
            ingress_ok=1
            break
        fi
        sleep 0.1
    done
    [[ "$ingress_ok" == 1 ]] || { echo "controller restart lost floating-IP ingress" >&2; exit 1; }

    for candidate in $(virsh -c qemu:///system list --all --name); do
        xml="$(virsh -c qemu:///system dumpxml "$candidate" 2>/dev/null || true)"
        if grep -Fq "server_id=\"$SERVER_ID\"" <<<"$xml"; then
            DOMAIN_NAME="$candidate"
            printf '%s\n' "$xml" >"$WORK_DIR/domain.xml"
            break
        fi
    done
    [[ -n "$DOMAIN_NAME" ]] || { echo "run-owned libvirt domain was not discoverable" >&2; exit 1; }
    grep -Fq 'managed_by="o3k-compute"' "$WORK_DIR/domain.xml"
    # Libvirt receives the executor-owned TAP directly; the bridge name is
    # intentionally not part of the domain XML for this bounded attachment.
    grep -Eq "target dev=['\"]o3ktap-" "$WORK_DIR/domain.xml"
fi

json -X PUT "$BASE/v2.0/floatingips/$FLOATING_ID" -H 'content-type: application/json' \
    --data '{"floatingip":{}}' >/dev/null
json -X DELETE "$BASE/v2.0/floatingips/$FLOATING_ID" >/dev/null
json -X DELETE "$BASE/v2.1/$PROJECT_ID/servers/$SERVER_ID" >/dev/null
RULE_IDS="$(json "$BASE/v2.0/security-group-rules?security_group_id=$SECURITY_GROUP_ID" | python3 -c 'import json,sys; print(" ".join(rule["id"] for rule in json.load(sys.stdin)["security_group_rules"]))')"
for rule_id in $RULE_IDS; do json -X DELETE "$BASE/v2.0/security-group-rules/$rule_id" >/dev/null; done
json -X DELETE "$BASE/v2.0/ports/$PORT_ID" >/dev/null
json -X DELETE "$BASE/v2.0/security-groups/$SECURITY_GROUP_ID" >/dev/null
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

if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 || "${O3K_P9_PUBLIC_API_LIBVIRT:-}" == 1 ]]; then
    if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 ]]; then
        OUTPUT_PATH="${O3K_P9_PUBLIC_API_QEMU_OUTPUT:-$ROOT_DIR/target/p9-public-api-real-qemu-packet-path.json}"
        ARTIFACT_TYPE="p9-public-api-real-qemu-packet-path"
        EVIDENCE_TIER="fake-control-plane-real-qemu-guest"
        FULL_PROFILE_VERIFIED=false
        GUEST_LABEL="real QEMU guest"
    else
        OUTPUT_PATH="${O3K_P9_PUBLIC_API_LIBVIRT_OUTPUT:-$ROOT_DIR/target/p9-public-api-real-libvirt-packet-path.json}"
        ARTIFACT_TYPE="p9-public-api-real-libvirt-packet-path"
        EVIDENCE_TIER="public-o3kd-api-real-libvirt-guest"
        FULL_PROFILE_VERIFIED=true
        GUEST_LABEL="real libvirt guest"
    fi
    mkdir -p "$(dirname "$OUTPUT_PATH")"
    cat >"$OUTPUT_PATH" <<JSON
{"artifact_type":"$ARTIFACT_TYPE","schema_version":1,"evidence_tier":"$EVIDENCE_TIER","full_profile_verified":$FULL_PROFILE_VERIFIED,"real_vm_verified":true,"public_api_lifecycle_verified":true,"fixed_ip_packet_path_verified":true,"routed_snat_verified":true,"public_dnat_verified":true,"stateful_policy_allow_verified":true,"stateful_policy_deny_verified":true,"security_group_projection_verified":true,"policy_update_under_real_traffic_verified":true,"external_unavailable_recovered_verified":true,"network_agent_restart_replay_verified":true,"controller_restart_verified":true,"owned_leaks":0,"owned_inconsistencies":0,"foreign_mutations":0,"source_revision":"$(git -C "$ROOT_DIR" rev-parse HEAD)"}
JSON
    echo "P9 public API -> $GUEST_LABEL gate passed (routed/public/policy traffic, restart, recovery, cleanup)"
else
    echo "P9 public API -> real network-agent process gate passed (policy pending, binding dispatch, cleanup)"
fi
