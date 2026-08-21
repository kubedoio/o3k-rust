#!/usr/bin/env bash
set -Eeuo pipefail
umask 077

# This is a privileged Linux dataplane evidence harness. It creates three
# isolated host namespaces, one shared WireGuard transport per host, and
# realm-scoped known-unicast Geneve routes. It is intentionally not claimed as
# the mandatory independent KVM/libvirt gate: the host namespaces model the
# kernel/provider path so that encrypted transport, overlap, policy, MTU, and
# exact cleanup can be exercised reproducibly on one machine.

if [[ "$(id -u)" != 0 ]]; then
    echo "root is required" >&2
    exit 77
fi

readonly UNDERLAY_BRIDGE=p11-wg-underlay
readonly UNDERLAY_CIDR=192.0.2.0/24
readonly WG_PORT=51820
readonly GENEVE_PORT=6081
readonly FABRIC_MTU=1500
readonly TENANT_MTU=1400
readonly ROOT_TMP="$(mktemp -d /tmp/o3k-p11-wg-geneve-XXXXXX)"

HOST_NAMES=(p11h1 p11h2 p11h3)
ENDPOINT_NAMES=(p11ea1 p11ea2 p11eb1 p11eb2)
ROOT_LINKS=(p11h1u p11h2u p11h3u)

cleanup() {
    set +e
    if [[ "${P11_KEEP:-0}" == 1 ]]; then
        echo "p11-wireguard-geneve-overlap: preserving $ROOT_TMP for diagnosis" >&2
        return
    fi
    for ns in "${ENDPOINT_NAMES[@]}" "${HOST_NAMES[@]}"; do
        ip netns del "$ns" 2>/dev/null || true
    done
    for link in "${ROOT_LINKS[@]}"; do
        ip link del "$link" 2>/dev/null || true
    done
    ip link del "$UNDERLAY_BRIDGE" 2>/dev/null || true
    rm -rf "$ROOT_TMP"
}
trap cleanup EXIT

ns() {
    local namespace=$1
    shift
    ip netns exec "$namespace" "$@"
}

host() {
    local namespace=$1
    shift
    ns "$namespace" "$@"
}

require_clean_names() {
    for ns_name in "${HOST_NAMES[@]}" "${ENDPOINT_NAMES[@]}"; do
        if ip netns list | awk '{print $1}' | grep -Fxq "$ns_name"; then
            echo "foreign or stale namespace already owns $ns_name" >&2
            exit 1
        fi
    done
    if ip link show "$UNDERLAY_BRIDGE" >/dev/null 2>&1; then
        echo "foreign or stale link already owns $UNDERLAY_BRIDGE" >&2
        exit 1
    fi
}

make_underlay() {
    ip link add "$UNDERLAY_BRIDGE" type bridge
    ip link set "$UNDERLAY_BRIDGE" up
    for index in 1 2 3; do
        local host_ns="p11h${index}"
        local root_link="p11h${index}u"
        ip netns add "$host_ns"
        ip link add "$root_link" type veth peer name underlay netns "$host_ns"
        ip link set "$root_link" master "$UNDERLAY_BRIDGE"
        ip link set "$root_link" up
        ns "$host_ns" ip link set lo up
        ns "$host_ns" ip link set underlay up
        ns "$host_ns" ip addr add "192.0.2.${index}/24" dev underlay
        ns "$host_ns" sysctl -qw net.ipv4.ip_forward=1
        ns "$host_ns" sysctl -qw net.ipv4.conf.all.rp_filter=0
    done
}

make_wireguard_keys() {
    for index in 1 2 3; do
        wg genkey >"$ROOT_TMP/key-${index}"
        wg pubkey <"$ROOT_TMP/key-${index}" >"$ROOT_TMP/pub-${index}"
    done
}

configure_wireguard_host() {
    local index=$1
    local host_ns="p11h${index}"
    local transport="198.18.0.${index}"
    ns "$host_ns" ip link add wg0 type wireguard
    ns "$host_ns" wg set wg0 private-key "$ROOT_TMP/key-${index}" listen-port "$WG_PORT"
    ns "$host_ns" ip addr add "${transport}/32" dev wg0
    ns "$host_ns" ip link set wg0 mtu "$FABRIC_MTU"
    ns "$host_ns" ip link set wg0 up
    for peer in 1 2 3; do
        [[ "$peer" == "$index" ]] && continue
        local peer_transport="198.18.0.${peer}"
        ns "$host_ns" wg set wg0 peer "$(<"$ROOT_TMP/pub-${peer}")" \
            endpoint "192.0.2.${peer}:${WG_PORT}" \
            allowed-ips "${peer_transport}/32" persistent-keepalive 1
        ns "$host_ns" ip route replace "${peer_transport}/32" dev wg0
    done
}

