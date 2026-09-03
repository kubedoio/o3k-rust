#!/usr/bin/env python3
"""Validate the PostgreSQL provider-level P13.5F scenario artifact."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
from pathlib import Path


SHA1 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
SCENARIOS = {
    "PG1-import-read-reconstruction",
    "PG2-mutable-drift-reconvergence",
    "PG3-remote-deletion-recreation",
    "PG4-independent-replacement",
    "PG5-router-interface-relationship",
    "PG6-volume-attachment-relationship",
    "PG7-operation-replay-unknown-outcome",
}
EVIDENCE_ONLY_PREFIXES = (
    # CI routing changes do not alter the tested runtime or harness semantics;
    # they only select the already committed evidence-validation gate.
    ".github/workflows/ci.yml",
    "scripts/validate_p13_5f_postgres_provider_parity.py",
    "docs/compatibility/p13-5/postgres-provider-parity/",
    "docs/compatibility/p13-5/p13-5f-backend-parity-aggregate-evidence.json",
    "docs/status/",
    "docs/compatibility/",
    "compatibility/product-profiles.yaml",
    "docs/P13_IMPLEMENTATION_PLAN.md",
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def validate(document: dict, artifact: Path) -> None:
    require(document.get("artifact_type") == "o3k-p13-5f-postgres-provider-parity", "invalid artifact_type")
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5F", "invalid phase")
    require(document.get("backend") == "postgresql", "backend identity is not PostgreSQL")
    require(document.get("provider_modified") is False, "provider must be unmodified")
    toolchain = document.get("toolchain", {})
    require(toolchain.get("opentofu") == "1.12.6", "OpenTofu pin is missing")
    require(toolchain.get("provider") == "terraform-provider-openstack/openstack 3.4.0", "provider pin is missing")
    require(SHA256.fullmatch(toolchain.get("provider_archive_sha256", "")) is not None, "provider archive SHA256 is missing")
    require(SHA256.fullmatch(toolchain.get("provider_binary_sha256", "")) is not None, "provider binary SHA256 is missing")
    require(SHA256.fullmatch(toolchain.get("opentofu_archive_sha256", "")) is not None, "OpenTofu archive SHA256 is missing")
    require(document.get("real_provider_execution") is True, "real provider execution is not proven")
    head = document.get("tested_runtime_head_sha", "")
    require(SHA1.fullmatch(head) is not None, "invalid tested_runtime_head_sha")
    repository = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
    current_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip()
    require(SHA1.fullmatch(current_head) is not None, "invalid current source head")
    if head != current_head:
        require(
            subprocess.run(["git", "merge-base", "--is-ancestor", head, current_head], cwd=repository).returncode == 0,
            "tested runtime head is not an ancestor of the evidence/review head",
        )
        changed = subprocess.check_output(
            ["git", "diff", "--name-only", f"{head}..{current_head}"], cwd=repository, text=True
        ).splitlines()
        require(changed, "evidence/review head differs without a descendant diff")
        require(
            all(any(path == prefix.rstrip("/") or path.startswith(prefix) for prefix in EVIDENCE_ONLY_PREFIXES) for path in changed),
            "post-tested-head changes include runtime or harness files",
        )

    execution = document.get("execution", {})
    require(execution.get("orchestrator") == "tests/p13_5f_postgres_provider_parity.sh", "invalid orchestrator binding")
    require(execution.get("status") in {"blocked", "failed", "passed"}, "invalid execution status")
    rows = document.get("scenarios", [])
    require(len(rows) == len(SCENARIOS), "scenario count is incomplete")
    require({row.get("scenario") for row in rows} == SCENARIOS, "scenario set is incomplete")
    for row in rows:
        require(row.get("result") in {"not_run", "blocked", "failed", "passed"}, f"invalid result: {row}")
        if row["result"] == "passed":
            require(row.get("externally_equivalent") is True, f"passed row lacks parity equivalence: {row['scenario']}")
            evidence_name = row.get("evidence", "")
            require(evidence_name and not Path(evidence_name).is_absolute(), f"evidence must be repository-relative: {row['scenario']}")
            evidence = repository / evidence_name
            require(evidence.is_file(), f"passed row lacks evidence: {row['scenario']}")
            child = json.loads(evidence.read_text(encoding="utf-8"))
            require(child.get("tested_runtime_head_sha") == head, f"child evidence head mismatch: {row['scenario']}")
            require(child.get("backend") == "postgresql", f"child evidence backend mismatch: {row['scenario']}")
            require(child.get("provider_modified") is False, f"child provider provenance is incomplete: {row['scenario']}")
            require(child.get("real_provider_execution") is True, f"child real-provider proof is missing: {row['scenario']}")
            require(child.get("result") == "passed", f"child evidence is not passed: {row['scenario']}")
            require(child.get("externally_equivalent") is True, f"child parity equivalence is missing: {row['scenario']}")
            require(row.get("backend") == "postgresql", f"row backend is not PostgreSQL: {row['scenario']}")
            require(row.get("provider_modified") is False, f"row provider provenance is incomplete: {row['scenario']}")
            require(row.get("restart_reconstruction") is True, f"row lacks restart reconstruction: {row['scenario']}")
            require(row.get("final_plan_noop") is True, f"row lacks final no-op: {row['scenario']}")
            if row["scenario"] in {"PG5-router-interface-relationship", "PG6-volume-attachment-relationship"}:
                require(row.get("parents_preserved") is True, f"parents were not preserved: {row['scenario']}")
                require(row.get("provider_leaks") == 0 and row.get("foreign_changes") == 0, f"relationship isolation failed: {row['scenario']}")
            if row["scenario"] == "PG4-independent-replacement":
                require(row.get("replacement_actions") or row.get("plan_actions"), "replacement actions are missing")
            if row["scenario"] == "PG7-operation-replay-unknown-outcome":
                require(row.get("fault_location") == "after_commit_before_response", "PG7 fault location is missing")
                require(row.get("backend_completion_observed") is True, "PG7 backend completion is missing")
                require(row.get("restart_boundary") is True, "PG7 restart boundary is missing")
    expected_verdict = "passed" if all(row["result"] == "passed" for row in rows) else "blocked"
    require(document.get("final_verdict") == expected_verdict, "final verdict does not match scenario results")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    validate(json.loads(args.evidence.read_text(encoding="utf-8")), args.evidence.resolve())
    print("P13.5F PostgreSQL provider parity evidence: PASS")


if __name__ == "__main__":
    main()
