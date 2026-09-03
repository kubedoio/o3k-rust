#!/usr/bin/env python3
"""Validate the P13.6A security/failure contract artifact."""

import json
import pathlib
import re
import sys

# Import the canonical baseline gate list from the manifest script so we
# validate against the same source of truth rather than duplicating a list.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from p13_baseline_gate_manifest import BASELINE_GATES

CONTRACT_PATH = "docs/compatibility/p13-6/p13-6a-security-failure-contract.json"

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

SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
# Only flag values that look like actual secret material, not field names or
# documentation that mentions the word "password"/"token".
SECRET_VALUE_RES = [
    re.compile(r"-----BEGIN"),
    re.compile(r"Bearer\s+[A-Za-z0-9._-]{10,}"),
    re.compile(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\."),  # JWT shape
]


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


def validate_contract(contract_path):
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
        bool(SHA_RE.match(contract.get("starting_main_sha", ""))),
        "starting_main_sha must be a 40-hex SHA"
    ))
    errors.append(check(
        bool(SHA_RE.match(contract.get("tested_runtime_head_sha", ""))),
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
        bool(SHA256_RE.match(tc.get("provider_binary_sha256", ""))),
        "toolchain.provider_binary_sha256 must be a 64-hex SHA256"
    ))

    # Baseline (with internal-consistency checks)
    bl = contract.get("baseline", {})
    errors.append(check(
        bl.get("status") in ("verified", "blocked"),
        "baseline.status must be verified or blocked"
    ))
    gates_passed = bl.get("gates_passed")
    gates_blocked = bl.get("gates_blocked")
    gates_total = bl.get("gates_total")
    errors.append(check(
        isinstance(gates_passed, int) and gates_passed >= 0,
        "baseline.gates_passed must be a non-negative integer"
    ))
    errors.append(check(
        gates_total == len(BASELINE_GATES),
        f"baseline.gates_total must be {len(BASELINE_GATES)} (the canonical P13.2-P13.4 gates)"
    ))
    errors.append(check(
        isinstance(gates_passed, int) and isinstance(gates_blocked, int)
        and gates_passed + gates_blocked == gates_total,
        f"baseline.gates_passed ({gates_passed}) + gates_blocked ({gates_blocked}) "
        f"must equal gates_total ({gates_total})"
    ))
    blocked_gates = bl.get("blocked_gates", [])
    errors.append(check(
        isinstance(gates_blocked, int) and gates_blocked == len(blocked_gates),
        f"baseline.gates_blocked ({gates_blocked}) must equal len(blocked_gates) "
        f"({len(blocked_gates)})"
    ))

    # Status-gate consistency
    if bl.get("status") == "verified":
        errors.append(check(
            gates_blocked == 0 and len(blocked_gates) == 0,
            "baseline.status=verified requires zero blocked gates"
        ))
    if bl.get("status") == "blocked":
        errors.append(check(
            isinstance(gates_blocked, int) and gates_blocked >= 1,
            "baseline.status=blocked requires at least one blocked gate"
        ))

    # Blocked gate uniqueness and membership in the canonical gate set
    blocked_scripts = [g.get("script") for g in blocked_gates]
    errors.append(check(
        len(blocked_scripts) == len(set(blocked_scripts)),
        f"blocked gate names must be unique; duplicates: "
        f"{[s for s in blocked_scripts if blocked_scripts.count(s) > 1]}"
    ))
    canonical_set = set(BASELINE_GATES)
    for script in blocked_scripts:
        errors.append(check(
            script in canonical_set,
            f"blocked gate '{script}' is not in the canonical baseline gate set"
        ))

    for gate in blocked_gates:
        errors.append(check(
            gate.get("script") and gate.get("reason"),
            "each blocked gate entry must have script and reason"
        ))
        errors.append(check(
            isinstance(gate.get("exit_code"), int) and gate["exit_code"] != 0,
            f"blocked gate '{gate.get('script')}' must have a non-zero exit_code"
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
    # The two projects must be genuinely distinct
    if "project_a" in tpm and "project_b" in tpm:
        errors.append(check(
            tpm["project_a"].get("project_id") != tpm["project_b"].get("project_id"),
            "project_a and project_b must have distinct project IDs"
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
    fixed_ids = set()
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
            fixed_ids.add(finding["id"])

    # Every fixed finding must be recorded in architecture.security_fixes_applied
    arch = contract.get("architecture", {})
    applied = {f.get("id", "") for f in arch.get("security_fixes_applied", [])}
    for finding in open_findings:
        if finding.get("status") == "fixed":
            fix_loc = finding.get("fix_location", "")
            errors.append(check(
                fix_loc in applied,
                f"fixed finding {finding['id']} has fix_location '{fix_loc}' "
                f"not found in architecture.security_fixes_applied {sorted(applied)}"
            ))
    # Direct cross-check: each fix entry must reference a real source path
    for fix in arch.get("security_fixes_applied", []):
        errors.append(check(
            fix.get("id", "").startswith(("crates/", "bins/")),
            f"security_fixes_applied entry must reference a repo source path, got {fix.get('id')!r}"
        ))
        errors.append(check(
            fix.get("class") == "existence_oracle",
            f"security_fixes_applied {fix.get('id')} must have class 'existence_oracle'"
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
    errors.append(check(
        arch.get("provider_modified") is False,
        "architecture.provider_modified must be false"
    ))
    errors.append(check(
        arch.get("non_disclosure_verified") is True,
        "architecture.non_disclosure_verified must be true"
    ))

    # No actual secret material anywhere in the contract
    secret_findings = walk_for_secrets(contract)
    for finding in secret_findings:
        errors.append(check(False, f"possible secret material: {finding}"))

    all_pass = all(errors)
    if all_pass:
        print(f"P13.6A contract validation: PASS ({len(errors)} checks)")
    else:
        fail_count = errors.count(False)
        print(f"P13.6A contract validation: FAIL ({fail_count} failures out of {len(errors)} checks)", file=sys.stderr)
        sys.exit(2)


def self_test():
    """Prove the validator rejects malformed baseline counts and gate entries."""
    import copy
    import tempfile

    base = load_json(CONTRACT_PATH)

    cases = [
        ("gates_passed + gates_blocked != gates_total",
         lambda c: c["baseline"].update(gates_passed=8, gates_blocked=3)),
        ("gates_blocked != len(blocked_gates)",
         lambda c: c["baseline"].update(gates_blocked=3)),
        ("duplicate blocked gate names",
         lambda c: c["baseline"]["blocked_gates"].append(
             copy.deepcopy(c["baseline"]["blocked_gates"][0]))),
        ("blocked gate missing exit_code",
         lambda c: c["baseline"]["blocked_gates"][0].pop("exit_code")),
        ("blocked gate with zero exit_code",
         lambda c: c["baseline"]["blocked_gates"][0].update(exit_code=0)),
        ("blocked gate not in canonical set",
         lambda c: c["baseline"]["blocked_gates"][0].update(
             script="tests/nonexistent_gate.sh")),
        ("status=verified with blocked gates",
         lambda c: c["baseline"].update(status="verified")),
        ("status=blocked with zero blocked gates",
         lambda c: (c["baseline"].update(gates_blocked=0, gates_passed=11),
                    c["baseline"].pop("blocked_gates"))),
    ]

    failures = 0
    for name, mutate in cases:
        fixture = copy.deepcopy(base)
        mutate(fixture)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", prefix="o3k-p13-6a-selftest-", delete=False
        ) as fh:
            json.dump(fixture, fh)
            path = fh.name
        try:
            # Run the validator as a subprocess so we can capture exit code
            import subprocess
            result = subprocess.run(
                [sys.executable, __file__, path],
                capture_output=True, text=True, timeout=30,
            )
            if result.returncode == 0:
                print(f"SELF-TEST FAIL: '{name}' was not rejected", file=sys.stderr)
                failures += 1
            else:
                print(f"self-test: '{name}' correctly rejected")
        finally:
            pathlib.Path(path).unlink(missing_ok=True)

    if failures:
        print(f"P13.6A validator self-test: FAIL ({failures} malformed inputs accepted)",
              file=sys.stderr)
        sys.exit(2)
    print(f"P13.6A validator self-test: PASS ({len(cases)} malformed inputs rejected)")


def main():
    if "--self-test" in sys.argv:
        self_test()
        return
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    path = args[0] if args else CONTRACT_PATH
    validate_contract(path)


if __name__ == "__main__":
    main()
