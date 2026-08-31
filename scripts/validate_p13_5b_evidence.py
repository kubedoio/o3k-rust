#!/usr/bin/env python3
"""Validate machine-readable P13.5B refresh/import evidence."""

from __future__ import annotations

import argparse
import json
import re
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


def validate(document: dict) -> None:
    assert document["artifact_type"] == "o3k-p13-5b-refresh-import-evidence"
    assert document["schema_version"] == 1
    assert document["phase"] == "P13.5B"
    assert document["profile"] == "p13-iac-compatibility-v1"
    assert document["toolchain"]["opentofu"] == "1.12.6"
    assert document["toolchain"]["provider"] == (
        "terraform-provider-openstack/openstack 3.4.0"
    )
    assert document["toolchain"]["provider_modified"] is False
    assert document["manual_state_edits"] is False
    assert document["canonical_authority"] == "o3k"
    assert document["execution_mode"] in {"gated", "exploratory_blocked_baseline"}
    scenarios = document["scenarios"]
    assert isinstance(scenarios, list) and scenarios
    expected_pairs = {(resource, scenario) for resource in RESOURCES for scenario in SCENARIOS}
    actual_pairs = {(item.get("resource"), item.get("scenario")) for item in scenarios}
    assert actual_pairs == expected_pairs
    assert len(actual_pairs) == len(scenarios)
    assert re.fullmatch(r"[0-9a-f]{40}", document["tested_o3k_head_sha"])
    contract = json.loads(Path("docs/compatibility/p13-5/p13-5a-convergence-contract.json").read_text())
    assert set(contract["resources"][i]["resource"] for i in range(len(contract["resources"]))) == RESOURCES
    assert document["toolchain"]["opentofu_archive_sha256"] == contract["toolchain"]["opentofu_archive_sha256"]
    assert document["toolchain"]["provider_archive_sha256"] == contract["toolchain"]["provider_archive_sha256"]
    assert document["toolchain"]["provider_binary_sha256"] == contract["toolchain"]["provider_binary_sha256"]
    assert document["toolchain"]["provider_sha256"] == contract["toolchain"]["provider_binary_sha256"]
    contract_imports = {item["resource"]: item["import"] for item in contract["resources"]}
    for scenario in scenarios:
        assert REQUIRED <= scenario.keys()
        assert scenario["resource"] in RESOURCES
        assert scenario["scenario"] in SCENARIOS
        assert scenario["result"] in RESULTS
        if scenario["result"] == "upstream_provider_unsupported":
            assert contract_imports[scenario["resource"]] == "unsupported"
        assert re.fullmatch(r"[0-9a-f]{40}", scenario["head_sha"])
        assert scenario["head_sha"] == document["tested_o3k_head_sha"]
        assert isinstance(scenario["plan_actions"], list)
        for field in ("refresh_plan_actions", "normal_plan_actions"):
            if field in scenario:
                assert all(isinstance(actions, list) for actions in scenario[field])
        assert isinstance(scenario["final_plan_noop"], bool)
        assert scenario["canonical_duplicate_count"] is None or isinstance(
            scenario["canonical_duplicate_count"], int
        )
        if scenario["result"] == "passed":
            # OpenTofu omits resource_changes for a no-op plan, so the
            # structured action list is empty.  Keep this as [] rather than
            # inferring a no-op from CLI text.
            assert scenario["plan_actions"] in ([], ["no-op"])
            for field in ("refresh_plan_actions", "normal_plan_actions"):
                if field in scenario:
                    assert all(actions in ([], ["no-op"]) for actions in scenario[field])
            assert scenario["final_plan_noop"] is True
            assert scenario["canonical_id"]
            assert scenario["owner_scope"]
            assert scenario["canonical_duplicate_count"] == 0
            assert scenario["canonical_resource_count"] == 1
            assert scenario["cleanup_result"] == "passed"
            routes = scenario["trace_observation"]["provider_read_routes"]
            assert routes
            assert scenario["first_read_route"] == f'{routes[0]["method"]} {routes[0]["path"]}'
            assert scenario["canonical_identity_observation"]["resource_id"] == scenario["canonical_id"]
            assert scenario["canonical_identity_observation"]["owner_scope"] == scenario["owner_scope"]
            assert scenario["canonical_identity_observation"]["observed_owner_scope"] == scenario["owner_scope"]
            if scenario["scenario"] == "stable-read":
                assert len(scenario["refresh_plan_actions"]) >= 2
                assert len(scenario["normal_plan_actions"]) >= 2
            else:
                assert scenario["provider_import_id"]
                assert len(scenario["normal_plan_actions"]) >= 1
        else:
            assert scenario["result"] != "passed"
    assert document["status"] in {"passed", "blocked"}
    if document["status"] == "passed":
        assert document["execution_mode"] == "gated"
        assert document["existing_p13_baseline"]["status"] == "verified"
        assert all(s["result"] == "passed" for s in scenarios)


def self_test() -> None:
    base = {
        "artifact_type": "o3k-p13-5b-refresh-import-evidence",
        "schema_version": 1,
        "phase": "P13.5B",
        "profile": "p13-iac-compatibility-v1",
        "status": "blocked",
        "tested_o3k_head_sha": "0" * 40,
        "execution_mode": "exploratory_blocked_baseline",
        "existing_p13_baseline": {"status": "blocked"},
        "manual_state_edits": False,
        "canonical_authority": "o3k",
        "toolchain": {
            "opentofu": "1.12.6",
            "provider": "terraform-provider-openstack/openstack 3.4.0",
            "opentofu_archive_sha256": "50a6106fa4de523d09c87af85f3db1dd47535fc005727fdca6852146476b88ec",
            "provider_archive_sha256": "11b3c88e24197a29b13cf5ab41771944bd16707b561645323e8cbb4f1da00b7b",
            "provider_binary_sha256": "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc",
            "provider_modified": False,
        },
        "scenarios": [
            {
                "resource": "openstack_compute_keypair_v2",
                "scenario": "import",
                "canonical_id": "keypair-name",
                "owner_scope": "project-a",
                "provider_import_id": "keypair-name",
                "first_read_route": "GET /v2.1/{project_id}/os-keypairs/{name}",
                "plan_actions": ["no-op"],
                "refresh_plan_actions": [],
                "normal_plan_actions": [["no-op"]],
                "final_plan_noop": True,
                "canonical_duplicate_count": 0,
                "canonical_resource_count": 1,
                "cleanup_result": "passed",
                "backend": "sqlite",
                "head_sha": "0" * 40,
                "trace_observation": {"provider_read_routes": [{"method": "GET", "path": "/v2.1/project/os-keypairs/keypair-name"}]},
                "canonical_identity_observation": {"resource_id": "keypair-name"},
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
                }
            )
    validate(base)
    negative = json.loads(json.dumps(base))
    negative["scenarios"][0]["plan_actions"] = ["update"]
    try:
        validate(negative)
    except AssertionError:
        pass
    else:
        raise AssertionError("non-no-op passed scenario was accepted")
    print("P13.5B evidence validator self-test: PASS")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence", nargs="?")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.evidence:
        parser.error("evidence is required unless --self-test is used")
    document = json.loads(Path(args.evidence).read_text())
    validate(document)
    print("P13.5B evidence structure: PASS")


if __name__ == "__main__":
    main()
