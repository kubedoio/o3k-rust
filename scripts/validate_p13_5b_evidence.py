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

IMPORT_ID_SHAPES = {
    "openstack_compute_keypair_v2": "name",
    "openstack_networking_network_v2": "uuid",
    "openstack_networking_subnet_v2": "uuid",
    "openstack_networking_port_v2": "uuid",
    "openstack_compute_instance_v2": "uuid",
    "openstack_networking_secgroup_v2": "uuid",
    "openstack_networking_secgroup_rule_v2": "uuid",
    "openstack_networking_router_v2": "uuid",
    "openstack_networking_router_interface_v2": "uuid",
    "openstack_networking_floatingip_v2": "uuid",
    "openstack_blockstorage_volume_v3": "uuid",
    "openstack_compute_volume_attach_v2": "server_uuid/attachment_uuid",
}

UUID_RE = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-7][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}"
BASELINE_GATES = {
    "tests/p13_2_core_lifecycle.sh",
    "tests/p13_2b_subnet_lifecycle.sh",
    "tests/p13_2c_port_lifecycle.sh",
    "tests/p13_2d_server_lifecycle.sh",
    "tests/p13_3_security_group_provider.sh",
    "tests/p13_3_security_group_port_provider.sh",
    "tests/p13_3_router_provider.sh",
    "tests/p13_3_floating_ip_provider.sh",
    "tests/p13_4_provider_volume_smoke.sh",
    "tests/p13_4_provider_volume_attachment_smoke.sh",
    "tests/p13_4_storage_lifecycle.sh",
}

IMPORT_ROUTES = {
    "openstack_compute_keypair_v2": re.compile(r"^GET /v2\.1/[^/]+/os-keypairs/[^/]+$"),
    "openstack_networking_network_v2": re.compile(r"^GET /v2\.0/networks/" + UUID_RE + r"$"),
    "openstack_networking_subnet_v2": re.compile(r"^GET /v2\.0/subnets/" + UUID_RE + r"$"),
    "openstack_networking_port_v2": re.compile(r"^GET /v2\.0/ports/" + UUID_RE + r"$"),
    "openstack_compute_instance_v2": re.compile(r"^GET /v2\.1/[^/]+/servers/" + UUID_RE + r"$"),
    "openstack_networking_secgroup_v2": re.compile(r"^GET /v2\.0/security-groups/" + UUID_RE + r"$"),
    "openstack_networking_secgroup_rule_v2": re.compile(r"^GET /v2\.0/security-group-rules/" + UUID_RE + r"$"),
    "openstack_networking_router_v2": re.compile(r"^GET /v2\.0/routers/" + UUID_RE + r"$"),
    "openstack_networking_router_interface_v2": re.compile(r"^GET /v2\.0/ports/" + UUID_RE + r"$"),
    "openstack_networking_floatingip_v2": re.compile(r"^GET /v2\.0/floatingips/" + UUID_RE + r"$"),
    "openstack_blockstorage_volume_v3": re.compile(r"^GET /v3/[^/]+/volumes/" + UUID_RE + r"$"),
    "openstack_compute_volume_attach_v2": re.compile(
        r"^GET /v2\.1/[^/]+/servers/" + UUID_RE + r"/os-volume_attachments/" + UUID_RE + r"$"
    ),
}


def validate_plan_observation(scenario: dict) -> None:
    """Require complete structured plan documents, not an inferred empty list."""

    observation = scenario.get("plan_observation")
    require(isinstance(observation, dict), "passed scenario needs plan_observation")
    for kind in ("refresh-only", "normal"):
        documents = observation.get(kind)
        require(isinstance(documents, list), f"missing structured {kind} plan documents")
        if kind == "normal":
            require(documents, "missing structured normal plan documents")
        for plan in documents:
            require(isinstance(plan, dict), f"{kind} plan document must be an object")
            changes_key = "resource_drift" if kind == "refresh-only" else "resource_changes"
            require(changes_key in plan, f"{kind} plan is missing {changes_key}")
            require(isinstance(plan[changes_key], list), f"{kind} {changes_key} must be a list")
            require(isinstance(plan.get("format_version"), str), f"{kind} plan is missing format_version")
            require("planned_values" in plan, f"{kind} plan is missing planned_values")
            require("prior_state" in plan, f"{kind} plan is missing prior_state")
            for change in plan[changes_key]:
                require(isinstance(change, dict), f"{kind} resource change must be an object")
                actions = change.get("change", {}).get("actions")
                require(isinstance(actions, list), f"{kind} resource change is missing actions")
                require(all(action in {"no-op", "create", "read", "update", "delete"} for action in actions), "invalid structured plan action")
                require(all(action == "no-op" for action in actions), f"{kind} structured plan is not a no-op")


