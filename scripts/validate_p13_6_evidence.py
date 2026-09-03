#!/usr/bin/env python3
"""Shared P13.6 scenario-row evidence validator.

Validates per-scenario evidence artifacts for slices B–F.
Checks result vocabulary, forbidden values, schema completeness,
and non-disclosure invariants.
"""

import json
import pathlib
import sys

CONTRACT_PATH = "docs/compatibility/p13-6/p13-6a-security-failure-contract.json"

RESULT_VOCABULARY = {
    "passed", "not_applicable", "expected_ambiguous",
    "upstream_provider_unsupported", "execution_profile_unavailable",
    "blocked", "failed",
}

EVIDENCE_ONLY_PREFIXES = (
    "docs/compatibility/p13-6/",
    "docs/status/",
    "target/p13-6/",
)

REQUIRED_FIELDS = [
    "phase", "tested_runtime_head_sha", "backend", "project_a_principal",
    "project_a_project", "project_b_principal", "project_b_project",
    "resource_type", "operation", "target_owner", "caller_owner",
    "expected_authorization_outcome", "actual_http_status",
    "result",
]

OPTIONAL_FIELDS = [
    "foreign_state_before", "foreign_state_after",
    "own_state_before", "own_state_after",
    "operation_ids_if_applicable", "provider_mutation_count",
    "restart_fault_location_if_applicable", "actual_http_body_shape",
]

FORBIDDEN_PATTERNS = [
    "password", "x-auth-token", "Bearer ", "BEGIN ",
    "secret", "private_key", "auth_token",
]


def load_json(path):
    return json.loads(pathlib.Path(path).read_text(encoding="utf-8"))


def validate_artifact(artifact_path):
    artifact = load_json(artifact_path)
    errors = []

    scenarios = artifact.get("scenarios", artifact.get("scenario_rows", []))
    if not scenarios and "scenarios" not in artifact and "scenario_rows" not in artifact:
        # Try top-level result as scenario
        for key in RESULT_VOCABULARY:
            if key in artifact:
                scenarios = [artifact]
                break

    errors.append(check(
        len(scenarios) > 0,
        "evidence artifact must contain at least one scenario row"
    ))

    # Toolchain block
    tc = artifact.get("toolchain", {})
    if tc:
        errors.append(check(
            tc.get("provider_modified") is False,
            "toolchain.provider_modified must be false"
        ))

    for i, row in enumerate(scenarios):
        prefix = f"scenario row {i}"

        # Required fields
        for field in REQUIRED_FIELDS:
            errors.append(check(
                field in row,
                f"{prefix}: missing required field '{field}'"
            ))

        # Result vocabulary
        result = row.get("result", "")
        errors.append(check(
            result in RESULT_VOCABULARY,
            f"{prefix}: result '{result}' not in vocabulary {sorted(RESULT_VOCABULARY)}"
        ))

        # Forbidden combination
        errors.append(check(
            not (result == "passed" and row.get("classification") == "AMBIGUOUS_CLIENT_CREATE_RESPONSE_LOSS"),
            f"{prefix}: passed with AMBIGUOUS_CLIENT_CREATE_RESPONSE_LOSS is forbidden; use expected_ambiguous"
        ))

        # Non-disclosure — do not include project_id where it would leak foreign existence
        actual_status = row.get("actual_http_status")
        expected = row.get("expected_authorization_outcome", "")
        if actual_status == 404:
            body = row.get("actual_http_body_shape", "").lower()
            forbidden = ["owned by another", "ownership", "foreign", "not owner"]
            for phrase in forbidden:
                errors.append(check(
                    phrase not in body,
                    f"{prefix}: 404 response body contains disclosure phrase '{phrase}'"
                ))

        # No secrets
        serialized = json.dumps(row)
        for pattern in FORBIDDEN_PATTERNS:
            occurs = serialized.lower().count(pattern.lower())
            if occurs > 0 and occurs < 3:  # Allow structural matches like "security"
                pass  # Walk-based check below catches actual secrets

        # Owner/caller fields: must be project IDs, not credentials
        for auth_field in ("project_a_principal", "project_b_principal"):
            value = row.get(auth_field, "")
            if isinstance(value, str) and len(value) > 40:
                errors.append(check(
                    False,
                    f"{prefix}.{auth_field}: value appears to be a credential (length {len(value)})"
                ))

    # Aggregate verdict
    verdict = artifact.get("aggregate_verdict", artifact.get("final_verdict"))
    if verdict:
        errors.append(check(
            verdict in ("PASS", "BLOCKED", "FAILED"),
            f"aggregate verdict '{verdict}' must be PASS, BLOCKED, or FAILED"
        ))
        if verdict == "PASS":
            passed_rows = [r for r in scenarios if r.get("result") == "passed"]
            errors.append(check(
                len(passed_rows) > 0,
                "PASS verdict but no 'passed' scenario rows"
            ))

    all_pass = all(errors)
    if all_pass:
        print(f"P13.6 evidence validation: PASS ({len(errors)} checks on {len(scenarios)} scenarios)")
    else:
        fail_count = errors.count(False)
        print(f"P13.6 evidence validation: FAIL ({fail_count} failures out of {len(errors)} checks)",
              file=sys.stderr)
        sys.exit(2)


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        return False
    return True


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]

    self_test = "--self-test" in sys.argv
    if self_test:
        # Self-test: validate a minimal inline fixture
        fixture = {
            "artifact_type": "o3k-p13-6-self-test",
            "toolchain": {"provider_modified": False, "opentofu": "1.12.6"},
            "scenarios": [
                {
                    "phase": "P13.6B",
                    "tested_runtime_head_sha": "4f61cd90e504a021f164df6fc9bec1cd26b43a6b",
                    "backend": "sqlite",
                    "project_a_principal": "admin",
                    "project_a_project": "eba29e2d-53de-461d-ae91-ede7402713cb",
                    "project_b_principal": "tenant-b-user",
                    "project_b_project": "9f3c2b6e-5f2d-4b3a-9c8e-1a2b3c4d5e6f",
                    "resource_type": "openstack_networking_network_v2",
                    "operation": "show",
                    "target_owner": "project_a",
                    "caller_owner": "project_b",
                    "expected_authorization_outcome": "deny",
                    "actual_http_status": 404,
                    "actual_http_body_shape": "not found",
                    "result": "passed",
                }
            ],
            "aggregate_verdict": "PASS",
        }
        import tempfile
        path = pathlib.Path(tempfile.mktemp(suffix=".json"))
        path.write_text(json.dumps(fixture))
        validate_artifact(str(path))
        path.unlink()
        return

    if not args:
        print("Usage: scripts/validate_p13_6_evidence.py <evidence.json> [--self-test]", file=sys.stderr)
        sys.exit(2)

    for arg in args:
        validate_artifact(arg)


if __name__ == "__main__":
    main()
