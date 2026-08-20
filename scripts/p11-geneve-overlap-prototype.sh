#!/usr/bin/env bash
# Focused kernel/iproute2 Geneve prototype for P11 v2.
#
# This intentionally proves only the realm-scoped Geneve primitive: two
# isolated realm pairs use the same 10.0.0.0/24 and the same endpoint IPs,
# while separate VNIs deliver each packet to the correct pair. It is not the
# P11 support gate: WireGuard, policy compilation, libvirt guests, storage,
# failure/restart evidence, and three independent hypervisors remain outside
# this prototype.
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    printf '%s\n' 'run this prototype as root' >&2
    exit 1
fi

for tool in ip ping tcpdump; do
    command -v "$tool" >/dev/null || {
        printf 'missing required tool: %s\n' "$tool" >&2
        exit 1
    }
done

realm_names=(p11pa p11pb p11pc p11pd)
endpoint_names=(p11ea p11eb p11ec p11ed)
root_links=(p11ua p11ub p11uc p11ud)
endpoint_links=(p11xa p11xb p11xc p11xd)

cleanup() {
    set +e
    for namespace in "${realm_names[@]}" "${endpoint_names[@]}"; do
        ip netns del "$namespace" 2>/dev/null
    done
    ip link del p11under 2>/dev/null
    rm -f /tmp/p11-a-ping /tmp/p11-b-ping /tmp/p11-wrong-a /tmp/p11-wrong-b
}

cleanup
trap cleanup EXIT

ip link add p11under type bridge
ip link set dev p11under up

setup_realm() {
    local realm=$1
    local endpoint=$2
    local underlay_ip=$3
    local remote_underlay_ip=$4
    local vni=$5
    local endpoint_ip=$6
    local remote_endpoint_ip=$7
    local gateway_mac=$8
    local local_tunnel_mac=$9
    local remote_tunnel_mac=${10}
    local realm_ns="p11p${realm}"
    local root_underlay="p11u${realm}"
    local realm_underlay="p11v${realm}"
    local root_endpoint="p11x${realm}"
    local realm_endpoint="p11e${realm}"

    ip netns add "$realm_ns"
    ip netns add "$endpoint"

    ip link add "$root_underlay" type veth peer name "$realm_underlay"
    ip link set dev "$realm_underlay" netns "$realm_ns"
    ip link set dev "$root_underlay" master p11under
    ip link set dev "$root_underlay" up
    ip netns exec "$realm_ns" ip link set dev "$realm_underlay" name underlay
    ip netns exec "$realm_ns" ip addr add "$underlay_ip/24" dev underlay
    ip netns exec "$realm_ns" ip link set dev underlay up
    ip netns exec "$realm_ns" ip link set dev lo up

    ip link add "$root_endpoint" type veth peer name "$realm_endpoint"
    ip link set dev "$root_endpoint" netns "$realm_ns"
    ip link set dev "$realm_endpoint" netns "$endpoint"
    ip netns exec "$realm_ns" ip link set dev "$root_endpoint" name endpoint
    ip netns exec "$realm_ns" ip link add dev realm-bridge type bridge
    ip netns exec "$realm_ns" ip link set dev realm-bridge address "$gateway_mac"
    ip netns exec "$realm_ns" ip link set dev realm-bridge up
    ip netns exec "$realm_ns" ip link set dev endpoint master realm-bridge
    ip netns exec "$realm_ns" ip link set dev endpoint up
    ip netns exec "$realm_ns" ip addr add 10.0.0.1/24 dev realm-bridge
    ip netns exec "$realm_ns" sysctl -qw net.ipv4.ip_forward=1
    ip netns exec "$realm_ns" sysctl -qw net.ipv4.conf.realm-bridge.proxy_arp=1
    ip netns exec "$realm_ns" ip neigh add proxy "$remote_endpoint_ip" dev realm-bridge

    ip netns exec "$realm_ns" ip link add dev geneve type geneve \
        id "$vni" remote "$remote_underlay_ip" dstport 6081
    ip netns exec "$realm_ns" ip link set dev geneve address "$local_tunnel_mac"
    ip netns exec "$realm_ns" ip link set dev geneve mtu 1400
    ip netns exec "$realm_ns" ip link set dev geneve up
    ip netns exec "$realm_ns" ip route replace "$remote_endpoint_ip/32" dev geneve
    ip netns exec "$realm_ns" ip neigh replace "$remote_endpoint_ip" \
        lladdr "$remote_tunnel_mac" nud permanent dev geneve

    ip netns exec "$endpoint" ip link set dev "$realm_endpoint" name eth0
    ip netns exec "$endpoint" ip link set dev lo up
    ip netns exec "$endpoint" ip addr add "$endpoint_ip/24" dev eth0
    ip netns exec "$endpoint" ip link set dev eth0 up
}

