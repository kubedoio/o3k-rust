#!/usr/bin/env bash
set -Eeuo pipefail

# Regression test for the Tempest JUnit summary generator embedded in
# tests/tempest-cinder-subset.sh. Feeds a synthetic JUnit XML through the same
# python block the script uses and validates the produced
# tempest-cinder-summary.json counts, test IDs, and skip reasons.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-tempest-summary.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

# Synthetic JUnit XML: 1 pass, 1 fail, 1 error, 1 skip with a reason.
cat > "${WORK_DIR}/tempest-results.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest" tests="4" failures="1" errors="1" skipped="1">
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_token_create_delete" classname="tempest.api.identity.v3.test_tokens.TokensV3Test"/>
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token" classname="tempest.api.identity.v3.test_tokens.TokensV3Test">
    <failure message="assertion failed"/>
  </testcase>
  <testcase name="tempest.api.volume.test_volumes.VolumesV3Test.test_volume_show" classname="tempest.api.volume.test_volumes.VolumesV3Test">
    <error message="boom"/>
  </testcase>
  <testcase name="tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_detach_volume" classname="tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest">
    <skipped message="unsupported-operation: external-cinder-attach deferred"/>
  </testcase>
</testsuite>
XML

SELECTED_IDS="tempest.api.identity.v3.test_tokens.TokensV3Test.test_token_create_delete,tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token,tempest.api.volume.test_volumes.VolumesV3Test.test_volume_show,tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_detach_volume"

# Invoke the same python block from the script (inlined here to avoid sourcing
# the whole runner, which would execute side effects).
python3 - "${WORK_DIR}" "46.0.0" "1.21.0" "${SELECTED_IDS}" <<'PY'
import json, sys, pathlib
out_dir, tempest_pin, plugin_pin, selected_ids = sys.argv[1:]
import xml.etree.ElementTree as ET

results_xml = pathlib.Path(out_dir) / "tempest-results.xml"
tests = failures = errors = skipped = 0
test_cases = []
skip_reasons = {}
if results_xml.exists():
    try:
        tree = ET.parse(results_xml)
        root = tree.getroot()
        tests = int(root.attrib.get("tests", 0))
        failures = int(root.attrib.get("failures", 0))
        errors = int(root.attrib.get("errors", 0))
        skipped = int(root.attrib.get("skipped", 0))
        for case in root.iter("testcase"):
            name = case.attrib.get("name", "")
            status = "passed"
            reason = ""
            if case.find("failure") is not None:
                status = "failed"
            elif case.find("error") is not None:
                status = "error"
            elif case.find("skipped") is not None:
                status = "skipped"
                skip = case.find("skipped")
                reason = (skip.attrib.get("message") or skip.text or "").strip()
            test_cases.append({"name": name, "status": status})
            if status == "skipped" and reason:
                skip_reasons[name] = reason
    except Exception:
        tests = failures = errors = skipped = -1

summary = {
    "tempest_revision": tempest_pin,
    "cinder_tempest_plugin": plugin_pin,
    "selected_test_ids": [tid for tid in selected_ids.split(",") if tid],
    "passed": max(tests - failures - errors - skipped, 0),
    "failed": failures + errors,
    "skipped": skipped,
    "profile_id": "openstack-service-testbed",
    "o3k_commit": "<recorded in workflow>",
    "cinder_version": "<recorded by runner>",
    "skip_reasons": skip_reasons,
    "test_cases": test_cases,
}
with open(pathlib.Path(out_dir) / "tempest-cinder-summary.json", "w") as f:
    json.dump(summary, f, indent=2)
PY

python3 - "${WORK_DIR}/tempest-cinder-summary.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["tempest_revision"] == "46.0.0", doc["tempest_revision"]
assert doc["cinder_tempest_plugin"] == "1.21.0", doc["cinder_tempest_plugin"]
assert doc["passed"] == 1, doc["passed"]
assert doc["failed"] == 2, doc["failed"]
assert doc["skipped"] == 1, doc["skipped"]
assert len(doc["selected_test_ids"]) == 4, doc["selected_test_ids"]
assert doc["profile_id"] == "openstack-service-testbed", doc["profile_id"]
statuses = {c["name"]: c["status"] for c in doc["test_cases"]}
assert statuses["tempest.api.identity.v3.test_tokens.TokensV3Test.test_token_create_delete"] == "passed"
assert statuses["tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token"] == "failed"
assert statuses["tempest.api.volume.test_volumes.VolumesV3Test.test_volume_show"] == "error"
assert statuses["tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_detach_volume"] == "skipped"
assert "unsupported-operation: external-cinder-attach deferred" in doc["skip_reasons"].values()
print("tempest summary generator tests passed")
PY
