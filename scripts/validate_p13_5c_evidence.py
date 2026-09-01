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
SCENARIOS = {"native-mutable-drift", "native-delete-drift"}
UUID_SHA = re.compile(r"[0-9a-f]{40}")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def load_contract(repository: Path) -> tuple[dict, str]:
    path = repository / "docs/compatibility/p13-5/p13-5a-convergence-contract.json"
    contract = json.loads(path.read_text())
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    return contract, digest


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
    require(row["canonical_id_after_native_mutation"], "passed row lacks native post-state identity")
    require(row["canonical_id_after_reapply"], "passed row lacks reapply identity")
    require(row["owner_scope"], "passed row lacks owner_scope")
    require(isinstance(row["refresh_only_actions"], list), "refresh_only_actions must be a list")
    require(isinstance(row["normal_plan_actions"], list), "normal_plan_actions must be a list")
    require(isinstance(row["unrelated_changes_count"], int) and row["unrelated_changes_count"] == 0, "unrelated plan changes are present")
    require(isinstance(row["final_plan_noop"], bool) and row["final_plan_noop"], "final plan is not no-op")
    require(row["head_sha"] == CURRENT_SHA, "row is not bound to current HEAD")
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


def validate(document: dict, repository: Path, allow_blocked: bool) -> None:
    global CURRENT_SHA
    CURRENT_SHA = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    contract, contract_digest = load_contract(repository)
    require(document.get("artifact_type") == "o3k-p13-5c-native-drift-evidence", "invalid artifact_type")
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5C", "invalid phase")
    require(document.get("profile") == "p13-iac-compatibility-v1", "invalid profile")
    require(document.get("canonical_authority") == "o3k", "canonical authority is not O3K")
    require(document.get("provider_modified") is False, "provider_modified must be false")
    require(document.get("p13_5a_contract_sha256") == contract_digest, "evidence is not bound to P13.5A contract")
    require(document.get("tested_o3k_head_sha") == CURRENT_SHA, "evidence is not bound to current HEAD")
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
        require(row.get("resource") in {item["resource"] for item in contract["resources"]}, "unknown resource")
        require(row.get("scenario") in SCENARIOS, "unknown scenario")
        require(UUID_SHA.fullmatch(row.get("head_sha", "")), "invalid row head_sha")
        require(row["head_sha"] == CURRENT_SHA, "scenario is not bound to current HEAD")
        if row["result"] == "passed":
            validate_passed(row, contract)
        else:
            require(isinstance(row.get("reason"), str) and row["reason"].strip(), "blocked/not_applicable row needs reason")
            require(not row.get("plan_observation"), "blocked row must not contain fabricated plan JSON")
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
            "result": "blocked",
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
    parser.add_argument("--allow-blocked", action="store_true", help="validate honest blocked evidence; strict completion rejects it")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.evidence:
        parser.error("evidence is required unless --self-test is used")
    repository = Path(__file__).resolve().parents[1]
    validate(json.loads(Path(args.evidence).read_text()), repository, args.allow_blocked)
    print("P13.5C evidence structure: PASS")


if __name__ == "__main__":
    main()
