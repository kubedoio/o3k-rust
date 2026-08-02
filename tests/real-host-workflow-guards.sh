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
if [[ "$*" == "-c qemu:///system list --all --name" && "${O3K_FAKE_VIRSH_DIRTY:-false}" == true ]]; then
    echo o3k-preexisting-domain
fi
SH
chmod +x "${FAKE_BIN}/virsh"
cat >"${FAKE_BIN}/ip" <<'SH'
#!/usr/bin/env bash
if [[ "${O3K_FAKE_IP_DIRTY:-false}" == true ]]; then
    echo '2: foreign0: <BROADCAST> mtu 1500 state UP'
fi
if [[ "${O3K_FAKE_IP_OWNED_LEAK:-false}" == true ]]; then
    echo '3: o3k-tap-leak: <BROADCAST> mtu 1500 state UP'
fi
if [[ "${O3K_FAKE_IP_UNSTABLE:-false}" == true ]]; then
    counter_file="${O3K_FAKE_IP_COUNTER:?}"
    count=0
    [[ -f "${counter_file}" ]] && count="$(<"${counter_file}")"
    count=$((count + 1))
    printf '%s\n' "${count}" >"${counter_file}"
    echo "${count}: unstable0: <BROADCAST> mtu 1500 state UP"
fi
SH
chmod +x "${FAKE_BIN}/ip"
cat >"${FAKE_BIN}/openstack" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == flavor\ list\ * ]]; then
    if [[ "${O3K_FAKE_OPENSTACK_LEAK:-false}" == true ]]; then
        echo '[{"ID":"leaked-openstack-resource","Name":"o3k-testlab-flavor"}]'
    else
        echo '[]'
    fi
    exit 0
fi
if [[ "$*" == *" list "* && "${O3K_FAKE_OPENSTACK_LEAK:-false}" == true ]]; then
    echo leaked-openstack-resource
fi
SH
chmod +x "${FAKE_BIN}/openstack"

export PATH="${FAKE_BIN}:${PATH}" O3K_REAL_HOST_ARTIFACT_DIR="${WORK_DIR}/artifacts"
export O3K_REAL_HOST_KVM_PATH=/dev/null GITHUB_REPOSITORY=kubedoio/o3k-rust
export GITHUB_EVENT_NAME=workflow_dispatch GITHUB_HEAD_REF= GITHUB_BASE_REF= GITHUB_REF=refs/heads/main
export GITHUB_OUTPUT="${WORK_DIR}/github-output" O3K_TEST_SECRET=do-not-upload-this-value
export O3K_REAL_HOST_OPENSTACK_INVENTORY=true OS_PASSWORD=fake-password
export O3K_REAL_HOST_PROTECTED_PATHS="${WORK_DIR}/protected-state.txt"
export O3K_REAL_HOST_WORKFLOW_RUN_ID=guard-run-1 O3K_REAL_HOST_WORKFLOW_RUN_ATTEMPT=1
export GITHUB_SHA=0123456789abcdef0123456789abcdef01234567
mkdir -p "${O3K_REAL_HOST_ARTIFACT_DIR}"
printf 'original protected state\n' >"${O3K_REAL_HOST_PROTECTED_PATHS}"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
json.dump({"artifact_type": "runner-capabilities", "schema_version": 1,
           "status": "passed", "redacted": True,
           "workflow_run_id": "guard-run-1", "workflow_run_attempt": "1",
           "source_commit": "0123456789abcdef0123456789abcdef01234567",
           "finished_at": 1},
          open(sys.argv[1], "w", encoding="utf-8"))
PY

bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
grep -qx 'ready=true' "${GITHUB_OUTPUT}"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "ready" and value["redacted"] is True
assert set(value["inventory_baseline"]["openstack"]["resources"]) == {
    "server", "image", "network", "subnet", "flavor"
}
assert "do-not-upload-this-value" not in json.dumps(value)
assert "environment_variables" not in value
assert value["inventory_baseline"]["foreign_state"]["protected_paths_sha256"]
assert value["inventory_baseline"]["network_links"] == []
PY

