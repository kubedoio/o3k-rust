#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script_path="${repo_root}/tests/$(basename "${BASH_SOURCE[0]}")"
artifact="${repo_root}/docs/compatibility/traceability.yaml"
if [[ "${1:-}" == "--artifact" ]]; then
  artifact="${2:?missing artifact path}"
fi

python3 - "${repo_root}" "${artifact}" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
artifact_path = pathlib.Path(sys.argv[2])

def load(relative):
    return json.loads((root / relative).read_text(encoding="utf-8"))

trace = json.loads(artifact_path.read_text(encoding="utf-8"))
baseline = load("docs/specs/testlab-api-baseline.json")
inventory = load("docs/compatibility/capability-inventory.json")
fixtures = load("docs/compatibility/contract-fixtures.json")

assert trace["schema_version"] == 1
assert trace["format"] == "json-compatible-yaml-1.2"
assert trace["protected_evidence_policy"] == "not-claimed"
assert trace["sources"] == {
    "baseline": "docs/specs/testlab-api-baseline.json",
    "inventory": "docs/compatibility/capability-inventory.json",
    "contract_fixtures": "docs/compatibility/contract-fixtures.json",
}

baseline_by_id = {item["id"]: item for item in baseline["operations"]}
inventory_by_id = {item["id"]: item for item in inventory["operations"]}
fixture_ids = {item["id"] for item in fixtures["fixtures"]}
assert len(baseline_by_id) == len(baseline["operations"]), "duplicate baseline operation"
assert len(inventory_by_id) == len(inventory["operations"]), "duplicate inventory operation"

requirements = trace["requirements"]
assert len(requirements) == len(baseline_by_id), "traceability must cover every baseline operation"
assert len({item["baseline_id"] for item in requirements}) == len(requirements), "duplicate traceability row"
assert {item["baseline_id"] for item in requirements} == set(baseline_by_id)

for item in requirements:
    baseline_id = item["baseline_id"]
    inventory_id = item["inventory_id"]
    baseline_item = baseline_by_id[baseline_id]
    inventory_item = inventory_by_id.get(inventory_id)
    assert inventory_item is not None, f"{baseline_id} references unknown inventory id"
    assert baseline_id == inventory_id, f"{baseline_id} has a non-identity inventory link"
    assert item["baseline_status"] == baseline_item["status"]
    assert item["inventory_release_relevance"] == inventory_item["release_relevance"]
    evidence = item["evidence"]
    assert evidence["implementation"] == inventory_item["implementation_state"]
    assert evidence["portable_contract"] == inventory_item["portable_contract"]
    assert evidence["cli"] == inventory_item["cli_verification"]
    assert evidence["protected_runner"] == "not-claimed"
    assert set(evidence) == {"implementation", "portable_contract", "cli", "protected_runner"}
    for reference in item["contract_tests"]:
        prefix, separator, fixture_id = reference.partition("#")
        assert prefix == "tests/compatibility-harness.py" and separator and fixture_id
        assert fixture_id == baseline_id
        assert fixture_id in fixture_ids, f"unknown contract fixture {fixture_id}"

excluded = {item["inventory_id"] for item in trace["inventory_exclusions"]}
assert excluded == set(inventory_by_id) - set(baseline_by_id)
assert all(item["reason"] for item in trace["inventory_exclusions"])
referenced_fixtures = {
    reference.partition("#")[2]
    for item in requirements
    for reference in item["contract_tests"]
}
assert referenced_fixtures == fixture_ids, "contract fixture coverage is not traceable"
assert "protected-runner-verified" not in artifact_path.read_text(encoding="utf-8")
print(f"validated traceability: {len(requirements)} baseline operations, {len(fixture_ids)} contract fixtures, protected evidence not claimed")
PY

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-traceability.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT
cp -- "${artifact}" "${temp_dir}/mutated.yaml"
python3 - "${temp_dir}/mutated.yaml" <<'PY'
import json
import sys

path = sys.argv[1]
data = json.loads(open(path, encoding="utf-8").read())
data["requirements"][0]["inventory_id"] = "missing.operation"
open(path, "w", encoding="utf-8").write(json.dumps(data, indent=2) + "\n")
PY
if bash "${script_path}" --artifact "${temp_dir}/mutated.yaml" >/dev/null 2>&1; then
  echo "traceability validator accepted a broken inventory link" >&2
  exit 1
fi
