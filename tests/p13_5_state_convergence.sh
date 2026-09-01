#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="$root_dir/docs/compatibility/p13-5/p13-5a-convergence-contract.json"
tofu="${O3K_P13_TOFU:-}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:-}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:-}"
provider_binary="${O3K_P13_PROVIDER_BINARY:-}"
provider_sha="${O3K_P13_PROVIDER_SHA256:-}"
python3 - "$contract" <<'PY'
import json, pathlib, sys
d=json.loads(pathlib.Path(sys.argv[1]).read_text())
assert d["artifact_type"] == "o3k-p13-5a-convergence-contract"
assert len(d["resources"]) == 12
required={"resource","canonical_o3k_mapping","import","import_identifier_shape","first_import_read","refresh_read_routes","remote_absence_behavior","mutable_attributes","replacement_attributes","relationship_parents","native_drift_cases","replacement_cases","retry_cases","backend_evidence_requirement","known_bounded_deviations","provenance"}
assert {r["resource"] for r in d["resources"]}.__len__() == 12
assert all(required <= set(r) for r in d["resources"])
computed=d["computed_defaulted_normalized_attributes"]
assert all(isinstance(r.get("computed_defaulted_normalized_attributes", computed.get(r["resource"])), list) for r in d["resources"])
assert d["starting_protected_main_sha"] == d["o3k_head_sha"]
assert d["status"] in {"contract_frozen_baseline_blocked", "baseline_verified"}
assert all(r["import"] in {"required","supported","unsupported","not_applicable"} for r in d["resources"])
assert d["toolchain"]["provider_modified"] is False
assert d["architecture"]["p13_6_boundary_preserved"] is True
print("P13.5A contract structure: PASS")
PY
if [[ "${P13_5B_SELF_TEST:-0}" == 1 ]]; then
  python3 "$root_dir/scripts/validate_p13_5b_evidence.py" --self-test
  echo "P13.5B harness self-test: PASS"
  exit 0
fi
if [[ -z "$tofu" || -z "$tofu_archive" || -z "$provider_archive" || -z "$provider_binary" || -z "$provider_sha" ]]; then
  echo "P13.5A baseline: BLOCKED (set O3K_P13_TOFU, O3K_P13_TOFU_ARCHIVE, O3K_P13_PROVIDER_ARCHIVE, O3K_P13_PROVIDER_BINARY, and O3K_P13_PROVIDER_SHA256)" >&2
  exit 2
fi
export O3K_P13_TOFU="$tofu"
if ! python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools; then
  echo "P13.5A baseline: BLOCKED (tool provenance verification failed)" >&2
  exit 2
fi
if ! version="$("$tofu" version | head -n 1)"; then
  echo "P13.5A baseline: BLOCKED (OpenTofu executable could not be run)" >&2
  exit 2
fi
[[ "$version" == *"OpenTofu v1.12.6"* ]] || { echo "wrong OpenTofu: $version" >&2; exit 2; }
if [[ "${P13_5A_RUN_BASELINE:-0}" != 1 ]]; then
  [[ "${P13_5B_RUN:-0}" != 1 ]] || {
    echo "P13.5B BLOCKED: run the parent harness with P13_5A_RUN_BASELINE=1 so the existing gates execute first" >&2
    exit 2
  }
  echo "P13.5A discovery harness: PASS (baseline execution opt-in)"
  echo "P13.5A convergence claims: NOT CLAIMED"
  exit 0
fi
export O3K_P13_O3KD="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
export O3K_P13_PASSWORD="${O3K_P13_PASSWORD:-p13-5-baseline-password}"
baseline_manifest="${P13_5B_BASELINE_MANIFEST:-$(mktemp /var/tmp/o3k-p13-5b-baseline.XXXXXX.json)}"
if ! python3 "$root_dir/scripts/p13_baseline_gate_manifest.py" --output "$baseline_manifest"; then
  echo "P13.2-P13.4 baseline: BLOCKED (see $baseline_manifest and per-gate logs)" >&2
  exit 2
fi
if [[ "${P13_5B_RUN:-0}" == 1 ]]; then
  export P13_5B_BASELINE_RESULT=verified
  export P13_5B_BASELINE_MANIFEST="$baseline_manifest"
  bash "$root_dir/tests/p13_5b_refresh_import.sh"
  exit $?
fi
if [[ "${P13_5C_RUN:-0}" == 1 ]]; then
  bash "$root_dir/tests/p13_5c_canonical_out_of_band_drift.sh"
  python3 "$root_dir/scripts/validate_p13_5c_evidence.py" --canonical-evidence \
    "${O3K_P13_5C_OUT_OF_BAND_EVIDENCE_OUTPUT:-$root_dir/target/p13-5c/canonical-out-of-band-drift-evidence.json}"
  # Native-surface rows remain fail-closed until each supported native delete
  # has real provider evidence; a blocked native run must not become aggregate PASS.
  bash "$root_dir/tests/p13_5c_native_drift.sh"
  exit $?
fi
echo "P13.5A existing P13 baseline: PASS"
