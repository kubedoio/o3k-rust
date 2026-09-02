#!/usr/bin/env python3
"""Validate machine-readable P13.5D replacement/relationship evidence."""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

SHA = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    raise SystemExit(f"P13.5D evidence invalid: {message}")


def validate(doc: dict) -> None:
    if doc.get("artifact_type") != "o3k-p13-5d-replacement-relationship-evidence":
        fail("wrong artifact type")
    if doc.get("phase") != "P13.5D":
        fail("wrong phase")
    if doc.get("provider_modified") is not False:
        fail("provider_modified must be false")
    if not SHA.fullmatch(doc.get("tested_o3k_head_sha", "")):
        fail("missing exact tested head SHA")
    if doc.get("toolchain", {}).get("opentofu") != "1.12.6":
        fail("wrong OpenTofu version")
    if doc.get("toolchain", {}).get("provider") != "terraform-provider-openstack/openstack 3.4.0":
        fail("wrong provider version")
    scenarios = doc.get("scenarios")
    if not isinstance(scenarios, list):
        fail("scenarios are missing")
    if doc.get("aggregate_verdict") == "BLOCKED":
        if not doc.get("reason"):
            fail("blocked evidence needs a reason")
        return
    if not scenarios:
        fail("scenarios are missing")
    if doc.get("aggregate_verdict") == "PASS":
        required = {"independent-resource", "router-interface", "volume-attachment"}
        seen = {row.get("scenario") for row in scenarios}
        if not required <= seen:
            fail("aggregate PASS lacks required independent and relationship scenarios")
    for row in scenarios:
        for key in ("resource", "scenario", "plan_actions", "parent_ids_before", "parent_ids_after", "restart_reconstruction"):
            if key not in row:
                fail(f"scenario lacks {key}")
        if row.get("state_surgery") is True or row.get("state_mutation") is True:
            fail("evidence must not use Terraform/OpenTofu state surgery")
        if row["restart_reconstruction"] is not True:
            fail("scenario lacks restart/reconstruction proof")
        if row["scenario"] in {"router-interface", "volume-attachment"}:
            if row.get("parents_preserved") is not True:
                fail("relationship scenario does not prove parent preservation")
            if not isinstance(row.get("parent_ids_before"), dict) or not isinstance(row.get("parent_ids_after"), dict):
                fail("relationship parent identity maps are invalid")
        if row.get("result") == "passed":
            for key in ("old_relationship_absent", "new_relationship_count", "provider_leaks", "foreign_changes", "final_plan_noop"):
                if key not in row:
                    fail(f"passed scenario lacks {key}")
            if row["old_relationship_absent"] is not True:
                fail("passed scenario still observes the old relationship")
            if row["new_relationship_count"] != 1 or row["provider_leaks"] != 0 or row["foreign_changes"] != 0:
                fail("passed scenario has cardinality/leak/foreign-state failure")
            if row["final_plan_noop"] is not True:
                fail("passed scenario does not converge to no-op")
    if doc.get("aggregate_verdict") == "PASS" and any(row.get("result") != "passed" for row in scenarios):
        fail("aggregate PASS contains non-passed scenarios")
    if doc.get("aggregate_verdict") == "PASS" and len(scenarios) < 3:
        fail("aggregate PASS lacks independent, router-interface, and volume-attachment coverage")


def self_test() -> None:
    row = {
        "resource": "openstack_networking_router_interface_v2",
        "scenario": "router-interface",
        "plan_actions": [], "parent_ids_before": {}, "parent_ids_after": {},
        "restart_reconstruction": True,
        "parents_preserved": True, "old_relationship_absent": True,
        "new_relationship_count": 1, "provider_leaks": 0, "foreign_changes": 0,
        "final_plan_noop": True, "result": "passed",
    }
    independent = dict(row, resource="openstack_networking_network_v2", scenario="independent-resource")
    attachment = dict(row, resource="openstack_compute_volume_attach_v2", scenario="volume-attachment")
    validate({"artifact_type": "o3k-p13-5d-replacement-relationship-evidence", "phase": "P13.5D",
              "provider_modified": False, "tested_o3k_head_sha": "0" * 40,
              "toolchain": {"opentofu": "1.12.6", "provider": "terraform-provider-openstack/openstack 3.4.0"},
              "scenarios": [independent, row, attachment], "aggregate_verdict": "PASS"})
    print("P13.5D evidence validator self-test: PASS")


parser = argparse.ArgumentParser()
parser.add_argument("evidence", nargs="?")
parser.add_argument("--self-test", action="store_true")
args = parser.parse_args()
if args.self_test:
    self_test()
elif args.evidence:
    validate(json.loads(Path(args.evidence).read_text(encoding="utf-8")))
    print("P13.5D evidence validation: PASS")
else:
    parser.error("provide evidence or --self-test")