unset O3K_REAL_HOST_PROTECTED_PATHS
if bash "${ROOT_DIR}/scripts/real-host-owned-inventory.sh" "${WORK_DIR}/missing-protected-paths.json"; then
    echo "missing protected-path configuration was accepted" >&2
    exit 1
fi
export O3K_REAL_HOST_PROTECTED_PATHS="${WORK_DIR}/protected-state.txt"

unset OS_PASSWORD
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "requested OpenStack inventory without credentials was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked"
assert value["reason"] == "owned_inventory_unavailable"
PY
export OS_PASSWORD=fake-password

export O3K_FAKE_IP_UNSTABLE=true O3K_FAKE_IP_COUNTER="${WORK_DIR}/ip-counter"
if bash "${ROOT_DIR}/scripts/real-host-owned-inventory.sh" "${WORK_DIR}/unstable.json"; then
    echo "unstable inventory was accepted" >&2
    exit 1
fi
python3 - "${WORK_DIR}/unstable.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "unavailable" and value["reason"] == "inventory_not_stable"
PY
unset O3K_FAKE_IP_UNSTABLE O3K_FAKE_IP_COUNTER

python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
json.dump({"artifact_type": "runner-capabilities", "schema_version": 1,
           "status": "failed", "reason": "runner_labels_mismatch", "redacted": True,
           "workflow_run_id": "guard-run-1", "workflow_run_attempt": "1",
           "source_commit": "0123456789abcdef0123456789abcdef01234567",
           "finished_at": 1},
          open(sys.argv[1], "w", encoding="utf-8"))
PY
: >"${GITHUB_OUTPUT}"
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "failed capability probe was accepted" >&2
    exit 1
fi
if grep -q '^ready=true$' "${GITHUB_OUTPUT}"; then
    echo "failed capability probe marked guard ready" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value == {"artifact_type": "real-host-workflow-result",
                 "status": "blocked", "reason": "capability_probe_failed",
                 "redacted": True, "finished_at": value["finished_at"]}
PY

python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
json.dump({"artifact_type": "runner-capabilities", "schema_version": 1,
           "status": "passed", "redacted": True,
           "workflow_run_id": "guard-run-1", "workflow_run_attempt": "1",
           "source_commit": "0123456789abcdef0123456789abcdef01234567",
           "finished_at": 1},
          open(sys.argv[1], "w", encoding="utf-8"))
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
export GITHUB_REF=refs/heads/feature-untrusted
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "non-main source ref was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "untrusted_source_ref"
PY
export GITHUB_REF=refs/heads/main

