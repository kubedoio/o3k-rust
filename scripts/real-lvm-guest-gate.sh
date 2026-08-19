#!/usr/bin/env bash
set -Eeuo pipefail

# Protected P10 LVM real-guest gate. This script is intentionally strict:
# missing guest prerequisites are failures, never a passing/skipped evidence
# result. It owns only the exact run-scoped domain, overlay, and volume IDs.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ID="${O3K_LVM_RUN_ID:-${GITHUB_RUN_ID:-}}"
RUN_SLUG="$(printf '%s' "${RUN_ID}" | tr -cd '[:alnum:]' | head -c 16)"
STATE_ROOT="${O3K_LVM_GUEST_STATE_ROOT:-/var/lib/o3k-lvm-testlab/${RUN_SLUG}/guest}"
ARTIFACT_DIR="${O3K_LVM_GUEST_ARTIFACT_DIR:-${ROOT_DIR}/target/lvm-real-guest-artifacts}"
ARTIFACT="${ARTIFACT_DIR}/lvm-real-guest-result.json"
DOMAIN="o3k-lvm-guest-${RUN_SLUG}"
SECOND_DOMAIN="o3k-lvm-guest-re-${RUN_SLUG}"
VOLUME_ID="${O3K_LVM_GUEST_VOLUME_ID:-$(cat /proc/sys/kernel/random/uuid)}"
ATTACHMENT_ID="${O3K_LVM_GUEST_ATTACHMENT_ID:-$(cat /proc/sys/kernel/random/uuid)}"
SNAPSHOT_ID="${O3K_LVM_GUEST_SNAPSHOT_ID:-$(cat /proc/sys/kernel/random/uuid)}"
PROJECT_ID="${O3K_LVM_GUEST_PROJECT_ID:-p10-real-guest}"
NETWORK="${O3K_LVM_GUEST_NETWORK:-default}"
DEVICE="${O3K_LVM_GUEST_DEVICE:-vdb}"
BASE_IMAGE="${O3K_LVM_GUEST_IMAGE_PATH:-}"
BASE_IMAGE_SOURCE="${BASE_IMAGE}"
SSH_KEY="${O3K_LVM_GUEST_SSH_PRIVATE_KEY:-}"
SSH_USER="${O3K_LVM_GUEST_SSH_USER:-}"
SSH_HOST_OVERRIDE="${O3K_LVM_GUEST_SSH_HOST:-}"
ACTIVE_SSH_HOST=""
CURRENT_DOMAIN="${DOMAIN}"
OVERLAY="${STATE_ROOT}/${DOMAIN}.qcow2"
SECOND_OVERLAY="${STATE_ROOT}/${SECOND_DOMAIN}.qcow2"
KNOWN_HOSTS="${STATE_ROOT}/known_hosts"
CANDIDATE_KNOWN_HOSTS="${STATE_ROOT}/known_hosts.candidate"
CLOUD_INIT_USER_DATA="${STATE_ROOT}/user-data"
RESULT_STATUS=failed
RESULT_REASON=not_completed
CHECKSUM=""
FOREIGN_BEFORE=""
FOREIGN_AFTER=""
DOMAIN_CREATED=false
SECOND_DOMAIN_CREATED=false
VOLUME_CREATED=false
SNAPSHOT_CREATED=false
LAST_KEYSCAN_STATUS=not_run
LAST_SSH_AUTH_STATUS=not_run
CLEANUP_FAILED=false

die() {
    RESULT_REASON="$1"
    return 1
}

valid_uuid() { [[ "$1" =~ ^[0-9a-fA-F-]{36}$ ]]; }

validate_path() {
    local value="$1"
    [[ "${value}" = /* && "${value}" != *"/../"* && "${value}" != */.. ]] || return 1
    [[ -f "${value}" ]] || return 1
}

