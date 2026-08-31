#!/usr/bin/env python3
"""Validate machine-readable P13.5B refresh/import evidence."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


RESOURCES = {
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
}
RESULTS = {
    "passed",
    "not_applicable",
    "upstream_provider_unsupported",
    "blocked",
    "deferred_p13_6",
}
SCENARIOS = {"stable-read", "import"}
REQUIRED = {
    "resource",
    "scenario",
    "canonical_id",
    "owner_scope",
    "provider_import_id",
    "provider_state_id",
    "first_read_route",
    "plan_actions",
    "final_plan_noop",
    "canonical_duplicate_count",
    "canonical_resource_count",
    "cleanup_result",
    "backend",
    "head_sha",
    "result",
}


class EvidenceValidationError(ValueError):
    """Raised when an evidence document violates the P13.5B contract."""


def require(condition: bool, message: str) -> None:
    """Enforce a validation invariant without relying on Python assertions."""

    if not condition:
        raise EvidenceValidationError(message)


def validate(document: dict, *, allow_incomplete: bool = False) -> None:
    require(
        document.get("artifact_type") == "o3k-p13-5b-refresh-import-evidence",
        "unexpected artifact_type",
    )
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5B", "unexpected phase")
    require(document.get("profile") == "p13-iac-compatibility-v1", "unexpected profile")
    toolchain = document.get("toolchain")
    require(isinstance(toolchain, dict), "toolchain must be an object")
    require(toolchain.get("opentofu") == "1.12.6", "unexpected OpenTofu version")
    require(toolchain.get("provider") == (
        "terraform-provider-openstack/openstack 3.4.0"
    ), "unexpected provider version")
    require(toolchain.get("provider_modified") is False, "provider must be unmodified")
    require(document.get("manual_state_edits") is False, "manual state edits are forbidden")
    require(document.get("canonical_authority") == "o3k", "canonical authority must be o3k")
    require(
        document.get("execution_mode") in {"gated", "exploratory_blocked_baseline"},
        "unexpected execution mode",
    )
    binding = document.get("evidence_binding")
    require(binding == {
        "mode": "source_commit_run_bound",
        "evidence_only_followup": True,
    }, "invalid evidence binding")
    scenarios = document.get("scenarios")
    require(isinstance(scenarios, list) and scenarios, "scenarios must be a non-empty list")
    expected_pairs = {(resource, scenario) for resource in RESOURCES for scenario in SCENARIOS}
    actual_pairs = {(item.get("resource"), item.get("scenario")) for item in scenarios}
    require(actual_pairs == expected_pairs, "scenarios must contain exactly both scenarios for all resources")
    require(len(actual_pairs) == len(scenarios), "scenario pairs must be unique")
    scenario_resources = [item.get("resource") for item in scenarios]
    require(
        set(scenario_resources) == RESOURCES,
        "scenario matrix must contain exactly the 12 contract resources",
    )
    tested_sha = document.get("tested_o3k_head_sha")
    require(isinstance(tested_sha, str) and re.fullmatch(r"[0-9a-f]{40}", tested_sha), "invalid tested_o3k_head_sha")
    repository = Path(__file__).resolve().parents[1]
    current_sha = subprocess.check_output(
        ["git", "-C", str(repository), "rev-parse", "HEAD"], text=True
    ).strip()
    if tested_sha != "0" * 40:
        require(
            subprocess.run(
                ["git", "-C", str(repository), "merge-base", "--is-ancestor", tested_sha, current_sha],
                check=False,
            ).returncode == 0,
            "tested_o3k_head_sha is not an ancestor of current HEAD",
        )
        changed = subprocess.check_output(
            ["git", "-C", str(repository), "diff", "--name-only", tested_sha, current_sha],
            text=True,
        ).splitlines()
        require(
            changed in ([], ["docs/compatibility/p13-5/p13-5b-refresh-import-evidence.json"]),
            "tested evidence contains changes outside the permitted evidence follow-up",
        )
    contract_path = Path(__file__).resolve().parents[1] / "docs/compatibility/p13-5/p13-5a-convergence-contract.json"
    contract = json.loads(contract_path.read_text())
    contract_resources = [item.get("resource") for item in contract.get("resources", [])]
    require(
        len(contract_resources) == len(RESOURCES)
        and len(set(contract_resources)) == len(RESOURCES)
        and set(contract_resources) == RESOURCES,
        "P13.5A contract must contain exactly 12 unique resources",
    )
    for hash_name in ("opentofu_archive_sha256", "provider_archive_sha256", "provider_binary_sha256"):
        require(
            toolchain.get(hash_name) == contract["toolchain"].get(hash_name),
            f"toolchain hash mismatch: {hash_name}",
        )
    require(
        toolchain.get("provider_sha256") == contract["toolchain"].get("provider_binary_sha256"),
        "provider_sha256 must match the contract provider binary hash",
    )
    contract_imports = {item["resource"]: item["import"] for item in contract["resources"]}
    single_id_resources = {"openstack_compute_keypair_v2", "openstack_networking_network_v2"}
    for scenario in scenarios:
        require(isinstance(scenario, dict), "each scenario must be an object")
        require(REQUIRED <= scenario.keys(), "scenario is missing required fields")
        require(scenario["resource"] in RESOURCES, "scenario has an unknown resource")
        require(scenario["scenario"] in SCENARIOS, "scenario has an unknown scenario name")
        require(scenario["result"] in RESULTS, "scenario has an unknown result")
        if scenario["result"] == "upstream_provider_unsupported":
            require(
                contract_imports[scenario["resource"]] == "unsupported",
                f'{scenario["resource"]} is contract-supported and cannot be marked upstream_provider_unsupported',
            )
        if scenario["result"] == "not_applicable":
            require(
                contract_imports[scenario["resource"]] == "not_applicable",
                f'{scenario["resource"]} is contract-supported and cannot be marked not_applicable',
            )
        require(
            re.fullmatch(r"[0-9a-f]{40}", scenario["head_sha"]),
            "invalid scenario head_sha",
        )
        require(scenario["head_sha"] == tested_sha, "scenario head_sha does not match tested head")
        require(isinstance(scenario["plan_actions"], list), "plan_actions must be a list")
        for field in ("refresh_plan_actions", "normal_plan_actions"):
            if field in scenario:
                require(
                    isinstance(scenario[field], list)
                    and all(isinstance(actions, list) for actions in scenario[field]),
                    f"{field} must be a list of action lists",
                )
        require(isinstance(scenario["final_plan_noop"], bool), "final_plan_noop must be boolean")
        require(
            scenario["canonical_duplicate_count"] is None
            or isinstance(scenario["canonical_duplicate_count"], int),
            "canonical_duplicate_count must be an integer or null",
        )
        if scenario["result"] == "passed":
            # OpenTofu omits resource_changes for a no-op plan, so the
            # structured action list is empty.  Keep this as [] rather than
            # inferring a no-op from CLI text.
            require(all(action == "no-op" for action in scenario["plan_actions"]), "passed scenario has non-no-op actions")
            for field in ("refresh_plan_actions", "normal_plan_actions"):
                if field in scenario:
                    require(
                        all(all(action == "no-op" for action in actions) for actions in scenario[field]),
                        f"passed scenario has non-no-op {field}",
                    )
            require(scenario["final_plan_noop"] is True, "passed scenario is not marked no-op")
            require(scenario["canonical_id"], "passed scenario is missing canonical_id")
            require(scenario["owner_scope"], "passed scenario is missing owner_scope")
            require(
                scenario["provider_state_id"] == scenario["canonical_id"],
                "provider state ID does not match canonical ID",
            )
            require(scenario["canonical_duplicate_count"] == 0, "passed scenario has duplicate canonical resources")
            require(scenario["canonical_resource_count"] == 1, "passed scenario must observe one canonical resource")
            require(
                scenario["cleanup_result"] == "passed"
                or (
                    scenario["resource"] == "openstack_compute_instance_v2"
                    and scenario["scenario"] == "import"
                    and scenario["cleanup_result"] == "retained"
                ),
                "passed scenario has unsuccessful cleanup",
            )
            trace_observation = scenario.get("trace_observation")
            require(isinstance(trace_observation, dict), "trace_observation must be an object")
            routes = trace_observation.get("provider_read_routes")
            require(routes, "passed scenario has no provider read routes")
            require(
                isinstance(trace_observation.get("trace_start_ordinal"), int),
                "trace_start_ordinal must be an integer",
            )
            require(all(
                isinstance(route, dict)
                and route["method"] == "GET"
                and isinstance(route["path"], str)
                and route["path"].startswith("/v2.")
                and isinstance(route["ordinal"], int)
                and route["ordinal"] >= trace_observation["trace_start_ordinal"]
                for route in routes
            ), "provider read trace contains an invalid route")
            require(
                scenario["first_read_route"] == f'{routes[0]["method"]} {routes[0]["path"]}',
                "first_read_route does not match the trace",
            )
            identity = scenario.get("canonical_identity_observation")
            require(isinstance(identity, dict), "canonical_identity_observation must be an object")
            require(identity.get("resource_id") == scenario["canonical_id"], "canonical identity ID mismatch")
            require(identity.get("owner_scope") == scenario["owner_scope"], "canonical owner scope mismatch")
            require(
                identity.get("observed_owner_scope") == scenario["owner_scope"],
                "observed canonical owner scope mismatch",
            )
            if scenario["scenario"] == "stable-read":
                require(len(scenario["refresh_plan_actions"]) >= 2, "stable-read requires repeated refresh plans")
                require(len(scenario["normal_plan_actions"]) >= 2, "stable-read requires repeated normal plans")
            else:
                require(scenario["provider_import_id"], "import scenario is missing provider_import_id")
                if scenario["resource"] in single_id_resources:
                    require(
                        scenario["provider_import_id"] == scenario["canonical_id"],
                        "single-ID import does not use the canonical ID",
                    )
                require(len(scenario["normal_plan_actions"]) >= 1, "import requires a normal plan")
        else:
            require(scenario["result"] != "passed", "non-passed scenario has passed result")
            require(scenario.get("reason"), "non-passed scenarios require a classification reason")
    require(document.get("status") in {"passed", "blocked"}, "invalid document status")
    if not allow_incomplete:
        require(document.get("status") == "passed", "strict validation rejects incomplete evidence")
    if document["status"] == "passed":
        require(document["execution_mode"] == "gated", "passed evidence must use gated execution")
        baseline = document.get("existing_p13_baseline")
        require(isinstance(baseline, dict) and baseline.get("status") == "verified", "passed evidence needs a verified baseline")
        require(all(s["result"] == "passed" for s in scenarios), "passed evidence cannot contain incomplete scenarios")


def self_test() -> None:
    base = {
        "artifact_type": "o3k-p13-5b-refresh-import-evidence",
        "schema_version": 1,
        "phase": "P13.5B",
        "profile": "p13-iac-compatibility-v1",
        "status": "blocked",
        "tested_o3k_head_sha": "0" * 40,
        "execution_mode": "exploratory_blocked_baseline",
        "evidence_binding": {"mode": "source_commit_run_bound", "evidence_only_followup": True},
        "existing_p13_baseline": {"status": "blocked"},
        "manual_state_edits": False,
        "canonical_authority": "o3k",
        "toolchain": {
            "opentofu": "1.12.6",
            "provider": "terraform-provider-openstack/openstack 3.4.0",
            "opentofu_archive_sha256": "50a6106fa4de523d09c87af85f3db1dd47535fc005727fdca6852146476b88ec",
            "provider_archive_sha256": "11b3c88e24197a29b13cf5ab41771944bd16707b561645323e8cbb4f1da00b7b",
            "provider_binary_sha256": "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc",
            "provider_sha256": "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc",
            "provider_modified": False,
        },
        "scenarios": [
            {
                "resource": "openstack_compute_keypair_v2",
                "scenario": "import",
                "canonical_id": "keypair-name",
                "owner_scope": "project-a",
                "provider_import_id": "keypair-name",
                "first_read_route": "GET /v2.1/project/os-keypairs/keypair-name",
                "plan_actions": ["no-op"],
                "refresh_plan_actions": [],
                "normal_plan_actions": [["no-op"]],
                "final_plan_noop": True,
                "canonical_duplicate_count": 0,
                "canonical_resource_count": 1,
                "cleanup_result": "passed",
                "backend": "sqlite",
                "head_sha": "0" * 40,
                "provider_state_id": "keypair-name",
                "trace_observation": {"trace_start_ordinal": 0, "provider_read_routes": [{"method": "GET", "path": "/v2.1/project/os-keypairs/keypair-name", "ordinal": 0}]},
                "canonical_identity_observation": {"resource_id": "keypair-name", "owner_scope": "project-a", "observed_owner_scope": "project-a"},
                "result": "passed",
            }
        ],
    }
    for resource in sorted(RESOURCES):
        for scenario_name in sorted(SCENARIOS):
            if (resource, scenario_name) == ("openstack_compute_keypair_v2", "import"):
                continue
            base["scenarios"].append(
                {
                    "resource": resource,
                    "scenario": scenario_name,
                    "canonical_id": "",
                    "owner_scope": "project-a",
                    "provider_import_id": "",
                    "provider_state_id": None,
                    "first_read_route": "",
                    "plan_actions": [],
                    "refresh_plan_actions": [],
                    "normal_plan_actions": [],
                    "final_plan_noop": False,
                    "canonical_duplicate_count": None,
                    "canonical_resource_count": None,
                    "cleanup_result": "not_run",
                    "backend": "sqlite",
                    "head_sha": "0" * 40,
                    "trace_observation": {"provider_read_routes": [], "trace_start_ordinal": None},
                    "canonical_identity_observation": {"resource_id": None},
                    "result": "blocked",
                    "reason": "fixture blocked for validator self-test",
                }
            )
    validate(base, allow_incomplete=True)
    negative = json.loads(json.dumps(base))
    negative["scenarios"][0]["plan_actions"] = ["update"]
    try:
        validate(negative)
    except EvidenceValidationError:
        pass
    else:
        raise EvidenceValidationError("non-no-op passed scenario was accepted")
    print("P13.5B evidence validator self-test: PASS")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence", nargs="?")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--allow-incomplete",
        action="store_true",
        help="accept blocked exploratory evidence; never use for completion gates",
    )
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.evidence:
        parser.error("evidence is required unless --self-test is used")
    document = json.loads(Path(args.evidence).read_text())
    validate(document, allow_incomplete=args.allow_incomplete)
    print("P13.5B evidence structure: PASS")


if __name__ == "__main__":
    main()
