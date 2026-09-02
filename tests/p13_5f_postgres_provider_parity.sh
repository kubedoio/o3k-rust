#!/usr/bin/env bash
set -euo pipefail

# PostgreSQL provider-level parity orchestrator.  The existing P13.5B journey
# is deliberately reused so the OpenTofu/provider boundary is identical to the
# portable evidence path.  This wrapper never promotes an unavailable or
# failed journey to PASS; callers receive a machine-readable blocked artifact.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${O3K_P13_5F_POSTGRES_EVIDENCE_OUTPUT:-$root_dir/target/p13-5f/postgres-provider-parity.json}"
mkdir -p "$(dirname "$output")"

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

work_dir="${O3K_P13_5F_WORK_DIR:-$(mktemp -d /var/tmp/o3k-p13-5f-postgres.XXXXXX)}"
mkdir -p "$work_dir"
log="$work_dir/p13-5b-refresh-import.log"
b_output="$work_dir/p13-5b-refresh-import-evidence.json"
baseline="$work_dir/p13-baseline-manifest.json"
if O3K_DATABASE_BACKEND=postgres \
   O3K_P13_ALLOW_DESTRUCTIVE_POSTGRES_RESET=1 \
   python3 "$root_dir/scripts/p13_baseline_gate_manifest.py" --output "$baseline" >"$work_dir/baseline.log" 2>&1; then
  baseline_result=verified
else
  baseline_result=blocked
fi

run_status=blocked
if [[ -x "${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}" ]] && [[ -n "${O3K_LVM_VOLUME_GROUP:-}" && -n "${O3K_LVM_THIN_POOL:-}" && -n "${O3K_LVM_PROVIDER_NAMESPACE:-}" ]]; then
  set +e
  O3K_DATABASE_BACKEND=postgres \
  O3K_P13_5B_EVIDENCE_OUTPUT="$b_output" \
  P13_5B_EXPLORATORY=1 \
  P13_5A_RUN_BASELINE=1 \
  P13_5B_BASELINE_RESULT="$baseline_result" \
  P13_5B_BASELINE_MANIFEST="$baseline" \
  O3K_P13_5B_KEEP_WORK_DIR=1 \
    "$root_dir/tests/p13_5b_refresh_import.sh" >"$log" 2>&1
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
if run_child p13-5c "$c_log" env P13_5A_RUN_BASELINE=1 P13_5B_BASELINE_MANIFEST="$baseline" \
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

python3 - "$output" "$root_dir" "$run_status" "$b_output" "$log" "$baseline" "$c_status" "$c_output" "$e_status" "$e_dir" <<'PY'
import json
import pathlib
import sys
from datetime import datetime, timezone

output = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
run_status, b_output, log, baseline, c_status, c_output, e_status, e_dir = sys.argv[3:]
head = __import__("subprocess").check_output(
    ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
).strip()

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
    "tested_o3k_head_sha": head,
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
if run_status == "passed" and pathlib.Path(b_output).is_file():
    b = json.loads(pathlib.Path(b_output).read_text(encoding="utf-8"))
    passed = {(row.get("resource"), row.get("kind")) for row in b.get("scenarios", []) if row.get("result") == "passed"}
    evidence = str(pathlib.Path(b_output).resolve())
    for row in document["scenarios"]:
        if row["scenario"] == "PG1-import-read-reconstruction" and any(kind == "import" for _, kind in passed):
            row.update(result="passed", externally_equivalent=True, evidence=evidence)
        elif row["scenario"] == "PG5-router-interface-relationship" and ("openstack_networking_router_interface_v2", "import") in passed:
            row.update(result="passed", externally_equivalent=True, evidence=evidence)
        elif row["scenario"] == "PG6-volume-attachment-relationship" and ("openstack_compute_volume_attach_v2", "import") in passed:
            row.update(result="passed", externally_equivalent=True, evidence=evidence)
    if all(row["result"] == "passed" for row in document["scenarios"]):
        document["final_verdict"] = "passed"
    c = json.loads(pathlib.Path(c_output).read_text(encoding="utf-8")) if pathlib.Path(c_output).is_file() else {}
    if c_status == "passed" and c.get("scenario", {}).get("result") == "passed":
        document["scenarios"][1].update(result="passed", externally_equivalent=True, evidence=str(pathlib.Path(c_output).resolve()))
    if e_status == "passed":
        document["scenarios"][6].update(result="passed", externally_equivalent=True, evidence=str(pathlib.Path(e_dir).resolve()))
output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
print(json.dumps({"output": str(output), "status": document["final_verdict"], "execution": run_status}))
PY
python3 "$root_dir/scripts/validate_p13_5f_postgres_provider_parity.py" "$output"
