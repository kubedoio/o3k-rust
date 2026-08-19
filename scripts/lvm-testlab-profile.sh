#!/usr/bin/env bash
set -Eeuo pipefail

# Provision or tear down only an exact, disposable loop-backed LVM profile.
# This helper never searches for an arbitrary VG to reuse. Operator-provided
# dedicated VGs are validated by the Rust provider and are intentionally
# outside this disposable lifecycle.

usage() {
    echo "usage: $0 provision|cleanup|verify" >&2
    exit 2
}

ACTION="${1:-}"
case "${ACTION}" in
    provision|cleanup|verify) ;;
    *) usage ;;
esac

STATE_BASE="${O3K_LVM_STATE_BASE:-/var/lib/o3k-lvm-testlab}"
RUN_ID="${O3K_LVM_RUN_ID:-${GITHUB_RUN_ID:-local-$(date +%s)}}"
RUN_SLUG="$(printf '%s' "${RUN_ID}" | tr -cd '[:alnum:]' | head -c 16)"
[[ -n "${RUN_SLUG}" ]] || RUN_SLUG=local
STATE_ROOT="${O3K_LVM_STATE_ROOT:-${STATE_BASE}/${RUN_SLUG}}"
NAMESPACE="${O3K_LVM_PROVIDER_NAMESPACE:-testlab-${RUN_SLUG}}"
IMAGE_BYTES="${O3K_LVM_IMAGE_BYTES:-2147483648}"
THIN_POOL_BYTES="${O3K_LVM_THIN_POOL_BYTES:-1610612736}"
VG_NAME="${O3K_LVM_VG_NAME:-o3k-lvm-${RUN_SLUG}}"
THIN_POOL="${O3K_LVM_THIN_POOL:-o3k-thin-${RUN_SLUG}}"
STATE_FILE="${STATE_ROOT}/profile.json"

die() {
    echo "lvm-testlab-profile: $*" >&2
    exit 1
}

valid_name() {
    [[ "$1" =~ ^[A-Za-z0-9_.-]{1,128}$ ]]
}

require_tools() {
    local tool
    for tool in sha256sum truncate losetup pvcreate pvremove vgcreate vgremove vgchange vgs lvs lvcreate; do
        command -v "${tool}" >/dev/null 2>&1 || die "required tool is unavailable: ${tool}"
    done
}

validate_inputs() {
    valid_name "${RUN_SLUG}" || die "invalid run slug"
    valid_name "${NAMESPACE}" || die "invalid provider namespace"
    valid_name "${VG_NAME}" || die "invalid volume-group name"
    valid_name "${THIN_POOL}" || die "invalid thin-pool name"
    [[ "${IMAGE_BYTES}" =~ ^[1-9][0-9]*$ ]] || die "invalid image size"
    [[ "${THIN_POOL_BYTES}" =~ ^[1-9][0-9]*$ ]] || die "invalid thin-pool size"
    (( THIN_POOL_BYTES < IMAGE_BYTES )) || die "thin pool must fit inside the disposable image"
    [[ "${STATE_ROOT}" = /* && "${STATE_ROOT}" != "/" && "${STATE_ROOT}" != "/tmp" && "${STATE_ROOT}" != "/var" && "${STATE_ROOT}" != "/var/lib" ]] \
        || die "unsafe state root"
    [[ "${STATE_ROOT}" != *"/../"* && "${STATE_ROOT}" != */.. ]] || die "unsafe state root"
    [[ "${STATE_ROOT##*/}" == "${RUN_SLUG}" ]] || die "state root must end in the exact run slug"
}

scope_hash() {
    printf '%s' "${NAMESPACE}" | sha256sum | awk '{print $1}'
}

write_state() {
    local loop_device="$1" hash="$2"
    mkdir -p -- "${STATE_ROOT}"
    python3 - "${STATE_FILE}" "${RUN_ID}" "${VG_NAME}" "${THIN_POOL}" "${NAMESPACE}" "${loop_device}" "${hash}" <<'PY'
import json, os, sys, tempfile

path, run_id, vg, pool, namespace, loop_device, scope_hash = sys.argv[1:]
value = {
    "artifact_type": "disposable-lvm-profile",
    "schema_version": 1,
    "status": "provisioned",
    "redacted": True,
    "run_id": run_id,
    "volume_group": vg,
    "thin_pool": pool,
    "provider_namespace": namespace,
    "loop_device": loop_device,
    "scope_hash": scope_hash,
}
directory = os.path.dirname(path)
fd, temporary = tempfile.mkstemp(prefix=".profile.", dir=directory, text=True)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
except BaseException:
    try:
        os.unlink(temporary)
    except FileNotFoundError:
        pass
    raise
PY
}

