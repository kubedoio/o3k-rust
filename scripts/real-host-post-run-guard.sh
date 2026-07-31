#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-host-workflow-artifacts}"
RESULT_PATH="${ARTIFACT_DIR}/real-host-workflow-result.json"
LIFECYCLE_RESULT="${ARTIFACT_DIR}/libvirt-result.json"
STEP_STATUS="${O3K_REAL_HOST_WORKFLOW_STEP_STATUS:-skipped}"
mkdir -p "${ARTIFACT_DIR}"

python3 - "${RESULT_PATH}" "${LIFECYCLE_RESULT}" "${STEP_STATUS}" <<'PY'
import json, sys, time
result_path, lifecycle_path, step_status = sys.argv[1:]
try:
    with open(result_path, encoding="utf-8") as stream:
        preflight = json.load(stream)
except (OSError, json.JSONDecodeError):
    preflight = {"status": "blocked", "reason": "guard_result_unavailable"}
status = preflight.get("status")
reason = preflight.get("reason", "unknown")
try:
    with open(lifecycle_path, encoding="utf-8") as stream:
        lifecycle_status = json.load(stream).get("status")
except (OSError, json.JSONDecodeError):
    lifecycle_status = None

if status == "blocked":
    final_status = "blocked"
elif status != "ready":
    final_status = "skipped"
    reason = "prerequisites_skipped"
elif step_status != "success":
    final_status = "failed" if step_status == "failure" else "skipped"
    reason = "workflow_step_failed" if step_status == "failure" else "workflow_step_skipped"
elif lifecycle_status != "passed":
    final_status = lifecycle_status if lifecycle_status in {"failed", "skipped"} else "failed"
    reason = "lifecycle_workflow_failed" if final_status == "failed" else "lifecycle_workflow_skipped"
else:
    final_status = "passed"
    reason = "workflow_and_prerequisites_passed"

result = {"artifact_type": "real-host-workflow-result", "status": final_status,
          "reason": reason, "redacted": True, "finished_at": int(time.time()),
          "preflight_status": status, "lifecycle_status": lifecycle_status}
if isinstance(preflight.get("environment"), dict):
    result["environment"] = preflight["environment"]
with open(result_path, "w", encoding="utf-8") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
if final_status != "passed":
    raise SystemExit(f"real-host workflow did not pass: {final_status} ({reason})")
PY

echo "real-host workflow passed"
