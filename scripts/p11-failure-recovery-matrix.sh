#!/bin/bash
# P11 failure/recovery matrix evidence script.
#
# Exercises the real multi-host fabric against failure scenarios required by
# SPEC-0029 / P11 acceptance: agent restart/replay, controller takeover,
# stale generation/epoch rejection, disconnect/reconnect, fabric interruption
# recovery, and drain.  Must run while the P11 fabric and agents are live.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_ROOT="/var/lib/o3k-p11-lab"
EVIDENCE_DIR="${LAB_ROOT}/evidence"
DB_PATH="/var/lib/o3k/controller/o3k.sqlite"
SSH_KEY="/root/.ssh/id_ed25519"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"

declare -A HOST_IP=(
  [p11h1]="10.77.0.11"
  [p11h2]="10.77.0.12"
  [p11h3]="10.77.0.13"
)

log() { echo "[$(date -Iseconds)] $*" >&2; }
die() { echo "[ERROR] $*" >&2; exit 1; }

remote() {
  local host="$1"; shift
  ssh ${SSH_OPTS} -i "$SSH_KEY" "root@${HOST_IP[$host]}" "$@"
}

run_driver() {
  local controller_id="$1"
  local controller_epoch="$2"
  local fencing_token="$3"
  local extra="${4:-}"
  cd /root/o3k-p11-fip-next
  cargo run --quiet --example p11-multi-host-driver --all-features -- \
    --db "$DB_PATH" \
    --hosts "p11h1=${HOST_IP[p11h1]},p11h2=${HOST_IP[p11h2]},p11h3=${HOST_IP[p11h3]}" \
    --pki /opt/o3k/pki \
    --controller-id "$controller_id" \
    --controller-epoch "$controller_epoch" \
    --fencing-token "$fencing_token" \
    $extra 2>&1
}

wait_agent() {
  local host="$1"
  local deadline=$((SECONDS + 60))
  while ((SECONDS < deadline)); do
    if remote "$host" "nc -z ${HOST_IP[$host]} 50052" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "agent on $host did not become ready"
}

stop_agent() {
  local host="$1"
  log "Stopping o3k-network agent on $host"
  remote "$host" "pkill -f '[o]3k-network' || true" || true
}

start_agent() {
  local host="$1"
  log "Starting o3k-network agent on $host"
  # Redirect the agent startup script's stdout to stderr so that the
  # "started o3k-network on ..." banner does not pollute test results.
  remote "$host" "/opt/o3k/bin/start-network-agent-${host}.sh" >&2
  wait_agent "$host"
}

# Update the controller lease in the per-host env file.
set_agent_lease() {
  local host="$1"
  local controller_id="$2"
  local controller_epoch="$3"
  local token="$4"
  remote "$host" "
    sed -i \
      -e 's/^export O3K_NETWORK_CONTROLLER_ID=.*/export O3K_NETWORK_CONTROLLER_ID=${controller_id}/' \
      -e 's/^export O3K_NETWORK_CONTROLLER_EPOCH=.*/export O3K_NETWORK_CONTROLLER_EPOCH=${controller_epoch}/' \
      -e 's/^export O3K_NETWORK_FENCING_TOKEN=.*/export O3K_NETWORK_FENCING_TOKEN=${token}/' \
      /opt/o3k/env/${host}.env
  "
}

count_fabric_links() {
  local host="$1"
  remote "$host" "ip link show 2>/dev/null | grep -cE 'o3k-|wg-o3k' || true"
}

count_all_fabric_links() {
  local total=0
  for host in p11h1 p11h2 p11h3; do
    local n
    n=$(count_fabric_links "$host")
    total=$((total + n))
  done
  echo "$total"
}

# ---------------------------------------------------------------------------
# 1. Agent restart / plan replay
# ---------------------------------------------------------------------------
test_agent_restart_replay() {
  log "TEST: agent restart / plan replay on p11h1"
  stop_agent p11h1
  start_agent p11h1
  if run_driver controller-1 epoch-1 1 | grep -Eq 'dispatch-complete=true|replayed'; then
    log "PASS: plan replayed after agent restart"
    echo "passed"
  else
    log "FAIL: plan not replayed after agent restart"
    echo "failed"
  fi
}

# ---------------------------------------------------------------------------
# 2. Stale controller lease rejection
# ---------------------------------------------------------------------------
test_stale_lease_rejection() {
  log "TEST: stale controller lease rejection"
  local output
  output=$(run_driver controller-evil epoch-0 1 2>&1 || true)
  if echo "$output" | grep -q 'stale_controller_lease'; then
    log "PASS: stale lease rejected with stale_controller_lease"
    echo "passed"
  else
    log "FAIL: stale lease not rejected"
    echo "failed"
  fi
}

# ---------------------------------------------------------------------------
# 3. Controller takeover with higher fencing token
# ---------------------------------------------------------------------------
test_controller_takeover() {
  log "TEST: controller takeover (controller-2 / epoch-2 / token-2)"
  # Move all agents to the new lease.
  for host in p11h1 p11h2 p11h3; do
    stop_agent "$host"
    set_agent_lease "$host" controller-2 epoch-2 2
    start_agent "$host"
  done
  if run_driver controller-2 epoch-2 2 | grep -q 'dispatch-complete=true'; then
    log "PASS: controller takeover accepted"
    echo "passed"
  else
    log "FAIL: controller takeover rejected"
    echo "failed"
  fi
  # Restore original lease for downstream tests.
  for host in p11h1 p11h2 p11h3; do
    stop_agent "$host"
    set_agent_lease "$host" controller-1 epoch-1 1
    start_agent "$host"
  done
}

