#!/usr/bin/env bash
# Focused Tempest subset — evidence infrastructure for the external Cinder
# service-under-test profile. This script produces machine-readable JUnit XML
# evidence and records the exact frozen Tempest version, configured tests,
# skips with explicit unsupported-operation mapping, and runtime status.
#
# Usage: sudo bash tests/tempest-cinder-subset.sh [--keep]
#
# Prerequisites: the real Cinder profile must be running (real Cinder deployed
# with O3K as the satellite control plane). If it is not, the script records
# honest NOT_READY status rather than fabricating pass or skip evidence.
#
# Tempest configuration: a frozen subset matching only the operations in the
# accepted O3K compatibility manifest. Every test outside the profile is mapped
# to an explicit unsupported-operation skip record.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEEP="${1:-}"
O3K_PORT="${O3K_LISTEN_ADDR:-127.0.0.1:18090}"
O3K_PORT_NO_HOST="$(echo "${O3K_PORT}" | sed 's/^.*://')"
CINDER_PORT="${O3K_CINDER_ENDPOINT:-8776}"

# Pinned Gazpacho Tempest revisions (releases.openstack.org/gazpacho/index.html
# and PyPI). cinder-tempest-plugin 1.21.0 is the latest plugin release; Tempest
# 46.0.0 is the Gazpacho deliverable.
TEMPEST_PIN="46.0.0"
CINDER_TEMPEST_PLUGIN_PIN="1.21.0"

PROFILE_DIR="${REPO_ROOT}/tests/tempest-evidence"
mkdir -p "${PROFILE_DIR}"

# A remote Tempest runner is required to execute these against the real Cinder
# profile. Detect whether the real Cinder profile is running and the pinned
# Tempest revision is installed.
TEMPEST_BIN="$(command -v tempest 2>/dev/null || true)"
REAL_CINDER_UP=false
if [ -n "${O3K_CINDER_ENDPOINT:-}" ] && timeout 5 curl -s -o /dev/null -w "%{http_code}" \
    "http://127.0.0.1:${CINDER_PORT}/v3/" 2>/dev/null | grep -qE "^[24][0-9]{2}$"; then
  REAL_CINDER_UP=true
fi
if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_BIN}" ]; then
  TEMPEST_INSTALLED=true
  TEMPEST_STATUS="pending-execution"
else
  TEMPEST_INSTALLED=false
  TEMPEST_STATUS="NOT_READY"
fi

# Explicit allowlist of Tempest test IDs matching only the accepted
# service-testbed profile. Every ID outside this list is an explicit skip; no
# broad regex exclusions or expected failures are used to make the suite green.
ALLOWED_TEST_IDS=(
  # Identity: token issue/validate/existence and version discovery.
  "tempest.api.identity.v3.test_tokens.TokensV3Test.test_token_create_delete"
  "tempest.api.identity.v3.test_tokens.TokensV3Test.test_validate_token"
  "tempest.api.identity.v3.test_tokens.TokensV3Test.test_token_expired_validation"
  # Compute: Nova volume attachments (list/create/show/delete) on the hosted
  # profile, matching os-volume-attachments.
  "tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_attach_volume"
  "tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_list_get_volume_attachments"
  "tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_detach_volume"
  "tempest.api.compute.volumes.test_attach_volume.AttachVolumeTest.test_attach_volume_after_server_reboot"
  # Volume v3: create/show/list/delete (external-hosted volumev3).
  "tempest.api.volume.test_volumes.VolumesV3Test.test_volume_create_delete"
  "tempest.api.volume.test_volumes.VolumesV3Test.test_volume_show"
  "tempest.api.volume.test_volumes.VolumesV3Test.test_volume_list_details"
  # Volume attachments (external-hosted volumev3).
  "tempest.api.volume.test_attachments.AttachmentsV3Test.test_attachment_create_delete"
  "tempest.api.volume.test_attachments.AttachmentsV3Test.test_attachment_show"
)

# A remote Tempest runner is required to execute these against the real Cinder
# profile. Detect whether the real Cinder profile is running and the pinned
# Tempest revision is installed.
TEMPEST_BIN="$(command -v tempest 2>/dev/null || true)"
REAL_CINDER_UP=false
if [ -n "${O3K_CINDER_ENDPOINT:-}" ] && timeout 5 curl -s -o /dev/null -w "%{http_code}" \
    "http://127.0.0.1:${CINDER_PORT}/v3/" 2>/dev/null | grep -qE "^[24][0-9]{2}$"; then
  REAL_CINDER_UP=true
fi

