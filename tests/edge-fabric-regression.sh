#!/usr/bin/env bash
set -Eeuo pipefail

# ---------------------------------------------------------------------------
# P11 dataplane regression test
#
# Builds the p11-regression-helper, creates two isolated network namespaces
# connected by a veth underlay, and runs apply/remove cycles while verifying
# kernel state, WireGuard connectivity, FDB entries, and cleanup.
#
# Must be run as root (creates namespaces, WireGuard, Geneve, bridges, TAPs).
# ---------------------------------------------------------------------------

ME="$(basename "$0")"
ROOT_A="/tmp/o3k-reg-a"
ROOT_B="/tmp/o3k-reg-b"
HELPER_BIN=""
PASS=0
FAIL=0

cleanup_all() {
    set +e
    # Remove all test namespaces and links left by the backend.
    for ns in o3k-r-a1000000 o3k-r-b1000000 o3k-fabric o3k-reg-a o3k-reg-b; do
        ip netns del "$ns" 2>/dev/null || true
    done
    for iface in o3k-u veth-ab veth-ba; do
        ip link del "$iface" 2>/dev/null || true
    done
    # Clean up iptables rules that may have been added.
    iptables -t nat -D POSTROUTING -s 169.254.253.0/30 -j MASQUERADE 2>/dev/null || true
    while iptables -t nat -D PREROUTING -p udp --dport 65001 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do true; done
    while iptables -t nat -D PREROUTING '!' -i o3k-u -p udp --dport 65001 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do true; done
    rm -rf "$ROOT_A" "$ROOT_B"
    set -e
}

assert() {
    local label="$1"
    shift
    if eval "$*" 2>/dev/null; then
        echo "[PASS] $label"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $label"
        FAIL=$((FAIL + 1))
    fi
}

output_assert() {
    # Like assert, but lets stdout through so the caller can inspect it.
    local label="$1"
    shift
    if output=$(eval "$*" 2>/dev/null); then
        echo "[PASS] $label"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $label"
        FAIL=$((FAIL + 1))
    fi
}

# ---------------------------------------------------------------------------
# Deterministic names computed from the hardcoded topology constants.
# These match the backend's naming functions (FNV-1a 64-bit).
# ---------------------------------------------------------------------------

tap_name() {
    # Usage: tap_name <realm_hex> <ep_hex>
    # Replicates endpoint_tap_name() from linux_fabric/naming.rs.
    local realm_hex="$1"
    local ep_hex="$2"
    python3 - "$realm_hex" "$ep_hex" <<'PY'
import struct, sys
ra = bytes.fromhex(sys.argv[1])
ep = bytes.fromhex(sys.argv[2])
h = 0xcbf29ce484222325
for b in ra + ep:
    h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
bs = struct.pack(">Q", h)
print(f"o3k-t-{bs[4]:02x}{bs[5]:02x}{bs[6]:02x}{bs[7]:02x}")
PY
}

tunnel_mac() {
    local realm_hex="$1"
    local host_id="$2"
    python3 - "$realm_hex" "$host_id" <<'PY'
import struct, sys
ra = bytes.fromhex(sys.argv[1])
host = sys.argv[2].encode()
h = 0xcbf29ce484222325
for b in ra + host:
    h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
bs = struct.pack(">Q", h)
print(f"02:{bs[2]:02x}:{bs[3]:02x}:{bs[4]:02x}:{bs[5]:02x}:{bs[6]:02x}")
PY
}

geneve_name() {
    local realm_hex="$1"
    local target="$2"
    python3 - "$realm_hex" "$target" <<'PY'
import struct, sys
ra = bytes.fromhex(sys.argv[1])
tgt = sys.argv[2].encode()
h = 0xcbf29ce484222325
for b in ra + tgt:
    h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
print(f"o3k-g-{h & 0xffffffff:08x}")
PY
}

bridge_name() {
    local realm_hex="$1"
    local target="$2"
    python3 - "$realm_hex" "$target" <<'PY'
import struct, sys
ra = bytes.fromhex(sys.argv[1])
tgt = sys.argv[2].encode()
h = 0xcbf29ce484222325
for b in b"c" + ra + tgt:
    h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
print(f"o3k-c-{h & 0xffffffff:08x}")
PY
}

# UUID hex strings used in the helper (big-endian, no hyphens).
REALM_A_HEX="a1000000000000000000000000000001"
REALM_B_HEX="b1000000000000000000000000000001"
EP_A1_HEX="a1000000000000000000000000000101"
EP_A2_HEX="a1000000000000000000000000000102"
EP_B1_HEX="b1000000000000000000000000000101"
EP_B2_HEX="b1000000000000000000000000000102"

