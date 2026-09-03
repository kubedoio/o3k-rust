#!/usr/bin/env bash
set -euo pipefail

# P13.5D real-provider replacement/relationship gate.  This gate is
# intentionally fail-closed: a missing exact-head baseline or toolchain cannot
# produce replacement evidence or an aggregate PASS.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
: "${O3K_P13_PROVIDER_ARCHIVE:?O3K_P13_PROVIDER_ARCHIVE is required}"
: "${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
: "${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
: "${P13_5D_BASELINE_MANIFEST:?P13_5D_BASELINE_MANIFEST is required}"
scenario_filter="${P13_5D_SCENARIO:-all}"
case ",$scenario_filter," in
  *,independent-resource,*|*,router-interface,*|*,volume-attachment,*|*,all,*) ;;
  *) echo "P13.5D invalid P13_5D_SCENARIO: $scenario_filter" >&2; exit 2 ;;
esac
if [[ "$scenario_filter" == all || "$scenario_filter" == *volume-attachment* ]]; then
  : "${O3K_LVM_VOLUME_GROUP:?O3K_LVM_VOLUME_GROUP is required for volume-attachment}"
  : "${O3K_LVM_THIN_POOL:?O3K_LVM_THIN_POOL is required for volume-attachment}"
  : "${O3K_LVM_PROVIDER_NAMESPACE:?O3K_LVM_PROVIDER_NAMESPACE is required for volume-attachment}"
fi

head_sha="$(git -C "$root_dir" rev-parse HEAD)"
python3 - "$P13_5D_BASELINE_MANIFEST" "$head_sha" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
if d.get("status") != "verified" or d.get("source_commit") != sys.argv[2]:
    raise SystemExit("P13.5D requires a verified complete baseline bound to this exact HEAD")
if d.get("gate_count") != 11 or any(g.get("result") != "passed" for g in d.get("gates", [])):
    raise SystemExit("P13.5D baseline is not complete 11/11 PASS")
PY
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
version="$($O3K_P13_TOFU version | head -n1)"
[[ "$version" == *"OpenTofu v1.12.6"* ]] || { echo "wrong OpenTofu: $version" >&2; exit 1; }

output="${O3K_P13_5D_EVIDENCE_OUTPUT:-$root_dir/target/p13-5d/replacement-relationship-evidence.json}"
mkdir -p "$(dirname "$output")"
export P13_5D_RUN=1 P13_5B_RUN=1 P13_5B_EXPLORATORY=1 P13_5B_BASELINE_RESULT=verified P13_5B_BASELINE_MANIFEST="$P13_5D_BASELINE_MANIFEST" P13_5A_RUN_BASELINE=1
export P13_5B_EVIDENCE_OUTPUT="$(dirname "$output")/p13-5b-refresh-import-evidence.json"
export O3K_P13_5D_ROW_DIR="$(mktemp -d /var/tmp/o3k-p13-5d-rows.XXXXXX)"
set +e
bash "$root_dir/tests/p13_5b_refresh_import.sh"
nested_rc=$?
set -e
echo "P13.5B fixture exit=$nested_rc (D validates its replacement rows independently)" >&2
python3 - "$output" "$head_sha" "$O3K_P13_5D_ROW_DIR" "$scenario_filter" <<'PY'
import json, sys
out, head, row_dir, scenario_filter = sys.argv[1:]
from pathlib import Path
rows = []
for path in sorted(Path(row_dir).glob("*.json")):
    row = json.loads(path.read_text())
    if scenario_filter != "all" and row.get("scenario") not in scenario_filter.split(","):
        continue
    rows.append(row)
expected = {"independent-resource", "router-interface", "volume-attachment"} if scenario_filter == "all" else set(scenario_filter.split(","))
if {row.get("scenario") for row in rows} != expected:
    raise SystemExit(f"P13.5D produced scenarios {sorted({row.get('scenario') for row in rows})}; expected {sorted(expected)}")
if any(row.get("result") != "passed" or row.get("parents_preserved") is not True or row.get("provider_leaks") != 0 or row.get("foreign_changes") != 0 or row.get("restart_reconstruction") is not True or row.get("final_plan_noop") is not True for row in rows):
    raise SystemExit("P13.5D replacement row failed required parity invariants")
doc = {
  "artifact_type": "o3k-p13-5d-replacement-relationship-evidence",
  "schema_version": 1, "phase": "P13.5D", "profile": "p13-iac-compatibility-v1",
  "tested_o3k_head_sha": head,
  "toolchain": {"opentofu": "1.12.6", "provider": "terraform-provider-openstack/openstack 3.4.0"},
  "provider_modified": False, "aggregate_verdict": "PASS",
  "selected_scenarios": sorted(expected),
  "scenarios": rows,
}
json.dump(doc, open(out, "w"), indent=2); open(out, "a").write("\n")
PY
echo "P13.5D real-provider replacement/relationship scenarios passed: $output" >&2
rm -rf "$O3K_P13_5D_ROW_DIR"
