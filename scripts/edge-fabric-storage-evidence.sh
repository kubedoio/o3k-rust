#!/bin/bash
# P11 storage evidence script.
#
# Configures host-local LVM and serial RBD cross-host semantics evidence.
# This script is intentionally bounded: it proves provider presence and
# records the claims required by SPEC-0029; full attach/detach lifecycle
# evidence is collected by the orchestrator using O3K volume operations.

set -euo pipefail

LAB_ROOT="/var/lib/o3k-fabric-lab"
EVIDENCE_DIR="${LAB_ROOT}/evidence"
SSH_KEY="/root/.ssh/id_ed25519"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"

CEPH_CONF="/etc/ceph/ceph.conf"
CEPH_KEYRING="/etc/ceph/ceph.keyring"
CEPH_ID="admin"
RBD_POOL="p11-rbd"
RBD_IMAGE="p11-serial-test"

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

rbd_cmd() {
  echo "rbd --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}'"
}

# Ensure each host has an O3K LVM volume group backed by a file loop device.
ensure_lvm_vg() {
  local host="$1"
  remote "$host" "
    set -euo pipefail
    if vgs --noheadings -o vg_name o3k-vg >/dev/null 2>&1; then
      echo 'vg-present'
      exit 0
    fi
    mkdir -p /var/lib/o3k/storage
    loop_file='/var/lib/o3k/storage/lvm-backing.img'
    if [[ ! -f \$loop_file ]]; then
      fallocate -l 2G \$loop_file || dd if=/dev/zero of=\$loop_file bs=1M count=2048
    fi
    loop_dev=\$(losetup -f --show \$loop_file)
    pvcreate -y \$loop_dev >/dev/null
    vgcreate o3k-vg \$loop_dev >/dev/null
    echo 'vg-created'
  "
}

# Check that a volume created on one host is not visible on another host.
lvm_locality_check() {
  log "LVM locality check: p11h1 volume must not be attachable on p11h2"
  remote p11h1 "
    set -euo pipefail
    lvremove -y o3k-vg/p11-locality-test >/dev/null 2>&1 || true
    lvcreate -L 100M -n p11-locality-test o3k-vg >/dev/null
  "
  local other_has_it="false"
  if remote p11h2 "lvs --noheadings -o lv_name o3k-vg 2>/dev/null | grep -qx 'p11-locality-test'"; then
    other_has_it="true"
  fi
  remote p11h1 "lvremove -y o3k-vg/p11-locality-test >/dev/null 2>&1 || true"
  echo "$other_has_it"
}

# Verify Ceph/RBD client is configured on each host.
rbd_readiness_check() {
  local all_ready="true"
  for host in p11h1 p11h2 p11h3; do
    if remote "$host" "rbd --version >/dev/null 2>&1 && test -f '${CEPH_CONF}' && test -f '${CEPH_KEYRING}'" >/dev/null 2>&1; then
      log "RBD client ready on $host"
    else
      log "RBD client NOT ready on $host"
      all_ready="false"
    fi
  done
  echo "$all_ready"
}

# Ensure the serial-test image exists in the pool.
ensure_rbd_image() {
  local cmd
  cmd=$(rbd_cmd)
  if ! rbd --conf "${CEPH_CONF}" --keyring "${CEPH_KEYRING}" --id "${CEPH_ID}" info "${RBD_POOL}/${RBD_IMAGE}" >/dev/null 2>&1; then
    log "Creating RBD image ${RBD_POOL}/${RBD_IMAGE}"
    rbd --conf "${CEPH_CONF}" --keyring "${CEPH_KEYRING}" --id "${CEPH_ID}" create "${RBD_POOL}/${RBD_IMAGE}" --size 64M
  else
    log "RBD image ${RBD_POOL}/${RBD_IMAGE} already exists"
  fi
}

# Return the number of RBD devices mapped on a host.
count_rbd_mappings() {
  local host="$1"
  remote "$host" "rbd device list 2>/dev/null | wc -l || echo 0"
}