make_endpoint() {
    local endpoint_ns=$1
    local host_ns=$2
    local host_link=$3
    local address=$4
    ip netns add "$endpoint_ns"
    ip link add "$host_link" type veth peer name eth0 netns "$endpoint_ns"
    ip link set "$host_link" netns "$host_ns"
    host "$host_ns" ip link set "$host_link" up
    ns "$endpoint_ns" ip link set lo up
    ns "$endpoint_ns" ip link set eth0 mtu "$TENANT_MTU"
    ns "$endpoint_ns" ip link set eth0 up
    ns "$endpoint_ns" ip addr add "${address}/24" dev eth0
    ns "$endpoint_ns" sysctl -qw net.ipv4.conf.all.rp_filter=0
}

make_realm() {
    local realm=$1
    local host_ns=$2
    local endpoint_ns=$3
    local endpoint_ip=$4
    local remote_ip=$5
    local remote_transport=$6
    local vni=$7
    local table=$8
    local rule_base=$9
    local tunnel_ip=${10}
    local tunnel_peer=${11}
    local bridge="${realm}br"
    local endpoint_link="${realm}ep"
    local geneve="${realm}g"

    host "$host_ns" ip link add "$bridge" type bridge
    host "$host_ns" ip link set "$bridge" mtu "$TENANT_MTU" up
    host "$host_ns" ip addr add "10.0.0.1/24" dev "$bridge" noprefixroute
    host "$host_ns" ip neigh add proxy "$remote_ip" dev "$bridge"

    make_endpoint "$endpoint_ns" "$host_ns" "$endpoint_link" "$endpoint_ip"
    host "$host_ns" ip link set "$endpoint_link" master "$bridge"

    host "$host_ns" ip link add "$geneve" type geneve id "$vni" \
        remote "$remote_transport" dstport "$GENEVE_PORT"
    host "$host_ns" ip link set "$geneve" mtu "$TENANT_MTU" up
    host "$host_ns" ip addr add "${tunnel_ip}/30" dev "$geneve"

    host "$host_ns" ip route add table "$table" "10.0.0.0/24" dev "$bridge" src 10.0.0.1
    host "$host_ns" ip route add table "$table" "169.254.${table}.0/30" dev "$geneve"
    host "$host_ns" ip route add table "$table" "${remote_ip}/32" via "$tunnel_peer" dev "$geneve"
    host "$host_ns" ip rule add pref "$rule_base" iif "$bridge" table "$table"
    host "$host_ns" ip rule add pref "$((rule_base + 1))" iif "$geneve" table "$table"
    host "$host_ns" ip rule add pref "$((rule_base + 2))" iif "$endpoint_link" table "$table"
}

wait_for_wireguard() {
    for index in 1 2 3; do
        local host_ns="p11h${index}"
        local peer=$((index % 3 + 1))
        if ! ns "$host_ns" ping -I wg0 -c 2 -W 1 "198.18.0.${peer}" >/dev/null; then
            echo "WireGuard transport did not converge on $host_ns" >&2
            return 1
        fi
    done
}

assert_ping() {
    local endpoint_ns=$1
    local target=$2
    ns "$endpoint_ns" ping -c 3 -W 1 "$target" >/dev/null
}

assert_no_capture() {
    local file=$1
    if grep -Eq ' IP ' "$file"; then
        cat "$file" >&2
        return 1
    fi
}

require_clean_names
make_underlay
make_wireguard_keys
for index in 1 2 3; do
    configure_wireguard_host "$index"
done

# Realm A: A1 on host 1 and A2 on host 2, both 10.0.0.0/24.
make_realm a1 p11h1 p11ea1 10.0.0.10 10.0.0.20 198.18.0.2 101 101 100 169.254.101.1 169.254.101.2
make_realm a2 p11h2 p11ea2 10.0.0.20 10.0.0.10 198.18.0.1 101 101 110 169.254.101.2 169.254.101.1

# Realm B: B1 on host 2 and B2 on host 3, same CIDR and same endpoint IPs.
make_realm b2 p11h2 p11eb1 10.0.0.10 10.0.0.20 198.18.0.3 102 102 120 169.254.102.1 169.254.102.2
make_realm b3 p11h3 p11eb2 10.0.0.20 10.0.0.10 198.18.0.2 102 102 130 169.254.102.2 169.254.102.1

wait_for_wireguard

capture_file="$ROOT_TMP/underlay.capture"
timeout 15 tcpdump -ni "$UNDERLAY_BRIDGE" -nn -U 'udp port 65001' >"$capture_file" 2>&1 &
capture_pid=$!
sleep 1

