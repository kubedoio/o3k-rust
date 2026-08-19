#!/usr/bin/env bash
set -Eeuo pipefail

# Black-box P9 control-plane gate.  It drives the public o3kd API, while the
# real o3k-network binary owns the host-local TAP/bridge/DHCP mutation.  This
# is intentionally a portable process gate by default. With
# O3K_P9_PUBLIC_API_QEMU=1 it continues the same lifecycle into a disposable
# real QEMU guest; that mode is fake-control-plane/real-guest evidence and is
# not a claim that the public compute API drove libvirt.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_PUBLIC_API_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_PUBLIC_API_INNER=1 "$0" "$@"
fi
[[ "${O3K_P9_DEBUG:-}" == 1 ]] && set -x

TOOLS=(cargo curl ip nft openssl setsid unshare python3)
if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 ]]; then
    TOOLS+=(qemu-system-x86_64 cpio gzip)
fi
for tool in "${TOOLS[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

cargo build -p o3kd -p o3k-network-bin >/dev/null
BIN="$ROOT_DIR/target/debug/o3kd"
NETWORK_BIN="$ROOT_DIR/target/debug/o3k-network-bin"
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
KERNEL="${O3K_P9_QEMU_KERNEL:-/boot/vmlinuz}"
BUSYBOX="${O3K_P9_QEMU_BUSYBOX:-/usr/bin/busybox}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-public-api.XXXXXX")"
O3KD_PID=""
NETWORK_PID=""
EXT_PID=""
QEMU_PID=""
EXTERNAL_SERVER_PID=""
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
    [[ -n "$QEMU_PID" ]] && kill "$QEMU_PID" 2>/dev/null
    [[ -n "$QEMU_PID" ]] && wait "$QEMU_PID" 2>/dev/null
    [[ -n "$EXTERNAL_SERVER_PID" ]] && kill "$EXTERNAL_SERVER_PID" 2>/dev/null
    [[ -n "$EXTERNAL_SERVER_PID" ]] && wait "$EXTERNAL_SERVER_PID" 2>/dev/null
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
SUBNET_JSON="$(json -X POST "$BASE/v2.0/subnets" -H 'content-type: application/json' \
    --data "{\"subnet\":{\"name\":\"p9-api-subnet\",\"network_id\":\"$NETWORK_ID\",\"cidr\":\"10.0.0.0/24\"}}")"
SUBNET_ID="$(printf '%s' "$SUBNET_JSON" | field subnet.id)"
GATEWAY_IP="$(printf '%s' "$SUBNET_JSON" | field subnet.gateway_ip)"
PORT_JSON="$(json -X POST "$BASE/v2.0/ports" -H 'content-type: application/json' \
    --data "{\"port\":{\"name\":\"p9-api-port\",\"network_id\":\"$NETWORK_ID\"}}")"
PORT_ID="$(printf '%s' "$PORT_JSON" | field port.id)"
FIXED_IP="$(printf '%s' "$PORT_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["port"]["fixed_ips"][0]["ip_address"])')"
PORT_MAC="$(printf '%s' "$PORT_JSON" | field port.mac_address)"

# Policy is admitted before scheduling.  The public API must persist it as
# pending, not reject it merely because the endpoint has not been bound yet.
json -X POST "$BASE/v2.0/network-policies" -H 'content-type: application/json' \
    -H 'idempotency-key: p9-api-policy' \
    --data "{\"policy\":{\"network_id\":\"$NETWORK_ID\",\"endpoint_id\":\"$PORT_ID\",\"direction\":\"egress\",\"protocol\":\"tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"destination\":\"198.51.100.0/24\",\"action\":\"deny\"}}" >/dev/null

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
    wait_marker O3K_PUBLIC_API_EGRESS_ALLOWED
    wait_marker O3K_PUBLIC_API_EGRESS_DENIED
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
    kill "$QEMU_PID"
    wait "$QEMU_PID" 2>/dev/null || true
    QEMU_PID=""
fi

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

if [[ "${O3K_P9_PUBLIC_API_QEMU:-}" == 1 ]]; then
    OUTPUT_PATH="${O3K_P9_PUBLIC_API_QEMU_OUTPUT:-$ROOT_DIR/target/p9-public-api-real-qemu-packet-path.json}"
    mkdir -p "$(dirname "$OUTPUT_PATH")"
    cat >"$OUTPUT_PATH" <<JSON
{"artifact_type":"p9-public-api-real-qemu-packet-path","schema_version":1,"evidence_tier":"fake-control-plane-real-qemu-guest","full_profile_verified":false,"real_vm_verified":true,"public_api_lifecycle_verified":true,"fixed_ip_packet_path_verified":true,"routed_snat_verified":true,"public_dnat_verified":true,"stateful_policy_allow_verified":true,"stateful_policy_deny_verified":true,"network_agent_restart_replay_verified":true,"owned_leaks":0,"owned_inconsistencies":0,"foreign_mutations":0,"source_revision":"$(git -C "$ROOT_DIR" rev-parse HEAD)"}
JSON
    echo "P9 public API -> real QEMU guest gate passed (fake control plane, routed/public/policy traffic, restart, cleanup)"
else
    echo "P9 public API -> real network-agent process gate passed (policy pending, binding dispatch, cleanup)"
fi
