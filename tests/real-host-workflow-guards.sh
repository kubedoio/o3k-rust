#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-real-host-guards.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT
FAKE_BIN="${WORK_DIR}/bin"
mkdir -p "${FAKE_BIN}"
for command in ip qemu-img openstack curl; do
    printf '#!/usr/bin/env bash\nexit 0\n' >"${FAKE_BIN}/${command}"
    chmod +x "${FAKE_BIN}/${command}"
done
cat >"${FAKE_BIN}/virsh" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == "-c qemu:///system uri" ]]; then echo qemu:///system; fi
SH
chmod +x "${FAKE_BIN}/virsh"

export PATH="${FAKE_BIN}:${PATH}" O3K_REAL_HOST_ARTIFACT_DIR="${WORK_DIR}/artifacts"
export O3K_REAL_HOST_KVM_PATH=/dev/null GITHUB_REPOSITORY=kubedoio/o3k-rust
export GITHUB_EVENT_NAME=workflow_dispatch GITHUB_HEAD_REF= GITHUB_BASE_REF=
export GITHUB_OUTPUT="${WORK_DIR}/github-output" O3K_TEST_SECRET=do-not-upload-this-value

bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
grep -qx 'ready=true' "${GITHUB_OUTPUT}"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "ready" and value["redacted"] is True
assert "do-not-upload-this-value" not in json.dumps(value)
assert "environment_variables" not in value
PY

export GITHUB_REPOSITORY=attacker/o3k-rust
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "non-canonical repository was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "non_canonical_repository"
PY

export GITHUB_REPOSITORY=kubedoio/o3k-rust
bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/libvirt-result.json" <<'PY'
import json, sys
json.dump({"status": "passed", "redacted": True}, open(sys.argv[1], "w", encoding="utf-8"))
PY
export O3K_REAL_HOST_WORKFLOW_STEP_STATUS=success
bash "${ROOT_DIR}/scripts/real-host-post-run-guard.sh"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["status"] == "passed"
PY

python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/libvirt-result.json" <<'PY'
import json, sys
json.dump({"status": "skipped", "redacted": True}, open(sys.argv[1], "w", encoding="utf-8"))
PY
bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
if bash "${ROOT_DIR}/scripts/real-host-post-run-guard.sh"; then
    echo "skipped lifecycle was accepted as a pass" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["status"] == "skipped"
PY

python3 - "${ROOT_DIR}/.github/workflows/real-host-validation.yml" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
for needle in ("workflow_dispatch:",
               "runs-on: [self-hosted, linux, x64, kvm, libvirt, o3k-testlab]",
               "cancel-in-progress: false", "environment: o3k-real-host-validation",
               "contents: read", "if: always()", "actions/upload-artifact@v4"):
    assert needle in text, needle
PY
echo "real-host workflow guard tests passed"
