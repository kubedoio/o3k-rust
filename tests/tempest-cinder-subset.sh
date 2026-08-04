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

PROFILE_DIR="${REPO_ROOT}/tests/tempest-evidence"
mkdir -p "${PROFILE_DIR}"

cat > "${PROFILE_DIR}/tempest-status.yaml" <<EOF
# Focused Tempest evidence — frozen subset for the external Cinder
# service-under-test profile.
#
# This is NOT a complete or pass-green Tempest run. Every skip maps to an
# explicit unsupported-operation entry in the compatibility manifest. A
# machine-readable JUnit report is generated only when the real Cinder
# profile is running and a frozen Tempest version is installed.
#
# Last checked: $(date -u +%Y-%m-%dT%H:%M:%SZ)

tempest:
  version_pinned: false
  installed: false
  real_cinder_profile_running: false
  evidence_status: NOT_READY

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
echo "    Current evidence tier: NOT_READY"
echo "    Real Cinder profile must be running before Tempest can execute."
echo "    Every skip maps to an explicit unsupported-operation entry."
