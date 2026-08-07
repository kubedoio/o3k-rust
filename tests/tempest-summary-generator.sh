#!/usr/bin/env bash
set -Eeuo pipefail

# Regression test for the shared Tempest JUnit -> summary converter
# (tests/tempest-summary.py). Feeds synthetic JUnit XML through the same module
# the subset script and the preflight use, and validates counts, test IDs,
# skip reasons, and the honesty contract (zero-test / malformed evidence must
# be rejected, never reported as useful evidence).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUMMARY_SCRIPT="${ROOT_DIR}/tests/tempest-summary.py"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-tempest-summary.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

SELECTED_IDS="tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token,tempest.api.volume.test_volumes_get.VolumesGetTest.test_volume_create_get_update_delete"

# Synthetic JUnit XML: 2 pass, 1 fail, 1 error, 1 skip with a reason.
cat > "${WORK_DIR}/tempest-results.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest" tests="5" failures="1" errors="1" skipped="1">
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token" classname="tempest.api.identity.v3.test_tokens.TokensV3Test"/>
  <testcase name="tempest.api.volume.test_volumes_get.VolumesGetTest.test_volume_create_get_update_delete" classname="tempest.api.volume.test_volumes_get.VolumesGetTest"/>
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token" classname="tempest.api.identity.v3.test_tokens.TokensV3Test">
    <failure message="assertion failed"/>
  </testcase>
  <testcase name="tempest.api.volume.test_volumes_list.VolumesListTestJSON.test_volume_list_with_details" classname="tempest.api.volume.test_volumes_list.VolumesListTestJSON">
    <error message="boom"/>
  </testcase>
  <testcase name="tempest.api.compute.volumes.test_attach_volume.AttachVolumeTestJSON.test_attach_detach_volume" classname="tempest.api.compute.volumes.test_attach_volume.AttachVolumeTestJSON">
    <skipped message="unsupported-operation: compute.create-server-network-uuid-nic deferred"/>
  </testcase>
</testsuite>
XML

python3 "${SUMMARY_SCRIPT}" \
  --junit "${WORK_DIR}/tempest-results.xml" \
  --out "${WORK_DIR}/tempest-cinder-summary.json" \
  --revision "46.0.0" --plugin "1.21.0" \
  --selected "${SELECTED_IDS}" || true

python3 - "${WORK_DIR}/tempest-cinder-summary.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["tempest_revision"] == "46.0.0", doc["tempest_revision"]
assert doc["cinder_tempest_plugin"] == "1.21.0", doc["cinder_tempest_plugin"]
assert doc["status"] == "failed", doc["status"]  # 1 failure + 1 error
assert doc["passed"] == 2, doc["passed"]
assert doc["failed"] == 2, doc["failed"]
assert doc["skipped"] == 1, doc["skipped"]
assert len(doc["selected_test_ids"]) == 2, doc["selected_test_ids"]
assert doc["profile_id"] == "openstack-service-testbed", doc["profile_id"]
statuses = {c["name"]: c["status"] for c in doc["test_cases"]}
assert statuses["tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token"] == "passed"
assert statuses["tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token"] == "failed"
assert statuses["tempest.api.volume.test_volumes_list.VolumesListTestJSON.test_volume_list_with_details"] == "error"
assert statuses["tempest.api.compute.volumes.test_attach_volume.AttachVolumeTestJSON.test_attach_detach_volume"] == "skipped"
assert "unsupported-operation: compute.create-server-network-uuid-nic deferred" in doc["skip_reasons"].values()
print("summary counts/status/IDs/skip-reasons OK")
PY

# A fully passing run reports status passed.
cat > "${WORK_DIR}/tempest-passing.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest" tests="2" failures="0" errors="0" skipped="0">
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token" classname="tempest.api.identity.v3.test_tokens.TokensV3Test"/>
  <testcase name="tempest.api.volume.test_volumes_get.VolumesGetTest.test_volume_create_get_update_delete" classname="tempest.api.volume.test_volumes_get.VolumesGetTest"/>
</testsuite>
XML
python3 "${SUMMARY_SCRIPT}" \
  --junit "${WORK_DIR}/tempest-passing.xml" \
  --out "${WORK_DIR}/tempest-passing.json" \
  --revision "46.0.0" --plugin "1.21.0" \
  --selected "${SELECTED_IDS}"
python3 - "${WORK_DIR}/tempest-passing.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "passed", doc["status"]
assert doc["passed"] == 2, doc["passed"]
assert doc["failed"] == 0, doc["failed"]
print("passing-run status OK")
PY

# Zero-test output must be rejected as harness-error, never reported as pass.
cat > "${WORK_DIR}/tempest-zero.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest" tests="0" failures="0" errors="0" skipped="0"/>
XML
if python3 "${SUMMARY_SCRIPT}" \
  --junit "${WORK_DIR}/tempest-zero.xml" \
  --out "${WORK_DIR}/tempest-zero.json" \
  --revision "46.0.0" --plugin "1.21.0" \
  --selected "${SELECTED_IDS}"; then
  echo "zero-test JUnit was accepted as evidence" >&2
  exit 1
fi
python3 - "${WORK_DIR}/tempest-zero.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "harness-error", doc["status"]
assert doc["passed"] == 0, doc["passed"]
print("zero-test rejection OK")
PY

# Missing/malformed JUnit must be rejected as harness-error.
if python3 "${SUMMARY_SCRIPT}" \
  --junit "${WORK_DIR}/does-not-exist.xml" \
  --out "${WORK_DIR}/tempest-missing.json" \
  --revision "46.0.0" --plugin "1.21.0" \
  --selected "${SELECTED_IDS}"; then
  echo "missing JUnit was accepted as evidence" >&2
  exit 1
fi
python3 - "${WORK_DIR}/tempest-missing.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "harness-error", doc["status"]
print("missing-JUnit rejection OK")
PY

echo "tempest summary generator tests passed"