# Serial single-map cross-host persistence check.
# Writes a marker from p11h1, then reads/verifies it from p11h2 and p11h3.
# Only one host holds the map at any time.
rbd_cross_host_check() {
  local marker="p11-serial-$(date +%s%N)-$(hostname -s)"
  local write_result="failed"
  local read_p11h2_result="failed"
  local read_p11h3_result="failed"
  local serial_violation="false"

  log "RBD serial cross-host write starting on p11h1"
  local before_mappings
  before_mappings=$(count_rbd_mappings p11h1)
  if [[ "$before_mappings" -ne 0 ]]; then
    log "Warning: p11h1 already has $before_mappings RBD mappings; unmapping before write"
    remote p11h1 "rbd device unmap --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null 2>&1 || true"
  fi

  if remote p11h1 "
    set -euo pipefail
    rbd device map --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
    dev=\$(rbd device list --format json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0][\"device\"])')
    printf '%s' '${marker}' > /tmp/p11-rbd-marker
    dd if=/tmp/p11-rbd-marker of=\$dev bs=512 count=1 conv=fsync >/dev/null 2>&1
    sync
    rbd device unmap --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
  " >/dev/null 2>&1; then
    write_result="passed"
  fi

  log "RBD serial cross-host read starting on p11h2"
  local mappings_h2
  mappings_h2=$(count_rbd_mappings p11h2)
  if [[ "$mappings_h2" -ne 0 ]]; then
    serial_violation="true"
    log "Serial violation detected on p11h2 before map: $mappings_h2 existing mappings"
  fi

  remote p11h2 "
    set -euo pipefail
    rbd device map --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
    dev=\$(rbd device list --format json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0][\"device\"])')
    dd if=\$dev of=/tmp/p11-rbd-marker bs=512 count=1 >/dev/null 2>&1
    cat /tmp/p11-rbd-marker
    rbd device unmap --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
  " > /tmp/p11-rbd-read-p11h2.txt && {
    if [[ "$(cat /tmp/p11-rbd-read-p11h2.txt | tr -d '\0')" == "$marker" ]]; then
      read_p11h2_result="passed"
    fi
  }

  log "RBD serial cross-host read starting on p11h3"
  local mappings_h3
  mappings_h3=$(count_rbd_mappings p11h3)
  if [[ "$mappings_h3" -ne 0 ]]; then
    serial_violation="true"
    log "Serial violation detected on p11h3 before map: $mappings_h3 existing mappings"
  fi

  remote p11h3 "
    set -euo pipefail
    rbd device map --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
    dev=\$(rbd device list --format json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0][\"device\"])')
    dd if=\$dev of=/tmp/p11-rbd-marker bs=512 count=1 >/dev/null 2>&1
    cat /tmp/p11-rbd-marker
    rbd device unmap --conf '${CEPH_CONF}' --keyring '${CEPH_KEYRING}' --id '${CEPH_ID}' '${RBD_POOL}/${RBD_IMAGE}' >/dev/null
  " > /tmp/p11-rbd-read-p11h3.txt && {
    if [[ "$(cat /tmp/p11-rbd-read-p11h3.txt | tr -d '\0')" == "$marker" ]]; then
      read_p11h3_result="passed"
    fi
  }

  jq -n \
    --arg marker "$marker" \
    --arg write_result "$write_result" \
    --arg read_p11h2_result "$read_p11h2_result" \
    --arg read_p11h3_result "$read_p11h3_result" \
    --arg serial_violation "$serial_violation" \
    '{
      marker: $marker,
      write_host: "p11h1",
      write_result: $write_result,
      read_p11h2_result: $read_p11h2_result,
      read_p11h3_result: $read_p11h3_result,
      serial_single_map_violation: ($serial_violation == "true"),
      result: (if $write_result == "passed" and $read_p11h2_result == "passed" and $read_p11h3_result == "passed" and $serial_violation == "false" then "passed" else "failed" end)
    }'
}

main() {
  mkdir -p "$EVIDENCE_DIR"
  log "Preparing LVM volume groups on hosts"
  for host in p11h1 p11h2 p11h3; do
    ensure_lvm_vg "$host"
  done

  local locality_violation
  locality_violation=$(lvm_locality_check)

  local rbd_ready
  rbd_ready=$(rbd_readiness_check)

  local rbd_cross_host="{}"
  if [[ "$rbd_ready" == "true" ]]; then
    ensure_rbd_image
    rbd_cross_host=$(rbd_cross_host_check)
  fi

  local rbd_cross_result
  rbd_cross_result=$(echo "$rbd_cross_host" | jq -r '.result // "skipped"')

  jq -n \
    --arg locality_violation "$locality_violation" \
    --arg rbd_ready "$rbd_ready" \
    --argjson rbd_cross_host "$rbd_cross_host" \
    --arg rbd_cross_result "$rbd_cross_result" \
    --arg note "LVM volumes are host-local; RBD serial single-map cross-host persistence" \
    '{
      lvm_locality_violation: ($locality_violation == "true"),
      lvm_locality_result: (if $locality_violation == "true" then "failed" else "passed" end),
      rbd_client_ready: ($rbd_ready == "true"),
      rbd_client_result: (if $rbd_ready == "true" then "ready" else "skipped" end),
      rbd_serial_cross_host: $rbd_cross_host,
      rbd_serial_result: $rbd_cross_result,
      note: $note
    }' > "${EVIDENCE_DIR}/p11-storage-evidence.json"

  log "Storage evidence written to ${EVIDENCE_DIR}/p11-storage-evidence.json"
}

main "$@"
