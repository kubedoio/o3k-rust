#!/usr/bin/env python3
"""Shared P13.6 scenario-row evidence validator.

Validates per-scenario evidence artifacts for slices B–F.
Checks result vocabulary, forbidden values, schema completeness,
and non-disclosure invariants.
"""

import json
import pathlib
import re
import sys
import tempfile

RESULT_VOCABULARY = {
    "passed", "not_applicable", "expected_ambiguous",
    "upstream_provider_unsupported", "execution_profile_unavailable",
    "blocked", "failed",
}

REQUIRED_FIELDS = [
    "phase", "tested_runtime_head_sha", "backend", "project_a_principal",
    "project_a_project", "project_b_principal", "project_b_project",
    "resource_type", "operation", "target_owner", "caller_owner",
    "expected_authorization_outcome", "actual_http_status",
    "result",
]

# The 12 contract resource types from the P13.6A contract.
EXPECTED_RESOURCES = [
    "openstack_compute_keypair_v2",
    "openstack_networking_network_v2",
    "openstack_networking_subnet_v2",
    "openstack_networking_port_v2",
    "openstack_compute_instance_v2",
    "openstack_networking_secgroup_v2",
    "openstack_networking_secgroup_rule_v2",
    "openstack_networking_router_v2",
    "openstack_networking_router_interface_v2",
    "openstack_networking_floatingip_v2",
    "openstack_blockstorage_volume_v3",
    "openstack_compute_volume_attach_v2",
]

# B-scenario field requirements: when phase starts with P13.6B, each row must
# carry a "resources_created" list.
B_PHASE_PREFIX = "P13.6B"

# Response-body phrases that would disclose foreign existence on a 404.
DISCLOSURE_PHRASES = [
    "owned by another", "ownership conflict", "not owner",
    "belongs to another project", "foreign",
]

