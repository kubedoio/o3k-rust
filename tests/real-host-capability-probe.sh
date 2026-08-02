#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-capability-probe.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT
FAKE_BIN="${WORK_DIR}/bin"
mkdir -p "${FAKE_BIN}"
for command in virsh qemu-img ip dnsmasq openstack xorriso; do
    printf '#!/usr/bin/env bash\n[[ "$*" == *qemu:///system* ]] && echo qemu:///system\nexit 0\n' >"${FAKE_BIN}/${command}"
    chmod +x "${FAKE_BIN}/${command}"
done
KVM_PATH="${WORK_DIR}/kvm"
python3 - "${KVM_PATH}" <<'PY'
import os, sys
path = sys.argv[1]
os.mkfifo(path)
PY
IMAGE_PATH="${WORK_DIR}/cirros.img"
touch "${IMAGE_PATH}"

export PATH="${FAKE_BIN}:${PATH}"
export O3K_REAL_HOST_ARTIFACT_DIR="${WORK_DIR}/artifacts"
export O3K_REAL_HOST_CAPABILITY_OUTPUT="${WORK_DIR}/artifacts/runner-capabilities.json"
export O3K_REAL_HOST_KVM_PATH="${KVM_PATH}"
export O3K_REAL_HOST_DISK_PATH="${WORK_DIR}"
export O3K_REAL_HOST_MIN_FREE_BYTES=1
export O3K_REAL_HOST_WORKFLOW_RUN_ID=portable-run-1
export O3K_REAL_HOST_WORKFLOW_RUN_ATTEMPT=1
export GITHUB_SHA=0123456789abcdef0123456789abcdef01234567
unset O3K_REAL_HOST_SERVICE_ACCOUNT
export O3K_REAL_HOST_RUNNER_LABELS="self-hosted,linux,x64,kvm,libvirt,o3k-testlab"
export O3K_TESTLAB_IMAGE_PATH="${IMAGE_PATH}"

bash "${ROOT_DIR}/scripts/real-host-capability-probe.sh"
python3 - "${O3K_REAL_HOST_CAPABILITY_OUTPUT}" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "skipped"
assert "/dev/kvm" in value["required_missing"]
assert value["redacted"] is True
assert "environment_variables" not in value
assert value["workflow_run_id"] == "portable-run-1"
assert value["workflow_run_attempt"] == "1"
assert value["source_commit"] == "0123456789abcdef0123456789abcdef01234567"
PY

export O3K_REAL_HOST_KVM_PATH=/dev/null
bash "${ROOT_DIR}/scripts/real-host-capability-probe.sh"
python3 - "${O3K_REAL_HOST_CAPABILITY_OUTPUT}" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "skipped"
assert "/dev/kvm" in value["required_missing"]
PY

unset O3K_REAL_HOST_SERVICE_ACCOUNT O3K_REAL_HOST_RUNNER_LABELS O3K_TESTLAB_IMAGE_PATH
export O3K_REAL_HOST_KVM_PATH=/dev/kvm
bash "${ROOT_DIR}/scripts/real-host-capability-probe.sh" || true
python3 - "${O3K_REAL_HOST_CAPABILITY_OUTPUT}" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] in {"skipped", "failed"}
assert "do-not-upload" not in json.dumps(value)
PY

export O3K_REAL_HOST_RUNNER_LABELS="self-hosted,linux,x64"
if bash "${ROOT_DIR}/scripts/real-host-capability-probe.sh"; then
    echo "unsafe runner labels unexpectedly passed" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_CAPABILITY_OUTPUT}" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "failed"
assert value["reason"] == "runner_labels_mismatch"
PY
echo "real-host capability probe tests passed"
