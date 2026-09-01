#!/usr/bin/env python3
"""Validate machine-readable P13.5C native-drift evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path


RESULTS = {"passed", "blocked", "not_applicable"}
CLASSIFICATIONS = {
    "passed",
    "native_surface_not_defined",
    "not_applicable",
    "execution_profile_unavailable",
    "upstream_provider_unsupported",
    "blocked",
}
SCENARIOS = {"native-mutable-drift", "native-delete-drift"}
UUID_SHA = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def load_contract(repository: Path) -> tuple[dict, str]:
    path = repository / "docs/compatibility/p13-5/p13-5a-convergence-contract.json"
    contract = json.loads(path.read_text())
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    return contract, digest


def validate_surface_amendment(document: dict, repository: Path) -> None:
    contract, contract_digest = load_contract(repository)
    require(
        document.get("artifact_type") == "o3k-p13-5c-drift-surface-amendment",
        "invalid drift-surface amendment artifact_type",
    )
    require(document.get("schema_version") == 2, "unsupported drift-surface amendment schema_version")
    require(document.get("phase") == "P13.5C", "invalid drift-surface amendment phase")
    require(document.get("status") == "documentation_only", "drift-surface amendment is not documentation-only")
    require(document.get("p13_5a_contract_sha256") == contract_digest, "drift-surface amendment is not bound to frozen P13.5A contract")
    surfaces = document.get("surface_classes")
    require(isinstance(surfaces, dict), "drift-surface amendment lacks surface_classes")
    require(set(surfaces) == {"canonical_out_of_band", "native_api"}, "drift-surface amendment has unexpected surface classes")
    operations = ["mutable", "deletion"]
    canonical = surfaces["canonical_out_of_band"]
    require(canonical.get("native_api_claim") is False, "canonical surface claims native API evidence")
    require(canonical.get("operations") == operations, "canonical surface operations are not separated")
    native = surfaces["native_api"]
    require(native.get("native_api_claim") is True, "native surface does not claim native API evidence")
    require(native.get("operations") == operations, "native surface operations are not separated")
    require(native.get("missing_surface_result") == "native_surface_not_defined", "native surface missing result is not explicit")
    extension = document.get("evidence_row_extension")
    require(isinstance(extension, dict), "drift-surface amendment lacks evidence_row_extension")
    require(extension.get("surface", {}).get("enum") == ["canonical_out_of_band", "native_api"], "invalid surface enum")
    require(extension.get("native_surface_status", {}).get("enum") == ["defined", "native_surface_not_defined", "not_checked"], "invalid native surface status enum")
    require(extension.get("operation", {}).get("enum") == operations, "invalid operation enum")
    requirements = extension.get("surface_requirements")
    require(isinstance(requirements, dict), "drift-surface amendment lacks surface_requirements")
    require(requirements.get("canonical_out_of_band") == {
        "surface": "canonical_out_of_band",
        "native_claim": False,
        "native_surface_status": "omitted",
    }, "invalid canonical surface requirements")
    require(requirements.get("native_api") == {
        "surface": "native_api",
        "native_claim": True,
        "native_surface_status": "required",
    }, "invalid native surface requirements")
    compatibility = document.get("compatibility")
    require(isinstance(compatibility, dict), "drift-surface amendment lacks compatibility declaration")
    require(compatibility.get("frozen_p13_5a_contract_modified") is False, "amendment modifies frozen P13.5A contract")
    require(compatibility.get("production_runtime_modified") is False, "amendment modifies production runtime")


def validate_redactions(value: object) -> None:
    sensitive = {"password", "token", "access_token", "secret", "private_key"}
    if isinstance(value, dict):
        for key, item in value.items():
            if key.lower() in sensitive:
                require(item == "[REDACTED]", f"sensitive plan field {key} is not redacted")
            else:
                validate_redactions(item)
    elif isinstance(value, list):
        for item in value:
            validate_redactions(item)


def validate_canonical_evidence(document: dict, repository: Path) -> None:
    """Validate the executable out-of-band compatibility drift artifact."""
    contract, contract_digest = load_contract(repository)
    require(document.get("artifact_type") == "o3k-p13-5c-canonical-out-of-band-drift-evidence", "invalid canonical drift artifact_type")
    require(document.get("schema_version") == 1, "unsupported canonical drift schema_version")
    require(document.get("phase") == "P13.5C", "invalid canonical drift phase")
    require(document.get("profile") == "p13-iac-compatibility-v1", "invalid canonical drift profile")
    require(document.get("status") in RESULTS, "invalid canonical drift status")
    require(document.get("surface") == "canonical_out_of_band", "canonical drift surface is not explicit")
    require(document.get("native_claim") is False, "canonical drift claims native API evidence")
    require(document.get("canonical_authority") == "o3k", "canonical authority is not O3K")
    require(document.get("provider_modified") is False, "provider modification is not explicitly false")
    tested = document.get("tested_o3k_head_sha")
    baseline = document.get("baseline")
    require(isinstance(baseline, dict) and baseline.get("status") == "verified" and UUID_SHA.fullmatch(baseline.get("source_commit", "")), "canonical drift lacks verified baseline binding")
    require(baseline["source_commit"] == tested, "canonical drift baseline is not bound to tested HEAD")
    require(SHA256.fullmatch(baseline.get("evidence_sha256", "")), "canonical drift baseline digest is missing")
    require(document.get("p13_5a_contract_sha256") == contract_digest, "canonical drift is not bound to P13.5A")
    current = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    require(isinstance(tested, str) and UUID_SHA.fullmatch(tested), "invalid canonical drift tested SHA")
    require(subprocess.run(["git", "-C", str(repository), "merge-base", "--is-ancestor", tested, current], check=False).returncode == 0, "canonical drift tested SHA is not an ancestor")
    toolchain = document.get("toolchain")
    require(isinstance(toolchain, dict), "canonical drift toolchain is missing")
    require(toolchain.get("opentofu") == contract["toolchain"]["opentofu"], "canonical drift OpenTofu mismatch")
    require(toolchain.get("provider") == contract["toolchain"]["provider"], "canonical drift provider mismatch")
    require(toolchain.get("provider_modified") is False, "canonical drift provider modification is not false")
    row = document.get("scenario")
    require(isinstance(row, dict), "canonical drift scenario is missing")
    require(row.get("resource") == "openstack_networking_network_v2", "canonical drift resource is outside bounded executable scope")
    require(row.get("scenario") == "canonical_out_of_band_mutable_drift", "invalid canonical drift scenario")
    require(row.get("operation") == "mutable", "canonical drift operation is not explicit")
    require(row.get("surface") == "canonical_out_of_band" and row.get("native_claim") is False, "canonical drift row surface is invalid")
    require(row.get("mutation_route") == "PUT /v2.0/networks/{id}", "canonical drift mutation route is not the accepted compatibility route")
    if document.get("status") != "passed":
        require(row.get("result") in {"blocked", "not_applicable"}, "blocked canonical artifact has invalid row result")
        require(isinstance(row.get("reason"), str) and row["reason"].strip(), "blocked canonical artifact lacks reason")
        require("plan_observation" not in row, "blocked canonical artifact contains fabricated plan JSON")
        return
    require(row.get("native_change") in {"name", "description", "admin_state_up"}, "canonical drift attribute is outside contract")
    require(row.get("canonical_id_before") == row.get("canonical_id_after_mutation") == row.get("canonical_id_after_reapply"), "canonical identity changed")
    require(row.get("owner_scope"), "canonical drift owner scope is missing")
    require(row.get("old_resource_absent") is False and row.get("new_resource_count") == 1, "canonical resource count invariant failed")
    require(row.get("canonical_same_id_count") == 1, "canonical identity count invariant failed")
    require(row.get("canonical_project_resource_count_before") == 1, "canonical project has unexpected initial resource count")
    require(row.get("canonical_project_resource_count_after_mutation") == 1, "canonical project gained a duplicate during mutation")
    require(row.get("canonical_project_resource_count_after_reapply") == 1, "canonical project has unexpected reapply resource count")
    require(row.get("canonical_project_resource_count_after_cleanup") == 0, "canonical project cleanup left resources")
    require(row.get("unrelated_changes_count") == 0, "unrelated plan changes are present")
    require(row.get("final_plan_noop") is True, "canonical final plan is not no-op")
    require(row.get("cleanup_http_status") == 404, "canonical drift cleanup did not remove the fixture")
    observations = row.get("canonical_observations")
    require(isinstance(observations, dict), "canonical observations are missing")
    require(observations.get("before", {}).get("records", [{}])[0].get("resource_id") == row.get("canonical_id_before"), "canonical before identity summary disagrees")
    require(observations.get("after_mutation", {}).get("records", [{}])[0].get("resource_id") == row.get("canonical_id_after_mutation"), "canonical mutation identity summary disagrees")
    require(observations.get("after_reapply", {}).get("records", [{}])[0].get("resource_id") == row.get("canonical_id_after_reapply"), "canonical reapply identity summary disagrees")
    require(observations.get("after_cleanup", {}).get("count") == 0, "canonical cleanup observation disagrees")
    observation = row.get("plan_observation")
    require(isinstance(observation, dict), "canonical drift plan observation is missing")
    for name in ("initial_normal", "refresh_only", "normal", "final_normal"):
        plan = observation.get(name)
        require(isinstance(plan, dict), f"canonical drift plan {name} is missing")
        require(isinstance(plan.get("format_version"), str), f"canonical drift plan {name} lacks format_version")
        require(isinstance(plan.get("planned_values"), dict) and isinstance(plan.get("prior_state"), dict), f"canonical drift plan {name} lacks state fields")
        validate_redactions(plan)
    refresh = observation["refresh_only"]
    drift = refresh.get("resource_drift", [])
    require(len(drift) == 1 and drift[0].get("address") == row["terraform_address"] and drift[0].get("change", {}).get("actions") == ["update"], "refresh-only lacks exact managed drift")
    normal = observation["normal"].get("resource_changes", [])
    managed = [change for change in normal if change.get("address") == row["terraform_address"] and change.get("change", {}).get("actions") == ["update"]]
    unrelated = [change for change in normal if change.get("address") != row["terraform_address"] and change.get("change", {}).get("actions") not in ([], ["no-op"])]
    require(managed, "canonical drift normal plan lacks exact in-place update")
    require(not unrelated, "canonical drift normal plan has unrelated changes")
    if document.get("status") == "passed":
        require(row.get("result") == "passed", "passed canonical artifact has incomplete row")


def expected_cells(contract: dict) -> set[tuple[str, str, str]]:
    cells: set[tuple[str, str, str]] = set()
    for item in contract["resources"]:
        resource = item["resource"]
        for attribute in item.get("mutable_attributes", []):
            cells.add((resource, "native-mutable-drift", attribute))
        cells.add((resource, "native-delete-drift", "remote absence"))
    return cells


def validate_plan_observation(row: dict) -> None:
    observation = row.get("plan_observation")
    require(isinstance(observation, dict), "passed row needs plan_observation")
    for kind, key in (("refresh-only", "resource_drift"), ("normal", "resource_changes")):
        documents = observation.get(kind)
        require(isinstance(documents, list) and documents, f"missing raw {kind} plan JSON")
        for document in documents:
            require(isinstance(document, dict), f"{kind} plan must be an object")
            require(isinstance(document.get("format_version"), str), f"{kind} plan lacks format_version")
            require(key in document and isinstance(document[key], list), f"{kind} plan lacks {key}")
            require("planned_values" in document and "prior_state" in document, f"{kind} plan lacks state fields")


def validate_passed(row: dict, contract: dict) -> None:
    resource, scenario, native_change = row["resource"], row["scenario"], row["native_change"]
    item = next(item for item in contract["resources"] if item["resource"] == resource)
    require(row["terraform_address"], "passed row lacks terraform_address")
    require(row["canonical_id_before"], "passed row lacks canonical_id_before")
    if scenario == "native-mutable-drift":
        require(row["canonical_id_after_native_mutation"], "passed mutable row lacks native post-state identity")
    else:
        require(
            row["canonical_id_after_native_mutation"] is None
            or row["canonical_id_after_native_mutation"],
            "passed delete row lacks native post-state identity marker",
        )
    require(row["canonical_id_after_reapply"], "passed row lacks reapply identity")
    require(row["owner_scope"], "passed row lacks owner_scope")
    require(isinstance(row["refresh_only_actions"], list), "refresh_only_actions must be a list")
    require(isinstance(row["normal_plan_actions"], list), "normal_plan_actions must be a list")
    require(isinstance(row["unrelated_changes_count"], int) and row["unrelated_changes_count"] == 0, "unrelated plan changes are present")
    require(isinstance(row["final_plan_noop"], bool) and row["final_plan_noop"], "final plan is not no-op")
    require(row["head_sha"] == TESTED_SHA, "row is not bound to tested HEAD")
    require(row["provider_modified"] is False, "provider modification is not explicitly false")
    validate_plan_observation(row)
    if scenario == "native-mutable-drift":
        require(native_change in item.get("mutable_attributes", []), "mutable drift is outside frozen contract")
        require(row["canonical_id_before"] == row["canonical_id_after_native_mutation"] == row["canonical_id_after_reapply"], "mutable drift changed canonical identity")
        require(row["old_resource_absent"] is False, "mutable drift claims old resource absent")
        require(row["new_resource_count"] == 1, "mutable drift does not retain one resource")
        require(row["normal_plan_actions"] and row["normal_plan_actions"][0].get("actions") == ["update"], "mutable drift lacks exact in-place update")
        require(row["normal_plan_actions"][0].get("address") == row["terraform_address"], "mutable drift address is not exact")
        require(row["normal_plan_actions"][0].get("replacement") is False, "mutable drift proposes replacement")
    else:
        require(native_change == "remote absence", "delete drift native_change is invalid")
        require(row["old_resource_absent"] is True, "delete drift lacks authoritative absence")
        require(row["new_resource_count"] == 1, "delete drift does not prove one replacement")
        require(row["canonical_id_before"] != row["canonical_id_after_reapply"], "delete drift did not create a new identity")
        require(row.get("surface") == "native_api" and row.get("native_surface_status") == "defined", "delete drift surface is invalid")
        require(row.get("native_absence_http_status") == 404 and row.get("compatibility_absence_http_status") == 404, "delete drift absence statuses are not 404")
        delete = row.get("native_delete")
        require(isinstance(delete, dict), "delete drift lacks native DELETE evidence")
        require(delete.get("status") == 204 and delete.get("replay_status") == 204, "native DELETE did not complete and replay successfully")
        expected_path = {
            "openstack_networking_network_v2": f"/o3k/v1/network/networks/{row['canonical_id_before']}",
            "openstack_blockstorage_volume_v3": f"/o3k/v1/volume/volumes/{row['canonical_id_before']}",
        }.get(resource)
        require(delete.get("http_path") == expected_path, "native DELETE path is not bound to the old identity")
        require(delete.get("idempotency_key") == "[REDACTED]" and SHA256.fullmatch(delete.get("idempotency_key_sha256", "")), "idempotency key is not safely represented")
        replay = delete.get("replay_result")
        require(isinstance(replay, dict) and replay.get("same_idempotency_key") is True, "DELETE replay lacks same-key evidence")
        require(replay.get("same_terminal_canonical_absence") is True and replay.get("second_destructive_effect_observed") is False, "DELETE replay did not converge to the same terminal state")
        observations = row.get("canonical_observations")
        require(isinstance(observations, dict), "delete drift lacks canonical observations")
        require(observations.get("before", {}).get("old_present") is True, "canonical before observation lacks old resource")
        require(observations.get("after_delete", {}).get("old_present") is False, "canonical delete observation retains old resource")
        require(observations.get("after_delete_replay", {}).get("old_present") is False, "canonical replay observation revives old resource")
        require(observations.get("after_reapply", {}).get("replacement_count") == 1, "canonical reapply observation lacks one replacement")
        leak = row.get("leak_or_foreign_state")
        require(isinstance(leak, dict) and leak.get("old_absent") is True and leak.get("scope_unchanged") is True and leak.get("unrelated_changes") is True, "delete drift leak/foreign-state result is incomplete")


def validate(document: dict, repository: Path, allow_blocked: bool) -> None:
    global TESTED_SHA
    current_sha = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    contract, contract_digest = load_contract(repository)
    require(document.get("artifact_type") == "o3k-p13-5c-native-drift-evidence", "invalid artifact_type")
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5C", "invalid phase")
    require(document.get("profile") == "p13-iac-compatibility-v1", "invalid profile")
    require(document.get("canonical_authority") == "o3k", "canonical authority is not O3K")
    require(document.get("provider_modified") is False, "provider_modified must be false")
    require(document.get("p13_5a_contract_sha256") == contract_digest, "evidence is not bound to P13.5A contract")
    TESTED_SHA = document.get("tested_o3k_head_sha")
    require(isinstance(TESTED_SHA, str) and UUID_SHA.fullmatch(TESTED_SHA), "invalid tested_o3k_head_sha")
    require(subprocess.run(["git", "-C", str(repository), "merge-base", "--is-ancestor", TESTED_SHA, current_sha], check=False).returncode == 0, "tested SHA is not an ancestor of current HEAD")
    changed = subprocess.check_output(["git", "-C", str(repository), "diff", "--name-only", TESTED_SHA, current_sha], text=True).splitlines()
    allowed_followups = {
        "docs/compatibility/p13-5/p13-5c-native-drift-evidence.json",
        "docs/compatibility/p13-5/p13-5c-canonical-out-of-band-drift-evidence.json",
    }
    require(set(changed).issubset(allowed_followups), "changes after tested SHA exceed evidence-only follow-up")
    toolchain = document.get("toolchain")
    require(isinstance(toolchain, dict), "toolchain is missing")
    require(toolchain.get("opentofu") == contract["toolchain"]["opentofu"], "OpenTofu version mismatch")
    require(toolchain.get("provider") == contract["toolchain"]["provider"], "provider version mismatch")
    require(toolchain.get("provider_modified") is False, "toolchain provider_modified must be false")

    rows = document.get("scenarios")
    require(isinstance(rows, list), "scenarios must be a list")
    expected = expected_cells(contract)
    actual = {(r.get("resource"), r.get("scenario"), r.get("native_change")) for r in rows}
    require(actual == expected, "scenario matrix does not exactly match frozen mutable/deletion scope")
    require(len(rows) == len(actual), "scenario cells are duplicated")
    require(document.get("status") in RESULTS, "invalid evidence status")
    for row in rows:
        require(isinstance(row, dict), "scenario row must be an object")
        require(row.get("result") in RESULTS, "invalid scenario result")
        require(row.get("classification") in CLASSIFICATIONS, "invalid scenario classification")
        require(row.get("surface") == "native_api", "native scenario surface is not explicit")
        require(row.get("native_surface_status") in {"defined", "native_surface_not_defined", "not_checked"}, "invalid native surface status")
        require(row.get("resource") in {item["resource"] for item in contract["resources"]}, "unknown resource")
        require(row.get("scenario") in SCENARIOS, "unknown scenario")
        require(row.get("operation") == ("mutable" if row["scenario"] == "native-mutable-drift" else "deletion"), "native scenario operation is inconsistent")
        require(UUID_SHA.fullmatch(row.get("head_sha", "")), "invalid row head_sha")
        require(row["head_sha"] == TESTED_SHA, "scenario is not bound to tested HEAD")
        if row["result"] == "passed":
            require(row["classification"] == "passed", "passed scenario has a non-passed classification")
            validate_passed(row, contract)
        else:
            require(isinstance(row.get("reason"), str) and row["reason"].strip(), "blocked/not_applicable row needs reason")
            require(not row.get("plan_observation"), "blocked row must not contain fabricated plan JSON")
            if "native_surface_not_defined" in row["reason"]:
                require(row["native_surface_status"] == "native_surface_not_defined", "undefined native surface status is inconsistent")
                require(row["classification"] == "native_surface_not_defined", "undefined native surface classification is inconsistent")
    if document["status"] == "passed":
        require(all(row["result"] == "passed" for row in rows), "passed evidence contains incomplete rows")
    elif not allow_blocked:
        raise ValueError("strict validation rejects blocked evidence")


def self_test() -> None:
    repository = Path(__file__).resolve().parents[1]
    contract, digest = load_contract(repository)
    rows = []
    for resource, scenario, native_change in sorted(expected_cells(contract)):
        rows.append({
            "resource": resource,
            "scenario": scenario,
            "native_change": native_change,
            "operation": "mutable" if scenario == "native-mutable-drift" else "deletion",
            "surface": "native_api",
            "native_surface_status": "not_checked",
            "result": "blocked",
            "classification": "blocked",
            "reason": "self-test blocked fixture",
            "head_sha": subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip(),
        })
    base = {
        "artifact_type": "o3k-p13-5c-native-drift-evidence",
        "schema_version": 1,
        "phase": "P13.5C",
        "profile": "p13-iac-compatibility-v1",
        "status": "blocked",
        "canonical_authority": "o3k",
        "provider_modified": False,
        "p13_5a_contract_sha256": digest,
        "tested_o3k_head_sha": rows[0]["head_sha"],
        "toolchain": {"opentofu": contract["toolchain"]["opentofu"], "provider": contract["toolchain"]["provider"], "provider_modified": False},
        "scenarios": rows,
    }
    validate(base, repository, allow_blocked=True)
    print(f"P13.5C evidence validator self-test: PASS ({len(rows)} cells)")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence", nargs="?")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--surface-amendment", type=Path, help="validate the documentation-only drift-surface amendment")
    parser.add_argument("--canonical-evidence", type=Path, help="validate canonical/out-of-band drift evidence")
    parser.add_argument("--allow-blocked", action="store_true", help="validate honest blocked evidence; strict completion rejects it")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    if args.canonical_evidence:
        validate_canonical_evidence(json.loads(args.canonical_evidence.read_text()), repository)
        print("P13.5C canonical/out-of-band evidence: PASS")
        return
    if args.surface_amendment:
        validate_surface_amendment(json.loads(args.surface_amendment.read_text()), repository)
        print("P13.5C drift-surface amendment: PASS")
        return
    if args.self_test:
        self_test()
        return
    if not args.evidence:
        parser.error("evidence is required unless --self-test is used")
    validate(json.loads(Path(args.evidence).read_text()), repository, args.allow_blocked)
    print("P13.5C evidence structure: PASS")


if __name__ == "__main__":
    main()
