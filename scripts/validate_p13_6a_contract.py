#!/usr/bin/env python3
"""Validate the P13.6A security/failure contract artifact."""

import json
import pathlib
import sys

CONTRACT_PATH = "docs/compatibility/p13-6/p13-6a-security-failure-contract.json"
PROFILE_PATH = "contracts/iac-openstack-profile-v1.yaml"

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

REQUIRED_SECURITY_DIMENSIONS = [
    "list", "show", "update", "delete", "import",
    "relationship", "replay", "restart_reconstruction",
]

RESULT_VOCABULARY = {
    "passed", "not_applicable", "expected_ambiguous",
    "upstream_provider_unsupported", "execution_profile_unavailable",
    "blocked", "failed",
}


def load_json(path):
    return json.loads(pathlib.Path(path).read_text(encoding="utf-8"))


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        return False
    return True


def validate_contract(contract_path, self_test=False):
    contract = load_json(contract_path)
    errors = []

    # Header checks
    errors.append(check(
        contract.get("artifact_type") == "o3k-p13-6a-security-failure-contract",
        "artifact_type must be o3k-p13-6a-security-failure-contract"
    ))
    errors.append(check(
        contract.get("schema_version") == 2,
        "schema_version must be 2"
    ))
    errors.append(check(
        contract.get("profile") == "p13-iac-compatibility-v1",
        "profile must be p13-iac-compatibility-v1"
    ))
    errors.append(check(
        contract.get("status") == "contract_frozen",
        "status must be contract_frozen"
    ))

    # SHA presence
    errors.append(check(
        len(contract.get("starting_main_sha", "")) == 40,
        "starting_main_sha must be a 40-hex SHA"
    ))
    errors.append(check(
        len(contract.get("tested_runtime_head_sha", "")) == 40,
        "tested_runtime_head_sha must be a 40-hex SHA"
    ))

    # Toolchain
    tc = contract.get("toolchain", {})
    errors.append(check(
        tc.get("opentofu") == "1.12.6",
        "toolchain.opentofu must be 1.12.6"
    ))
    errors.append(check(
        tc.get("provider_modified") is False,
        "toolchain.provider_modified must be false"
    ))
    errors.append(check(
        len(tc.get("provider_binary_sha256", "")) == 64,
        "toolchain.provider_binary_sha256 must be a 64-hex SHA256"
    ))

    # Baseline
    bl = contract.get("baseline", {})
    errors.append(check(
        bl.get("status") in ("verified", "blocked"),
        "baseline.status must be verified or blocked"
    ))
    errors.append(check(
        isinstance(bl.get("gates_passed"), int) and bl["gates_passed"] >= 0,
        "baseline.gates_passed must be a non-negative integer"
    ))
    total = bl.get("gates_total", 0)
    errors.append(check(
        total == 11,
        "baseline.gates_total must be 11 (the 11 P13.2-P13.4 gates)"
    ))

    # Resource matrix completeness
    matrix = contract.get("resource_security_matrix", [])
    matrix_resources = {r["resource"] for r in matrix}
    errors.append(check(
        matrix_resources == set(EXPECTED_RESOURCES),
        f"resource_security_matrix must contain exactly these resources: {sorted(EXPECTED_RESOURCES)}. "
        f"Missing: {set(EXPECTED_RESOURCES) - matrix_resources}. "
        f"Extra: {matrix_resources - set(EXPECTED_RESOURCES)}"
    ))

    for row in matrix:
        resource = row.get("resource", "?")
        for dim in REQUIRED_SECURITY_DIMENSIONS:
            entry = row.get(dim)
            errors.append(check(
                entry is not None and isinstance(entry, dict) and "applicable" in entry,
                f"{resource}.{dim} must have an 'applicable' field"
            ))
            if entry and entry.get("applicable"):
                errors.append(check(
                    "enforcement" in entry,
                    f"{resource}.{dim}.enforcement is required when applicable=true"
                ))
            if entry and entry.get("applicable") is False:
                errors.append(check(
                    "reason" in entry,
                    f"{resource}.{dim}.reason is required when applicable=false"
                ))

        # Owner field must be present
        errors.append(check(
            row.get("owner_field"),
            f"{resource}.owner_field is required"
        ))

    # Two-project identity model
    tpm = contract.get("two_project_identity_model", {})
    errors.append(check(
        "project_a" in tpm and "project_b" in tpm,
        "two_project_identity_model must have project_a and project_b"
    ))
    for proj_key in ("project_a", "project_b"):
        proj = tpm.get(proj_key, {})
        errors.append(check(
            proj.get("project_id"),
            f"two_project_identity_model.{proj_key}.project_id is required"
        ))
    errs = tpm.get("admin_bypass_check", {})
    errors.append(check(
        errs.get("verified") is True,
        "admin_bypass_check.verified must be true (proven by two_tenant_isolation test)"
    ))

    # Non-disclosure contract
    ndc = contract.get("non_disclosure_contract", {})
    errors.append(check(
        ndc.get("general_principle"),
        "non_disclosure_contract.general_principle is required"
    ))
    errors.append(check(
        isinstance(ndc.get("forbidden_response_contents"), list) and len(ndc["forbidden_response_contents"]) >= 3,
        "non_disclosure_contract.forbidden_response_contents must have at least 3 entries"
    ))
    open_findings = ndc.get("open_findings", [])
    errors.append(check(
        len(open_findings) >= 2,
        "non_disclosure_contract.open_findings must capture at least the FIP-001 and SG-001 findings"
    ))
    for finding in open_findings:
        errors.append(check(
            finding.get("id") and finding.get("status") in ("open", "fixed"),
            f"open_finding {finding.get('id', '?')} must have id and status"
        ))
        if finding.get("status") == "fixed":
            errors.append(check(
                finding.get("fix_location"),
                f"open_finding {finding['id']} with status fixed must have fix_location"
            ))

    # Operation/idempotency scope matrix
    oism = contract.get("operation_idempotency_scope_matrix", {})
    coverage_key = "idempotency_reservation_coverage"
    scope_bound = oism.get(coverage_key, {}).get("scope_bound")
    errors.append(check(
        scope_bound is True,
        f"operation_idempotency_scope_matrix.{coverage_key}.scope_bound must be true"
    ))
    errors.append(check(
        isinstance(oism.get("same_key_cross_project_behavior"), dict),
        "operation_idempotency_scope_matrix.same_key_cross_project_behavior is required"
    ))

    # Failure matrix
    fm = contract.get("failure_matrix", [])
    errors.append(check(
        len(fm) >= 4,
        "failure_matrix must have at least 4 operation classes (CREATE, UPDATE, DELETE, RELATIONSHIP)"
    ))
    for row in fm:
        op = row.get("operation_class", "?")
        for bkey in ("boundary_1_before_acceptance", "boundary_2_after_acceptance_before_provider",
                      "boundary_3_after_provider_before_terminal", "boundary_4_after_terminal_before_client_response",
                      "boundary_5_restart_with_pending"):
            entry = row.get(bkey)
            if entry is not None and isinstance(entry, dict):
                errors.append(check(
                    entry.get("expected"),
                    f"{op}.{bkey}.expected is required"
                ))

    # Result vocabulary
    rv = contract.get("result_vocabulary", [])
    errors.append(check(
        set(rv) == RESULT_VOCABULARY,
        f"result_vocabulary must be exactly {sorted(RESULT_VOCABULARY)}"
    ))

    # Evidence schema
    es = contract.get("evidence_schema", {})
    errors.append(check(
        len(es.get("required_per_scenario_fields", [])) >= 15,
        "evidence_schema.required_per_scenario_fields must have at least 15 fields"
    ))

    # Scenario boundary
    sb = contract.get("scenario_boundary", {})
    errors.append(check(
        "p13_6" in sb and "p13_7" in sb,
        "scenario_boundary must have p13_6 and p13_7 entries"
    ))

    # Architecture
    arch = contract.get("architecture", {})
    errors.append(check(
        arch.get("provider_modified") is False,
        "architecture.provider_modified must be false"
    ))
    errors.append(check(
        arch.get("non_disclosure_verified") is True,
        "architecture.non_disclosure_verified must be true"
    ))

    secret_hints = ["password", "x-auth-token", "secret", "token", "credential"]
    serialized = json.dumps(contract)
    for hint in secret_hints:
        # Look at string values only, not structural names like "project_id"
        pass
    # Walk-based no-secret check
    def _no_secrets(obj, path=""):
        if isinstance(obj, str):
            for hint in secret_hints:
                if hint in obj.lower() and len(obj) > 40:
                    print(f"WARNING: possible secret at {path}: value length {len(obj)} contains '{hint}'", file=sys.stderr)
        elif isinstance(obj, dict):
            for k, v in obj.items():
                _no_secrets(v, f"{path}.{k}")
        elif isinstance(obj, list):
            for i, v in enumerate(obj):
                _no_secrets(v, f"{path}[{i}]")
    _no_secrets(contract)

    all_pass = all(errors)
    if all_pass:
        print(f"P13.6A contract validation: PASS ({len(errors)} checks)")
    else:
        fail_count = errors.count(False)
        print(f"P13.6A contract validation: FAIL ({fail_count} failures out of {len(errors)} checks)", file=sys.stderr)
        sys.exit(2)


def main():
    self_test = "--self-test" in sys.argv
    validate_contract(CONTRACT_PATH, self_test=self_test)


if __name__ == "__main__":
    main()
