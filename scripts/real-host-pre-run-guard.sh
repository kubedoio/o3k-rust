#!/usr/bin/env bash
set -Eeuo pipefail

# Run before host-mutating TestLab commands. Never dump the environment.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-host-workflow-artifacts}"
RESULT_PATH="${ARTIFACT_DIR}/real-host-workflow-result.json"
EXPECTED_REPOSITORY="kubedoio/o3k-rust"
KVM_PATH="${O3K_REAL_HOST_KVM_PATH:-/dev/kvm}"
INVENTORY_PATH="${ARTIFACT_DIR}/real-host-owned-inventory-baseline.json"
CAPABILITY_PATH="${O3K_REAL_HOST_CAPABILITY_OUTPUT:-${ARTIFACT_DIR}/runner-capabilities.json}"
mkdir -p "${ARTIFACT_DIR}"

blocked_reason=
if [[ "${GITHUB_REPOSITORY:-}" != "${EXPECTED_REPOSITORY}" ]]; then
    blocked_reason=non_canonical_repository
elif [[ "${GITHUB_EVENT_NAME:-}" != workflow_dispatch ]]; then
    blocked_reason=untrusted_event_context
elif [[ -n "${GITHUB_HEAD_REF:-}" || -n "${GITHUB_BASE_REF:-}" ]]; then
    blocked_reason=untrusted_fork_context
fi

if [[ -n "${blocked_reason}" ]]; then
    python3 - "${RESULT_PATH}" "${blocked_reason}" <<'PY'
import json, sys, time
path, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-host-workflow-result", "status": "blocked",
               "reason": reason, "redacted": True, "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-host workflow guard blocked: ${blocked_reason}" >&2
    exit 1
fi

capability_status=unavailable
if [[ -r "${CAPABILITY_PATH}" ]]; then
    capability_status="$(python3 - "${CAPABILITY_PATH}" <<'PY'
import json, sys
try:
    value = json.load(open(sys.argv[1], encoding="utf-8"))
except (OSError, json.JSONDecodeError):
    print("unavailable")
else:
    print(value.get("status", "unavailable") if value.get("redacted") is True else "unavailable")
PY
)"
fi
if [[ "${capability_status}" != passed ]]; then
    capability_result_status=skipped
    capability_reason=capability_probe_unavailable
    capability_exit=0
    if [[ "${capability_status}" == failed ]]; then
        capability_result_status=blocked
        capability_reason=capability_probe_failed
        capability_exit=1
    fi
    python3 - "${RESULT_PATH}" "${capability_result_status}" "${capability_reason}" <<'PY'
import json, sys, time
path, status, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-host-workflow-result", "status": status,
               "reason": reason, "redacted": True, "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-host capability probe did not pass; lifecycle skipped" >&2
    exit "${capability_exit}"
fi

declare -A tools=(
    [virsh]=0 [ip]=0 [qemu-img]=0 [openstack]=0 [curl]=0 [python3]=0
)
missing_tools=()
for command in "${!tools[@]}"; do
    if command -v "${command}" >/dev/null 2>&1; then
        tools["${command}"]=1
    else
        missing_tools+=("${command}")
    fi
done

kvm_present=false
kvm_readable=false
[[ -e "${KVM_PATH}" ]] && kvm_present=true
[[ -r "${KVM_PATH}" ]] && kvm_readable=true
libvirt_uri_available=false
if [[ "${tools[virsh]}" == 1 ]] && virsh -c qemu:///system uri >/dev/null 2>&1; then
    libvirt_uri_available=true
fi

os_id=unknown
os_version=unknown
if [[ -r /etc/os-release ]]; then
    os_id="$(sed -n 's/^ID=//p' /etc/os-release | head -n 1 | tr -d '"' | tr -cd '[:alnum:]._+-')"
    os_version="$(sed -n 's/^VERSION_ID=//p' /etc/os-release | head -n 1 | tr -d '"' | tr -cd '[:alnum:]._+-')"
fi

ready=true
reason=ready
if ((${#missing_tools[@]} > 0)); then
    ready=false
    reason=missing_required_tool
elif [[ "${kvm_present}" != true || "${kvm_readable}" != true ]]; then
    ready=false
    reason=kvm_unavailable
elif [[ "${libvirt_uri_available}" != true ]]; then
    ready=false
    reason=libvirt_system_uri_unavailable
fi

python3 - "${RESULT_PATH}" "${ready}" "${reason}" "${os_id}" "${os_version}" \
    "$(uname -srm)" "${kvm_present}" "${kvm_readable}" "${libvirt_uri_available}" \
    "${tools[virsh]}" "${tools[ip]}" "${tools[qemu-img]}" "${tools[openstack]}" \
    "${tools[curl]}" "${tools[python3]}" <<'PY'
import json, sys, time
(path, ready, reason, os_id, os_version, uname, kvm_present, kvm_readable,
 libvirt_uri, virsh, ip, qemu_img, openstack, curl, python3) = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-host-workflow-result",
               "status": "ready" if ready == "true" else "skipped",
               "reason": reason, "redacted": True, "finished_at": int(time.time()),
               "environment": {"uname": uname, "os_id": os_id or "unknown",
                                "os_version": os_version or "unknown",
                                "kvm_device_present": kvm_present == "true",
                                "kvm_device_readable": kvm_readable == "true",
                                "libvirt_system_uri_available": libvirt_uri == "true",
                                "tools": {"virsh": virsh == "1", "ip": ip == "1",
                                          "qemu-img": qemu_img == "1", "openstack": openstack == "1",
                                          "curl": curl == "1", "python3": python3 == "1"}}},
              output, indent=2)
    output.write("\n")
PY

if [[ "${ready}" != true ]]; then
    echo "real-host workflow prerequisites are unavailable: ${reason}" >&2
    exit 0
fi

if ! bash "${ROOT_DIR}/scripts/real-host-owned-inventory.sh" "${INVENTORY_PATH}"; then
    ready=false
    reason=owned_inventory_unavailable
elif ! python3 - "${INVENTORY_PATH}" <<'PY'
import json, sys
inventory = json.load(open(sys.argv[1], encoding="utf-8"))
if inventory.get("status") != "available" or inventory.get("domains"):
    raise SystemExit(1)
if any(inventory.get("openstack", {}).get("resources", {}).values()):
    raise SystemExit(1)
PY
then
    ready=false
    reason=baseline_not_clean
fi

python3 - "${RESULT_PATH}" "${ready}" "${reason}" "${INVENTORY_PATH}" <<'PY'
import json, sys
path, ready, reason, inventory_path = sys.argv[1:]
result = json.load(open(path, encoding="utf-8"))
try:
    result["inventory_baseline"] = json.load(open(inventory_path, encoding="utf-8"))
except (OSError, json.JSONDecodeError):
    result["inventory_baseline"] = {"status": "unavailable", "redacted": True}
result["status"] = "ready" if ready == "true" else "blocked"
result["reason"] = reason
with open(path, "w", encoding="utf-8") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
PY

if [[ "${ready}" != true ]]; then
    echo "real-host workflow baseline guard blocked: ${reason}" >&2
    exit 1
fi
printf 'ready=true\n' >>"${GITHUB_OUTPUT:-/dev/null}"
echo "real-host workflow guard ready"
