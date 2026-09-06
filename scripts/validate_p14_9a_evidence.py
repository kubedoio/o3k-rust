#!/usr/bin/env python3
"""Fail-closed validator for the P14.9A process/evidence runner."""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ARTIFACT = "o3k-p14-9a-evidence"
PROFILE = "p14-openstack-cold-migration-v1"
GATES = [f"G{i:02d}" for i in range(1, 21)]
HEX40 = re.compile(r"^[0-9a-f]{40}$")
SECRET = re.compile(
    r"(-----BEGIN|bearer\s+[A-Za-z0-9._-]{8,}|eyJ[A-Za-z0-9_-]{8,}\.|"
    r"(password|token|private[_-]?key|secret|credential)\s*[:=]\s*[^<\s]{8,})",
    re.I,
)


class Failure(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def walk(value, path="$", findings=None):
    findings = [] if findings is None else findings
    if isinstance(value, str) and SECRET.search(value):
        findings.append(path)
    elif isinstance(value, dict):
        for key, child in value.items():
            if re.search(r"(password|token|private[_-]?key|secret|credential)", key, re.I):
                findings.append(f"{path}.{key}")
                continue
            walk(child, f"{path}.{key}", findings)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            walk(child, f"{path}[{index}]", findings)
    return findings


def validate(document: dict) -> None:
    require(document.get("artifact_type") == ARTIFACT, "artifact_type mismatch")
    require(document.get("schema_version") == 1, "schema_version must be 1")
    require(document.get("profile") == PROFILE, "profile mismatch")
    require(HEX40.fullmatch(document.get("tested_runtime_head_sha", "")), "runtime HEAD is not a SHA-1")
    require(document.get("execution_result") in {"passed", "blocked", "failed"}, "invalid execution result")
    require(isinstance(document.get("source"), dict), "source identity missing")
    require(isinstance(document.get("destination"), dict), "destination identity missing")
    require(document["source"].get("profile") == "openstack-source", "source profile mismatch")
    require(document["destination"].get("profile") in {"native-rust-testlab", "small-edge-cloud"}, "destination profile mismatch")
    toolchain = document.get("toolchain", {})
    require(toolchain.get("opentofu") == "1.12.6", "OpenTofu pin mismatch")
    require(toolchain.get("provider") == "terraform-provider-openstack/openstack 3.4.0", "provider pin mismatch")
    require(toolchain.get("provider_modified") is False, "provider must be unmodified")
    gates = document.get("gates")
    require(isinstance(gates, list) and [row.get("gate") for row in gates] == GATES, "mandatory gate set/order mismatch")
    for row in gates:
        require(row.get("result") in {"passed", "blocked", "failed"}, f"invalid result for {row.get('gate')}")
        require(isinstance(row.get("evidence_ref"), str) and row["evidence_ref"].strip(), f"missing evidence ref for {row.get('gate')}")
        if row["result"] == "passed":
            require(row.get("source_bound") is True, f"{row['gate']} is not source-bound")
            require(row.get("destination_bound") is True, f"{row['gate']} is not destination-bound")
            require(isinstance(row.get("proof"), dict) and row["proof"], f"{row['gate']} has no proof")
    summary = document.get("summary", {})
    for field in ("owned_leaks", "inconsistencies", "foreign_state_changes"):
        require(isinstance(summary.get(field), int) and summary[field] >= 0, f"invalid {field}")
    findings = walk(document)
    if findings:
        raise Failure(f"secret-shaped value at {findings[0]}")
    if document["execution_result"] == "passed":
        require(document.get("environment_ready") is True, "PASS requires environment_ready")
        require(all(row["result"] == "passed" for row in gates), "PASS requires all gates passed")
        require(summary == {"owned_leaks": 0, "inconsistencies": 0, "foreign_state_changes": 0}, "PASS requires zero cleanup findings")
        require(document.get("opentofu", {}).get("final_plan") == "NO-OP", "PASS requires process NO-OP")
    else:
        require(isinstance(document.get("environment_ready"), bool), "environment_ready must be boolean")


def blocked_document() -> dict:
    return {
        "artifact_type": ARTIFACT,
        "schema_version": 1,
        "phase": "P14.9A",
        "profile": PROFILE,
        "tested_runtime_head_sha": "0" * 40,
        "source": {"profile": "openstack-source", "endpoint_fingerprint": "redacted"},
        "destination": {"profile": "native-rust-testlab", "endpoint_fingerprint": "redacted"},
        "toolchain": {"opentofu": "1.12.6", "provider": "terraform-provider-openstack/openstack 3.4.0", "provider_modified": False},
        "environment_ready": False,
        "execution_result": "blocked",
        "gates": [{"gate": gate, "result": "blocked", "evidence_ref": f"prerequisite:{gate}"} for gate in GATES],
        "summary": {"owned_leaks": 0, "inconsistencies": 0, "foreign_state_changes": 0},
        "opentofu": {"final_plan": "not-run"},
    }


def self_test() -> None:
    document = blocked_document()
    validate(document)
    tampered = json.loads(json.dumps(document))
    tampered["execution_result"] = "passed"
    try:
        validate(tampered)
    except Failure:
        pass
    else:
        raise Failure("tampered PASS was accepted")
    tampered = json.loads(json.dumps(document))
    tampered["gates"][0]["proof"] = {"token": "not-a-real-token"}
    try:
        validate(tampered)
    except Failure:
        pass
    else:
        raise Failure("secret-shaped evidence was accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", nargs="?")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test()
            print("P14.9A evidence validator self-test: PASS")
            return 0
        require(args.path is not None, "evidence path is required")
        document = json.loads(pathlib.Path(args.path).read_text(encoding="utf-8"))
        validate(document)
        print(f"P14.9A evidence: {document['execution_result'].upper()}")
        return 0
    except (Failure, OSError, json.JSONDecodeError) as error:
        print(f"P14.9A evidence validation failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
