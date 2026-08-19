#!/usr/bin/env bash
set -Eeuo pipefail

# Real-QEMU packet gate using the actual o3k-network binary and its Linux
# providers. This remains an isolated evidence tier: it does not claim the
# public o3kd API lifecycle or the protected deployment gate.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${O3K_P9_AGENT_QEMU_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_AGENT_QEMU_INNER=1 "$0"
fi
for tool in cargo ip nft nsenter unshare setsid qemu-system-x86_64 cpio gzip; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done
KERNEL="${O3K_P9_QEMU_KERNEL:-/boot/vmlinuz}"
BUSYBOX="${O3K_P9_QEMU_BUSYBOX:-/usr/bin/busybox}"
[[ -r "$KERNEL" && -x "$BUSYBOX" ]] || exit 2
cargo build -p o3k-network-bin >/dev/null
cargo build -p o3k-network-protocol --example network-agent-client >/dev/null
BIN="$ROOT_DIR/target/debug/o3k-network-bin"
CLIENT="$ROOT_DIR/target/debug/examples/network-agent-client"
FIXTURES="$ROOT_DIR/crates/o3k-compute-agent/tests/fixtures"
DNSMASQ="$ROOT_DIR/tests/p9-test-dnsmasq"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-agent-qemu.XXXXXX")"
ROOTFS="$WORK_DIR/rootfs"
INITRD="$WORK_DIR/initrd.cpio.gz"
AGENT_PID=""
EXT_PID=""
QEMU_PID=""
EXTERNAL_SERVER_PID=""
UPLINK=p9qext0
EXT_PEER=p9qextpeer0
BRIDGE=o3kp9br0
PUBLIC_ADDR=198.51.100.10
REALM=00000000-0000-0000-0000-000000000004
ENDPOINT=00000000-0000-0000-0000-000000000003
BASE_OP=00000000-0000-0000-0000-000000000002
POLICY_OP=00000000-0000-0000-0000-000000000006
REMOVE_OP=00000000-0000-0000-0000-000000000005
TAP_SUFFIX="$(printf '%s' "$ENDPOINT" | sha256sum | cut -c1-8)"
TAP="o3ktap-$TAP_SUFFIX"
cleanup() {
    set +e
    [[ -n "$QEMU_PID" ]] && kill "$QEMU_PID" 2>/dev/null
    [[ -n "$QEMU_PID" ]] && wait "$QEMU_PID" 2>/dev/null
    [[ -n "$EXTERNAL_SERVER_PID" ]] && kill "$EXTERNAL_SERVER_PID" 2>/dev/null
    [[ -n "$EXTERNAL_SERVER_PID" ]] && wait "$EXTERNAL_SERVER_PID" 2>/dev/null
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

mkdir -p "$ROOTFS"/{bin,etc,dev,proc,sys,tmp,www} "$WORK_DIR"/{executor,ownership,dhcp,routed,policy,public}
cp "$BUSYBOX" "$ROOTFS/bin/busybox"
ln -s busybox "$ROOTFS/init"
for applet in cat httpd ip kill mount poweroff route sh sleep wget; do ln -s busybox "$ROOTFS/bin/$applet"; done
cat >"$ROOTFS/etc/guest-init" <<'GUEST'
#!/bin/sh
set -eu
exec >/dev/console 2>&1
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
ip link set lo up
ip link set eth0 up
ip addr add 10.0.0.3/24 dev eth0
ip route add default via 10.0.0.1
mkdir -p /tmp/www
printf 'o3k-agent-qemu\n' >/tmp/www/index.html
httpd -f -p 8080 -h /tmp/www &
echo O3K_AGENT_GUEST_READY
if cat /proc/cmdline | grep -q o3k-recovery; then
    sleep 1
    poweroff -f
fi
wget -q -T 3 -O /dev/null http://198.51.100.2:8081/
echo O3K_AGENT_GUEST_EGRESS_OK
sleep 5
 wget -q -T 2 -O /dev/null http://198.51.100.2:8082/ &
probe_pid=$!
sleep 4
kill "$probe_pid" 2>/dev/null || true
wait "$probe_pid" 2>/dev/null || true
echo O3K_AGENT_GUEST_DENY_OK
poweroff -f
GUEST
chmod 0755 "$ROOTFS/etc/guest-init"
ln -sf /etc/guest-init "$ROOTFS/init"
(cd "$ROOTFS" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9 >"$INITRD")

setsid unshare -n sleep 180 &
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
ns python3 -m http.server 8081 --bind 198.51.100.2 >/dev/null 2>&1 &
EXTERNAL_SERVER_PID=$!
ns python3 -m http.server 8082 --bind 198.51.100.2 >/dev/null 2>&1 &

start_agent() {
    O3K_NETWORK_AGENT_ID=agent-qemu \
    O3K_NETWORK_AGENT_EPOCH=epoch-1 \
    O3K_NETWORK_CONTROLLER_ID=controller-qemu \
    O3K_NETWORK_CONTROLLER_EPOCH=epoch-1 \
    O3K_NETWORK_FENCING_TOKEN=7 \
    O3K_NETWORK_ROOT="$WORK_DIR/executor" \
    O3K_NETWORK_BRIDGE="$BRIDGE" \
    O3K_NETWORK_OWNERSHIP_ROOT="$WORK_DIR/ownership" \
    O3K_NETWORK_DHCP_ROOT="$WORK_DIR/dhcp" \
    O3K_NETWORK_DNSMASQ="$DNSMASQ" \
    O3K_NETWORK_EXTERNAL_REALM_ID="$REALM" \
    O3K_NETWORK_UPLINK="$UPLINK" \
    O3K_NETWORK_ROUTED_ROOT="$WORK_DIR/routed" \
    O3K_NETWORK_POLICY_ROOT="$WORK_DIR/policy" \
    O3K_NETWORK_PUBLIC_ROOT="$WORK_DIR/public" \
    O3K_NETWORK_LISTEN=127.0.0.1:19183 \
    O3K_NETWORK_TLS_CERT="$FIXTURES/server-chain.pem" \
    O3K_NETWORK_TLS_KEY="$FIXTURES/server-key.pem" \
    O3K_NETWORK_TLS_CLIENT_CA="$FIXTURES/ca.pem" \
    "$BIN" >"$WORK_DIR/agent.log" 2>&1 &
    AGENT_PID=$!
}
client() {
    timeout 20 "$CLIENT" https://127.0.0.1:19183 o3k-control-plane \
        "$FIXTURES/ca.pem" "$FIXTURES/agent-chain.pem" "$FIXTURES/agent-key-pkcs8.pem" \
        agent-qemu epoch-1 controller-qemu epoch-1 7 "$1" "$2" "$3" \
        4102444800000 "$4" "${5:-}"
}
cat >"$WORK_DIR/base.json" <<JSON
{"schema_version":1,"plan_id":"$ENDPOINT","node_id":"agent-qemu","operation_id":"$BASE_OP","deadline_unix_ms":4102444800000,"resource_generations":{"$ENDPOINT":1},"intents":[{"AddressRealm":{"realm_id":"$REALM","prefix":{"network":"10.0.0.0","prefix_len":24},"gateway":"10.0.0.1"}},{"EndpointAttachment":{"endpoint_id":"$ENDPOINT","mac":"02:00:00:00:00:03","fixed_ip":"10.0.0.3","generation":1}},{"AddressAssignment":{"endpoint_id":"$ENDPOINT","address":"10.0.0.3","generation":1}},{"Egress":{"external_realm_id":"$REALM","enabled":true,"nat":true}},{"PublicAddressBinding":{"id":"00000000-0000-0000-0000-000000000005","project_id":"project-a","public_address":"$PUBLIC_ADDR","endpoint_id":"$ENDPOINT","generation":1}}],"fingerprint_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}
JSON
sed "s/\"operation_id\":\"$BASE_OP\"/\"operation_id\":\"$POLICY_OP\"/; s/\"intents\":\[/\"intents\":[{\"Policy\":{\"endpoint_id\":\"$ENDPOINT\",\"direction\":\"Egress\",\"protocol\":\"Tcp\",\"ports\":{\"start\":8082,\"end\":8082},\"source\":null,\"destination\":{\"network\":\"198.51.100.0\",\"prefix_len\":24},\"action\":\"Deny\"}},/" "$WORK_DIR/base.json" >"$WORK_DIR/policy.json"
sed "s/\"operation_id\":\"$POLICY_OP\"/\"operation_id\":\"$REMOVE_OP\"/" "$WORK_DIR/policy.json" >"$WORK_DIR/remove.json"
start_agent
deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
    if result="$(client 00000000-0000-0000-0000-000000000010 "$BASE_OP" agent-apply "$WORK_DIR/base.json" 2>/dev/null)"; then
        [[ "$result" == "succeeded false" ]] && break
    fi
    sleep 0.2
done
[[ "${result:-}" == "succeeded false" ]] || { cat "$WORK_DIR/agent.log" >&2; exit 1; }
[[ "$TAP" == o3ktap-* ]]
kill "$AGENT_PID"
wait "$AGENT_PID" 2>/dev/null || true
AGENT_PID=""
start_agent
deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
    if replay="$(client 00000000-0000-0000-0000-000000000010 "$BASE_OP" agent-apply "$WORK_DIR/base.json" 2>/dev/null)"; then
        [[ "$replay" == "replayed true" ]] && break
    fi
    sleep 0.2
done
[[ "${replay:-}" == "replayed true" ]] || { cat "$WORK_DIR/agent.log" >&2; exit 1; }

SERIAL="$WORK_DIR/serial.log"
qemu-system-x86_64 -enable-kvm -cpu host -m 128M -smp 1 -kernel "$KERNEL" -initrd "$INITRD" \
    -append 'console=ttyS0,115200 init=/init net.ifnames=0 biosdevname=0' -nographic -nodefaults -no-reboot \
    -serial "file:$SERIAL" -device virtio-net-pci,netdev=net0,mac=02:00:00:00:00:03 \
    -netdev tap,id=net0,ifname="$TAP",script=no,downscript=no >/dev/null 2>&1 &
QEMU_PID=$!
wait_marker() {
    local marker="$1" deadline=$((SECONDS + 30))
    while ((SECONDS < deadline)); do
        grep -q "$marker" "$SERIAL" 2>/dev/null && return 0
        kill -0 "$QEMU_PID" 2>/dev/null || {
            echo "guest exited before marker $marker" >&2
            sed -n '1,220p' "$SERIAL" >&2
            nft list table ip o3k_policy >&2 2>/dev/null || true
            return 1
        }
        sleep 0.1
    done
    echo "timed out waiting for marker $marker" >&2
    sed -n '1,220p' "$SERIAL" >&2
    nft list table ip o3k_policy >&2 2>/dev/null || true
    return 1
}
wait_marker O3K_AGENT_GUEST_READY
ns curl --fail --silent --max-time 3 "http://$PUBLIC_ADDR:8080/" >/dev/null
wait_marker O3K_AGENT_GUEST_EGRESS_OK
[[ "$(client 00000000-0000-0000-0000-000000000011 "$POLICY_OP" agent-policy "$WORK_DIR/policy.json" 2>/dev/null)" == "succeeded false" ]]
wait_marker O3K_AGENT_GUEST_DENY_OK
deadline=$((SECONDS + 10))
while ((SECONDS < deadline)) && ! nft list chain ip o3k_policy forward 2>/dev/null | grep -Eq 'counter packets [1-9][0-9]*.* drop'; do
    sleep 0.1
done
nft list chain ip o3k_policy forward 2>/dev/null | grep -Eq 'counter packets [1-9][0-9]*.* drop' || {
    echo "real guest traffic did not hit the policy drop rule" >&2
    nft list chain ip o3k_policy forward >&2 || true
    exit 1
}
wait "$QEMU_PID"
QEMU_PID=""
qemu-system-x86_64 -enable-kvm -cpu host -m 128M -smp 1 -kernel "$KERNEL" -initrd "$INITRD" \
    -append 'console=ttyS0,115200 init=/init net.ifnames=0 biosdevname=0 o3k-recovery=1' -nographic -nodefaults -no-reboot \
    -serial "file:$WORK_DIR/recovery.log" -device virtio-net-pci,netdev=net0,mac=02:00:00:00:00:03 \
    -netdev tap,id=net0,ifname="$TAP",script=no,downscript=no >/dev/null 2>&1 &
QEMU_PID=$!
recovery_marker() { local deadline=$((SECONDS + 30)); while ((SECONDS < deadline)); do grep -q O3K_AGENT_GUEST_READY "$WORK_DIR/recovery.log" 2>/dev/null && return 0; sleep 0.1; done; return 1; }
recovery_marker
ns curl --fail --silent --max-time 3 "http://$PUBLIC_ADDR:8080/" >/dev/null
wait "$QEMU_PID"
QEMU_PID=""
remove_result="$(client 00000000-0000-0000-0000-000000000012 "$REMOVE_OP" agent-remove "$WORK_DIR/remove.json" remove 2>"$WORK_DIR/remove.err" || true)"
[[ "$remove_result" == "succeeded false" ]] || {
    echo "agent cleanup command failed: $remove_result" >&2
    cat "$WORK_DIR/remove.err" >&2 || true
    ps -ef >&2 || true
    ip -br link >&2 || true
    nft list ruleset >&2 || true
    cat "$WORK_DIR/agent.log" >&2 || true
    exit 1
}
deadline=$((SECONDS + 15))
while { ip link show "$BRIDGE" >/dev/null 2>&1 || nft list table ip o3k_p9 >/dev/null 2>&1 || nft list table ip o3k_public >/dev/null 2>&1 || nft list table ip o3k_policy >/dev/null 2>&1; } && ((SECONDS < deadline)); do sleep 0.1; done
if ip link show "$BRIDGE" >/dev/null 2>&1 || ip link show "$TAP" >/dev/null 2>&1 || ip -4 addr show dev "$UPLINK" | grep -q "$PUBLIC_ADDR/32"; then
    echo "agent-owned guest network resources remained after cleanup" >&2
    exit 1
fi
echo "P9 real-QEMU agent packet path passed (agent-created TAP, routed SNAT, public DNAT, policy, restart, cleanup)"
