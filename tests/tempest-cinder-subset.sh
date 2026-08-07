#!/usr/bin/env bash
# Focused Tempest subset — evidence infrastructure for the external Cinder
# service-under-test profile (Gate C). This script produces machine-readable
# JUnit XML evidence and records the exact frozen Tempest version, configured
# tests, skips with explicit unsupported-operation mapping, and runtime status.
#
# Usage: sudo bash tests/tempest-cinder-subset.sh [--keep]
#
# Prerequisites: the real Cinder profile must be running (real Cinder deployed
# with O3K as the satellite control plane) and a DEDICATED Tempest virtualenv
# must be valid at O3K_TEMPEST_VENV. If they are not, the script records honest
# NOT_READY / harness-error status rather than fabricating pass or skip
# evidence. Tempest and Cinder never share a virtualenv; every Tempest binary
# is invoked explicitly through O3K_TEMPEST_VENV.
#
# Environment:
#   O3K_TEMPEST_VENV               dedicated Tempest venv (REQUIRED for execution)
#   O3K_TEMPEST_WORKSPACE          workspace (default: tests/tempest-evidence/tempest-workspace)
#   O3K_TEMPEST_PIN                tempest pin (default 46.0.0)
#   O3K_CINDER_TEMPEST_PLUGIN_PIN  plugin pin (default 1.21.0)
#   O3K_CINDER_ENDPOINT            Cinder port (from the runner)
#   O3K_LISTEN_ADDR                O3K listen address (default 127.0.0.1:18090)
#   O3K_PW                         admin password for [auth] overrides
#
# The allowlist is the single source of truth in
# tests/tempest-evidence/tempest-allowlist.txt (validated by the preflight).

set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEEP="${1:-}"
O3K_LISTEN_ADDR="${O3K_LISTEN_ADDR:-127.0.0.1:18090}"
O3K_PORT_NO_HOST="$(echo "${O3K_LISTEN_ADDR}" | sed 's/^.*://')"
CINDER_PORT="${O3K_CINDER_ENDPOINT:-8776}"

# Pinned Gazpacho Tempest revisions (releases.openstack.org/gazpacho/index.html
# and PyPI). cinder-tempest-plugin 1.21.0 is the latest plugin release; Tempest
# 46.0.0 is the Gazpacho deliverable. The runner overrides these for Flamingo.
TEMPEST_PIN="${O3K_TEMPEST_PIN:-46.0.0}"
CINDER_TEMPEST_PLUGIN_PIN="${O3K_CINDER_TEMPEST_PLUGIN_PIN:-1.21.0}"

PROFILE_DIR="${REPO_ROOT}/tests/tempest-evidence"
mkdir -p "${PROFILE_DIR}"
ALLOWLIST_FILE="${REPO_ROOT}/tests/tempest-evidence/tempest-allowlist.txt"

ALLOWED_TEST_IDS=()
while IFS= read -r line; do
  case "${line}" in
    ""|"#"*) continue ;;
    *) ALLOWED_TEST_IDS+=("${line}") ;;
  esac
done < "${ALLOWLIST_FILE}"

# The dedicated Tempest environment. Never fall back to the Cinder venv:
# explicit binaries only.
TEMPEST_VENV_PY="${O3K_TEMPEST_VENV:-}/bin/python"
TEMPEST_VENV_BIN=""
if [ -n "${O3K_TEMPEST_VENV:-}" ] && [ -x "${TEMPEST_VENV_PY}" ]; then
  TEMPEST_VENV_BIN="${O3K_TEMPEST_VENV}/bin"
fi
SUBNIT2JUNITXML_BIN="${TEMPEST_VENV_BIN}/subunit2junitxml"

# Detect whether the real Cinder profile is running.
REAL_CINDER_UP=false
if [ -n "${O3K_CINDER_ENDPOINT:-}" ] && timeout 5 curl -s -o /dev/null -w "%{http_code}" \
    "http://127.0.0.1:${CINDER_PORT}/v3/" 2>/dev/null | grep -qE "^[24][0-9]{2}$"; then
  REAL_CINDER_UP=true
fi

TEMPEST_INSTALLED=false
if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_VENV_BIN}" ]; then
  TEMPEST_INSTALLED=true
fi

if [ "${REAL_CINDER_UP}" = true ] && [ -z "${TEMPEST_VENV_BIN}" ]; then
  # The profile is running but the dedicated Tempest environment is invalid:
  # this is a harness error, never a Cinder integration failure. Record it
  # explicitly instead of silently falling back to an ambient Tempest.
  echo "ERROR: dedicated Tempest venv missing or not executable (O3K_TEMPEST_VENV=${O3K_TEMPEST_VENV:-<unset>})" >&2
  cat > "${PROFILE_DIR}/tempest-cinder-summary.json" <<JSON
{"artifact_type": "tempest-cinder-summary.json", "evidence_tier": "tempest",
 "status": "harness-error", "reason": "dedicated Tempest venv missing or not executable",
 "tempest_revision": "${TEMPEST_PIN}",
 "cinder_tempest_plugin": "${CINDER_TEMPEST_PLUGIN_PIN}",
 "selected_test_ids": [], "passed": 0, "failed": 0, "skipped": 0,
 "test_cases": [], "skip_reasons": {}}
