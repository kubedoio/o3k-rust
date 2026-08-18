#!/usr/bin/env bash
set -Eeuo pipefail

# Disposable Linux-kernel component test for P9. The outer invocation enters
# a private network namespace, so no host links, routes, addresses, nftables
# state, or processes are mutated. This is not real-VM evidence.

OUT_PATH="${1:-}"

if [[ "${O3K_P9_ISOLATED_INNER:-}" != 1 ]]; then
    exec unshare -n -- env O3K_P9_ISOLATED_INNER=1 "$0" "$OUT_PATH"
fi

for tool in ip nft nsenter unshare curl python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "missing required tool: $tool" >&2
        exit 2
    }
done

guest_pid=
external_pid=
guest_server_pid=
external_server_pid=
table_name=o3k_p9_isolated
router_guest=p9r0
router_external=p9r1
guest_link=p9g0
external_link=p9e0

write_result() {
    local status="$1"
    local reason="$2"
    [[ -n "$OUT_PATH" ]] || return 0
    python3 - "$OUT_PATH" "$status" "$reason" <<'PY'
import json
import sys
from time import time

path, status, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as stream:
    json.dump(
        {
            "artifact_type": "p9-isolated-packet-path",
            "schema_version": 1,
            "evidence_tier": "isolated-linux-kernel",
            "full_profile_verified": False,
            "real_vm_verified": False,
            "status": status,
            "reason": reason,
            "redacted": True,
            "finished_at": int(time()),
        },
        stream,
        indent=2,
    )
    stream.write("\n")
PY
}

cleanup() {
    set +e
    [[ -n "$guest_server_pid" ]] && kill "$guest_server_pid" 2>/dev/null
    [[ -n "$external_server_pid" ]] && kill "$external_server_pid" 2>/dev/null
    [[ -n "$guest_pid" ]] && kill "$guest_pid" 2>/dev/null
    [[ -n "$external_pid" ]] && kill "$external_pid" 2>/dev/null
    nft delete table ip "$table_name" 2>/dev/null
    ip link del "$router_guest" 2>/dev/null
    ip link del "$router_external" 2>/dev/null
}
trap cleanup EXIT

fail() {
    write_result failed "$1"
    echo "P9 isolated packet path failed: $1" >&2
    exit 1
}

unshare -n --fork --pid --mount-proc sleep 120 &
guest_pid=$!
unshare -n --fork --pid --mount-proc sleep 120 &
external_pid=$!
sleep 0.1

ip link add "$router_guest" type veth peer name "$guest_link"
ip link set "$guest_link" netns "$guest_pid"
ip link add "$router_external" type veth peer name "$external_link"
ip link set "$external_link" netns "$external_pid"

ip link set lo up
ip link set "$router_guest" up
ip link set "$router_external" up
ip addr add 10.0.0.1/24 dev "$router_guest"
ip addr add 198.51.100.1/24 dev "$router_external"
sysctl -q -w net.ipv4.ip_forward=1 >/dev/null

ns() {
    local pid="$1"
    shift
    nsenter -t "$pid" -n -- "$@"
}

ns "$guest_pid" ip link set lo up
ns "$guest_pid" ip link set "$guest_link" up
ns "$guest_pid" ip addr add 10.0.0.2/24 dev "$guest_link"
ns "$guest_pid" ip route add default via 10.0.0.1

ns "$external_pid" ip link set lo up
ns "$external_pid" ip link set "$external_link" up
ns "$external_pid" ip addr add 198.51.100.2/24 dev "$external_link"
ns "$external_pid" ip route add default via 198.51.100.1

nft -f - <<EOF
table ip $table_name {
    comment "o3k-p9-isolated-smoke"
    chain prerouting {
        type nat hook prerouting priority -100; policy accept;
        ip daddr 198.51.100.1 tcp dport 8080 dnat to 10.0.0.2:8080
    }
    chain forward {
        type filter hook forward priority -100; policy drop;
        ct state established,related accept
        iifname "$router_guest" oifname "$router_external" ip daddr 198.51.100.2 tcp dport 8081 accept
        iifname "$router_external" oifname "$router_guest" ip daddr 10.0.0.2 tcp dport 8080 accept
    }
    chain postrouting {
        type nat hook postrouting priority 100; policy accept;
        ip saddr 10.0.0.0/24 oifname "$router_external" masquerade
    }
}
EOF

ns "$external_pid" python3 -m http.server 8081 --bind 198.51.100.2 \
    >/dev/null 2>&1 &
external_server_pid=$!
sleep 0.2
ns "$guest_pid" curl --fail --silent --show-error --max-time 3 \
    http://198.51.100.2:8081/ >/dev/null || fail egress_or_snat

ns "$guest_pid" python3 -m http.server 8080 --bind 10.0.0.2 \
    >/dev/null 2>&1 &
guest_server_pid=$!
sleep 0.2
ns "$external_pid" curl --fail --silent --show-error --max-time 3 \
    http://198.51.100.1:8080/ >/dev/null || fail floating_dnat

nft insert rule ip "$table_name" forward iifname "$router_guest" \
    oifname "$router_external" ip daddr 198.51.100.2 tcp dport 8081 drop \
    comment "o3k-p9-policy-deny"
if ns "$guest_pid" curl --fail --silent --max-time 2 \
    http://198.51.100.2:8081/ >/dev/null 2>&1; then
    fail policy_deny_not_enforced
fi

nft list table ip "$table_name" >/dev/null || fail owned_table_missing
cleanup
trap - EXIT
write_result passed isolated_linux_packet_path_only
echo "P9 isolated packet path passed (real VM gate remains unverified)"