SECRET_VALUE_RES = [
    re.compile(r"-----BEGIN"),
    re.compile(r"Bearer\s+[A-Za-z0-9._-]{10,}"),
    re.compile(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\."),  # JWT shape
]

# Fields whose values are identifiers (names/UUIDs), never credentials.
IDENTIFIER_FIELDS = {
    "project_a_principal", "project_b_principal",
    "project_a_project", "project_b_project",
    "target_owner", "caller_owner",
}


def load_json(path):
    return json.loads(pathlib.Path(path).read_text(encoding="utf-8"))


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        return False
    return True


def walk_for_secrets(obj, path="$", findings=None):
    if findings is None:
        findings = []
    if isinstance(obj, str):
        for pattern in SECRET_VALUE_RES:
            if pattern.search(obj):
                findings.append(f"{path}: value matches secret pattern {pattern.pattern!r}")
    elif isinstance(obj, dict):
        for key, value in obj.items():
            walk_for_secrets(value, f"{path}.{key}", findings)
    elif isinstance(obj, list):
        for index, value in enumerate(obj):
            walk_for_secrets(value, f"{path}[{index}]", findings)
    return findings


def extract_scenarios(artifact):
    if isinstance(artifact.get("scenarios"), list):
        return artifact["scenarios"]
    if isinstance(artifact.get("scenario_rows"), list):
        return artifact["scenario_rows"]
    if artifact.get("result") in RESULT_VOCABULARY:
        return [artifact]
    return []


def validate_artifact(artifact_path):
    artifact = load_json(artifact_path)
    errors = []

    scenarios = extract_scenarios(artifact)
    errors.append(check(
        len(scenarios) > 0,
        "evidence artifact must contain at least one scenario row"
    ))

    tc = artifact.get("toolchain", {})
    if tc:
        errors.append(check(
            tc.get("provider_modified") is False,
            "toolchain.provider_modified must be false"
        ))

    # Collect resource types for 12-resource coverage validation
    covered_resource_types = set()
    is_b_evidence = artifact.get("phase", "").startswith(B_PHASE_PREFIX) or any(
        r.get("phase", "").startswith(B_PHASE_PREFIX) for r in scenarios
    )

    for i, row in enumerate(scenarios):
        prefix = f"scenario row {i}"

        for field in REQUIRED_FIELDS:
            errors.append(check(
                field in row,
                f"{prefix}: missing required field '{field}'"
            ))

        result = row.get("result", "")
        errors.append(check(
            result in RESULT_VOCABULARY,
            f"{prefix}: result '{result}' not in vocabulary {sorted(RESULT_VOCABULARY)}"
        ))

        errors.append(check(
            not (result == "passed" and row.get("classification") == "AMBIGUOUS_CLIENT_CREATE_RESPONSE_LOSS"),
            f"{prefix}: passed with AMBIGUOUS_CLIENT_CREATE_RESPONSE_LOSS is forbidden; use expected_ambiguous"
        ))

        # B-specific: require resources_created field
        if is_b_evidence:
            errors.append(check(
                "resources_created" in row,
                f"{prefix}: missing 'resources_created' field (required for P13.6B evidence)"
            ))
            if "resources_created" in row:
                errors.append(check(
                    isinstance(row["resources_created"], list),
                    f"{prefix}: 'resources_created' must be a list"
                ))
            if "resource_types_coverage" in row:
                errors.append(check(
                    isinstance(row["resource_types_coverage"], list),
                    f"{prefix}: 'resource_types_coverage' must be a list"
                ))
                covered_resource_types.update(row["resource_types_coverage"])
            # A row whose own resource_type is a contract type accounts for
            # that type even without an explicit coverage list (e.g. the
            # execution_profile_unavailable classification rows).
            if row.get("resource_type") in EXPECTED_RESOURCES:
                covered_resource_types.add(row["resource_type"])

        # Reject "passed" results that don't have actual_http_status set properly
        if result == "passed":
            errors.append(check(
                row.get("actual_http_status") is not None
                and isinstance(row.get("actual_http_status"), (int, float))
                and row.get("actual_http_status") != 0,
                f"{prefix}: 'passed' result requires non-zero actual_http_status (got {row.get('actual_http_status')})"
            ))

        actual_status = row.get("actual_http_status")
        if actual_status == 404:
            body = str(row.get("actual_http_body_shape", "")).lower()
            for phrase in DISCLOSURE_PHRASES:
                errors.append(check(
                    phrase not in body,
                    f"{prefix}: 404 response body contains disclosure phrase '{phrase}'"
                ))

        for auth_field in IDENTIFIER_FIELDS:
            value = row.get(auth_field, "")
            if isinstance(value, str) and len(value) > 64:
                errors.append(check(
                    False,
                    f"{prefix}.{auth_field}: value longer than 64 chars looks like a credential, not an identifier"
                ))

    # Validate that all 12 contract resource types are accounted for. For B
    # evidence this is unconditional: an artifact with no coverage data at all
    # must fail, not slip through.
    if is_b_evidence:
        missing = sorted(set(EXPECTED_RESOURCES) - covered_resource_types)
        errors.append(check(
            len(missing) == 0,
            f"resource types not covered in B evidence: {missing}. All 12 contract types must be accounted for "
            f"(some via execution_profile_unavailable classification)"
        ))

    secret_findings = walk_for_secrets(artifact)
    for finding in secret_findings:
        errors.append(check(False, f"possible secret material: {finding}"))

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


def self_test():
    fixture = {
        "artifact_type": "o3k-p13-6-self-test",
        "toolchain": {"provider_modified": False, "opentofu": "1.12.6"},
        "phase": "P13.6B",
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
                "resources_created": [],
                "resource_types_coverage": [
                    "openstack_compute_keypair_v2",
                    "openstack_networking_network_v2",
                    "openstack_networking_subnet_v2",
                    "openstack_networking_port_v2",
                    "openstack_compute_instance_v2",
                    "openstack_networking_secgroup_v2",
                    "openstack_networking_secgroup_rule_v2",
                    "openstack_networking_router_v2",
                    "openstack_networking_router_interface_v2",
                    "openstack_networking_floatingip_v2",
                    "openstack_blockstorage_volume_v3",
                    "openstack_compute_volume_attach_v2",
                ],
            }
        ],
        "aggregate_verdict": "PASS",
    }
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", prefix="o3k-p13-6-selftest-", delete=False
    ) as handle:
        json.dump(fixture, handle)
        path = handle.name
    try:
        validate_artifact(path)
    finally:
        pathlib.Path(path).unlink(missing_ok=True)


def main():
    if "--self-test" in sys.argv:
        self_test()
        return
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if not args:
        print("Usage: scripts/validate_p13_6_evidence.py <evidence.json> [--self-test]", file=sys.stderr)
        sys.exit(2)
    for arg in args:
        validate_artifact(arg)


if __name__ == "__main__":
    main()