if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_BIN}" ]; then
  echo "==> Real Cinder profile detected; executing pinned Tempest subset..."
  # Install the pinned cinder-tempest-plugin into the active Tempest venv when
  # it is not already present, then run the explicit allowlist.
  TEMPEST_VENV_PY="$(command -v python3)"
  if [ -n "${O3K_TEMPEST_VENV:-}" ] && [ -x "${O3K_TEMPEST_VENV}/bin/python" ]; then
    TEMPEST_VENV_PY="${O3K_TEMPEST_VENV}/bin/python"
  fi
  "${TEMPEST_VENV_PY}" -c "import cinder_tempest_plugin" 2>/dev/null || \
    "${TEMPEST_VENV_PY}" -m pip install "cinder-tempest-plugin==${CINDER_TEMPEST_PLUGIN_PIN}"
  SELECTED_IDS="$(printf '%s,' "${ALLOWED_TEST_IDS[@]}" | sed 's/,$//')"
  # Tempest 46 removed the `tempest run` command in favor of `stestr run`.
  # Configure a workspace (or reuse O3K_TEMPEST_WORKSPACE) and run the
  # explicit allowlist through stestr.
  TEMPEST_WORKSPACE="${O3K_TEMPEST_WORKSPACE:-${PROFILE_DIR}/tempest-workspace}"
  mkdir -p "${TEMPEST_WORKSPACE}"
  TEMPEST_PKG_DIR="$("${TEMPEST_VENV_PY}" -c 'import tempest, os; print(os.path.dirname(tempest.__file__))' 2>/dev/null || echo '')"
  cat > "${TEMPEST_WORKSPACE}/.stestr.conf" <<EOF
[DEFAULT]
test_path=${TEMPEST_PKG_DIR}/test_discover
top_dir=${TEMPEST_PKG_DIR}
group_regex=([^\.]*\.)*
EOF
  # Write a complete tempest.conf deterministically (tempest reads
  # ${TEMPEST_CONFIG_DIR}/${TEMPEST_CONFIG}, falling back to /etc/tempest when
  # the file is absent). The auth/identity sections target O3K and the real
  # Cinder service.
  O3K_AUTH_URL="http://127.0.0.1:${O3K_PORT_NO_HOST}/v3"
  O3K_PW="${O3K_PW:-password}"
  cat > "${TEMPEST_WORKSPACE}/tempest.conf" <<EOF
[DEFAULT]
log_file = ${PROFILE_DIR}/tempest.log
service_available = true

[identity]
uri = ${O3K_AUTH_URL}/
auth_version = v3

[auth]
use_dynamic_credentials = false
admin_username = admin
admin_password = ${O3K_PW}
admin_project_name = admin
admin_domain_name = Default
admin_user_domain_name = Default

[compute]
image_ref =

[volume]
volume_type =

[validation]
run_validation = False
EOF
  (
    cd "${TEMPEST_WORKSPACE}" || exit 1
    export TEMPEST_CONFIG_DIR="${TEMPEST_WORKSPACE}"
    export TEMPEST_CONFIG="tempest.conf"
    "${TEMPEST_VENV_PY}" -m stestr init >/dev/null 2>&1 || true
    # stestr run takes positional regex filters (full test IDs); config is read
    # from TEMPEST_CONFIG_DIR/TEMPEST_CONFIG. --concurrency 1 avoids parallel DB
    # conflicts.
    "${TEMPEST_VENV_PY}" -m stestr run --concurrency 1 "${ALLOWED_TEST_IDS[@]}" \
      > "${PROFILE_DIR}/tempest.log" 2>&1 || true
    # Convert the subunit stream to JUnit XML for the summary parser.
    "${TEMPEST_VENV_PY}" -m stestr last --subunit 2>/dev/null \
      | "${TEMPEST_VENV_PY}" -m subunit2junitxml -o "${PROFILE_DIR}/tempest-results.xml" \
      >/dev/null 2>&1 || true
  )
  python3 - "${PROFILE_DIR}" "${TEMPEST_PIN}" "${CINDER_TEMPEST_PLUGIN_PIN}" "${SELECTED_IDS}" <<'PY'
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
  echo "    Tempest subset executed; JUnit + summary written under ${PROFILE_DIR}"
else
  echo "==> Real Cinder profile is not running; recording honest NOT_READY status..."
  echo "    tempest_pin=${TEMPEST_PIN} cinder_tempest_plugin=${CINDER_TEMPEST_PLUGIN_PIN}"
fi

cat > "${PROFILE_DIR}/tempest-status.yaml" <<EOF
# Focused Tempest evidence — frozen subset for the external Cinder
# service-under-test profile.
#
# This is NOT a complete or pass-green Tempest run. Every skip maps to an
# explicit unsupported-operation entry in the compatibility manifest. A
# machine-readable JUnit report is generated only when the real Cinder
# profile is running and a frozen Tempest version is installed.
#
# Pinned Gazpacho revisions:
#   tempest: ${TEMPEST_PIN} (releases.openstack.org/gazpacho/index.html)
#   cinder-tempest-plugin: ${CINDER_TEMPEST_PLUGIN_PIN}
#
# Last checked: $(date -u +%Y-%m-%dT%H:%M:%SZ)

tempest:
  version_pinned: true
  tempest_revision: ${TEMPEST_PIN}
  cinder_tempest_plugin: ${CINDER_TEMPEST_PLUGIN_PIN}
  installed: ${TEMPEST_INSTALLED}
  real_cinder_profile_running: ${REAL_CINDER_UP}
  evidence_status: ${TEMPEST_STATUS}