def validate_trace_observation(scenario: dict) -> None:
    trace = scenario.get("trace_observation")
    require(isinstance(trace, dict), "trace_observation must be an object")
    start = trace.get("trace_start_ordinal")
    end = trace.get("trace_end_ordinal")
    require(isinstance(start, int) and isinstance(end, int) and 0 <= start < end, "trace window must be bounded")
    routes = trace.get("provider_read_routes")
    require(isinstance(routes, list) and routes, "passed scenario has no provider read routes")
    require(all(
        isinstance(route, dict)
        and route.get("method") == "GET"
        and isinstance(route.get("path"), str)
        and isinstance(route.get("ordinal"), int)
        and start <= route["ordinal"] < end
        for route in routes
    ), "provider read trace is outside its bounded window or is not a GET")
    require(all(
        isinstance(route, dict)
        and isinstance(route.get("method"), str)
        and isinstance(route.get("path"), str)
        and isinstance(route.get("ordinal"), int)
        and start <= route["ordinal"] < end
        for route in trace.get("provider_mutation_routes", [])
    ), "provider mutation trace is malformed or outside its bounded window")
    require(trace.get("provider_mutation_routes", []) == [], "refresh/import observation contains provider mutation")
    for window_name in ("refresh_only_windows", "normal_plan_windows"):
        windows = trace.get(window_name)
        require(isinstance(windows, list), f"missing {window_name}")
        if window_name == "normal_plan_windows":
            require(windows, "missing normal plan window")
        for window in windows:
            require(
                isinstance(window, dict)
                and isinstance(window.get("start_ordinal"), int)
                and isinstance(window.get("end_ordinal"), int)
                and window["start_ordinal"] < window["end_ordinal"]
                and start <= window["start_ordinal"] < window["end_ordinal"] <= end
                and window.get("mutation_routes") == [],
                f"invalid or mutating {window_name} observation",
            )


def validate_import_id(scenario: dict) -> None:
    value = scenario["provider_import_id"]
    resource = scenario["resource"]
    shape = IMPORT_ID_SHAPES[resource]
    if shape == "name":
        require(value == scenario["canonical_id"] and bool(re.fullmatch(r"[^/]+", value)), "import ID is not the canonical name")
    elif shape == "uuid":
        require(bool(re.fullmatch(UUID_RE, value)), "import ID is not a UUID")
        require(value.lower() == scenario["canonical_id"].lower(), "UUID import ID does not match canonical ID")
    else:
        require(bool(re.fullmatch(UUID_RE + "/" + UUID_RE, value)), "relationship import ID is not server/attachment UUID")
        expected = scenario.get("provider_import_components")
        require(isinstance(expected, dict), "composite import ID needs provider_import_components")
        require(value == f'{expected.get("server_id")}/{expected.get("attachment_id")}', "composite import ID components do not match state")


def validate_state_identity(scenario: dict) -> None:
    observation = scenario.get("provider_state_observation")
    require(isinstance(observation, dict), "passed scenario needs provider_state_observation")
    require(observation.get("observed") is True, "provider state identity was not observed")
    require(observation.get("source") in {"tofu_show_json_state", "tofu_state_json"}, "invalid provider state identity source")
    require(observation.get("state_id") == scenario["provider_state_id"], "provider_state_id is not bound to observed state")
    require(scenario["provider_state_id"], "provider state ID is missing")


