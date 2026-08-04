#!/usr/bin/env bash
set -Eeuo pipefail

# Post-run guard for the protected real Cinder service-testbed workflow.
# Validates the runner's evidence artifacts (foreign-state inventory before and
# after the run, cleanup verification, evidence manifest) and produces a
# machine-readable aggregate result. Fails when any run-owned resource remains
# or foreign state changed. Never dumps the environment.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-cinder-workflow-artifacts}"
RESULT_PATH="${ARTIFACT_DIR}/real-cinder-workflow-result.json"
STEP_STATUS="${O3K_REAL_HOST_WORKFLOW_STEP_STATUS:-skipped}"
STATE_ROOT="${O3K_STATE_ROOT:-/var/lib/o3k-cinder-testbed/unknown}"
mkdir -p "${ARTIFACT_DIR}"

if [[ "${STEP_STATUS}" == skipped ]]; then
    python3 - "${RESULT_PATH}" <<'PY'
import json, sys, time
with open(sys.argv[1], "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "skipped",
               "reason": "prerequisites_skipped", "redacted": True,
               "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-cinder workflow skipped"
    exit 0
fi

EVIDENCE_DIR="$(sudo -n find "${STATE_ROOT}" -maxdepth 2 -name evidence.yaml -type f 2>/dev/null | sort | tail -n 1 | xargs -r dirname 2>/dev/null || true)"
if [[ -z "${EVIDENCE_DIR}" || ! -d "${EVIDENCE_DIR}" ]]; then
    python3 - "${RESULT_PATH}" <<'PY'
import json, sys, time
with open(sys.argv[1], "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "failed",
               "reason": "evidence_manifest_unavailable", "redacted": True,
               "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-cinder evidence manifest unavailable" >&2
    exit 1
fi

BEFORE="${EVIDENCE_DIR}/foreign-state-before.json"
AFTER="${EVIDENCE_DIR}/foreign-state-after.json"
MANIFEST="${EVIDENCE_DIR}/evidence.yaml"

python3 - "${RESULT_PATH}" "${BEFORE}" "${AFTER}" "${MANIFEST}" "${STEP_STATUS}" "${EVIDENCE_DIR}" <<'PY'
import json, os, sys, time

result_path, before_path, after_path, manifest_path, step_status, evidence_dir = sys.argv[1:]
failures = []

def read_json(path):
    try:
        with open(path, encoding="utf-8") as stream:
            return json.load(stream)
    except (OSError, json.JSONDecodeError):
        return None

def read_yaml(path):
    try:
        import yaml
        with open(path, encoding="utf-8") as stream:
            return yaml.safe_load(stream)
    except Exception:
        return None

before = read_json(before_path)
after = read_json(after_path)
manifest = read_yaml(manifest_path)

if before is None:
    failures.append("foreign_state_before_unavailable")
if after is None:
    failures.append("foreign_state_after_unavailable")
if manifest is None:
    failures.append("evidence_manifest_unparseable")
if step_status not in {"success"}:
    failures.append("workflow_step_failed")

cleanup_status = None
foreign_unchanged = None
run_owned_remaining = None
if after is not None:
    cleanup_status = after.get("cleanup_status")
    foreign_unchanged = after.get("foreign_unchanged")
    run_owned_remaining = after.get("run_owned_resources_remaining")

if cleanup_status != "passed":
    failures.append("run_owned_resources_remaining")
if foreign_unchanged is not True:
    failures.append("foreign_state_changed")

if failures:
    final_status = "failed"
    reason = failures[0]
else:
    final_status = "passed"
    reason = "workflow_and_prerequisites_passed"

result = {
    "artifact_type": "real-cinder-workflow-result",
    "status": final_status,
    "reason": reason,
    "redacted": True,
    "finished_at": int(time.time()),
    "step_status": step_status,
    "evidence_dir": evidence_dir,
    "cleanup_status": cleanup_status,
    "foreign_unchanged": foreign_unchanged,
    "run_owned_resources_remaining": run_owned_remaining or [],
    "profile": (manifest or {}).get("profile"),
    "cinder_version": (manifest or {}).get("cinder_version"),
    "evidence_tiers": (manifest or {}).get("evidence_tiers"),
}
if failures:
    result["failures"] = failures

with open(result_path, "w", encoding="utf-8") as output:
    json.dump(result, output, indent=2)
    output.write("\n")

if final_status != "passed":
    raise SystemExit(f"real-cinder workflow did not pass: {final_status} ({reason})")
PY

echo "real-cinder workflow passed"
