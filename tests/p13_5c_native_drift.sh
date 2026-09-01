#!/usr/bin/env bash
set -euo pipefail

# P13.5C matrix/evidence harness. The actual native mutation driver is kept
# injectable: until one exists, this emits explicit blocked rows and never
# fabricates OpenTofu plans or canonical mutations.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${O3K_P13_5C_EVIDENCE_OUTPUT:-$root_dir/target/p13-5c/native-drift-evidence.json}"
if [[ "$output" != /* ]]; then output="$root_dir/$output"; fi
mkdir -p "$(dirname "$output")"

python3 - "$root_dir/docs/compatibility/p13-5/p13-5a-convergence-contract.json" "$output" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys

contract_path = pathlib.Path(sys.argv[1])
output = pathlib.Path(sys.argv[2])
contract = json.loads(contract_path.read_text())
head = subprocess.check_output(["git", "-C", str(contract_path.parents[3]), "rev-parse", "HEAD"], text=True).strip()
rows = []
native_resources = {
    "openstack_compute_instance_v2",
    "openstack_networking_network_v2",
    "openstack_blockstorage_volume_v3",
}
for item in contract["resources"]:
    resource = item["resource"]
    for attribute in item.get("mutable_attributes", []):
        reason = (
            "native_update_route_unavailable: manifest declares update but the native route/application is missing"
            if resource == "openstack_networking_network_v2"
            else "native_update_unsupported: registered native resource has no update operation"
            if resource in native_resources
            else "native_surface_not_defined: no accepted native mutable surface exists for this compatibility projection"
        )
        rows.append({
            "resource": resource,
            "scenario": "native-mutable-drift",
            "native_change": attribute,
            "reason": reason,
        })
    delete_reason = (
        "native_delete_supported_but_not_executed: no real OpenTofu/native mutation run was performed"
        if resource in native_resources
        else "native_surface_not_defined: no accepted native delete surface exists for this compatibility projection"
    )
    rows.append({
        "resource": resource,
        "scenario": "native-delete-drift",
        "native_change": "remote absence",
        "reason": delete_reason,
    })
for row in rows:
    row.update({
        "surface": "native_api",
        "native_surface_status": "native_surface_not_defined" if "native_surface_not_defined" in row["reason"] else "defined",
        "terraform_address": None,
        "canonical_id_before": None,
        "canonical_id_after_native_mutation": None,
        "canonical_id_after_reapply": None,
        "owner_scope": None,
        "refresh_only_actions": [],
        "normal_plan_actions": [],
        "unrelated_changes_count": None,
        "old_resource_absent": None,
        "new_resource_count": None,
        "final_plan_noop": False,
        "backend": "sqlite",
        "head_sha": head,
        "provider_modified": False,
        "result": "blocked",
    })
document = {
    "artifact_type": "o3k-p13-5c-native-drift-evidence",
    "schema_version": 1,
    "phase": "P13.5C",
    "profile": "p13-iac-compatibility-v1",
    "status": "blocked",
    "canonical_authority": "o3k",
    "provider_modified": False,
    "p13_5a_contract_sha256": hashlib.sha256(contract_path.read_bytes()).hexdigest(),
    "tested_o3k_head_sha": head,
    "toolchain": {
        "opentofu": contract["toolchain"]["opentofu"],
        "provider": contract["toolchain"]["provider"],
        "opentofu_archive_sha256": contract["toolchain"]["opentofu_archive_sha256"],
        "provider_archive_sha256": contract["toolchain"]["provider_archive_sha256"],
        "provider_binary_sha256": contract["toolchain"]["provider_binary_sha256"],
        "provider_modified": False,
    },
    "scenarios": rows,
}
output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
print(f"P13.5C evidence written: {output}")
PY

python3 "$root_dir/scripts/validate_p13_5c_evidence.py" --allow-blocked "$output"
echo "P13.5C execution status: BLOCKED (no native mutation driver configured)"
exit 2