def validate_verified_baseline(baseline: object, tested_sha: str) -> None:
    """Require an executed, commit-bound record for every accepted baseline gate."""

    require(isinstance(baseline, dict) and baseline.get("status") == "verified", "passed evidence needs a verified baseline")
    require(baseline.get("artifact_type") == "o3k-p13-2-4-baseline-gate-manifest", "baseline artifact type is invalid")
    require(isinstance(baseline.get("run_id"), str) and baseline["run_id"], "baseline evidence needs a run_id")
    require(baseline.get("source_commit") == tested_sha, "baseline evidence is not bound to tested_o3k_head_sha")
    gates = baseline.get("gates")
    require(isinstance(gates, list) and len(gates) == len(BASELINE_GATES), "baseline evidence needs exactly 11 gate results")
    require(baseline.get("gate_count") == len(BASELINE_GATES), "baseline gate_count is invalid")
    require(baseline.get("execution", {}).get("working_tree_clean_before") is True, "baseline did not start from a clean worktree")
    require(
        {gate.get("path") for gate in gates if isinstance(gate, dict)} == BASELINE_GATES,
        "baseline evidence does not cover the complete P13.2-P13.4 gate set",
    )
    require(len({gate.get("path") for gate in gates if isinstance(gate, dict)}) == len(BASELINE_GATES), "baseline gate paths are not unique")
    require(all(
        isinstance(gate, dict)
        and isinstance(gate.get("path"), str)
        and gate.get("result") == "passed"
        and gate.get("head_sha") == tested_sha
        and gate.get("exit_code") == 0
        for gate in gates
    ), "baseline gate evidence is incomplete or not bound to the tested commit")
    supplied_digest = baseline.get("evidence_sha256")
    require(isinstance(supplied_digest, str) and re.fullmatch(r"[0-9a-f]{64}", supplied_digest), "baseline evidence needs a valid digest binding")
    unsigned = dict(baseline)
    unsigned.pop("evidence_sha256", None)
    expected_digest = __import__("hashlib").sha256((__import__("json").dumps(unsigned, sort_keys=True, separators=(",", ":")) + "\n").encode()).hexdigest()
    require(supplied_digest == expected_digest, "baseline evidence digest does not match its contents")


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
            validate_state_identity(scenario)
            require(scenario["canonical_duplicate_count"] == 0, "passed scenario has duplicate canonical resources")
            require(scenario["canonical_resource_count"] == 1, "passed scenario must observe one canonical resource")
            require(scenario["cleanup_result"] == "passed", "passed scenario has unsuccessful cleanup")
            validate_plan_observation(scenario)
            validate_trace_observation(scenario)
            routes = scenario["trace_observation"]["provider_read_routes"]
            require(
                scenario["first_read_route"] == f'{routes[0]["method"]} {routes[0]["path"]}',
                "first_read_route does not match the trace",
            )
            require(IMPORT_ROUTES[scenario["resource"]].fullmatch(scenario["first_read_route"]), "first_read_route does not match the frozen contract route")
            identity = scenario.get("canonical_identity_observation")
            require(isinstance(identity, dict), "canonical_identity_observation must be an object")
            require(identity.get("source") == "canonical_store", "canonical identity is not store-observed")
            require(identity.get("count_source") == "canonical_store", "canonical count is not store-observed")
            require(identity.get("resource_id") == scenario["canonical_id"], "canonical identity ID mismatch")
            require(identity.get("owner_scope") == scenario["owner_scope"], "canonical owner scope mismatch")
            require(
                identity.get("observed_owner_scope") == scenario["owner_scope"],
                "observed canonical owner scope mismatch",
            )
            observations = {
                phase: identity.get(phase)
                for phase in ("before", "after_read", "after_cleanup")
            }
            require(
                all(isinstance(observation, dict) for observation in observations.values()),
                "canonical store observations must include before/read/cleanup snapshots",
            )
            require(
                all(observation.get("source") == "canonical_store" and observation.get("count_source") == "canonical_store"
                    for observation in observations.values()),
                "canonical snapshots must be store-backed",
            )
            require(observations["before"].get("requested_id") == scenario["canonical_id"], "canonical before ID mismatch")
            require(observations["after_read"].get("requested_id") == scenario["canonical_id"], "canonical read ID mismatch")
            require(observations["before"].get("count") == 1, "canonical before count must be exactly one")
            require(observations["after_read"].get("count") == 1, "canonical read count must be exactly one")
            require(observations["after_cleanup"].get("count") == 0, "canonical cleanup count must be zero")
            require(scenario.get("canonical_resource_count_after_read") == 1, "canonical read count is not emitted")
            require(scenario.get("canonical_resource_count_after_cleanup") == 0, "canonical cleanup count is not emitted")
            if scenario["scenario"] == "stable-read":
                require(len(scenario["refresh_plan_actions"]) >= 2, "stable-read requires repeated refresh plans")
                require(len(scenario["normal_plan_actions"]) >= 2, "stable-read requires repeated normal plans")
            else:
                require(scenario["provider_import_id"], "import scenario is missing provider_import_id")
                validate_import_id(scenario)
                require(len(scenario["normal_plan_actions"]) >= 1, "import requires a normal plan")
            if scenario["resource"] == "openstack_compute_volume_attach_v2" and scenario["scenario"] == "import":
                parent = scenario.get("canonical_parent_observation")
                require(isinstance(parent, dict) and parent.get("parent_retention") == "passed", "volume attachment parent retention is not proven")
                require(parent.get("server_status") == 200 and parent.get("volume_status") == 200, "volume attachment parents were not retained")
                require(parent.get("relationship_count_before") == 1 and parent.get("relationship_count_after_read") == 1, "volume attachment relationship changed during import")
            if scenario["resource"] == "openstack_networking_router_interface_v2" and scenario["scenario"] == "import":
                parent = scenario.get("canonical_parent_observation")
                require(isinstance(parent, dict) and parent.get("parent_retention") == "passed", "router interface parent retention is not proven")
                require(parent.get("router_status") == 200 and parent.get("target_subnet_status") == 200 and parent.get("target_network_status") == 200, "router interface parents were not retained")
        else:
            require(scenario["result"] != "passed", "non-passed scenario has passed result")
            require(scenario.get("reason"), "non-passed scenarios require a classification reason")
    require(document.get("status") in {"passed", "blocked"}, "invalid document status")
    if not allow_incomplete:
        require(document.get("status") == "passed", "strict validation rejects incomplete evidence")
    if document["status"] == "passed":
        require(document["execution_mode"] == "gated", "passed evidence must use gated execution")
        validate_verified_baseline(document.get("existing_p13_baseline"), tested_sha)
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
                "provider_state_observation": {"observed": True, "source": "tofu_show_json_state", "state_id": "keypair-name"},
                "plan_observation": {
                    "refresh-only": [{"format_version": "1.0", "resource_drift": [], "planned_values": {}, "prior_state": {}}],
                    "normal": [{"format_version": "1.0", "resource_changes": [], "planned_values": {}, "prior_state": {}}],
                },
                "trace_observation": {
                    "trace_start_ordinal": 0,
                    "trace_end_ordinal": 10,
                    "provider_read_routes": [{"method": "GET", "path": "/v2.1/project/os-keypairs/keypair-name", "ordinal": 1}],
                    "provider_mutation_routes": [],
                    "refresh_only_windows": [{"start_ordinal": 0, "end_ordinal": 3, "mutation_routes": []}],
                    "normal_plan_windows": [{"start_ordinal": 3, "end_ordinal": 10, "mutation_routes": []}],
                },
                "canonical_identity_observation": {
                    "resource_id": "keypair-name", "owner_scope": "project-a", "observed_owner_scope": "project-a",
                    "source": "canonical_store", "count_source": "canonical_store",
                    "before": {"source": "canonical_store", "count_source": "canonical_store", "requested_id": "keypair-name", "count": 1},
                    "after_read": {"source": "canonical_store", "count_source": "canonical_store", "requested_id": "keypair-name", "count": 1},
                    "after_cleanup": {"source": "canonical_store", "count_source": "canonical_store", "requested_id": "keypair-name", "count": 0},
                },
                "canonical_resource_count_after_read": 1,
                "canonical_resource_count_after_cleanup": 0,
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
    def expect_rejected(mutator, label: str) -> None:
        negative = json.loads(json.dumps(base))
        mutator(negative["scenarios"][0])
        try:
            validate(negative, allow_incomplete=True)
        except EvidenceValidationError:
            return
        raise EvidenceValidationError(f"self-test accepted {label}")

    expect_rejected(lambda scenario: scenario.update(plan_actions=["update"]), "non-no-op plan")
    expect_rejected(lambda scenario: scenario["plan_observation"]["normal"][0].pop("resource_changes"), "vacuous plan JSON")
    expect_rejected(lambda scenario: scenario["provider_state_observation"].update(observed=False), "fabricated provider state")
    expect_rejected(lambda scenario: scenario["trace_observation"].update(trace_end_ordinal=0), "unbounded trace")
    expect_rejected(lambda scenario: scenario["trace_observation"].update(provider_mutation_routes=[{"method": "DELETE", "path": "/v2.1/project/os-keypairs/keypair-name", "ordinal": 2}]), "provider mutation")
    expect_rejected(lambda scenario: scenario["canonical_identity_observation"].update(source="compatibility_projection_read"), "circular canonical observation")
    expect_rejected(lambda scenario: scenario.update(provider_import_id="not-the-canonical-name"), "wrong import identifier")
    try:
        validate_verified_baseline({"status": "verified"}, "0" * 40)
    except EvidenceValidationError:
        pass
    else:
        raise EvidenceValidationError("self-test accepted an unbound baseline")
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
