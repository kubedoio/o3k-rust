#!/usr/bin/env bash
set -Eeuo pipefail

# Disposable real-VM packet-path gate for P9. The guest is a QEMU VM running
# the host's Linux kernel, but the entire dataplane is inside an unshared
# network namespace. This is stronger than a network namespace-only test and
# is intentionally not claimed as the protected full-profile host gate.

OUT_PATH="${1:-}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KERNEL="${O3K_P9_QEMU_KERNEL:-/boot/vmlinuz}"
BUSYBOX="${O3K_P9_QEMU_BUSYBOX:-/usr/bin/busybox}"
GUEST_INIT="${ROOT_DIR}/tests/p9-real-qemu-guest-init"

if [[ "${O3K_P9_REAL_QEMU_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_REAL_QEMU_INNER=1 "$0" "$OUT_PATH"
fi

for tool in qemu-system-x86_64 ip nft nsenter unshare setsid curl python3 cpio; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "missing required tool: $tool" >&2
        exit 2
    }
done
[[ -r "$KERNEL" ]] || { echo "kernel is not readable: $KERNEL" >&2; exit 2; }
[[ -x "$BUSYBOX" ]] || { echo "busybox is not executable: $BUSYBOX" >&2; exit 2; }

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p9-real-qemu.XXXXXX")"
ROOTFS="${WORK_DIR}/rootfs"
INITRD="${WORK_DIR}/guest-initramfs.cpio.gz"
SERIAL_LOG="${WORK_DIR}/serial.log"
mkdir -p "$ROOTFS"/{bin,dev,etc,proc,sys,tmp,www}

guest_pid=
external_pid=
external_server_pid=
qemu_pid=
table_name=o3k_p9_real_qemu
tap_name=p9qtap0
router_guest=p9qrouter0
router_external=p9quplink0
external_link=p9qexternal0

write_result() {
    local status="$1" reason="$2"
    [[ -n "$OUT_PATH" ]] || return 0
    python3 - "$OUT_PATH" "$status" "$reason" "$ROOT_DIR" <<'PY'
import json
import subprocess
import sys
import time

path, status, reason, root = sys.argv[1:]
try:
    revision = subprocess.check_output(
        ["git", "-C", root, "rev-parse", "HEAD"], text=True
    ).strip()
except Exception:
    revision = "unknown"
with open(path, "w", encoding="utf-8") as stream:
    json.dump(
        {
            "artifact_type": "p9-real-qemu-packet-path",
            "schema_version": 1,
            "evidence_tier": "real-qemu-guest-isolated",
            "full_profile_verified": False,
            "real_vm_verified": status == "passed",
            "source_revision": revision,
            "status": status,
            "reason": reason,
            "packet_path": {
                "fixed_ip": status == "passed",
                "routed_egress_snat": status == "passed",
                "public_ingress_dnat": status == "passed",
                "stateful_policy_deny": status == "passed",
            },
            "owned_leaks": 0 if status == "passed" else None,
            "foreign_mutations": 0 if status == "passed" else None,
            "redacted": True,
            "finished_at": int(time.time()),
        },
        stream,
        indent=2,
        sort_keys=True,
    )
    stream.write("\n")
PY
}

cleanup() {
    set +e
    [[ -n "$qemu_pid" ]] && kill "$qemu_pid" 2>/dev/null
    [[ -n "$qemu_pid" ]] && wait "$qemu_pid" 2>/dev/null
    [[ -n "$external_server_pid" ]] && kill "$external_server_pid" 2>/dev/null
    [[ -n "$external_server_pid" ]] && wait "$external_server_pid" 2>/dev/null
    [[ -n "$guest_pid" ]] && kill -- -"$guest_pid" 2>/dev/null
    [[ -n "$external_pid" ]] && kill -- -"$external_pid" 2>/dev/null
    [[ -n "$guest_pid" ]] && wait "$guest_pid" 2>/dev/null
    [[ -n "$external_pid" ]] && wait "$external_pid" 2>/dev/null
    nft delete table ip "$table_name" 2>/dev/null
    ip link del "$router_guest" 2>/dev/null
    ip link del "$router_external" 2>/dev/null
    ip link del "$tap_name" 2>/dev/null
    if [[ "${O3K_P9_KEEP_FAILURE_ARTIFACTS:-}" == 1 ]]; then
        echo "P9 real-QEMU diagnostic artifacts: $WORK_DIR" >&2
    else
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

fail() {
    write_result failed "$1"
    echo "P9 real-QEMU packet path failed: $1" >&2
    [[ -f "$SERIAL_LOG" ]] && sed -n '1,160p' "$SERIAL_LOG" >&2
    exit 1
}

for path in "$BUSYBOX" "$GUEST_INIT"; do
    cp "$path" "$ROOTFS/bin/$(basename "$path")"
done
chmod 0755 "$ROOTFS/bin/p9-real-qemu-guest-init"
ln -s busybox "$ROOTFS/init"
for applet in httpd ip mount poweroff route sh sleep wget; do
    ln -s busybox "$ROOTFS/bin/$applet"
done

cp "$GUEST_INIT" "$ROOTFS/etc/guest-init"
ln -sf /etc/guest-init "$ROOTFS/init"
(cd "$ROOTFS" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9 >"$INITRD")

setsid unshare -n sleep 120 &
guest_pid=$!
setsid unshare -n sleep 120 &
external_pid=$!
sleep 0.1

ip link add "$router_guest" type veth peer name p9qguestpeer
ip link set p9qguestpeer netns "$guest_pid"
ip link add "$router_external" type veth peer name "$external_link"
ip link set "$external_link" netns "$external_pid"
ip tuntap add dev "$tap_name" mode tap
ip link set lo up
ip link set "$tap_name" up
ip link set "$router_guest" up
ip link set "$router_external" up
ip addr add 10.0.0.1/24 dev "$tap_name"
ip addr add 198.51.100.1/24 dev "$router_external"
sysctl -q -w net.ipv4.ip_forward=1 >/dev/null

ns() {
    local pid="$1"
    shift
    nsenter -t "$pid" -n -- "$@"
}

ns "$guest_pid" ip link set lo up
ns "$guest_pid" ip link set p9qguestpeer up
ns "$guest_pid" ip addr add 10.0.0.2/24 dev p9qguestpeer
ns "$guest_pid" ip route add default via 10.0.0.1
ns "$external_pid" ip link set lo up
ns "$external_pid" ip link set "$external_link" up
ns "$external_pid" ip addr add 198.51.100.2/24 dev "$external_link"
ns "$external_pid" ip route add default via 198.51.100.1

nft -f - <<EOF
table ip $table_name {
    comment "o3k-p9-real-qemu"
    chain prerouting {
        type nat hook prerouting priority -100; policy accept;
        ip daddr 198.51.100.1 tcp dport 8080 dnat to 10.0.0.2:8080
    }
    chain forward {
        type filter hook forward priority -100; policy drop;
        ct state established,related accept
        iifname "$tap_name" oifname "$router_external" ip daddr 198.51.100.2 tcp dport 8081 accept
        iifname "$router_external" oifname "$tap_name" ip daddr 10.0.0.2 tcp dport 8080 accept
    }
    chain postrouting {
        type nat hook postrouting priority 100; policy accept;
        ip saddr 10.0.0.0/24 oifname "$router_external" masquerade
    }
}
EOF

setsid nsenter -t "$external_pid" -n -- python3 -m http.server 8081 --bind 198.51.100.2 \
    >/dev/null 2>&1 &
external_server_pid=$!
sleep 0.2

qemu-system-x86_64 \
    -enable-kvm -cpu host -m 128M -smp 1 \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append 'console=ttyS0,115200 earlyprintk=serial init=/init net.ifnames=0 biosdevname=0' \
    -nographic -nodefaults -no-reboot -serial "file:${SERIAL_LOG}" \
    -device virtio-net-pci,netdev=net0,mac=02:00:00:00:00:02 \
    -netdev tap,id=net0,ifname="$tap_name",script=no,downscript=no \
    >/dev/null 2>&1 &
qemu_pid=$!

wait_marker() {
    local marker="$1" deadline=$((SECONDS + 30))
    while ((SECONDS < deadline)); do
        grep -q "$marker" "$SERIAL_LOG" 2>/dev/null && return 0
        kill -0 "$qemu_pid" 2>/dev/null || return 1
        sleep 0.1
    done
    return 1
}

wait_marker O3K_GUEST_READY || fail guest_boot_or_fixed_ip
ns "$external_pid" curl --fail --silent --show-error --max-time 3 \
    http://198.51.100.1:8080/ >/dev/null || fail public_ingress_dnat
wait_marker O3K_GUEST_EGRESS_OK || fail routed_egress_snat

nft insert rule ip "$table_name" forward iifname "$tap_name" \
    oifname "$router_external" ip daddr 198.51.100.2 tcp dport 8081 drop \
    comment "o3k-p9-real-qemu-policy-deny"
wait_marker O3K_GUEST_DENY_OK || fail stateful_policy_deny

wait "$qemu_pid" || fail guest_shutdown
qemu_pid=

trap - EXIT
cleanup
if nft list table ip "$table_name" >/dev/null 2>&1; then
    fail owned_nft_table_remains_before_cleanup
fi
if ip link show "$tap_name" >/dev/null 2>&1; then
    fail owned_tap_remains_before_cleanup
fi

write_result passed real_qemu_guest_packet_path_and_cleanup
echo "P9 real-QEMU guest packet path passed (protected full-profile gate remains separate)"