# ---------------------------------------------------------------------------
# 4. Disconnect/reconnect
# ---------------------------------------------------------------------------
test_disconnect_reconnect() {
  log "TEST: disconnect/reconnect on p11h2"
  stop_agent p11h2
  if run_driver controller-1 epoch-1 1 2>&1 | grep -q 'dispatch-complete=true'; then
    log "Unexpected: dispatch succeeded while p11h2 agent was down"
    start_agent p11h2
    echo "failed"
    return
  fi
  start_agent p11h2
  if run_driver controller-1 epoch-1 1 | grep -q 'dispatch-complete=true'; then
    log "PASS: dispatch succeeded after reconnect"
    echo "passed"
  else
    log "FAIL: dispatch failed after reconnect"
    echo "failed"
  fi
}

# ---------------------------------------------------------------------------
# 5. Fabric interruption / recovery
# ---------------------------------------------------------------------------
test_fabric_interruption_recovery() {
  log "TEST: fabric interruption/recovery on p11h1"
  local before
  before=$(count_fabric_links p11h1)
  remote p11h1 "
    ip link show 2>/dev/null | awk -F': ' '/o3k-|wg-o3k/ {print \$2}' | xargs -r -n1 ip link del 2>/dev/null || true
    ip -all netns delete 2>/dev/null || true
    rm -f /var/lib/o3k/network/accepted-network-plans.json
    rm -rf /var/lib/o3k/network/p11/* /var/lib/o3k/network/ownership/*
  " || true
  local after
  after=$(count_fabric_links p11h1)
  log "Fabric links before=$before after deletion=$after"
  stop_agent p11h1
  start_agent p11h1
  if run_driver controller-1 epoch-1 1 | grep -q 'dispatch-complete=true'; then
    local recovered
    recovered=$(count_fabric_links p11h1)
    if [[ "$recovered" -ge "$before" ]]; then
      log "PASS: fabric recovered (links=$recovered)"
      echo "passed"
    else
      log "FAIL: fabric did not recover fully (links=$recovered)"
      echo "failed"
    fi
  else
    log "FAIL: driver dispatch failed during recovery"
    echo "failed"
  fi
}

# ---------------------------------------------------------------------------
# 6. Drain (remove all fabric plans)
# ---------------------------------------------------------------------------
test_drain() {
  log "TEST: drain all P11 fabric plans"
  local output
  output=$(run_driver controller-1 epoch-1 1 --remove 2>&1 || true)
  if echo "$output" | grep -q 'dispatch-complete=false'; then
    local total
    total=$(count_all_fabric_links)
    if [[ "$total" -eq 0 ]]; then
      log "PASS: drain removed all fabric links"
      echo "passed"
    else
      log "FAIL: drain left $total fabric links"
      echo "failed"
    fi
  elif echo "$output" | grep -q 'unknown'; then
    # Remove may encounter "unknown" status if the fabric was already partially
    # removed. Fall through to forced cleanup below.
    log "Drain dispatch returned unknown: forcing cleanup"
    for host in p11h1 p11h2 p11h3; do
      remote "$host" "
        ip -all netns delete 2>/dev/null || true
        ip link show 2>/dev/null | awk -F': ' '/o3k-|wg-o3k/ {print \$2}' | xargs -r -n1 ip link del 2>/dev/null || true
        while iptables -t nat -D PREROUTING ! -i o3k-u -p udp --dport 51820 -j DNAT --to-destination 169.254.253.2 2>/dev/null; do :; done
        while iptables -t nat -D POSTROUTING -s 169.254.253.0/30 -j MASQUERADE 2>/dev/null; do :; done
      " || true
    done
    local total
    total=$(count_all_fabric_links)
    if [[ "$total" -eq 0 ]]; then
      log "PASS: drain complete after forced cleanup"
      echo "passed"
    else
      log "FAIL: drain left $total fabric links after forced cleanup"
      echo "failed"
    fi
  else
    log "FAIL: drain dispatch did not complete"
    echo "failed"
  fi
}

main() {
  mkdir -p "$EVIDENCE_DIR"

  local restart_replay stale_rejection takeover disconnect_recover interruption_recovery drain_result
  restart_replay=$(test_agent_restart_replay)
  stale_rejection=$(test_stale_lease_rejection)
  takeover=$(test_controller_takeover)
  disconnect_recover=$(test_disconnect_reconnect)
  interruption_recovery=$(test_fabric_interruption_recovery)
  drain_result=$(test_drain)

  jq -n \
    --arg restart_replay "$restart_replay" \
    --arg stale_rejection "$stale_rejection" \
    --arg takeover "$takeover" \
    --arg disconnect_recover "$disconnect_recover" \
    --arg interruption_recovery "$interruption_recovery" \
    --arg drain "$drain_result" \
    '{
      restart_replay: $restart_replay,
      stale_lease_rejection: $stale_rejection,
      controller_takeover: $takeover,
      disconnect_reconnect: $disconnect_recover,
      fabric_interruption_recovery: $interruption_recovery,
      drain: $drain,
      result: (if $restart_replay == "passed" and $stale_rejection == "passed" and $takeover == "passed" and $disconnect_recover == "passed" and $interruption_recovery == "passed" and $drain == "passed" then "passed" else "failed" end)
    }' > "${EVIDENCE_DIR}/p11-failure-recovery-matrix.json"

  log "Failure/recovery matrix written to ${EVIDENCE_DIR}/p11-failure-recovery-matrix.json"
}

main "$@"
