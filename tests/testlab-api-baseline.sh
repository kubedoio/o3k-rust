#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import json
from pathlib import Path

root = Path.cwd()
baseline = json.loads((root / "docs/specs/testlab-api-baseline.json").read_text())
inventory = json.loads((root / "docs/compatibility/capability-inventory.json").read_text())

assert baseline["status"] == "normative"
assert baseline["release"] == "v0.2.0-testlab"
assert baseline["openstack_compatibility"] == {
    "series": "2026.1",
    "codename": "Gazpacho",
    "selection": "static",
    "specification": "compatibility/openstack-targets.yaml",
    "backward_compatibility": {"series": "2025.2", "codename": "Flamingo"},
}
assert baseline["policies"]["project_paths"]["mismatch_status"] == 404
assert baseline["policies"]["microversions"]["requested_above_baseline"] == 406
assert baseline["policies"]["microversions"]["placement_allocation"] == "1.28"
assert baseline["policies"]["microversions"]["placement_allocation_exact"] is True
assert baseline["policies"]["errors"]["envelope"]["required_fields"] == ["code", "title", "message"]

operations = {operation["id"]: operation for operation in baseline["operations"]}
inventory_ids = {operation["id"] for operation in inventory["operations"]}
assert len(operations) == len(baseline["operations"]), "duplicate baseline operation"
assert set(operations) <= inventory_ids, "baseline references unknown inventory operation"
assert set(baseline["workflow"]) <= set(operations), "workflow references unclassified operation"
assert operations["compute.flavor_collection_list"]["status"] == "required"
assert 405 not in operations["compute.flavor_collection_list"]["errors"]
assert operations["compute.flavor_collection_create"]["status"] == "required"
assert operations["compute.flavor_member_delete"]["status"] == "required"
assert operations["compute.keypair_collection_import"]["status"] == "required"
assert operations["compute.keypair_collection_import"]["issue"] == 293
assert baseline["inventory_scope"]["unlisted_status"] == "unsupported"
assert {"290", "291", "292", "293", "78", "93"} <= set(baseline["issue_map"])
print(f"validated normative baseline: {len(operations)} classified operations, {len(baseline['workflow'])} workflow steps")
PY