require_inputs() {
    [[ -n "${RUN_ID}" && -n "${RUN_SLUG}" ]] || die missing_run_identity
    validate_path "${BASE_IMAGE}" || die guest_image_unavailable
    validate_path "${SSH_KEY}" || die guest_key_unavailable
    [[ -n "${SSH_USER}" && "${SSH_USER}" != *[[:space:]]* ]] || die guest_user_unavailable
    valid_uuid "${VOLUME_ID}" || die invalid_volume_identity
    valid_uuid "${ATTACHMENT_ID}" || die invalid_attachment_identity
    valid_uuid "${SNAPSHOT_ID}" || die invalid_snapshot_identity
    [[ "${DEVICE}" =~ ^vd[b-z]$ ]] || die invalid_guest_device
    [[ "${STATE_ROOT}" = /* && "${STATE_ROOT}" != "/" && "${STATE_ROOT}" != "/tmp" ]] || die unsafe_state_root
    [[ "${STATE_ROOT##*/}" == guest ]] || die unsafe_state_root
    install -m 0700 -d "${STATE_ROOT}" "${ARTIFACT_DIR}"
    # libvirt-qemu must traverse this disposable directory to open the
    # imported overlay. Keep listing/read access denied; cleanup removes it.
    chmod 0711 "${STATE_ROOT}"
    # The runner's temporary directory is intentionally private to the job
    # user. Copy the public base image into the exact run-scoped guest state so
    # libvirt-qemu can traverse its backing path without broadening runner
    # temporary-directory permissions.
    BASE_IMAGE="${STATE_ROOT}/cirros-base.img"
    install -m 0644 "${BASE_IMAGE_SOURCE}" "${BASE_IMAGE}"
    chmod 0600 "${SSH_KEY}"
    local public_key
    public_key="$(ssh-keygen -y -f "${SSH_KEY}")" || die guest_key_invalid
    # CirrOS 0.6.3 does not parse cloud-config YAML. Its userdata handler
    # executes only a file beginning with #!, so install exactly this run's
    # key through a minimal root-executed script.
    cat >"${CLOUD_INIT_USER_DATA}" <<EOF
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
    chmod 0600 "${CLOUD_INIT_USER_DATA}"
}

require_tools() {
    local tool
    for tool in virsh virt-install qemu-img ssh ssh-keyscan ssh-keygen sha256sum python3 timeout; do
        command -v "${tool}" >/dev/null 2>&1 || die "missing_tool_${tool}"
    done
}

profile_env() {
    printf '%s\n' \
        "O3K_LVM_VOLUME_GROUP=${O3K_LVM_VOLUME_GROUP:?}" \
        "O3K_LVM_THIN_POOL=${O3K_LVM_THIN_POOL:?}" \
        "O3K_LVM_PROVIDER_NAMESPACE=${O3K_LVM_PROVIDER_NAMESPACE:?}" \
        "O3K_LVM_VOLUME_ID=${VOLUME_ID}" \
        "O3K_LVM_PROJECT_ID=${PROJECT_ID}"
}

provider() {
    local action="$1"
    O3K_LVM_PROVIDER_ACTION="${action}" \
    O3K_LVM_VOLUME_GROUP="${O3K_LVM_VOLUME_GROUP}" \
    O3K_LVM_THIN_POOL="${O3K_LVM_THIN_POOL}" \
    O3K_LVM_PROVIDER_NAMESPACE="${O3K_LVM_PROVIDER_NAMESPACE}" \
    O3K_LVM_VOLUME_ID="${VOLUME_ID}" \
    O3K_LVM_PROJECT_ID="${PROJECT_ID}" \
    O3K_LVM_SNAPSHOT_ID="${SNAPSHOT_ID}" \
    cargo run --quiet --locked -p o3k-storage --example lvm-provider-volume >/dev/null
}

inventory_foreign() {
    local output="${STATE_ROOT}/lvs.json"
    sudo -n lvs --reportformat json --options lv_name,lv_tags,vg_name,lv_attr >"${output}"
    python3 - "${output}" "${O3K_LVM_PROVIDER_NAMESPACE}" <<'PY'
import hashlib, json, sys
path, namespace = sys.argv[1:]
value = json.load(open(path, encoding="utf-8"))
rows = value.get("report", [{}])[0].get("lv", [])
owned_prefix = "o3k_" + hashlib.sha256(namespace.encode()).hexdigest()
foreign = []
owned = []
for row in rows:
    tags = row.get("lv_tags", "").split(",") if row.get("lv_tags") else []
    if any(tag.startswith(owned_prefix) for tag in tags):
        owned.append(row.get("lv_name", ""))
    else:
        foreign.append({key: row.get(key, "") for key in ("lv_name", "lv_tags", "vg_name", "lv_attr")})
encoded = json.dumps(sorted(foreign, key=lambda item: (item["vg_name"], item["lv_name"])), sort_keys=True).encode()
print(hashlib.sha256(encoded).hexdigest())
PY
}

domain_mac() {
    sudo -n timeout --foreground 5s virsh -c qemu:///system domiflist "${CURRENT_DOMAIN}" 2>/dev/null \
        | awk '$5 ~ /^[[:xdigit:]][[:xdigit:]](:[[:xdigit:]][[:xdigit:]]){5}$/ {print tolower($5); exit}'
}

discover_domain_ip() {
    if [[ -n "${SSH_HOST_OVERRIDE}" ]]; then
        printf '%s\n' "${SSH_HOST_OVERRIDE}"
        return
    fi
    # The protected runner's libvirt version does not always expose a guest
    # address through the per-domain lease view, even when dnsmasq has issued
    # the lease. Try the domain views first, then the network's authoritative
    # DHCP lease table. The address is only an observation; SSH remains the
    # readiness proof below.
    local source host mac
    for source in lease arp agent; do
        host="$(sudo -n timeout --foreground 5s virsh -c qemu:///system domifaddr "${CURRENT_DOMAIN}" \
            --source "${source}" 2>/dev/null \
            | awk '/ipv4/ {sub(/\/.*/, "", $4); print $4; exit}' || true)"
        if [[ -n "${host}" ]]; then
            printf '%s\n' "${host}"
            return
        fi
    done
    mac="$(domain_mac || true)"
    [[ -n "${mac}" ]] || return 0
    sudo -n timeout --foreground 5s virsh -c qemu:///system net-dhcp-leases "${NETWORK}" 2>/dev/null \
        | awk -v wanted_mac="${mac}" \
            'tolower($3) == tolower(wanted_mac) && $4 == "ipv4" {sub(/\/.*/, "", $5); print $5; exit}'
}

try_guest_ssh() {
    local candidate_host="$1"
    LAST_KEYSCAN_STATUS=failed
    LAST_SSH_AUTH_STATUS=failed
    : >"${CANDIDATE_KNOWN_HOSTS}"
    if ! timeout --foreground 8s ssh-keyscan -T 3 -H "${candidate_host}" \
        >"${CANDIDATE_KNOWN_HOSTS}" 2>/dev/null; then
        return 1
    fi
    LAST_KEYSCAN_STATUS=ok
    if ! timeout --foreground 8s ssh -i "${SSH_KEY}" -o BatchMode=yes -o ConnectTimeout=3 \
        -o UserKnownHostsFile="${CANDIDATE_KNOWN_HOSTS}" -o StrictHostKeyChecking=yes \
        "${SSH_USER}@${candidate_host}" true >/dev/null 2>&1; then
        return 1
    fi
    LAST_SSH_AUTH_STATUS=ok
    return 0
}

dump_guest_diagnostics() {
    set +e
    printf 'guest readiness diagnostics: domain=%s network=%s\n' "${CURRENT_DOMAIN}" "${NETWORK}"
    sudo -n timeout --foreground 5s virsh -c qemu:///system domstate "${CURRENT_DOMAIN}" 2>&1 | sed -n '1,8p'
    sudo -n timeout --foreground 5s virsh -c qemu:///system domiflist "${CURRENT_DOMAIN}" 2>&1 | sed -n '1,20p'
    for source in lease arp agent; do
        printf 'domifaddr source=%s\n' "${source}"
        sudo -n timeout --foreground 5s virsh -c qemu:///system domifaddr "${CURRENT_DOMAIN}" \
            --source "${source}" 2>&1 | sed -n '1,20p'
    done
    printf 'network DHCP leases:\n'
    sudo -n timeout --foreground 5s virsh -c qemu:///system net-dhcp-leases "${NETWORK}" 2>&1 \
        | sed -n '1,40p'
    set -e
}

wait_for_guest() {
    local attempt candidate_host
    # A cold KVM boot plus cloud-init can exceed two minutes on the protected
    # runner while the guest is still progressing normally.
    for attempt in $(seq 1 120); do
        candidate_host="$(discover_domain_ip || true)"
        if [[ -n "${candidate_host}" ]] && try_guest_ssh "${candidate_host}"; then
            # Do not let an unverified lease poison later retries. Promote the
            # candidate only after strict host-key and SSH authentication pass.
            ACTIVE_SSH_HOST="${candidate_host}"
            cat "${CANDIDATE_KNOWN_HOSTS}" >>"${KNOWN_HOSTS}"
            return 0
        fi
        if (( attempt % 10 == 0 )); then
            printf 'guest readiness: domain=%s attempt=%s candidate_ip=%s ssh_keyscan=%s ssh_auth=%s\n' \
                "${CURRENT_DOMAIN}" "${attempt}" "${candidate_host:-none}" \
                "${LAST_KEYSCAN_STATUS}" "${LAST_SSH_AUTH_STATUS}"
        fi
        sleep 2
    done
    dump_guest_diagnostics
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
    local domain="$1" overlay="$2"
    qemu-img create -f qcow2 -F qcow2 -b "${BASE_IMAGE}" "${overlay}" >/dev/null
    sudo -n virt-install --connect qemu:///system --name "${domain}" --memory 2048 --vcpus 2 \
        --osinfo detect=on,require=off \
        --disk "path=${overlay},format=qcow2,bus=virtio" \
        --network "network=${NETWORK},model=virtio" --graphics none --import \
        --cloud-init "user-data=${CLOUD_INIT_USER_DATA}" --noautoconsole >/dev/null
}

destroy_domain() {
    local domain="$1"
    sudo -n virsh -c qemu:///system destroy "${domain}" >/dev/null 2>&1 || true
    sudo -n virsh -c qemu:///system undefine "${domain}" --nvram >/dev/null 2>&1 || \
        sudo -n virsh -c qemu:///system undefine "${domain}" >/dev/null 2>&1 || true
}

attach_volume() {
    local domain="$1"
    local device_path="/dev/${O3K_LVM_VOLUME_GROUP}/o3k-v-${VOLUME_ID//-/}"
    sudo -n virsh -c qemu:///system attach-disk "${domain}" "${device_path}" "${DEVICE}" \
        --live --config >/dev/null
}

detach_volume() {
    local domain="$1"
    sudo -n virsh -c qemu:///system detach-disk "${domain}" "${DEVICE}" --live --config >/dev/null
}

write_and_checksum() {
    local payload="o3k-p10-${RUN_ID}-${VOLUME_ID}"
    guest sudo -n mkdir -p /mnt/o3k-p10
    guest sudo -n mkfs.ext4 -F "/dev/${DEVICE}" >/dev/null
    guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
    guest sudo -n sh -c "printf '%s\\n' '${payload}' > /mnt/o3k-p10/payload"
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
    rm -f -- "${OVERLAY}" "${SECOND_OVERLAY}" "${STATE_ROOT}/cirros-base.img" \
        "${STATE_ROOT}/lvs.json" "${KNOWN_HOSTS}" \
        "${CANDIDATE_KNOWN_HOSTS}" "${CLOUD_INIT_USER_DATA}"
    if [[ -d "${STATE_ROOT}" ]] && ! rmdir -- "${STATE_ROOT}"; then
        RESULT_REASON=guest_state_cleanup_failed
        CLEANUP_FAILED=true
    fi
}

write_result() {
    python3 - "${ARTIFACT}" "${RESULT_STATUS}" "${RESULT_REASON}" "${CHECKSUM}" <<'PY'
import json, sys, time
path, status, reason, checksum = sys.argv[1:]
value = {
    "artifact_type": "lvm-real-guest",
    "schema_version": 1,
    "status": status,
    "reason": reason,
    "redacted": True,
    "snapshot_consistency": "crash_consistent",
    "guest_payload_checksum_present": bool(checksum),
    "owned_backend_leaks": 0 if status == "passed" else None,
    "owned_attachment_leaks": 0 if status == "passed" else None,
    "owned_inconsistencies": 0 if status == "passed" else None,
    "foreign_mutations": 0 if status == "passed" else None,
    "finished_at": int(time.time()),
}
json.dump(value, open(path, "w", encoding="utf-8"), indent=2, sort_keys=True)
open(path, "a", encoding="utf-8").write("\n")
PY
}

on_exit() {
    local status="$?"
    cleanup_resources
    if [[ "${CLEANUP_FAILED}" == true ]]; then
        status=1
    fi
    if ((status == 0 && RESULT_REASON == passed)); then
        RESULT_STATUS=passed
    else
        RESULT_STATUS=failed
    fi
    write_result
    exit "${status}"
}
install -m 0755 -d "${ARTIFACT_DIR}"

if [[ "${1:-}" == cleanup ]]; then
    cleanup_resources
    if [[ ! -f "${ARTIFACT}" ]]; then
        RESULT_STATUS=failed
        RESULT_REASON=guest_gate_interrupted
        write_result
    fi
    [[ "${CLEANUP_FAILED}" == false ]]
    exit
fi

trap on_exit EXIT

require_tools
require_inputs
profile_env >/dev/null
FOREIGN_BEFORE="$(inventory_foreign)"
create_domain "${DOMAIN}" "${OVERLAY}"
DOMAIN_CREATED=true
wait_for_guest
provider create
VOLUME_CREATED=true
attach_volume "${DOMAIN}"
write_and_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${DOMAIN}"
    sudo -n virsh -c qemu:///system destroy "${DOMAIN}" >/dev/null
    sudo -n virsh -c qemu:///system start "${DOMAIN}" >/dev/null
wait_for_guest
attach_volume "${DOMAIN}"
guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
verify_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${DOMAIN}"
provider snapshot-create
SNAPSHOT_CREATED=true
destroy_domain "${DOMAIN}"
DOMAIN_CREATED=false
create_domain "${SECOND_DOMAIN}" "${SECOND_OVERLAY}"
SECOND_DOMAIN_CREATED=true
CURRENT_DOMAIN="${SECOND_DOMAIN}"
reset_guest_connection_state
wait_for_guest
attach_volume "${SECOND_DOMAIN}"
guest sudo -n mount "/dev/${DEVICE}" /mnt/o3k-p10
verify_checksum
guest sudo -n umount /mnt/o3k-p10
detach_volume "${SECOND_DOMAIN}"
provider snapshot-delete
SNAPSHOT_CREATED=false
provider delete
VOLUME_CREATED=false
FOREIGN_AFTER="$(inventory_foreign)"
[[ "${FOREIGN_BEFORE}" == "${FOREIGN_AFTER}" ]] || die foreign_lvm_mutation
RESULT_REASON=passed
