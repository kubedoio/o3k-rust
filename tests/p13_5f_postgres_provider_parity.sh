#!/usr/bin/env bash
set -euo pipefail

# PostgreSQL provider-level parity orchestrator.  The existing P13.5B journey
# is deliberately reused so the OpenTofu/provider boundary is identical to the
# portable evidence path.  This wrapper never promotes an unavailable or
# failed journey to PASS; callers receive a machine-readable blocked artifact.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${O3K_P13_5F_POSTGRES_EVIDENCE_OUTPUT:-$root_dir/target/p13-5f/postgres-provider-parity.json}"
mkdir -p "$(dirname "$output")"

write_abort_artifact() {
  [[ -s "$output" ]] && return 0
  python3 - "$output" "$root_dir" <<'PY'
import json, os, pathlib, subprocess, sys
out, root = sys.argv[1:]
head = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip()
head = os.environ.get("O3K_P13_SOURCE_HEAD_SHA", head)
names = ["PG1-import-read-reconstruction", "PG2-mutable-drift-reconvergence", "PG3-remote-deletion-recreation", "PG4-independent-replacement", "PG5-router-interface-relationship", "PG6-volume-attachment-relationship", "PG7-operation-replay-unknown-outcome"]
document = {"artifact_type": "o3k-p13-5f-postgres-provider-parity", "schema_version": 1, "phase": "P13.5F", "tested_o3k_head_sha": head, "backend": "postgresql", "provider_modified": False, "execution": {"orchestrator": "tests/p13_5f_postgres_provider_parity.sh", "status": "failed", "failure_artifact": True}, "scenarios": [{"scenario": name, "result": "blocked", "externally_equivalent": False, "reason": "orchestrator exited before scenario completion"} for name in names], "final_verdict": "blocked"}
pathlib.Path(out).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
PY
}
trap write_abort_artifact EXIT

if [[ -z "${O3K_DATABASE_URL:-}" ]]; then
  echo "P13.5F PostgreSQL parity BLOCKED: O3K_DATABASE_URL is required" >&2
  exit 2
fi
if ! command -v pg_isready >/dev/null 2>&1 || ! pg_isready -d "$O3K_DATABASE_URL" >/dev/null 2>&1; then
  echo "P13.5F PostgreSQL parity BLOCKED: PostgreSQL is not ready" >&2
  exit 2
fi
for required in O3K_P13_TOFU O3K_P13_PROVIDER_BINARY O3K_P13_PROVIDER_ARCHIVE O3K_P13_TOFU_ARCHIVE; do
  if [[ -z "${!required:-}" || ! -f "${!required}" ]]; then
    echo "P13.5F PostgreSQL parity BLOCKED: missing pinned toolchain input $required" >&2
    exit 2
  fi
done
if [[ ! "${O3K_P13_PROVIDER_SHA256:-}" =~ ^[[:xdigit:]]{64}$ ]]; then
  echo "P13.5F PostgreSQL parity BLOCKED: O3K_P13_PROVIDER_SHA256 must be a 64-hex digest" >&2
  exit 2
fi
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools

# The accepted P13.3 router gate consumes this shared harness credential.  CI
# only supplies the database URL, so provide the same disposable default used
# by the reused P13.5 journeys instead of allowing `set -u` to abort the
# baseline before it can execute.
export O3K_P13_PASSWORD="${O3K_P13_PASSWORD:-p13-5b-refresh-import-password}"
export O3K_P13_SOURCE_HEAD_SHA="${O3K_P13_SOURCE_HEAD_SHA:-$(git -C "$root_dir" rev-parse HEAD)}"

work_dir="${O3K_P13_5F_WORK_DIR:-$(mktemp -d /var/tmp/o3k-p13-5f-postgres.XXXXXX)}"
mkdir -p "$work_dir"
log="$work_dir/p13-5b-refresh-import.log"
b_output="$work_dir/p13-5b-refresh-import-evidence.json"
baseline="$work_dir/p13-baseline-manifest.json"
replacement_row_dir="$work_dir/p13-5d-rows"
mkdir -p "$replacement_row_dir"
if O3K_DATABASE_BACKEND=postgres \
   O3K_P13_ALLOW_DESTRUCTIVE_POSTGRES_RESET=1 \
   python3 "$root_dir/scripts/p13_baseline_gate_manifest.py" --output "$baseline" >"$work_dir/baseline.log" 2>&1; then
  baseline_result=verified
else
  baseline_result=blocked
fi

