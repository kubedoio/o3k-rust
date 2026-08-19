#!/usr/bin/env bash
set -Eeuo pipefail

# Protected P10 Ceph RBD real-guest gate. The MicroCeph cluster is runner
# infrastructure; this script owns only the exact run-scoped pool namespace,
# RBD image, snapshots, libvirt domains, and guest state below.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ID="${O3K_CEPH_RUN_ID:-${GITHUB_RUN_ID:-}}"
RUN_SLUG="$(printf '%s' "${RUN_ID}" | tr -cd '[:alnum:]' | head -c 16)"
STATE_ROOT="${O3K_CEPH_GUEST_STATE_ROOT:-/var/lib/o3k-ceph-testlab/${RUN_SLUG}/guest}"
ARTIFACT_DIR="${O3K_CEPH_GUEST_ARTIFACT_DIR:-${ROOT_DIR}/target/ceph-rbd-real-guest-artifacts}"
ARTIFACT="${ARTIFACT_DIR}/ceph-rbd-real-guest-result.json"
DOMAIN="o3k-ceph-guest-${RUN_SLUG}"
SECOND_DOMAIN="o3k-ceph-guest-re-${RUN_SLUG}"
VOLUME_ID="${O3K_CEPH_VOLUME_ID:-$(cat /proc/sys/kernel/random/uuid)}"
ATTACHMENT_ID="${O3K_CEPH_ATTACHMENT_ID:-$(cat /proc/sys/kernel/random/uuid)}"
SNAPSHOT_ID="${O3K_CEPH_SNAPSHOT_ID:-$(cat /proc/sys/kernel/random/uuid)}"
PROJECT_ID="${O3K_CEPH_PROJECT_ID:-p10-ceph-real-guest}"
NETWORK="${O3K_CEPH_GUEST_NETWORK:-default}"
DEVICE="${O3K_CEPH_GUEST_DEVICE:-vdb}"
BASE_IMAGE_SOURCE="${O3K_CEPH_GUEST_IMAGE_PATH:-}"
BASE_IMAGE=""
SSH_KEY="${O3K_CEPH_GUEST_SSH_PRIVATE_KEY:-}"
SSH_USER="${O3K_CEPH_GUEST_SSH_USER:-}"
SSH_HOST_OVERRIDE="${O3K_CEPH_GUEST_SSH_HOST:-}"
ACTIVE_SSH_HOST=""
CURRENT_DOMAIN="${DOMAIN}"
OVERLAY="${STATE_ROOT}/${DOMAIN}.qcow2"
SECOND_OVERLAY="${STATE_ROOT}/${SECOND_DOMAIN}.qcow2"
KNOWN_HOSTS="${STATE_ROOT}/known_hosts"
CANDIDATE_KNOWN_HOSTS="${STATE_ROOT}/known_hosts.candidate"
USER_DATA="${STATE_ROOT}/user-data"
META_DATA="${STATE_ROOT}/meta-data"
MAPPED_DEVICE=""
RESULT_STATUS=failed
RESULT_REASON=not_completed
CHECKSUM=""
FOREIGN_BEFORE=""
FOREIGN_AFTER=""
CLEANUP_FAILED=false

die() { RESULT_REASON="$1"; return 1; }
valid_uuid() { [[ "$1" =~ ^[0-9a-fA-F-]{36}$ ]]; }
validate_path() { [[ "$1" = /* && "$1" != *"/../"* && "$1" != */.. && -f "$1" ]]; }

