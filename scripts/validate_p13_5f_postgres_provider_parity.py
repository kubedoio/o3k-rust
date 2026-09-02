#!/usr/bin/env python3
"""Validate the PostgreSQL provider-level P13.5F scenario artifact."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


SHA1 = re.compile(r"^[0-9a-f]{40}$")
SCENARIOS = {
    "PG1-import-read-reconstruction",
    "PG2-mutable-drift-reconvergence",
    "PG3-remote-deletion-recreation",
    "PG4-independent-replacement",
    "PG5-router-interface-relationship",
    "PG6-volume-attachment-relationship",
    "PG7-operation-replay-unknown-outcome",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def validate(document: dict, artifact: Path) -> None:
    require(document.get("artifact_type") == "o3k-p13-5f-postgres-provider-parity", "invalid artifact_type")
    require(document.get("schema_version") == 1, "unsupported schema_version")
    require(document.get("phase") == "P13.5F", "invalid phase")
    require(document.get("backend") == "postgresql", "backend identity is not PostgreSQL")
    require(document.get("provider_modified") is False, "provider must be unmodified")
    head = document.get("tested_o3k_head_sha", "")
    require(SHA1.fullmatch(head) is not None, "invalid tested_o3k_head_sha")
    repository = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
    expected_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip()
    require(head == expected_head, "artifact is not bound to the current exact source head")

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
            evidence = Path(row.get("evidence", ""))
            require(evidence.is_file(), f"passed row lacks evidence: {row['scenario']}")
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
