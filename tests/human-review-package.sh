#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-human-review.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

python3 - "${WORK_DIR}/pending.json" "${WORK_DIR}/approved.json" "${WORK_DIR}/bad-reviewer.json" "${WORK_DIR}/bad-finding.json" <<'PY'
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
    "scope": ["compute-agent mTLS", "libvirt ownership"],
    "findings": [{"id": "SEC-001", "severity": "low", "disposition": "fixed"}],
    "approvals": {"release_blocking_findings": True, "destructive_cleanup": True},
    "unresolved_risks": ["Real-host evidence is still required."],
}
values = [dict(base), dict(base, status="approved"), dict(base), dict(base)]
values[2]["reviewer"] = dict(base["reviewer"], is_implementing_agent=True)
values[3]["findings"] = [{"id": "SEC-001", "severity": "high"}]
for path, value in zip(sys.argv[1:], values):
    with open(path, "w", encoding="utf-8") as stream:
        json.dump(value, stream)
PY

bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/pending.json"
bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/approved.json" --require-approved

if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/bad-reviewer.json"; then
  echo "accepted implementing agent as reviewer" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/bad-finding.json"; then
  echo "accepted finding without disposition" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-human-review.sh" --input "${WORK_DIR}/pending.json" --require-approved; then
  echo "accepted pending artifact as release approval" >&2
  exit 1
fi

echo "human review package validation tests passed"