read_state() {
    [[ -f "${STATE_FILE}" ]] || die "profile state is unavailable"
    python3 - "${STATE_FILE}" <<'PY'
import json, re, sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
required = ("run_id", "volume_group", "thin_pool", "provider_namespace", "loop_device", "scope_hash")
if value.get("artifact_type") != "disposable-lvm-profile" or value.get("status") != "provisioned":
    raise SystemExit("invalid profile state")
for key in required:
    if not isinstance(value.get(key), str) or not value[key]:
        raise SystemExit("invalid profile state field")
if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value["volume_group"]):
    raise SystemExit("unsafe volume-group name in profile state")
if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value["thin_pool"]):
    raise SystemExit("unsafe thin-pool name in profile state")
if not re.fullmatch(r"o3k-[A-Za-z0-9_.-]+", value["loop_device"].split("/")[-1]):
    # losetup names are normally /dev/loopN; accept only the device path and
    # validate the exact device through losetup below.
    if not re.fullmatch(r"/dev/loop[0-9]+", value["loop_device"]):
        raise SystemExit("unsafe loop device in profile state")
print(value["run_id"])
print(value["volume_group"])
print(value["thin_pool"])
print(value["provider_namespace"])
print(value["loop_device"])
print(value["scope_hash"])
PY
}

provision() {
    require_tools
    validate_inputs
    [[ ! -e "${STATE_ROOT}" ]] || die "run-owned state already exists: ${STATE_ROOT}"
    mkdir -p -- "${STATE_ROOT}"
    local image_file="${STATE_ROOT}/${VG_NAME}.img"
    local loop_device=""
    local hash
    hash="$(scope_hash)"
    trap 'if [[ -n "${loop_device}" ]]; then losetup -d "${loop_device}" 2>/dev/null || true; fi; rm -rf -- "${STATE_ROOT}"' ERR
    truncate -s "${IMAGE_BYTES}" "${image_file}"
    loop_device="$(losetup --find --show "${image_file}")"
    [[ "${loop_device}" =~ ^/dev/loop[0-9]+$ ]] || die "losetup returned an unexpected device"
    if vgs "${VG_NAME}" >/dev/null 2>&1; then
        die "refusing to adopt pre-existing VG ${VG_NAME}"
    fi
    pvcreate --force --yes "${loop_device}" >/dev/null
    vgcreate --addtag "o3k_storage_${hash}" "${VG_NAME}" "${loop_device}" >/dev/null
    lvcreate --type thin-pool --yes --name "${THIN_POOL}" --size "${THIN_POOL_BYTES}B" \
        --addtag "o3k_pool_${hash}" "${VG_NAME}" >/dev/null
    write_state "${loop_device}" "${hash}"
    trap - ERR
    echo "provisioned disposable LVM profile: ${STATE_FILE}"
}

verify() {
    require_tools
    validate_inputs
    read_state >/dev/null
    local hash
    hash="$(scope_hash)"
    vgs --noheadings --options vg_name,vg_tags "${VG_NAME}" | grep -Fq "o3k_storage_${hash}" \
        || die "configured VG is not marked as this O3K profile"
    lvs --noheadings --options lv_name,lv_tags,vg_name "${VG_NAME}/${THIN_POOL}" \
        | grep -Fq "o3k_pool_${hash}" || die "configured thin pool is not marked as this O3K profile"
    echo "verified disposable LVM profile: ${STATE_FILE}"
}

cleanup() {
    require_tools
    local state_lines run_id vg pool namespace loop_device stored_hash expected_hash
    mapfile -t state_lines < <(read_state)
    ((${#state_lines[@]} == 6)) || die "invalid profile state"
    run_id="${state_lines[0]}"; vg="${state_lines[1]}"; pool="${state_lines[2]}"
    namespace="${state_lines[3]}"; loop_device="${state_lines[4]}"; stored_hash="${state_lines[5]}"
    expected_hash="$(printf '%s' "${namespace}" | sha256sum | awk '{print $1}')"
    [[ "${stored_hash}" == "${expected_hash}" ]] || die "profile namespace fingerprint mismatch"
    [[ "${run_id}" == "${RUN_ID}" ]] || die "profile run identity mismatch"
    [[ "${loop_device}" =~ ^/dev/loop[0-9]+$ ]] || die "unsafe loop device in profile state"
    vgs --noheadings --options vg_name,vg_tags "${vg}" | grep -Fq "o3k_storage_${stored_hash}" \
        || die "refusing to remove an unmarked or foreign VG"
    lvs --noheadings --options lv_name,lv_tags,vg_name "${vg}/${pool}" \
        | grep -Fq "o3k_pool_${stored_hash}" || die "refusing to remove an unmarked thin pool"
    vgchange --yes -an "${vg}" >/dev/null
    vgremove --yes "${vg}" >/dev/null
    pvremove --force --yes "${loop_device}" >/dev/null
    losetup -d "${loop_device}"
    rm -f -- "${STATE_FILE}" "${STATE_ROOT}/${vg}.img"
    rmdir -- "${STATE_ROOT}"
    echo "cleaned disposable LVM profile for ${run_id}"
}

case "${ACTION}" in
    provision) provision ;;
    verify) verify ;;
    cleanup) cleanup ;;
esac