export O3K_FAKE_VIRSH_DIRTY=true
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "pre-existing owned resource was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "baseline_not_clean"
assert "do-not-upload-this-value" not in json.dumps(value)
PY
unset O3K_FAKE_VIRSH_DIRTY
bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/libvirt-result.json" <<'PY'
import json, sys
json.dump({"status": "passed", "redacted": True}, open(sys.argv[1], "w", encoding="utf-8"))
PY
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/compute-agent-process-mtls-result.json" <<'PY'
import json, sys
json.dump({
    "artifact_type": "compute-agent-process-mtls",
    "status": "passed",
    "redacted": True,
    "scope": "o3kd-to-o3k-compute-to-libvirt",
    "evidence": {
        "command": "inspect",
        "command_state": "accepted",
        "error_category": "not_found",
        "operation_state": "failed",
        "observation_state": "failed_not_found",
        "observation_operation_state": "failed",
        "redacted": True,
        "transitions": ["accepted", "operation_failed", "observation_failed"],
        "transport": "mutual_tls",
    },
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
export O3K_REAL_HOST_WORKFLOW_STEP_STATUS=success
bash "${ROOT_DIR}/scripts/real-host-post-run-guard.sh"
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["status"] == "passed"
PY
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/resource-leak-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["artifact_type"] == "resource-leak-result"
assert value["status"] == "passed"
PY

export O3K_REAL_HOST_WORKFLOW_STEP_STATUS=success
bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"
printf 'mutated protected state\n' >"${O3K_REAL_HOST_PROTECTED_PATHS}"
export O3K_FAKE_VIRSH_DIRTY=true O3K_FAKE_OPENSTACK_LEAK=true O3K_FAKE_IP_DIRTY=true O3K_FAKE_IP_OWNED_LEAK=true
if bash "${ROOT_DIR}/scripts/real-host-post-run-guard.sh"; then
    echo "owned resource leak was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "failed" and value["reason"] == "resource_leak_detected"
assert "o3k-preexisting-domain" in value["leaks"]["domains"]
assert "o3k-tap-leak" in value["leaks"]["network_links"]
assert "leaked-openstack-resource" in value["leaks"]["openstack"]["image"]
assert "leaked-openstack-resource" in value["leaks"]["openstack"]["flavor"]
assert value["foreign_state_changed"] is True
assert "foreign0" not in json.dumps(value)
assert "do-not-upload-this-value" not in json.dumps(value)
PY
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/resource-leak-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "failed"
assert value["foreign_state_changed"] is True
PY
unset O3K_FAKE_VIRSH_DIRTY O3K_FAKE_OPENSTACK_LEAK O3K_FAKE_IP_DIRTY O3K_FAKE_IP_OWNED_LEAK

python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
value = {"artifact_type": "runner-capabilities", "schema_version": 1,
         "status": "passed", "redacted": True, "finished_at": 1,
         "workflow_run_id": "old-run", "workflow_run_attempt": "1",
         "source_commit": "0123456789abcdef0123456789abcdef01234567"}
json.dump(value, open(sys.argv[1], "w", encoding="utf-8"))
PY
if bash "${ROOT_DIR}/scripts/real-host-pre-run-guard.sh"; then
    echo "stale capability artifact was accepted" >&2
    exit 1
fi
python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/real-host-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked"
assert value["reason"] == "capability_probe_unavailable"
PY

python3 - "${O3K_REAL_HOST_ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
json.dump({"artifact_type": "runner-capabilities", "schema_version": 1,
           "status": "passed", "redacted": True, "finished_at": 1,
           "workflow_run_id": "guard-run-1", "workflow_run_attempt": "1",
           "source_commit": "0123456789abcdef0123456789abcdef01234567"},
          open(sys.argv[1], "w", encoding="utf-8"))
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
workflow_step = text.split("      - name: Run public real-host lifecycle\n", 1)[1]
workflow_step = workflow_step.split("        run: bash tests/testlab-libvirt.sh\n", 1)[0]
assert "          OS_PASSWORD:" not in workflow_step
for needle in ("workflow_dispatch:",
               "runs-on: [self-hosted, linux, x64, kvm, libvirt, o3k-testlab]",
               "cancel-in-progress: false", "environment: o3k-real-host-validation",
               "Bootstrap disposable TestLab",
               "scripts/bootstrap-disposable-testlab.sh",
               "scripts/cleanup-disposable-testlab.sh",
               "disposable-testlab-bootstrap.json",
               "Probe runner capabilities", "runner-capabilities.json",
               "Download and verify CirrOS image",
               "CIRROS_IMAGE_SHA256: 7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b",
               "sha256sum --check --strict --status",
               "O3K_TESTLAB_IMAGE_PATH=",
               "continue-on-error: true", "timeout-minutes: 60",
               "contents: read",
               "if: always()", "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
               "retention-days: 14",
               "target/real-host-workflow-artifacts/console-result.json",
               "Run compute-agent process-boundary evidence",
               "tests/real-compute-agent-process-mtls.sh",
               "compute-agent-process-mtls-result.json"):
    assert needle in text, needle
assert "if: github.repository == 'kubedoio/o3k-rust' && github.ref == 'refs/heads/main'" in text
assert "ref: ${{ github.sha }}" in text
assert "persist-credentials: false" in text
assert "Verify immutable source checkout" in text
assert "target/real-host-workflow-artifacts/console.log" not in text
assert "target/real-host-workflow-artifacts/server-show.json" not in text
assert pathlib.Path(sys.argv[1]).parents[2].joinpath("scripts/real-host-owned-inventory.sh").exists()
post_guard = pathlib.Path(sys.argv[1]).parents[2].joinpath("scripts/real-host-post-run-guard.sh").read_text(encoding="utf-8")
assert "compute-agent-process-mtls-result.json" in post_guard
assert "compute_agent_process_probe_failed" in post_guard
PY
echo "real-host workflow guard tests passed"
