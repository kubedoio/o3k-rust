#!/usr/bin/env bash
set -Eeuo pipefail

# Real-libvirt coverage is deliberately opt-in. A missing prerequisite is a
# visible "skipped" result, never a fake pass.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_TESTLAB_ARTIFACT_DIR:-${ROOT_DIR}/target/testlab-artifacts}"
mkdir -p "${ARTIFACT_DIR}"
rm -f "${ARTIFACT_DIR}/libvirt-result.json"

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

for command in virsh ip; do
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

write_result skipped "preflight passed; lifecycle harness requires a TestLab image and managed network permissions"
echo "real-libvirt preflight passed; lifecycle harness is not implemented in this runner"