JSON
  exit 1
fi

if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_VENV_BIN}" ]; then
  echo "==> Real Cinder profile detected; executing pinned Tempest subset..."

  # Fail explicitly if the dedicated Tempest environment is invalid: a broken
  # harness is never reported as a Cinder integration failure.
  "${TEMPEST_VENV_PY}" -c "import tempest" >/dev/null 2>&1 || {
    echo "ERROR: dedicated Tempest venv is invalid (${O3K_TEMPEST_VENV})" >&2
    cat > "${PROFILE_DIR}/tempest-cinder-summary.json" <<JSON
{"artifact_type": "tempest-cinder-summary.json", "evidence_tier": "tempest",
 "status": "harness-error", "reason": "dedicated Tempest venv invalid",
 "tempest_revision": "${TEMPEST_PIN}",
 "cinder_tempest_plugin": "${CINDER_TEMPEST_PLUGIN_PIN}",
 "selected_test_ids": [], "passed": 0, "failed": 0, "skipped": 0,
 "test_cases": [], "skip_reasons": {}}
JSON
    exit 1
  }

  # The exact pinned versions must be present; a mismatch is a harness error,
  # never a Cinder integration failure.
  if ! "${TEMPEST_VENV_PY}" - "${TEMPEST_PIN}" "${CINDER_TEMPEST_PLUGIN_PIN}" <<'PY'
import importlib.metadata as md
import sys

expected_tempest, expected_plugin = sys.argv[1:]
actual_tempest = md.version("tempest")
actual_plugin = md.version("cinder-tempest-plugin")
if actual_tempest != expected_tempest:
    sys.exit("tempest %s != expected %s" % (actual_tempest, expected_tempest))
if actual_plugin != expected_plugin:
    sys.exit("cinder-tempest-plugin %s != expected %s" % (actual_plugin, expected_plugin))
PY
  then
    echo "ERROR: pinned Tempest/plugin version mismatch in the dedicated venv" >&2
    cat > "${PROFILE_DIR}/tempest-cinder-summary.json" <<JSON
{"artifact_type": "tempest-cinder-summary.json", "evidence_tier": "tempest",
 "status": "harness-error", "reason": "pinned Tempest/plugin version mismatch",
 "tempest_revision": "${TEMPEST_PIN}",
 "cinder_tempest_plugin": "${CINDER_TEMPEST_PLUGIN_PIN}",
 "selected_test_ids": [], "passed": 0, "failed": 0, "skipped": 0,
 "test_cases": [], "skip_reasons": {}}
JSON
    exit 1
  fi

  SELECTED_IDS="$(printf '%s,' "${ALLOWED_TEST_IDS[@]}" | sed 's/,$//')"
  # stestr matches full discovered IDs ("module.Class.method[id-<uuid>[,tags]]").
  # Anchor each allowlisted ID at the parameterized suffix so exactly the
  # allowlisted test runs (never the *_from_image / *_as_clone variants).
  RUN_FILTERS=()
  for id in "${ALLOWED_TEST_IDS[@]}"; do
    RUN_FILTERS+=("${id}\\[")
  done
  TEMPEST_WORKSPACE="${O3K_TEMPEST_WORKSPACE:-${PROFILE_DIR}/tempest-workspace}"
  mkdir -p "${TEMPEST_WORKSPACE}"
  # Tempest 46 removed the `tempest run` command in favor of `stestr run`.
  # Configure a workspace (or reuse O3K_TEMPEST_WORKSPACE) and run the
  # explicit allowlist through stestr.
  TEMPEST_PKG_DIR="$("${TEMPEST_VENV_PY}" -c 'import tempest, os; print(os.path.dirname(tempest.__file__))' 2>/dev/null || echo '')"
  cat > "${TEMPEST_WORKSPACE}/.stestr.conf" <<EOF
[DEFAULT]
test_path=${TEMPEST_PKG_DIR}/test_discover
top_dir=${TEMPEST_PKG_DIR}
group_regex=([^\.]*\.)*
EOF
  # Generate the full tempest.conf via tempest init, then apply the profile
  # auth/identity overrides so the tests talk to O3K and the real Cinder.
  O3K_AUTH_URL="http://127.0.0.1:${O3K_PORT_NO_HOST}/v3"
  O3K_PW="${O3K_PW:-password}"
  (
    cd "${TEMPEST_WORKSPACE}" || exit 1
    "${TEMPEST_VENV_BIN}/tempest" init "${TEMPEST_WORKSPACE}" >/dev/null 2>&1 || true
  )
  cat >> "${TEMPEST_WORKSPACE}/etc/tempest.conf" <<EOF