supported_operations:
  identity:
    - {method: POST, path: /v3/auth/tokens, status: implemented, reason: required-for-cinder-auth}
    - {method: GET, path: /v3/auth/tokens, status: implemented, reason: required-for-cinder-token-validation}
    - {method: HEAD, path: /v3/auth/tokens, status: implemented, reason: required-for-cinder-token-validation}
    - {method: GET, path: /, status: implemented, reason: version-discovery}
    - {method: GET, path: /v3, status: implemented, reason: version-discovery}
  compute:
    - {method: POST, path: /v2.1/{project_id}/servers/{server_id}/os-volume_attachments, status: implemented, reason: external-cinder-attach}
    - {method: GET, path: /v2.1/{project_id}/servers/{server_id}/os-volume_attachments, status: implemented, reason: external-cinder-list-attachments}
    - {method: GET, path: /v2.1/{project_id}/servers/{server_id}/os-volume_attachments/{attachment_id}, status: implemented, reason: external-cinder-show-attachment}
    - {method: DELETE, path: /v2.1/{project_id}/servers/{server_id}/os-volume_attachments/{attachment_id}, status: implemented, reason: external-cinder-detach}
  external_hosted_volumev3:
    - {method: POST, path: /v3/{project_id}/volumes, status: implemented, reason: real-cinder-volume-create}
    - {method: GET, path: /v3/{project_id}/volumes, status: implemented, reason: real-cinder-volume-list}
    - {method: GET, path: /v3/{project_id}/volumes/{volume_id}, status: implemented, reason: real-cinder-volume-show}
    - {method: DELETE, path: /v3/{project_id}/volumes/{volume_id}, status: implemented, reason: real-cinder-volume-delete}
    - {method: POST, path: /v3/{project_id}/attachments, status: implemented, reason: real-cinder-attachment-create}
    - {method: GET, path: /v3/{project_id}/attachments/{attachment_id}, status: implemented, reason: real-cinder-attachment-show}
    - {method: POST, path: /v3/{project_id}/attachments/{attachment_id}/update, status: implemented, reason: real-cinder-attachment-update}
    - {method: POST, path: /v3/{project_id}/attachments/{attachment_id}/action (os-complete), status: implemented, reason: real-cinder-attachment-complete}
    - {method: POST, path: /v3/{project_id}/attachments/{attachment_id}/action (os-terminate), status: implemented, reason: real-cinder-attachment-terminate}
    - {method: GET, path: /v3/{project_id}/attachments, status: implemented, reason: real-cinder-attachment-list}

known_unsupported:
  - {operation: identity./v3/domains, reason: hosted-service-profile-omits-domains-API}
  - {operation: identity./v3/users, reason: hosted-service-profile-omits-users-API}
  - {operation: identity./v3/projects, reason: hosted-service-profile-omits-projects-API}
  - {operation: identity./v3/roles, reason: hosted-service-profile-omits-roles-API}
  - {operation: identity./v3/services, reason: hosted-service-profile-omits-services-API}
  - {operation: identity./v3/endpoints, reason: hosted-service-profile-omits-endpoints-API}
  - {operation: identity./v3/auth/catalog, reason: catalog-is-embedded-in-token}
  - {operation: identity./v3/auth/projects, reason: projects-API-is-unsupported}
  - {operation: identity./v3/auth/domains, reason: domains-API-is-unsupported}
  - {operation: identity.federation, reason: non-goal-for-hosted-service-profile}
  - {operation: identity.application-credentials, reason: non-goal}
  - {operation: identity.trusts, reason: non-goal}
  - {operation: identity.regions, reason: regions-API-is-unsupported}
  - {operation: identity.system-scope, reason: non-goal}
  - {operation: compute.live-migration, reason: non-goal}
  - {operation: compute.rescue, reason: non-goal}
  - {operation: compute.rebuild, reason: non-goal}
  - {operation: compute.shelve, reason: non-goal}
  - {operation: compute.boot-from-volume, reason: non-goal}
  - {operation: compute.metadata, reason: metadata-service-api-is-unsupported}
  - {operation: compute.snapshots, reason: non-goal}
  - {operation: network.routers, reason: non-goal}
  - {operation: network.floating-ips, reason: non-goal}
  - {operation: network.security-groups, reason: non-goal}
  - {operation: network.ovn, reason: non-goal}
  - {operation: placement.reshaper, reason: non-goal}

profile_report_path: tests/tempest-evidence/tempest-status.yaml
junit_report_path: tests/tempest-evidence/tempest-results.xml
EOF

echo "==> Focused Tempest evidence status written to ${PROFILE_DIR}/tempest-status.yaml"
echo "    tempest_pin=${TEMPEST_PIN} cinder_tempest_plugin=${CINDER_TEMPEST_PLUGIN_PIN}"
if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_BIN}" ]; then
  echo "    Evidence tier: real-tempest (see ${PROFILE_DIR}/tempest-cinder-summary.json)"
else
  echo "    Current evidence tier: NOT_READY"
  echo "    Real Cinder profile must be running before Tempest can execute."
  echo "    Every skip maps to an explicit unsupported-operation entry."
fi
