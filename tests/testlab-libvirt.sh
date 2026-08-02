#!/usr/bin/env bash
set -Eeuo pipefail

# Real-libvirt coverage is deliberately opt-in. A missing prerequisite is a
# visible "skipped" result, never a fake pass.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_TESTLAB_ARTIFACT_DIR:-${ROOT_DIR}/target/testlab-artifacts}"
mkdir -p "${ARTIFACT_DIR}"
rm -f "${ARTIFACT_DIR}/libvirt-result.json" "${ARTIFACT_DIR}/openstack-cli-result.json"

write_result() {
    local status="$1" reason="$2"
    python3 - "${ARTIFACT_DIR}/libvirt-result.json" "${status}" "${reason}" <<'PY'
import json, sys, time
path, status, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({
        "artifact_type": "libvirt-preflight", "status": status,
        "reason": reason, "profile": "libvirt", "redacted": True,
        "cleanup": {"status": "not_run"}, "finished_at": int(time.time()),
    }, output, indent=2)
    output.write("\n")
PY
}

for command in virsh ip qemu-img; do
    if ! command -v "${command}" >/dev/null 2>&1; then
        write_result skipped "missing command: ${command}"
        echo "real-libvirt profile skipped: missing ${command}" >&2
        exit 0
    fi
done
if [[ ! -e /dev/kvm ]]; then
    write_result skipped "KVM device is unavailable"
    echo "real-libvirt profile skipped: /dev/kvm is unavailable" >&2
    exit 0
fi
if ! virsh -c qemu:///system uri >/dev/null 2>&1; then
    write_result skipped "qemu:///system is unavailable"
    echo "real-libvirt profile skipped: qemu:///system is unavailable" >&2
    exit 0
fi

if ! command -v openstack >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
    write_result skipped "preflight passed; openstack CLI and curl are required for the public lifecycle harness"
    echo "real-libvirt preflight passed; lifecycle harness skipped because openstack or curl is unavailable" >&2
    exit 0
fi

# The lifecycle harness deliberately reuses the public OpenStack CLI workflow.
# It must be pointed at an already-installed libvirt profile; this script owns
# prerequisite validation and artifact selection, not daemon provisioning.
set +e
O3K_TESTLAB_PROFILE=libvirt \
    O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}" \
    bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"
status=$?
set -e
if [[ ! -f "${ARTIFACT_DIR}/openstack-cli-result.json" ]]; then
    write_result failed "lifecycle harness did not produce an evidence artifact"
    exit 1
fi
if ! python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as stream:
    result = json.load(stream)
if not isinstance(result, dict):
    raise SystemExit("lifecycle artifact root is not an object")
if result.get("artifact_type") != "openstack-cli-e2e":
    raise SystemExit("lifecycle artifact type is invalid")
if result.get("profile") != "libvirt" or result.get("redacted") is not True:
    raise SystemExit("lifecycle artifact profile or redaction marker is invalid")
if not isinstance(result.get("finished_at"), int):
    raise SystemExit("lifecycle artifact timestamp is invalid")
cleanup = result.get("cleanup")
if not isinstance(cleanup, dict) or cleanup.get("status") not in {"passed", "failed", "not_run"}:
    raise SystemExit("lifecycle artifact cleanup status is invalid")
status = result.get("status")
if status not in {"passed", "failed", "skipped"}:
    raise SystemExit("lifecycle artifact status is invalid")
if status == "passed":
    lifecycle = result.get("lifecycle")
    expected = {"create", "show", "list", "stop", "start", "reboot", "console", "delete"}
    if not isinstance(lifecycle, dict) or set(lifecycle) != expected or not all(lifecycle.values()):
        raise SystemExit("passed lifecycle artifact does not prove every public operation")
if status == "failed" and cleanup.get("status") != "passed":
    raise SystemExit("failed lifecycle artifact does not prove cleanup")
PY
then
    write_result failed "lifecycle harness produced an invalid evidence artifact"
    exit 1
fi
if [[ "${O3K_TESTLAB_CONFIG_DRIVE:-true}" == true && -n "${O3K_TESTLAB_STATE_ROOT:-}" ]]; then
    python3 - "${ARTIFACT_DIR}/config-drive-evidence.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream)
if value.get("artifact_type") != "config-drive-libvirt-attachment" or value.get("status") != "passed":
    raise SystemExit("protected config-drive evidence did not prove libvirt attachment")
if value.get("redacted") is not True or value.get("read_only") is not True or value.get("source_under_owned_state") is not True:
    raise SystemExit("config-drive evidence is not safely redacted or owned")
PY
fi
cp "${ARTIFACT_DIR}/openstack-cli-result.json" "${ARTIFACT_DIR}/libvirt-result.json"
if [[ ${status} -ne 0 ]]; then
    echo "real-libvirt lifecycle harness failed; see ${ARTIFACT_DIR}/libvirt-result.json" >&2
    exit "${status}"
fi
echo "real-libvirt lifecycle harness completed; artifact: ${ARTIFACT_DIR}/libvirt-result.json"