[identity]
uri = ${O3K_AUTH_URL}/
uri_v3 = ${O3K_AUTH_URL}/
auth_version = v3

[auth]
use_dynamic_credentials = false
admin_username = admin
admin_password = ${O3K_PW}
admin_project_name = admin
admin_domain_name = Default
admin_user_domain_name = Default

[validation]
run_validation = False
EOF
  (
    cd "${TEMPEST_WORKSPACE}" || exit 1
    # Tempest reads ${TEMPEST_CONFIG_DIR}/tempest.conf; stestr reads .stestr.conf
    # from the workspace cwd.
    export TEMPEST_CONFIG_DIR="${TEMPEST_WORKSPACE}/etc"
    "${TEMPEST_VENV_PY}" -m stestr init >/dev/null 2>&1 || true
    # stestr run takes positional regex filters (full test IDs); config is read
    # from TEMPEST_CONFIG_DIR. --concurrency 1 avoids parallel DB conflicts.
    # A zero-test execution is never accepted as evidence (see summary step).
    "${TEMPEST_VENV_PY}" -m stestr run --concurrency 1 "${RUN_FILTERS[@]}" \
      > "${PROFILE_DIR}/tempest.log" 2>&1 || true
    # Convert the subunit stream to JUnit XML for the summary parser.
    "${TEMPEST_VENV_PY}" -m stestr last --subunit 2>/dev/null \
      | "${SUBNIT2JUNITXML_BIN}" -o "${PROFILE_DIR}/tempest-results.xml" \
      >/dev/null 2>&1 || true
  )
  # Shared JUnit -> summary converter; rejects malformed/empty/zero-test output.
  python3 "${REPO_ROOT}/tests/tempest-summary.py" \
    --junit "${PROFILE_DIR}/tempest-results.xml" \
    --out "${PROFILE_DIR}/tempest-cinder-summary.json" \
    --revision "${TEMPEST_PIN}" \
    --plugin "${CINDER_TEMPEST_PLUGIN_PIN}" \
    --selected "${SELECTED_IDS}" \
    --o3k-commit "${GITHUB_SHA:-<recorded in workflow>}" \
    --cinder-version "${O3K_CINDER_VERSION:-<recorded by runner>}" \
    || echo "WARN: Tempest summary recorded harness-error/failed status"
  echo "    Tempest subset executed; JUnit + summary written under ${PROFILE_DIR}"
else
  echo "==> Real Cinder profile is not running; recording honest NOT_READY status..."
  echo "    tempest_pin=${TEMPEST_PIN} cinder_tempest_plugin=${CINDER_TEMPEST_PLUGIN_PIN}"
  if [ "${REAL_CINDER_UP}" = false ] && [ "${TEMPEST_INSTALLED}" = false ]; then
    cat > "${PROFILE_DIR}/tempest-cinder-summary.json" <<JSON
{"artifact_type": "tempest-cinder-summary.json", "evidence_tier": "tempest",
 "status": "not-executed", "reason": "real Cinder profile not running or dedicated Tempest venv absent",
 "tempest_revision": "${TEMPEST_PIN}",
 "cinder_tempest_plugin": "${CINDER_TEMPEST_PLUGIN_PIN}",
 "selected_test_ids": [], "passed": 0, "failed": 0, "skipped": 0,
 "test_cases": [], "skip_reasons": {}}
JSON
  fi
fi

cat > "${PROFILE_DIR}/tempest-status.yaml" <<EOF
# Focused Tempest evidence — frozen subset for the external Cinder
# service-under-test profile.
#
# This is NOT a complete or pass-green Tempest run. Every skip maps to an
# explicit unsupported-operation entry in the compatibility manifest. A
# machine-readable JUnit report is generated only when the real Cinder
# profile is running and a frozen Tempest version is installed in the
# DEDICATED Tempest venv (O3K_TEMPEST_VENV).
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
  dedicated_tempest_venv: ${O3K_TEMPEST_VENV:-none}
  real_cinder_profile_running: ${REAL_CINDER_UP}
  evidence_status: ${TEMPEST_STATUS:-recorded}

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
  - {operation: identity.token-revocation, reason: non-goal-for-hosted-service-profile}
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
  - {operation: compute.create-server-network-uuid-nic, reason: profile-accepts-port-uuids-only}
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
if [ "${REAL_CINDER_UP}" = true ] && [ -n "${TEMPEST_VENV_BIN}" ]; then
  echo "    Evidence tier: real-tempest (see ${PROFILE_DIR}/tempest-cinder-summary.json)"
else
  echo "    Current evidence tier: NOT_READY"
  echo "    Real Cinder profile must be running before Tempest can execute."
  echo "    Every skip maps to an explicit unsupported-operation entry."
fi