run_status=blocked
native_volume_skip=0
if [[ -z "${O3K_LVM_VOLUME_GROUP:-}" || -z "${O3K_LVM_THIN_POOL:-}" || -z "${O3K_LVM_PROVIDER_NAMESPACE:-}" ]]; then
  native_volume_skip=1
fi
if [[ -x "${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}" ]]; then
  set +e
  O3K_DATABASE_BACKEND=postgres \
  O3K_P13_5B_EVIDENCE_OUTPUT="$b_output" \
  P13_5B_EXPLORATORY=1 \
  P13_5A_RUN_BASELINE=1 \
  P13_5B_BASELINE_RESULT="$baseline_result" \
  P13_5B_BASELINE_MANIFEST="$baseline" \
  O3K_P13_5B_SKIP_NATIVE_VOLUME="$native_volume_skip" \
  P13_5D_RUN=1 \
  O3K_P13_5D_ROW_DIR="$replacement_row_dir" \
  O3K_P13_5B_KEEP_WORK_DIR=1 \
    bash "$root_dir/tests/p13_5b_refresh_import.sh" >"$log" 2>&1
  rc=$?
  set -e
  if [[ $rc -eq 0 ]]; then run_status=passed; else run_status=failed; fi
else
  echo "P13.5F: P13.5B prerequisites unavailable; retaining preflight evidence" >"$log"
fi

run_child() {
  local label="$1" log_path="$2"
  shift 2
  set +e
  env O3K_DATABASE_BACKEND=postgres "$@" >"$log_path" 2>&1
  local rc=$?
  set -e
  [[ $rc -eq 0 ]]
}

c_output="$work_dir/p13-5c-canonical-drift.json"
c_log="$work_dir/p13-5c-canonical-drift.log"
c_status=blocked
if run_child p13-5c "$c_log" env P13_5A_RUN_BASELINE=1 P13_5B_BASELINE_MANIFEST="$baseline" P13_5C_ALLOW_BLOCKED_BASELINE=1 P13_5C_REMOTE_DELETE=1 \
  O3K_P13_5C_OUT_OF_BAND_EVIDENCE_OUTPUT="$c_output" bash "$root_dir/tests/p13_5c_canonical_out_of_band_drift.sh"; then
  c_status=passed
fi

e_dir="$work_dir/p13-5e-evidence"
e_log="$work_dir/p13-5e-fault-proxy.log"
e_status=blocked
mkdir -p "$e_dir"
if run_child p13-5e "$e_log" O3K_P13_EVIDENCE_DIR="$e_dir" bash "$root_dir/tests/p13_5e_real_provider_fault_proxy.sh"; then
  e_status=passed
fi

d_output="$work_dir/p13-5d-replacement.json"
d_log="$work_dir/p13-5d-replacement.log"
d_status=blocked
if python3 - "$d_output" "$replacement_row_dir" "$O3K_P13_SOURCE_HEAD_SHA" >"$d_log" 2>&1 <<'PY'
import json
import pathlib
import sys

output, row_dir, head = sys.argv[1:]
rows = [json.loads(path.read_text(encoding="utf-8")) for path in sorted(pathlib.Path(row_dir).glob("*.json"))]
if not rows:
    raise SystemExit("no executed replacement rows were emitted")
if any(row.get("result") != "passed" for row in rows):
    raise SystemExit("an executed replacement row did not pass")
document = {
    "artifact_type": "o3k-p13-5d-replacement-relationship-evidence",
    "schema_version": 1,
    "phase": "P13.5D",
    "profile": "p13-iac-compatibility-v1",
    "tested_o3k_head_sha": head,
    "provider_modified": False,
    "aggregate_verdict": "PASS",
    "scenarios": rows,
}
pathlib.Path(output).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
then
  d_status=passed
fi

python3 - "$output" "$root_dir" "$run_status" "$b_output" "$log" "$baseline" "$c_status" "$c_output" "$c_log" "$e_status" "$e_dir" "$e_log" "$d_status" "$d_output" "$d_log" <<'PY'
import json
import pathlib
import sys
from datetime import datetime, timezone

output = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
run_status, b_output, log, baseline, c_status, c_output, c_log, e_status, e_dir, e_log, d_status, d_output, d_log = sys.argv[3:]
head = __import__("subprocess").check_output(
    ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
).strip()
source_head = __import__("os").environ.get("O3K_P13_SOURCE_HEAD_SHA", head)

