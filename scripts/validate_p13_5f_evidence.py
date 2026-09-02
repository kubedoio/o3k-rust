#!/usr/bin/env python3
"""Validate the P13.5F backend-parity aggregate evidence artifact."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


SHA1 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
BASELINE_GATES = 11
SCENARIOS = {
    "stable_read", "import", "mutable_drift_reconvergence", "native_deletion_recreation",
    "independent_replacement", "router_interface_relationship", "volume_attachment_relationship",
    "read_retry", "pre_commit_retry", "committed_update_response_loss", "committed_delete_response_loss",
    "operation_replay", "relationship_replay", "lost_create_response",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def validate(document: dict) -> None:
    require(document.get("artifact_type") == "o3k-p13-5f-backend-parity-aggregate-evidence", "invalid artifact_type")
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5", "invalid phase")
    require(document.get("final_aggregate_verdict") in {"passed", "blocked"}, "invalid aggregate verdict")
    require(SHA1.fullmatch(document.get("source_head_sha", "")) is not None, "invalid source_head_sha")
    toolchain = document.get("toolchain", {})
    require(toolchain.get("opentofu") == "1.12.6", "unexpected OpenTofu version")
    require(toolchain.get("provider") == "terraform-provider-openstack/openstack 3.4.0", "unexpected provider version")
    require(SHA256.fullmatch(toolchain.get("provider_binary_sha256", "")) is not None, "invalid provider hash")
    require(toolchain.get("provider_modified") is False, "provider must be unmodified")

    backends = document.get("backend_results", {})
    require(set(backends) == {"sqlite", "postgresql"}, "both backend results are required")
    for name, result in backends.items():
        require(result.get("provider_matrix") == "passed", f"{name} provider matrix did not pass")
        run = result.get("gated_provider_run", {})
        gates = run.get("gate_count", run.get("gates", run.get("scenarios")))
        passed = run.get("passed_gates", run.get("passed", run.get("final_plan_noop")))
        require(gates == BASELINE_GATES, f"{name} gate count is not {BASELINE_GATES}")
        require(passed == BASELINE_GATES, f"{name} does not pass every baseline gate")
        if name == "postgresql":
            require(run.get("database_isolation") == "fresh public schema before every gate", "PostgreSQL isolation evidence is missing")

    references = document.get("evidence_references", {})
    require(set(references) == {"p13_5a", "p13_5b", "p13_5c", "p13_5d", "p13_5e"}, "A-E evidence references are incomplete")
    repository = Path(__file__).resolve().parents[1]
    for name, reference in references.items():
        path = repository / reference
        require(path.is_file(), f"missing {name} evidence artifact: {reference}")
        require(path.suffix == ".json", f"{name} evidence reference is not JSON: {reference}")

    matrix = document.get("scenario_matrix", [])
    require(len(matrix) == len(SCENARIOS), "scenario matrix contains duplicate or missing rows")
    require({row.get("scenario") for row in matrix} == SCENARIOS, "scenario matrix does not cover P13.5 scenarios")
    for cells in matrix:
        scenario = cells["scenario"]
        cells = {key: value for key, value in cells.items() if key != "scenario"}
        require(set(cells) == {"sqlite", "postgresql"}, f"{scenario} is missing a backend")
        for backend, result in cells.items():
            expected = "expected_ambiguous" if scenario == "lost_create_response" else "passed"
            if document.get("final_aggregate_verdict") == "blocked" and result == "blocked":
                continue
            require(result == expected, f"{scenario}/{backend} result is {result!r}, expected {expected!r}")

    cleanup = document.get("cleanup", {})
    require(all(cleanup.get(key) == "passed" for key in ("local_server", "provider_matrix", "lvm_profile")), "cleanup evidence is incomplete")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    validate(json.loads(args.evidence.read_text(encoding="utf-8")))
    print("P13.5F aggregate evidence: PASS")


if __name__ == "__main__":
    main()