# Realm A: A1 -> A2, VNI 101.
setup_realm a p11ea 198.18.0.1 198.18.0.2 101 10.0.0.10 10.0.0.20 \
    02:aa:00:00:00:01 02:aa:00:00:00:11 02:aa:00:00:00:12
setup_realm b p11eb 198.18.0.2 198.18.0.1 101 10.0.0.20 10.0.0.10 \
    02:aa:00:00:00:02 02:aa:00:00:00:12 02:aa:00:00:00:11

# Realm B: B1 -> B2, same CIDR/IPs, VNI 102.
setup_realm c p11ec 198.18.0.3 198.18.0.4 102 10.0.0.10 10.0.0.20 \
    02:bb:00:00:00:01 02:bb:00:00:00:11 02:bb:00:00:00:12
setup_realm d p11ed 198.18.0.4 198.18.0.3 102 10.0.0.20 10.0.0.10 \
    02:bb:00:00:00:02 02:bb:00:00:00:12 02:bb:00:00:00:11

ip netns exec p11ea ping -c 2 -W 1 10.0.0.20 >/tmp/p11-a-ping
ip netns exec p11ec ping -c 2 -W 1 10.0.0.20 >/tmp/p11-b-ping

grep -q '2 received' /tmp/p11-a-ping
grep -q '2 received' /tmp/p11-b-ping
ip netns exec p11ea ip neigh show 10.0.0.20 dev eth0 | grep -qi '02:aa:00:00:00:01'
ip netns exec p11ec ip neigh show 10.0.0.20 dev eth0 | grep -qi '02:bb:00:00:00:01'

# The identical destination IP must not appear in the other realm's guest.
timeout 3 ip netns exec p11ed tcpdump -ni eth0 -c 1 'icmp and src host 10.0.0.10' \
    >/tmp/p11-wrong-b 2>&1 &
wrong_b_pid=$!
sleep 0.2
ip netns exec p11ea ping -c 1 -W 1 10.0.0.20 >/dev/null
if wait "$wrong_b_pid"; then
    printf '%s\n' 'cross-realm packet reached Realm B' >&2
    exit 1
fi

timeout 3 ip netns exec p11eb tcpdump -ni eth0 -c 1 \
    'icmp and src host 10.0.0.10' >/tmp/p11-wrong-a 2>&1 &
wrong_a_pid=$!
sleep 0.2
ip netns exec p11ec ping -c 1 -W 1 10.0.0.20 >/dev/null
if wait "$wrong_a_pid"; then
    printf '%s\n' 'cross-realm packet reached Realm A' >&2
    exit 1
fi

printf '%s\n' 'p11-geneve-overlap-prototype: realm-a-in-realm-traffic=passed'
printf '%s\n' 'p11-geneve-overlap-prototype: realm-b-in-realm-traffic=passed'
printf '%s\n' 'p11-geneve-overlap-prototype: overlapping-cidr-and-ip=passed'
printf '%s\n' 'p11-geneve-overlap-prototype: cross-realm-misdelivery=0'
printf '%s\n' 'p11-geneve-overlap-prototype: vni-a=101 vni-b=102'
printf '%s\n' 'p11-geneve-overlap-prototype: cleanup=passed'
