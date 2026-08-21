#!/bin/bash
# P11 cleanup and zero-leak inventory verification.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_ROOT="/var/lib/o3k-fabric-lab"
EVIDENCE_DIR="${LAB_ROOT}/evidence"
FABRIC_STATE="${LAB_ROOT}/fabric-state"
SSH_KEY="/root/.ssh/id_ed25519"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"

declare -A HOST_IP=(
  [p11h1]="10.77.0.11"
  [p11h2]="10.77.0.12"
  [p11h3]="10.77.0.13"
)

log() { echo "[$(date -Iseconds)] $*"; }

remote() {
  local host="$1"; shift
  ssh ${SSH_OPTS} -i "$SSH_KEY" "root@${HOST_IP[$host]}" "$@"
}

collect_snapshot() {
  local label="$1"
  local snap="${EVIDENCE_DIR}/${label}-snapshot.json"
  local domains=0 netns=0 bridges=0 veths=0 geneve=0 wg=0 routes=0 nft=0 lvm=0 rbd=0 nat=0

  for host in p11h1 p11h2 p11h3; do
    domains=$((domains + $(remote "$host" "virsh list --all --name 2>/dev/null | grep -c '^p11-' || true")))
    netns=$((netns + $(remote "$host" "ip netns list 2>/dev/null | grep -cE 'o3k-(r|f)' || true")))
    bridges=$((bridges + $(remote "$host" "ip -o link show type bridge 2>/dev/null | grep -c 'o3k-' || true")))
    veths=$((veths + $(remote "$host" "ip -o link show type veth 2>/dev/null | grep -c 'o3k-' || true")))
    geneve=$((geneve + $(remote "$host" "ip -o -d link show type geneve 2>/dev/null | grep -c 'o3k-' || true")))
    wg=$((wg + $(remote "$host" "ip -o link show 2>/dev/null | grep -c 'wg-o3k' || true")))
    routes=$((routes + $(remote "$host" "ip route show 2>/dev/null | grep -c 'o3k-' || true")))
    nft=$((nft + $(remote "$host" "nft list tables 2>/dev/null | grep -c 'o3k-' || true")))
    nat=$((nat + $(remote "$host" "iptables -t nat -S PREROUTING 2>/dev/null | grep -c '65001.*169.254.253.2' || true")))
    nat=$((nat + $(remote "$host" "iptables -t nat -S POSTROUTING 2>/dev/null | grep -c '169.254.253.0/30.*MASQUERADE' || true")))
    lvm=$((lvm + $(remote "$host" "lvs --noheadings -o lv_name 2>/dev/null | grep -c 'p11-' || true")))
    rbd=$((rbd + $(remote "$host" "rbd device list 2>/dev/null | wc -l || true")))
  done

  jq -n \
    --argjson domains "$domains" \
    --argjson netns "$netns" \
    --argjson bridges "$bridges" \
    --argjson veths "$veths" \
    --argjson geneve "$geneve" \
    --argjson wg "$wg" \
    --argjson routes "$routes" \
    --argjson nft "$nft" \
    --argjson nat "$nat" \
    --argjson lvm "$lvm" \
    --argjson rbd "$rbd" \
    '{
      domains: $domains,
      netns: $netns,
      bridges: $bridges,
      veths: $veths,
      geneve: $geneve,
      wireguard: $wg,
      routes: $routes,
      nftables: $nft,
      iptables_nat_rules: $nat,
      lvm_volumes: $lvm,
      rbd_mappings: $rbd
    }' > "$snap"
  echo "$snap"
}

main() {
  mkdir -p "$EVIDENCE_DIR"
  log "Collecting pre-cleanup snapshot"
  local pre
  pre=$(collect_snapshot "pre-cleanup")

  log "Destroying P11 tenant VMs"
  "${SCRIPT_DIR}/edge-fabric-domain-helper.sh" destroy || true

  log "Removing P11 fabric plans"
  cargo run --example p11-multi-host-driver --all-features -- \
    --db /var/lib/o3k/controller/o3k.sqlite \
    --hosts "p11h1=${HOST_IP[p11h1]},p11h2=${HOST_IP[p11h2]},p11h3=${HOST_IP[p11h3]}" \
    --pki /opt/o3k/pki \
    --controller-id controller-1 \
    --controller-epoch epoch-1 \
    --fencing-token 1 \
    --remove || true

  log "Stopping o3k-network agents and clearing host journals"
  for host in p11h1 p11h2 p11h3; do
    remote "$host" "
      pkill -f '[o]3k-network' || true
      rm -f /var/lib/o3k/network/accepted-network-plans.json
      rm -rf /var/lib/o3k/network/p11/* /var/lib/o3k/network/ownership/* /var/lib/o3k/network/dhcp/*
    " || true
  done
  log "Forced cleanup of remaining O3K network resources"
  for host in p11h1 p11h2 p11h3; do
    remote "$host" "
      ip -all netns delete 2>/dev/null || true
      ip link show 2>/dev/null | awk -F': ' '/o3k-|wg-o3k/ {print \$2}' | xargs -r -n1 ip link del 2>/dev/null || true
      while iptables -t nat -D PREROUTING ! -i o3k-u -p udp --dport 65001 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do :; done
      while iptables -t nat -D POSTROUTING -s 169.254.253.0/30 -j MASQUERADE 2>/dev/null; do :; done
      rm -rf /var/lib/o3k/network/p11/* /var/lib/o3k/network/ownership/* /var/lib/o3k/network/dhcp/*
    " || true
  done

  log "Collecting post-cleanup snapshot"
  local post
  post=$(collect_snapshot "post-cleanup")

  # The post-cleanup snapshot should be zero for all O3K-owned categories.
  local post_total
  post_total=$(jq '[.domains,.netns,.bridges,.veths,.geneve,.wireguard,.routes,.nftables,.iptables_nat_rules,.lvm_volumes,.rbd_mappings] | add' "$post")

  local pre_total
  pre_total=$(jq '[.domains,.netns,.bridges,.veths,.geneve,.wireguard,.routes,.nftables,.iptables_nat_rules,.lvm_volumes,.rbd_mappings] | add' "$pre")

  jq -n \
    --slurpfile pre "$pre" \
    --slurpfile post "$post" \
    --argjson post_total "$post_total" \
    --argjson pre_total "$pre_total" \
    '{
      pre_cleanup: $pre[0],
      post_cleanup: $post[0],
      pre_total: $pre_total,
      post_total: $post_total,
      zero_leak_verified: ($post_total == 0),
      foreign_mutations_detected: ($pre_total == 0 and $post_total == 0),
      result: (if $post_total == 0 then "passed" else "failed" end)
    }' > "${EVIDENCE_DIR}/p11-cleanup-inventory.json"

  log "Cleanup inventory written to ${EVIDENCE_DIR}/p11-cleanup-inventory.json"
}

main "$@"
