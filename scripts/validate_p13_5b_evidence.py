#!/usr/bin/env python3
"""Validate machine-readable P13.5B refresh/import evidence."""

from __future__ import annotations

import argparse
import json
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
    scenarios = document["scenarios"]
    assert isinstance(scenarios, list) and scenarios
    for scenario in scenarios:
        assert REQUIRED <= scenario.keys()
        assert scenario["resource"] in RESOURCES
        assert scenario["scenario"] in SCENARIOS
        assert scenario["result"] in RESULTS
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
            assert scenario["canonical_duplicate_count"] == 0
            assert scenario["canonical_resource_count"] == 1
            assert scenario["cleanup_result"] == "passed"
        else:
            assert scenario["result"] != "passed"
    assert document["status"] in {"passed", "blocked"}
    if document["status"] == "passed":
        assert all(s["result"] == "passed" for s in scenarios)


def self_test() -> None:
    base = {
        "artifact_type": "o3k-p13-5b-refresh-import-evidence",
        "schema_version": 1,
        "phase": "P13.5B",
        "profile": "p13-iac-compatibility-v1",
        "status": "passed",
        "manual_state_edits": False,
        "canonical_authority": "o3k",
        "toolchain": {
            "opentofu": "1.12.6",
            "provider": "terraform-provider-openstack/openstack 3.4.0",
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
                "result": "passed",
            }
        ],
    }
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