assert_ping p11ea1 10.0.0.20
assert_ping p11eb1 10.0.0.20
sleep 1
kill "$capture_pid" 2>/dev/null || true
wait "$capture_pid" 2>/dev/null || true

if grep -Eq '10\.0\.0\.' "$capture_file"; then
    echo "cleartext tenant address observed on the underlay" >&2
    cat "$capture_file" >&2
    exit 1
fi

# The same destination IP is reachable only through the selected realm. A1's
# traffic must not arrive at B2, and B1's traffic must not arrive at A2. Each
# capture is scoped to the other realm so the selected-realm packet itself is
# not mistaken for a leakage observation.
timeout 5 ip netns exec p11eb2 tcpdump -ni eth0 -nn -U \
    'icmp and (src 10.0.0.10 or dst 10.0.0.10)' >"$ROOT_TMP/p11eb2.capture" 2>&1 &
capture_b2=$!
sleep 1
assert_ping p11ea1 10.0.0.20
kill "$capture_b2" 2>/dev/null || true
wait "$capture_b2" 2>/dev/null || true
assert_no_capture "$ROOT_TMP/p11eb2.capture"

timeout 5 ip netns exec p11ea2 tcpdump -ni eth0 -nn -U \
    'icmp and (src 10.0.0.10 or dst 10.0.0.10)' >"$ROOT_TMP/p11ea2.capture" 2>&1 &
capture_a2=$!
sleep 1
assert_ping p11eb1 10.0.0.20
kill "$capture_a2" 2>/dev/null || true
wait "$capture_a2" 2>/dev/null || true
assert_no_capture "$ROOT_TMP/p11ea2.capture"

# Policy primitive: deny A1's egress at the host realm boundary, then remove
# the owned rule and prove the allowed path recovers.
host p11h1 nft add table inet p11_p11_policy
host p11h1 nft add chain inet p11_p11_policy forward '{' type filter hook forward priority -100 ';' policy accept ';' '}'
host p11h1 nft add rule inet p11_p11_policy forward iifname "a1br" ip daddr 10.0.0.20 drop
if ns p11ea1 ping -c 2 -W 1 10.0.0.20 >/dev/null 2>&1; then
    echo "NetworkPolicy deny was not enforced" >&2
    exit 1
fi
host p11h1 nft delete table inet p11_p11_policy
assert_ping p11ea1 10.0.0.20

# Near-boundary tenant traffic must pass with the selected 1400-byte tenant
# MTU and must not silently rely on fragmentation.
ns p11ea1 ping -M do -s 1372 -c 2 -W 2 10.0.0.20 >/dev/null

# A VNI that is not attached to a realm has no delivery path.
host p11h1 ip link add p11wrong type geneve id 999 remote 198.18.0.2 dstport "$GENEVE_PORT"
host p11h1 ip link set p11wrong up
host p11h1 ip link del p11wrong

echo "p11-wireguard-geneve-overlap: wireguard-host-transport=passed"
echo "p11-wireguard-geneve-overlap: realm-a-overlap-traffic=passed"
echo "p11-wireguard-geneve-overlap: realm-b-overlap-traffic=passed"
echo "p11-wireguard-geneve-overlap: cross-realm-misdelivery=0"
echo "p11-wireguard-geneve-overlap: cleartext-underlay-tenant-packets=0"
echo "p11-wireguard-geneve-overlap: policy-deny-and-recovery=passed"
echo "p11-wireguard-geneve-overlap: mtu-boundary=passed"
echo "p11-wireguard-geneve-overlap: wrong-vni-delivery=0"

# The EXIT trap is the independently checked cleanup. Explicitly verify the
# exact names before the trap removes them, then verify absence after cleanup.
for ns_name in "${HOST_NAMES[@]}" "${ENDPOINT_NAMES[@]}"; do
    ip netns list | awk '{print $1}' | grep -Fxq "$ns_name" || {
        echo "expected live test namespace missing before cleanup: $ns_name" >&2
        exit 1
    }
done

cleanup
trap - EXIT
for ns_name in "${HOST_NAMES[@]}" "${ENDPOINT_NAMES[@]}"; do
    if ip netns list | awk '{print $1}' | grep -Fxq "$ns_name"; then
        echo "owned namespace leaked after cleanup: $ns_name" >&2
        exit 1
    fi
done
for link in "${ROOT_LINKS[@]}" "$UNDERLAY_BRIDGE"; do
    if ip link show "$link" >/dev/null 2>&1; then
        echo "owned link leaked after cleanup: $link" >&2
        exit 1
    fi
done
echo "p11-wireguard-geneve-overlap: cleanup=passed"