require_inputs() {
    [[ -n "${RUN_ID}" && -n "${RUN_SLUG}" ]] || die missing_run_identity
    validate_path "${BASE_IMAGE_SOURCE}" || die guest_image_unavailable
    validate_path "${SSH_KEY}" || die guest_key_unavailable
    [[ -n "${SSH_USER}" && "${SSH_USER}" != *[[:space:]]* ]] || die guest_user_unavailable
    valid_uuid "${VOLUME_ID}" || die invalid_volume_identity
    valid_uuid "${ATTACHMENT_ID}" || die invalid_attachment_identity
    valid_uuid "${SNAPSHOT_ID}" || die invalid_snapshot_identity
    [[ "${DEVICE}" =~ ^vd[b-z]$ ]] || die invalid_guest_device
    [[ "${STATE_ROOT}" = /* && "${STATE_ROOT}" != "/" && "${STATE_ROOT}" != "/tmp" && "${STATE_ROOT##*/}" == guest ]] \
        || die unsafe_state_root
    install -m 0700 -d "${STATE_ROOT}" "${ARTIFACT_DIR}"
    chmod 0711 "${STATE_ROOT}"
    BASE_IMAGE="${STATE_ROOT}/cirros-base.img"
    install -m 0644 "${BASE_IMAGE_SOURCE}" "${BASE_IMAGE}"
    chmod 0600 "${SSH_KEY}"
    local public_key
    public_key="$(ssh-keygen -y -f "${SSH_KEY}")" || die guest_key_invalid
    cat >"${USER_DATA}" <<EOF
#!/bin/sh
set -eu
umask 077
mkdir -p /home/cirros/.ssh
cat > /home/cirros/.ssh/authorized_keys <<'O3K_EPHEMERAL_KEY'
${public_key}
O3K_EPHEMERAL_KEY
chown -R cirros:cirros /home/cirros/.ssh
chmod 700 /home/cirros/.ssh
chmod 600 /home/cirros/.ssh/authorized_keys
EOF
    chmod 0600 "${USER_DATA}"
    cat >"${META_DATA}" <<EOF
{
  "instance-id": "o3k-ceph-${RUN_SLUG}",
  "local-hostname": "${DOMAIN}"
}
EOF
    chmod 0600 "${META_DATA}"
}

require_tools() {
    local tool
    for tool in virsh virt-install qemu-img genisoimage ssh ssh-keyscan ssh-keygen sha256sum python3 timeout; do
        command -v "${tool}" >/dev/null 2>&1 || die "missing_tool_${tool}"
    done
}

provider() {
    local action="$1"
    O3K_CEPH_PROVIDER_ACTION="${action}" \
    O3K_CEPH_POOL="${O3K_CEPH_POOL:?}" \
    O3K_CEPH_NAMESPACE="${O3K_CEPH_NAMESPACE:?}" \
    O3K_CEPH_PROVIDER_NAMESPACE="${O3K_CEPH_PROVIDER_NAMESPACE:?}" \
    O3K_CEPH_CONF_PATH="${O3K_CEPH_CONF_PATH:?}" \
    O3K_CEPH_CLIENT_ID="${O3K_CEPH_CLIENT_ID:?}" \
    O3K_CEPH_KEYRING_PATH="${O3K_CEPH_KEYRING_PATH:?}" \
    O3K_CEPH_CAPACITY_BYTES="${O3K_CEPH_CAPACITY_BYTES:?}" \
    O3K_CEPH_VOLUME_ID="${VOLUME_ID}" \
    O3K_CEPH_VOLUME_SIZE_BYTES="${O3K_CEPH_VOLUME_SIZE_BYTES:?}" \
    O3K_CEPH_PROJECT_ID="${PROJECT_ID}" \
    O3K_CEPH_ATTACHMENT_ID="${ATTACHMENT_ID}" \
    O3K_CEPH_SNAPSHOT_ID="${SNAPSHOT_ID}" \
    O3K_CEPH_HOST_ID="${HOSTNAME}" \
        cargo run --quiet --locked -p o3k-storage --example ceph-rbd-provider-volume
}

ceph_rbd() {
    rbd --conf "${O3K_CEPH_CONF_PATH:?}" --id "${O3K_CEPH_CLIENT_ID:?}" \
        --keyring "${O3K_CEPH_KEYRING_PATH:?}" "$@"
}

inventory_images() {
    local output="${STATE_ROOT}/rbd-images.json"
    ceph_rbd --pool "${O3K_CEPH_POOL:?}" --namespace "${O3K_CEPH_NAMESPACE:?}" ls --format json >"${output}"
    python3 - "${output}" <<'PY'
import hashlib, json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
encoded = json.dumps(sorted(value), separators=(",", ":")).encode()
print(hashlib.sha256(encoded).hexdigest())
PY
}

domain_mac() {
    sudo -n timeout --foreground 5s virsh -c qemu:///system domiflist "${CURRENT_DOMAIN}" 2>/dev/null \
        | awk '$5 ~ /^[[:xdigit:]][[:xdigit:]](:[[:xdigit:]][[:xdigit:]]){5}$/ {print tolower($5); exit}'
}

discover_domain_ip() {
    if [[ -n "${SSH_HOST_OVERRIDE}" ]]; then printf '%s\n' "${SSH_HOST_OVERRIDE}"; return; fi
    local source host mac
    for source in lease arp agent; do
        host="$(sudo -n timeout --foreground 5s virsh -c qemu:///system domifaddr "${CURRENT_DOMAIN}" --source "${source}" 2>/dev/null \
            | awk '/ipv4/ {sub(/\/.*/, "", $4); print $4; exit}' || true)"
        [[ -n "${host}" ]] && { printf '%s\n' "${host}"; return; }
    done
    mac="$(domain_mac || true)"
    [[ -n "${mac}" ]] || return 0
    sudo -n timeout --foreground 5s virsh -c qemu:///system net-dhcp-leases "${NETWORK}" 2>/dev/null \
        | awk -v wanted_mac="${mac}" 'tolower($3)==tolower(wanted_mac) && $4=="ipv4" {sub(/\/.*/, "", $5); print $5; exit}'
}

try_guest_ssh() {
    local candidate="$1"
    : >"${CANDIDATE_KNOWN_HOSTS}"
    timeout --foreground 3s bash -c ': >/dev/tcp/$1/22' _ "${candidate}" 2>/dev/null || return 1
    timeout --foreground 8s ssh-keyscan -T 3 -H "${candidate}" >"${CANDIDATE_KNOWN_HOSTS}" 2>/dev/null || return 1
    timeout --foreground 8s ssh -i "${SSH_KEY}" -o BatchMode=yes -o ConnectTimeout=3 \
        -o UserKnownHostsFile="${CANDIDATE_KNOWN_HOSTS}" -o StrictHostKeyChecking=yes \
        "${SSH_USER}@${candidate}" true >/dev/null 2>&1 || return 1
}

wait_for_guest() {
    local attempt candidate
    for attempt in $(seq 1 120); do
        candidate="$(discover_domain_ip || true)"
        if [[ -n "${candidate}" ]] && try_guest_ssh "${candidate}"; then
            ACTIVE_SSH_HOST="${candidate}"
            cat "${CANDIDATE_KNOWN_HOSTS}" >>"${KNOWN_HOSTS}"
            return 0
        fi
        if (( attempt % 10 == 0 )); then
            printf 'Ceph guest readiness: domain=%s attempt=%s candidate_ip=%s\n' "${CURRENT_DOMAIN}" "${attempt}" "${candidate:-none}"
        fi
        sleep 2
    done
    sudo -n timeout --foreground 5s virsh -c qemu:///system domstate "${CURRENT_DOMAIN}" 2>&1 | sed -n '1,8p'
    sudo -n timeout --foreground 5s virsh -c qemu:///system domiflist "${CURRENT_DOMAIN}" 2>&1 | sed -n '1,20p'
    die guest_ssh_unavailable
}

guest() {
    [[ -n "${ACTIVE_SSH_HOST}" ]] || die guest_ssh_not_authenticated
    timeout --foreground 60s ssh -i "${SSH_KEY}" -o BatchMode=yes -o ConnectTimeout=5 \
        -o UserKnownHostsFile="${KNOWN_HOSTS}" -o StrictHostKeyChecking=yes \
        "${SSH_USER}@${ACTIVE_SSH_HOST}" -- "$@"
}

reset_guest_connection_state() {
    ACTIVE_SSH_HOST=""
    rm -f -- "${CANDIDATE_KNOWN_HOSTS}" "${KNOWN_HOSTS}"
}

create_domain() {
    local domain="$1" overlay="$2" seed_iso="${STATE_ROOT}/${domain}.cidata.iso"
    qemu-img create -f qcow2 -F qcow2 -b "${BASE_IMAGE}" "${overlay}" >/dev/null
    genisoimage -quiet -output "${seed_iso}" -volid cidata -joliet -rock "${USER_DATA}" "${META_DATA}"
    chmod 0644 "${seed_iso}"
    sudo -n virt-install --connect qemu:///system --name "${domain}" --memory 2048 --vcpus 2 \
        --osinfo detect=on,require=off --disk "path=${overlay},format=qcow2,bus=virtio" \
        --network "network=${NETWORK},model=virtio" --rng /dev/urandom \
        --serial "file,path=${STATE_ROOT}/${domain}.serial.log" \
        --disk "path=${seed_iso},device=cdrom,readonly=on" --graphics none --import --noautoconsole >/dev/null
}

destroy_domain() {
    local domain="$1"
    sudo -n virsh -c qemu:///system destroy "${domain}" >/dev/null 2>&1 || true
    sudo -n virsh -c qemu:///system undefine "${domain}" --nvram >/dev/null 2>&1 || \
        sudo -n virsh -c qemu:///system undefine "${domain}" >/dev/null 2>&1 || true
}

attach_volume() {
    MAPPED_DEVICE="$(provider prepare)"
    [[ "${MAPPED_DEVICE}" = /dev/* ]] || die ceph_device_unavailable
    sudo -n virsh -c qemu:///system attach-disk "${1}" "${MAPPED_DEVICE}" "${DEVICE}" --live --config >/dev/null
}

detach_volume() {
    local domain="$1"
    sudo -n virsh -c qemu:///system detach-disk "${domain}" "${DEVICE}" --live --config >/dev/null
    provider terminate >/dev/null
    MAPPED_DEVICE=""
}

write_and_checksum() {
    local payload="o3k-p10-ceph-${RUN_ID}-${VOLUME_ID}"
    guest sudo -n mkdir -p /mnt/o3k-p10
    guest sudo -n mkfs.ext4 -F "/dev/${DEVICE}" >/dev/null
    guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
    printf '%s\n' "${payload}" | guest sudo -n tee /mnt/o3k-p10/payload >/dev/null
    guest sudo -n sync
    CHECKSUM="$(guest sudo -n sha256sum /mnt/o3k-p10/payload | awk '{print $1}')"
    [[ "${CHECKSUM}" =~ ^[0-9a-f]{64}$ ]] || die guest_checksum_unavailable
}

verify_checksum() {
    local observed
    observed="$(guest sudo -n sha256sum /mnt/o3k-p10/payload | awk '{print $1}')"
    [[ "${observed}" == "${CHECKSUM}" ]] || die guest_payload_changed
}

cleanup_resources() {
    set +e
    destroy_domain "${DOMAIN}"
    destroy_domain "${SECOND_DOMAIN}"
    if [[ -n "${MAPPED_DEVICE}" ]]; then provider terminate >/dev/null 2>&1 || true; fi
    rm -f -- "${OVERLAY}" "${SECOND_OVERLAY}" "${BASE_IMAGE}" "${KNOWN_HOSTS}" \
        "${CANDIDATE_KNOWN_HOSTS}" "${USER_DATA}" "${META_DATA}" \
        "${STATE_ROOT}/${DOMAIN}.serial.log" "${STATE_ROOT}/${SECOND_DOMAIN}.serial.log" \
        "${STATE_ROOT}/${DOMAIN}.cidata.iso" "${STATE_ROOT}/${SECOND_DOMAIN}.cidata.iso" \
        "${STATE_ROOT}/rbd-images.json"
    if [[ -d "${STATE_ROOT}" ]] && ! rmdir -- "${STATE_ROOT}"; then
        RESULT_REASON=guest_state_cleanup_failed
        CLEANUP_FAILED=true
    fi
}

write_result() {
    python3 - "${ARTIFACT}" "${RESULT_STATUS}" "${RESULT_REASON}" "${CHECKSUM}" <<'PY'
import json, sys, time
path, status, reason, checksum = sys.argv[1:]
json.dump({
    "artifact_type": "ceph-rbd-real-guest", "schema_version": 1,
    "status": status, "reason": reason, "redacted": True,
    "snapshot_consistency": "crash_consistent",
    "guest_payload_checksum_present": bool(checksum),
    "owned_backend_leaks": 0 if status == "passed" else None,
    "owned_attachment_leaks": 0 if status == "passed" else None,
    "owned_inconsistencies": 0 if status == "passed" else None,
    "foreign_mutations": 0 if status == "passed" else None,
    "finished_at": int(time.time()),
}, open(path, "w", encoding="utf-8"), indent=2, sort_keys=True)
open(path, "a", encoding="utf-8").write("\n")
PY
}

on_exit() {
    local status="$?"
    cleanup_resources
    [[ "${CLEANUP_FAILED}" == true ]] && status=1
    if [[ "${status}" -eq 0 && "${RESULT_REASON}" == passed ]]; then RESULT_STATUS=passed; else RESULT_STATUS=failed; fi
    write_result
    exit "${status}"
}

install -m 0755 -d "${ARTIFACT_DIR}"
if [[ "${1:-}" == cleanup ]]; then
    cleanup_resources
    if [[ ! -f "${ARTIFACT}" ]]; then RESULT_STATUS=failed; RESULT_REASON=guest_gate_interrupted; write_result; fi
    [[ "${CLEANUP_FAILED}" == false ]]
    exit
fi

trap on_exit EXIT
require_tools
require_inputs
FOREIGN_BEFORE="$(inventory_images)"
provider create >/dev/null
create_domain "${DOMAIN}" "${OVERLAY}"
wait_for_guest
attach_volume "${DOMAIN}"
write_and_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${DOMAIN}"
sudo -n virsh -c qemu:///system destroy "${DOMAIN}" >/dev/null
sudo -n virsh -c qemu:///system start "${DOMAIN}" >/dev/null
reset_guest_connection_state
wait_for_guest
attach_volume "${DOMAIN}"
guest sudo -n mkdir -p /mnt/o3k-p10
guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
verify_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${DOMAIN}"
provider snapshot-create >/dev/null
destroy_domain "${DOMAIN}"
create_domain "${SECOND_DOMAIN}" "${SECOND_OVERLAY}"
CURRENT_DOMAIN="${SECOND_DOMAIN}"
reset_guest_connection_state
wait_for_guest
attach_volume "${SECOND_DOMAIN}"
guest sudo -n mkdir -p /mnt/o3k-p10
guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
verify_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${SECOND_DOMAIN}"
provider snapshot-delete >/dev/null
provider delete >/dev/null
FOREIGN_AFTER="$(inventory_images)"
[[ "${FOREIGN_BEFORE}" == "${FOREIGN_AFTER}" ]] || die foreign_ceph_mutation
RESULT_REASON=passed
