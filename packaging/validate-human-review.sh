#!/usr/bin/env bash
set -Eeuo pipefail

INPUT=
REQUIRE_APPROVED=false
while (($#)); do
  case "$1" in
    --input) INPUT="${2:?missing human-review artifact}"; shift 2;;
    --require-approved) REQUIRE_APPROVED=true; shift;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done

if [[ -z "$INPUT" ]]; then
  echo "--input is required" >&2
  exit 2
fi

export INPUT REQUIRE_APPROVED
python3 <<'PY'
import json
import os
import re
import sys

path = os.environ["INPUT"]
require_approved = os.environ["REQUIRE_APPROVED"] == "true"
errors = []
try:
    with open(path, encoding="utf-8") as stream:
        value = json.load(stream)
except (OSError, json.JSONDecodeError) as error:
    print(f"human-review: cannot read JSON artifact ({error})", file=sys.stderr)
    raise SystemExit(1)

if not isinstance(value, dict):
    errors.append("artifact root must be an object")
else:
    if value.get("artifact_type") != "human-architecture-security-review":
        errors.append("artifact_type must be 'human-architecture-security-review'")
    if value.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    status = value.get("status")
    if status not in {"pending", "approved", "rejected"}:
        errors.append("status must be pending, approved, or rejected")
    reviewer = value.get("reviewer")
    if not isinstance(reviewer, dict):
        errors.append("reviewer must be an object")
    else:
        for field in ("name", "organization", "role"):
            if not isinstance(reviewer.get(field), str) or not reviewer[field].strip():
                errors.append(f"reviewer.{field} must be non-empty")
        if reviewer.get("is_implementing_agent") is not False:
            errors.append("reviewer.is_implementing_agent must be false")
    if not isinstance(value.get("reviewed_commit"), str) or not re.fullmatch(r"[0-9a-f]{40}", value["reviewed_commit"]):
        errors.append("reviewed_commit must be a 40-character lowercase commit SHA")
    if not isinstance(value.get("review_record_url"), str) or not value["review_record_url"].startswith("https://"):
        errors.append("review_record_url must be an https URL")
    scope = value.get("scope")
    if not isinstance(scope, list) or not scope or not all(isinstance(item, str) and item.strip() for item in scope):
        errors.append("scope must be a non-empty list of strings")
    findings = value.get("findings")
    if not isinstance(findings, list):
        errors.append("findings must be a list")
    else:
        finding_ids = set()
        for index, finding in enumerate(findings):
            if not isinstance(finding, dict):
                errors.append(f"findings[{index}] must be an object")
                continue
            for field in ("id", "severity", "disposition"):
                if not isinstance(finding.get(field), str) or not finding[field].strip():
                    errors.append(f"findings[{index}].{field} must be non-empty")
            if finding.get("severity") not in {"critical", "high", "medium", "low", "informational"}:
                errors.append(f"findings[{index}].severity is invalid")
            if finding.get("disposition") not in {"fixed", "accepted", "deferred", "not-applicable"}:
                errors.append(f"findings[{index}].disposition is invalid")
            finding_id = finding.get("id")
            if isinstance(finding_id, str) and finding_id.strip():
                if finding_id in finding_ids:
                    errors.append(f"findings[{index}].id must be unique")
                finding_ids.add(finding_id)
    approvals = value.get("approvals")
    if not isinstance(approvals, dict):
        errors.append("approvals must be an object")
    else:
        for field in ("release_blocking_findings", "destructive_cleanup"):
            if approvals.get(field) is not True:
                errors.append(f"approvals.{field} must be true")
    risks = value.get("unresolved_risks")
    if not isinstance(risks, list) or not all(isinstance(item, str) and item.strip() for item in risks):
        errors.append("unresolved_risks must be a list of strings")
    if require_approved and value.get("status") != "approved":
        errors.append("status must be approved for a release decision")

if errors:
    for error in errors:
        print(f"human-review: {error}", file=sys.stderr)
    raise SystemExit(1)
print(f"validated human-review artifact: status={value['status']}")
PY
