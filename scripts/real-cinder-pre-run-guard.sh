#!/usr/bin/env bash
set -Eeuo pipefail

# Pre-run guard for the protected real Cinder service-testbed workflow.
# Verifies the trusted execution context, the runner capability probe, and that
# no stale run-owned Cinder state exists before any mutation. Never dumps the
# environment.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-cinder-workflow-artifacts}"
RESULT_PATH="${ARTIFACT_DIR}/real-cinder-workflow-result.json"
CAPABILITY_PATH="${O3K_REAL_HOST_CAPABILITY_OUTPUT:-${ARTIFACT_DIR}/runner-capabilities.json}"
STATE_BASE="${O3K_CINDER_STATE_BASE:-/var/lib/o3k-cinder-testbed}"
EXPECTED_RUN_ID="${O3K_REAL_HOST_WORKFLOW_RUN_ID:-}"
EXPECTED_RUN_ATTEMPT="${O3K_REAL_HOST_WORKFLOW_RUN_ATTEMPT:-}"
EXPECTED_SOURCE_COMMIT="${GITHUB_SHA:-}"
EXPECTED_REF=refs/heads/main
mkdir -p "${ARTIFACT_DIR}"
rm -f -- "${RESULT_PATH}"

blocked_reason=
if [[ "${GITHUB_REPOSITORY:-}" != "kubedoio/o3k-rust" ]]; then
    blocked_reason=non_canonical_repository
elif [[ "${GITHUB_EVENT_NAME:-}" != workflow_dispatch ]]; then
    blocked_reason=untrusted_event_context
elif [[ -n "${GITHUB_HEAD_REF:-}" || -n "${GITHUB_BASE_REF:-}" ]]; then
    blocked_reason=untrusted_fork_context
elif [[ "${GITHUB_REF:-}" != "${EXPECTED_REF}" ]]; then
    blocked_reason=untrusted_source_ref
fi

if [[ -n "${blocked_reason}" ]]; then
    python3 - "${RESULT_PATH}" "${blocked_reason}" <<'PY'
import json, sys, time
path, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "blocked",
               "reason": reason, "redacted": True, "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-cinder workflow guard blocked: ${blocked_reason}" >&2
    exit 1
fi

capability_status=unavailable
if [[ -r "${CAPABILITY_PATH}" ]]; then
    capability_status="$(python3 - "${CAPABILITY_PATH}" "${EXPECTED_RUN_ID}" \
        "${EXPECTED_RUN_ATTEMPT}" "${EXPECTED_SOURCE_COMMIT}" <<'PY'
import json, sys
path, expected_run_id, expected_attempt, expected_source_commit = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as stream:
        value = json.load(stream)
except (OSError, json.JSONDecodeError):
    print("unavailable")
else:
    valid = (
        value.get("artifact_type") == "runner-capabilities"
        and value.get("schema_version") == 1
        and value.get("redacted") is True
        and isinstance(value.get("finished_at"), int)
        and not isinstance(value.get("finished_at"), bool)
    )
    if expected_run_id and value.get("workflow_run_id") != expected_run_id:
        valid = False
    if expected_attempt and value.get("workflow_run_attempt") != expected_attempt:
        valid = False
    if expected_source_commit and value.get("source_commit") != expected_source_commit:
        valid = False
    print(value.get("status", "unavailable") if valid else "unavailable")
PY
)"
fi
if [[ "${capability_status}" != passed ]]; then
    python3 - "${RESULT_PATH}" "${capability_status}" <<'PY'
import json, sys, time
path, status = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "blocked",
               "reason": "capability_probe_unavailable" if status == "unavailable" else "capability_probe_failed",
               "capability_status": status, "redacted": True,
               "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-cinder capability probe did not pass; workflow blocked" >&2
    exit 1
fi

# Stale-resource check: no prior run-owned Cinder state may exist.
mapfile -t stale_dirs < <(find "${STATE_BASE}" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -n 20)
if [[ ${#stale_dirs[@]} -gt 0 ]]; then
    python3 - "${RESULT_PATH}" <<PY
import json, os, sys, time
stale_dirs = ${stale_dirs@Q}
with open("${RESULT_PATH}", "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "blocked",
               "reason": "stale_run_owned_state",
               "stale_state_dirs": stale_dirs,
               "redacted": True, "finished_at": int(time.time())},
              output, indent=2)
    output.write("\n")
PY
    echo "real-cinder stale run-owned state detected under ${STATE_BASE}" >&2
    exit 1
fi

# Stale host-resource check: a prior aborted run may have left a run-owned VG,
# loop device, MariaDB database/user, or RabbitMQ user/vhost behind. Any such
# resource must fail the run before mutation so a fresh run never collides with
# a half-cleaned predecessor. Names use the run-owned o3k- prefixes.
python3 - "${RESULT_PATH}" <<'PY'
import json, subprocess, sys, time

def run(args):
    try:
        return subprocess.run(args, capture_output=True, text=True, check=True).stdout
    except Exception:
        return ""

stale = []

for vg in run(["vgs", "--noheadings", "-o", "vg_name"]).split():
    vg = vg.strip()
    if vg.startswith("o3k-vg-"):
        stale.append({"resource": "lvm_vg", "name": vg})
for line in run(["losetup", "-a"]).splitlines():
    if "o3k" in line:
        stale.append({"resource": "loop_device", "name": line.split(":", 1)[0]})
for db in run(["mysql", "-N", "-e", "SHOW DATABASES;"]).split():
    db = db.strip()
    if db.startswith("o3k_cinder_"):
        stale.append({"resource": "mariadb_database", "name": db})
for user in run(["mysql", "-N", "-e", "SELECT User FROM mysql.user;"]).split():
    user = user.strip()
    if user.startswith("o3k_cinder_"):
        stale.append({"resource": "mariadb_user", "name": user})
for vhost in run(["rabbitmqctl", "list_vhosts"]).splitlines()[1:]:
    vhost = vhost.strip()
    if vhost.startswith("o3k_cinder_"):
        stale.append({"resource": "rabbitmq_vhost", "name": vhost})
for user in run(["rabbitmqctl", "list_users"]).splitlines()[1:]:
    user = user.split()[0] if user.split() else ""
    if user.startswith("o3k_cinder_"):
        stale.append({"resource": "rabbitmq_user", "name": user})

if stale:
    with open(sys.argv[1], "w", encoding="utf-8") as output:
        json.dump({"artifact_type": "real-cinder-workflow-result", "status": "blocked",
                   "reason": "stale_run_owned_host_resources",
                   "stale_host_resources": stale,
                   "redacted": True, "finished_at": int(time.time())},
                  output, indent=2)
        output.write("\n")
    print("real-cinder stale run-owned host resources detected", file=sys.stderr)
    raise SystemExit(1)
PY

python3 - "${RESULT_PATH}" <<PY
import json, sys, time
with open("${RESULT_PATH}", "w", encoding="utf-8") as output:
    json.dump({"artifact_type": "real-cinder-workflow-result", "status": "ready",
               "redacted": True, "finished_at": int(time.time()),
               "capability_status": "passed"},
              output, indent=2)
    output.write("\n")
PY
echo "ready=true"