scenarios = [
    "PG1-import-read-reconstruction",
    "PG2-mutable-drift-reconvergence",
    "PG3-remote-deletion-recreation",
    "PG4-independent-replacement",
    "PG5-router-interface-relationship",
    "PG6-volume-attachment-relationship",
    "PG7-operation-replay-unknown-outcome",
]
document = {
    "artifact_type": "o3k-p13-5f-postgres-provider-parity",
    "schema_version": 1,
    "phase": "P13.5F",
    "tested_o3k_head_sha": source_head,
    "generated_at": datetime.now(timezone.utc).isoformat(),
    "backend": "postgresql",
    "provider_modified": False,
    "execution": {
        "orchestrator": "tests/p13_5f_postgres_provider_parity.sh",
        "reused_journey": "tests/p13_5b_refresh_import.sh",
        "status": run_status,
        "evidence": str(pathlib.Path(b_output).resolve()),
        "log": str(pathlib.Path(log).resolve()),
        "baseline_manifest": str(pathlib.Path(baseline).resolve()),
        "child_runs": {
            "P13.5C": {"status": c_status, "evidence": str(pathlib.Path(c_output).resolve()), "log": str(pathlib.Path(c_log).resolve())},
            "P13.5E": {"status": e_status, "evidence": str(pathlib.Path(e_dir).resolve()), "log": str(pathlib.Path(e_log).resolve())},
            "P13.5D": {"status": d_status, "evidence": str(pathlib.Path(d_output).resolve()), "log": str(pathlib.Path(d_log).resolve())},
        },
    },
    "scenarios": [
        {
            "scenario": name,
            "result": "not_run",
            "externally_equivalent": False,
            "reason": "dedicated PostgreSQL parity journey is not implemented or was not executable",
        }
        for name in scenarios
    ],
    "final_verdict": "blocked",
}
if pathlib.Path(b_output).is_file():
    b = json.loads(pathlib.Path(b_output).read_text(encoding="utf-8"))
    passed = {(row.get("resource"), row.get("kind")) for row in b.get("scenarios", []) if row.get("result") == "passed"}
    evidence = str(pathlib.Path(b_output).resolve())
    for row in document["scenarios"]:
        if row["scenario"] == "PG1-import-read-reconstruction" and ("openstack_networking_network_v2", "import") in passed:
            row.update(result="passed", externally_equivalent=True, evidence=evidence)
        # Import/read coverage does not prove relationship lifecycle parity.
        # PG5 and PG6 require their dedicated relationship journeys below;
        # never promote them from a generic import result.
    if all(row["result"] == "passed" for row in document["scenarios"]):
        document["final_verdict"] = "passed"
    c = json.loads(pathlib.Path(c_output).read_text(encoding="utf-8")) if pathlib.Path(c_output).is_file() else {}
    if c_status == "passed" and c.get("scenario", {}).get("result") == "passed":
        document["scenarios"][1].update(result="passed", externally_equivalent=True, evidence=str(pathlib.Path(c_output).resolve()))
        deletion = c.get("scenario", {}).get("remote_deletion_recreation", {})
        if deletion.get("result") == "passed" and deletion.get("old_resource_absent") is True and deletion.get("identity_changed") is True:
            document["scenarios"][2].update(result="passed", externally_equivalent=True, evidence=str(pathlib.Path(c_output).resolve()))
    # P13.5E proves fault-proxy recovery, but does not by itself prove the
    # PostgreSQL durable Operation replay/unknown-outcome contract required by
    # PG7. Keep PG7 blocked until that dedicated journey emits evidence.
    if d_status == "passed" and pathlib.Path(d_output).is_file():
        d = json.loads(pathlib.Path(d_output).read_text(encoding="utf-8"))
        if d.get("status") == "passed":
            document["scenarios"][3].update(result="passed", externally_equivalent=True, evidence=str(pathlib.Path(d_output).resolve()))
        elif d.get("aggregate_verdict") == "PASS":
            replacement_evidence = str(pathlib.Path(d_output).resolve())
            for replacement in d.get("scenarios", []):
                scenario = replacement.get("scenario")
                if scenario == "router-interface":
                    document["scenarios"][4].update(result="passed", externally_equivalent=True, evidence=replacement_evidence)
                elif scenario == "volume-attachment":
                    document["scenarios"][5].update(result="passed", externally_equivalent=True, evidence=replacement_evidence)
                elif scenario == "independent-resource":
                    document["scenarios"][3].update(result="passed", externally_equivalent=True, evidence=replacement_evidence)
            # The locally assembled D artifact uses aggregate_verdict rather
            # than the legacy child-run status field; its rows are the
            # authoritative structured evidence for PG4–PG6.
output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
print(json.dumps({"output": str(output), "status": document["final_verdict"], "execution": run_status}))
PY
python3 "$root_dir/scripts/validate_p13_5f_postgres_provider_parity.py" "$output"