TAP_A1="$(tap_name "$REALM_A_HEX" "$EP_A1_HEX")"
TAP_A2="$(tap_name "$REALM_A_HEX" "$EP_A2_HEX")"
TAP_B1="$(tap_name "$REALM_B_HEX" "$EP_B1_HEX")"
TAP_B2="$(tap_name "$REALM_B_HEX" "$EP_B2_HEX")"
GENEVE_A_B="$(geneve_name "$REALM_A_HEX" "reg-host-b")"
GENEVE_B_B="$(geneve_name "$REALM_B_HEX" "reg-host-b")"
BRIDGE_A="$(bridge_name "$REALM_A_HEX" "reg-host-b")"
BRIDGE_B="$(bridge_name "$REALM_B_HEX" "reg-host-b")"

echo "=== Precomputed names ==="
echo "  TAP_A1=$TAP_A1  TAP_A2=$TAP_A2"
echo "  TAP_B1=$TAP_B1  TAP_B2=$TAP_B2"
echo "  GENEVE_A_B=$GENEVE_A_B  GENEVE_B_B=$GENEVE_B_B"
echo "  BRIDGE_A=$BRIDGE_A  BRIDGE_B=$BRIDGE_B"
echo ""

run_round() {
    local round="$1"
    local wg_args="$2"
    local underlay_endpoint_a="$3"
    local underlay_endpoint_b="$4"

    echo "=== Round $round ==="

    # 1. Build helper.
    echo "--- Building p11-regression-helper ---"
    cargo build --example p11-regression-helper --all-features 2>/dev/null
    HELPER_BIN="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | python3 -c "import sys,json; print(json.load(sys.stdin)['target_directory'])")/debug/examples/p11-regression-helper"
    if [[ ! -x "$HELPER_BIN" ]]; then
        echo "[FAIL] helper binary not found at $HELPER_BIN"
        return 1
    fi
    echo "[PASS] helper built: $HELPER_BIN"

    # 2. Create network namespaces and veth underlay.
    echo "--- Setting up underlay ---"
    ip netns add o3k-reg-a
    ip netns add o3k-reg-b
    ip link add veth-ab type veth peer name veth-ba
    ip link set veth-ab netns o3k-reg-a
    ip link set veth-ba netns o3k-reg-b
    ip netns exec o3k-reg-a ip addr add 10.77.0.1/24 dev veth-ab
    ip netns exec o3k-reg-a ip link set veth-ab up
    ip netns exec o3k-reg-b ip addr add 10.77.0.2/24 dev veth-ba
    ip netns exec o3k-reg-b ip link set veth-ba up
    ip netns exec o3k-reg-a ip link set lo up
    ip netns exec o3k-reg-b ip link set lo up
    ip netns exec o3k-reg-a sysctl -w net.ipv4.ip_forward=1 >/dev/null
    ip netns exec o3k-reg-b sysctl -w net.ipv4.ip_forward=1 >/dev/null
    echo "[PASS] underlay ready"

    # 3. Generate WireGuard keypairs.
    echo "--- Generating WireGuard keys ---"
    mkdir -p "$ROOT_A" "$ROOT_B"
    wg genkey > "$ROOT_A/wireguard-private.key" 2>/dev/null
    chmod 600 "$ROOT_A/wireguard-private.key"
    wg genkey > "$ROOT_B/wireguard-private.key" 2>/dev/null
    chmod 600 "$ROOT_B/wireguard-private.key"
    PUB_A="$(wg pubkey < "$ROOT_A/wireguard-private.key")"
    PUB_B="$(wg pubkey < "$ROOT_B/wireguard-private.key")"
    echo "[PASS] keys generated"

    # 4. Apply on host-A.
    echo "--- Applying on host-A ---"
    ip netns exec o3k-reg-a "$HELPER_BIN" \
        --root "$ROOT_A" --mode apply \
        --host-id reg-host-a --transport-ip 198.18.0.1 \
        --peer-host-id reg-host-b --peer-transport-ip 198.18.0.2 \
        --peer-public-key "$PUB_B" \
        --underlay-endpoint "$underlay_endpoint_a" \
        $wg_args \
        2>&1
    echo "[PASS] host-A apply"

    # 5. Apply on host-B.
    echo "--- Applying on host-B ---"
    ip netns exec o3k-reg-b "$HELPER_BIN" \
        --root "$ROOT_B" --mode apply \
        --host-id reg-host-b --transport-ip 198.18.0.2 \
        --peer-host-id reg-host-a --peer-transport-ip 198.18.0.1 \
        --peer-public-key "$PUB_A" \
        --underlay-endpoint "$underlay_endpoint_b" \
        $wg_args \
        2>&1
    echo "[PASS] host-B apply"

    # -------------------------------------------------------------------
    # Kernel state assertions
    # -------------------------------------------------------------------

    # 6. TAP interfaces exist — each host only has its local TAPs.
    for tap in "$TAP_A1" "$TAP_B1"; do
        assert "TAP $tap exists on host-A" \
            ip netns exec o3k-reg-a ip link show "$tap"
    done
    for tap in "$TAP_A2" "$TAP_B2"; do
        assert "TAP $tap exists on host-B" \
            ip netns exec o3k-reg-b ip link show "$tap"
    done

    # 7. Geneve interfaces exist in the fabric namespace.
    for ns in o3k-reg-a o3k-reg-b; do
        for geneve in "$GENEVE_A_B" "$GENEVE_B_B"; do
            assert "Geneve $geneve exists in $ns fabric ns" \
                ip netns exec "$ns" ip netns exec o3k-fabric ip link show "$geneve"
        done
    done

    # 8. WireGuard interface exists with correct transport IP.
    for ns in o3k-reg-a o3k-reg-b; do
        assert "WireGuard wg-o3k exists in $ns" \
            ip netns exec "$ns" ip netns exec o3k-fabric ip link show wg-o3k
    done
    assert "WireGuard transport IP on host-A" \
        ip netns exec o3k-reg-a ip netns exec o3k-fabric ip addr show wg-o3k \
        | grep -q "198.18.0.1/32"
    assert "WireGuard transport IP on host-B" \
        ip netns exec o3k-reg-b ip netns exec o3k-fabric ip addr show wg-o3k \
        | grep -q "198.18.0.2/32"

    # 9. Realm namespaces exist with gateway.
    assert "Realm A namespace exists on host-A" \
        ip netns exec o3k-reg-a ip netns exec o3k-r-a1000000 true
    assert "Realm B namespace exists on host-A" \
        ip netns exec o3k-reg-a ip netns exec o3k-r-b1000000 true
    assert "Realm A namespace exists on host-B" \
        ip netns exec o3k-reg-b ip netns exec o3k-r-a1000000 true
    assert "Realm B namespace exists on host-B" \
        ip netns exec o3k-reg-b ip netns exec o3k-r-b1000000 true

    # 10. Dataplane: WireGuard tunnel connectivity.
    assert "WireGuard ping host-A -> host-B transport IP" \
        ip netns exec o3k-reg-a ip netns exec o3k-fabric \
            ping -c 3 -W 2 -i 0.3 198.18.0.2
    assert "WireGuard ping host-B -> host-A transport IP" \
        ip netns exec o3k-reg-b ip netns exec o3k-fabric \
            ping -c 3 -W 2 -i 0.3 198.18.0.1

    # 11. FDB entries on realm-per-target bridges have "static" (not "permanent").
    #     Bridges and FDB are inside the fabric namespace; check from there.
    for ns in o3k-reg-a o3k-reg-b; do
        local_mac_a="$(tunnel_mac "$REALM_A_HEX" "reg-host-a")"
        local_mac_b="$(tunnel_mac "$REALM_B_HEX" "reg-host-a")"
        remote_mac_a="$(tunnel_mac "$REALM_A_HEX" "reg-host-b")"
        remote_mac_b="$(tunnel_mac "$REALM_B_HEX" "reg-host-b")"
        if [[ "$ns" == "o3k-reg-b" ]]; then
            local_mac_a="$(tunnel_mac "$REALM_A_HEX" "reg-host-b")"
            local_mac_b="$(tunnel_mac "$REALM_B_HEX" "reg-host-b")"
            remote_mac_a="$(tunnel_mac "$REALM_A_HEX" "reg-host-a")"
            remote_mac_b="$(tunnel_mac "$REALM_B_HEX" "reg-host-a")"
        fi
        fdb_show() { ip netns exec "$ns" ip netns exec o3k-fabric bridge fdb show br "$1" 2>/dev/null; }
        for mac in "$local_mac_a" "$remote_mac_a"; do
            assert "FDB $BRIDGE_A has static entry for $mac in $ns" \
                fdb_show "$BRIDGE_A" | grep -q "$mac.*static"
        done
        for mac in "$local_mac_b" "$remote_mac_b"; do
            assert "FDB $BRIDGE_B has static entry for $mac in $ns" \
                fdb_show "$BRIDGE_B" | grep -q "$mac.*static"
        done
    done

    # 12. Tenant routes are NOT visible in the underlay namespace.
    for ns in o3k-reg-a o3k-reg-b; do
        assert "No tenant route 10.0.0.0/24 in $ns underlay" \
            ! ip netns exec "$ns" ip route show table main \
            | grep -q "10\.0\.0\.0/24"
    done

    # 13. Tenant routes ARE present in realm namespaces.
    assert "Tenant route in realm A ns on host-A" \
        ip netns exec o3k-reg-a ip netns exec o3k-r-a1000000 \
            ip route show table main | grep -q "10\.0\.0\.0/24"
    assert "Tenant route in realm B ns on host-B" \
        ip netns exec o3k-reg-b ip netns exec o3k-r-b1000000 \
            ip route show table main | grep -q "10\.0\.0\.0/24"

    # -------------------------------------------------------------------
    # Remove phase
    # -------------------------------------------------------------------

    echo "--- Removing on host-A ---"
    ip netns exec o3k-reg-a "$HELPER_BIN" \
        --root "$ROOT_A" --mode remove \
        --host-id reg-host-a --transport-ip 198.18.0.1 \
        --peer-host-id reg-host-b --peer-transport-ip 198.18.0.2 \
        --peer-public-key "$PUB_B" \
        --underlay-endpoint "$underlay_endpoint_a" \
        $wg_args \
        2>&1
    echo "[PASS] host-A remove"

    echo "--- Removing on host-B ---"
    ip netns exec o3k-reg-b "$HELPER_BIN" \
        --root "$ROOT_B" --mode remove \
        --host-id reg-host-b --transport-ip 198.18.0.2 \
        --peer-host-id reg-host-a --peer-transport-ip 198.18.0.1 \
        --peer-public-key "$PUB_A" \
        --underlay-endpoint "$underlay_endpoint_b" \
        $wg_args \
        2>&1
    echo "[PASS] host-B remove"

    # 14. Verify cleanup: namespaces and interfaces gone.
    for ns in o3k-r-a1000000 o3k-r-b1000000 o3k-fabric; do
        assert "Realm/fabric ns $ns removed from host-A" \
            ! ip netns exec o3k-reg-a ip netns exec "$ns" true 2>/dev/null
        assert "Realm/fabric ns $ns removed from host-B" \
            ! ip netns exec o3k-reg-b ip netns exec "$ns" true 2>/dev/null
    done

    # Verify wg interface is gone (inside fabric namespace, which is also removed).
    # The fabric namespace deletion above implies wg-o3k is gone; this is a belt.
    assert "WireGuard interface removed from host-A" \
        ! ip netns exec o3k-reg-a ip netns exec o3k-fabric ip link show wg-o3k 2>/dev/null
    assert "WireGuard interface removed from host-B" \
        ! ip netns exec o3k-reg-b ip netns exec o3k-fabric ip link show wg-o3k 2>/dev/null

    # 15. Clean up namespaces and state for this round.
    echo "--- Cleaning up round state ---"
    for ns in o3k-reg-a o3k-reg-b; do
        ip netns del "$ns" 2>/dev/null || true
    done
    for iface in veth-ab veth-ba; do
        ip link del "$iface" 2>/dev/null || true
    done
    rm -rf "$ROOT_A" "$ROOT_B"
    # Clean up lingering iptables rules.
    iptables -t nat -D POSTROUTING -s 169.254.253.0/30 -j MASQUERADE 2>/dev/null || true
    while iptables -t nat -D PREROUTING -p udp --dport 65001 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do true; done
    while iptables -t nat -D PREROUTING '!' -i o3k-u -p udp --dport 65001 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do true; done
    echo "[PASS] round cleanup done"
}

# ===========================================================================
# Main
# ===========================================================================

if [[ $EUID -ne 0 ]]; then
    echo "This test must be run as root." >&2
    exit 1
fi

for tool in cargo ip wg python3 sysctl iptables; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Missing required tool: $tool" >&2
        exit 1
    }
done

trap cleanup_all EXIT

# Run two rounds for idempotency proof — first with default port, second with custom port.
echo "=== Round 1: default WireGuard port 65001 ==="
run_round 1 "" ""
echo ""

echo "=== Round 2: custom WireGuard port 65123 ==="
run_round 2 "--wireguard-port 65123" "10.77.0.2:65123" "10.77.0.1:65123"

echo ""
echo "=============================================="
echo "  P11 dataplane regression complete"
echo "  PASS: $PASS  FAIL: $FAIL"
echo "=============================================="

if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
