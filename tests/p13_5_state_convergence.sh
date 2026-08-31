#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="$root_dir/docs/compatibility/p13-5/p13-5a-convergence-contract.json"
tofu="${O3K_P13_TOFU:-}"
python3 - "$contract" <<'PY'
import json, pathlib, sys
d=json.loads(pathlib.Path(sys.argv[1]).read_text())
assert d["artifact_type"] == "o3k-p13-5a-convergence-contract"
assert len(d["resources"]) == 12
required={"resource","canonical_o3k_mapping","import","import_identifier_shape","first_import_read","refresh_read_routes","remote_absence_behavior","mutable_attributes","replacement_attributes","relationship_parents","native_drift_cases","replacement_cases","retry_cases","backend_evidence_requirement","known_bounded_deviations","provenance"}
assert {r["resource"] for r in d["resources"]}.__len__() == 12
assert all(required <= set(r) for r in d["resources"])
assert all(r["import"] in {"required","supported","unsupported","not_applicable"} for r in d["resources"])
assert d["toolchain"]["provider_modified"] is False
assert d["architecture"]["p13_6_boundary_preserved"] is True
print("P13.5A contract structure: PASS")
PY
if [[ -z "$tofu" || -z "${O3K_P13_PROVIDER_ARCHIVE:-}" || -z "${O3K_P13_PROVIDER_BINARY:-}" ]]; then
  echo "P13.5A baseline: BLOCKED (set pinned O3K_P13 tool variables)" >&2
  exit 2
fi
export O3K_P13_TOFU="$tofu"
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
version="$($tofu version | head -n 1)"
[[ "$version" == *"OpenTofu v1.12.6"* ]] || { echo "wrong OpenTofu: $version" >&2; exit 2; }
if [[ "${P13_5A_RUN_BASELINE:-0}" != 1 ]]; then
  echo "P13.5A discovery harness: PASS (baseline execution opt-in)"
  echo "P13.5A convergence claims: NOT CLAIMED"
  exit 0
fi
export O3K_P13_O3KD="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
for gate in tests/p13_2_core_lifecycle.sh tests/p13_2b_subnet_lifecycle.sh tests/p13_2c_port_lifecycle.sh tests/p13_2d_server_lifecycle.sh tests/p13_3_security_group_provider.sh tests/p13_3_security_group_port_provider.sh tests/p13_3_router_provider.sh tests/p13_3_floating_ip_provider.sh tests/p13_4_provider_volume_smoke.sh tests/p13_4_provider_volume_attachment_smoke.sh tests/p13_4_storage_lifecycle.sh; do
  echo "== baseline $gate"
  bash "$root_dir/$gate"
done
echo "P13.5A existing P13 baseline: PASS"
