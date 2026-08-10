#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-human-review.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

python3 - "${WORK_DIR}/pending.json" "${WORK_DIR}/approved.json" "${WORK_DIR}/rejected.json" "${WORK_DIR}/bad-reviewer.json" "${WORK_DIR}/bad-finding.json" "${WORK_DIR}/duplicate-finding.json" "${WORK_DIR}/missing-scope.json" "${WORK_DIR}/deferred-critical.json" <<'PY'
import json
import sys

base = {
    "artifact_type": "human-architecture-security-review",
    "schema_version": 1,
    "status": "pending",
    "reviewer": {
        "name": "Example Reviewer",
        "organization": "Example Security",
        "role": "Independent reviewer",
        "is_implementing_agent": False,
    },
    "reviewed_commit": "0123456789abcdef0123456789abcdef01234567",
    "review_record_url": "https://example.invalid/review/92",
    "scope": [
        "Keystone and project isolation", "Compute-agent mTLS",
        "Journal and reconciliation", "Placement and scheduler",
        "Images and paths", "Config-drive", "Libvirt and ownership",
        "Bridge/TAP/DHCP", "Console and logs",
        "Installer/reset/uninstall/runner",
    ],
    "findings": [{"id": "SEC-001", "severity": "low", "disposition": "fixed"}],
    "approvals": {"release_blocking_findings": False, "destructive_cleanup": False, "foreign_state_safeguards": False},
    "unresolved_risks": ["Real-host evidence is still required."],
}
approved = dict(base, status="approved", approvals={"release_blocking_findings": True, "destructive_cleanup": True, "foreign_state_safeguards": True})
values = [dict(base), approved, dict(base, status="rejected"), dict(base), dict(base), dict(base), dict(base), dict(base)]
values[3]["reviewer"] = dict(base["reviewer"], is_implementing_agent=True)
values[4]["findings"] = [{"id": "SEC-001", "severity": "high"}]
values[5]["findings"] = [
    {"id": "SEC-001", "severity": "low", "disposition": "fixed"},
    {"id": "SEC-001", "severity": "medium", "disposition": "accepted"},
]
values[6]["scope"] = ["Keystone and project isolation"]
values[7]["findings"] = [{"id": "SEC-002", "severity": "critical", "disposition": "deferred"}]
for path, value in zip(sys.argv[1:], values):
    with open(path, "w", encoding="utf-8") as stream:
        json.dump(value, stream)
PY

bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/pending.json"
bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/approved.json" --require-approved
bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/rejected.json"

if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/bad-reviewer.json"; then
  echo "accepted implementing agent as reviewer" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/bad-finding.json"; then
  echo "accepted finding without disposition" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/duplicate-finding.json"; then
  echo "accepted duplicate finding identifier" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/missing-scope.json"; then
  echo "accepted review with incomplete threat-model scope" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/deferred-critical.json"; then
  echo "accepted a deferred critical finding" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/pending.json" --require-approved; then
  echo "accepted pending artifact as release approval" >&2
  exit 1
fi

echo "human review package validation tests passed"
